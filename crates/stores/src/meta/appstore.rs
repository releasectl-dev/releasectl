//! App Store Connect の掲載情報（Go 版 `internal/storemeta/appstore.go`）: ロケールごとの appInfo と
//! version のローカライズ、審査情報、年齢区分。アプリの設定は appstore_app.rs、画像は
//! appstore_shots.rs / appstore_creatives.rs。

use super::{changed_attrs, keys, non_editable_attrs, put_if};
use crate::Reconciler;
use crate::appstore::{Resource, attr, created_id, get_all, parse_list};
use crate::http::{Client, Response};
use releasectl_core::meta::MetaConfig;
use releasectl_core::{AppStoreVersion, Report, Status};
use serde_json::{Map, Value, json};
use std::collections::HashMap;

pub(super) struct MetaAppStore {
    pub(super) cfg: MetaConfig,
    pub(super) cli: Client,
}

pub(super) fn build(cfg: &MetaConfig) -> Result<Box<dyn Reconciler>, String> {
    // 画像・動画は do_raw（アップロード用の長いタイムアウト）で送る。
    let cli = crate::appstore::new_client()?;
    Ok(Box::new(MetaAppStore {
        cfg: cfg.clone(),
        cli,
    }))
}

/// 掲載情報を編集できる版 / appInfo の状態。
const EDITABLE_STATES: [&str; 5] = [
    "PREPARE_FOR_SUBMISSION",
    "DEVELOPER_REJECTED",
    "REJECTED",
    "METADATA_REJECTED",
    "INVALID_BINARY",
];

pub(super) fn is_editable(state: &str) -> bool {
    EDITABLE_STATES.contains(&state)
}

/// 状態の属性（旧 appStoreState と、新しい state / appVersionState のどれでも）。
pub(super) fn state_of(r: &Resource) -> &str {
    ["appStoreState", "state", "appVersionState"]
        .iter()
        .map(|k| attr(r, k))
        .find(|s| !s.is_empty())
        .unwrap_or("")
}

/// 既存のローカライズ: locale → (id, 今の属性)。
pub(super) type Locs = HashMap<String, (String, Value)>;

/// 年齢区分 "none": すべての申告を最小にする。属性の名前が変わっても、その項目の 400 で止まるだけ。
const AGE_RATING_NONE: [(&str, Value); 14] = [
    ("alcoholTobaccoOrDrugUseOrReferences", Value::Null),
    ("contests", Value::Null),
    ("gamblingSimulated", Value::Null),
    ("medicalOrTreatmentInformation", Value::Null),
    ("profanityOrCrudeHumor", Value::Null),
    ("sexualContentGraphicAndNudity", Value::Null),
    ("sexualContentOrNudity", Value::Null),
    ("horrorOrFearThemes", Value::Null),
    ("matureOrSuggestiveThemes", Value::Null),
    ("violenceCartoonOrFantasy", Value::Null),
    ("violenceRealistic", Value::Null),
    ("violenceRealisticProlongedGraphicOrSadistic", Value::Null),
    ("gambling", Value::Bool(false)),
    ("unrestrictedWebAccess", Value::Bool(false)),
];

fn age_rating_none() -> Map<String, Value> {
    AGE_RATING_NONE
        .iter()
        .map(|(k, v)| {
            let v = if v.is_null() {
                Value::String("NONE".into())
            } else {
                v.clone()
            };
            (k.to_string(), v)
        })
        .collect()
}

impl Reconciler for MetaAppStore {
    fn name(&self) -> &'static str {
        "appstore"
    }

    fn export(&self, opts: &crate::ExportOpts, r: &mut Report) -> Result<Value, String> {
        self.export_store(opts, r)
    }

    fn reconcile(&self, apply: bool, r: &mut Report) -> Result<(), String> {
        // target が API の内部 ID（appInfo / 版の ID）で、アプリか版に 1 つしかない op は key に target を入れない。
        // どの版かは JSON の appStoreVersion で分かる。
        r.set_key_singleton(&[
            "app",
            "appInfo",
            "ageRating",
            "contentRights",
            "price",
            "copyright",
            "reviewDetail",
            "version",
            "screenshots",
        ]);
        let bundle = self.cfg.bundle_id.as_str();
        if bundle.is_empty() {
            r.add("app", &self.cfg.app_name, Status::Skip, "bundleId not set");
            return Ok(());
        }
        let Some(app_id) = self.find_app_id()? else {
            r.add(
                "app",
                bundle,
                Status::Skip,
                "no App Store app for this bundle id",
            );
            return Ok(());
        };
        r.store_id("appId", &app_id);

        // appInfo: name / subtitle / privacyPolicyUrl と、年齢区分（appInfo にぶら下がる）。
        let app_info = match self.find_editable_app_info(&app_id) {
            Ok(Some(id)) => {
                self.reconcile_app_info_localizations(apply, &id, r);
                Some(id)
            }
            Ok(None) => {
                r.add("appInfo", bundle, Status::Skip, "no editable app info");
                None
            }
            Err(e) => {
                r.add("appInfo", bundle, Status::Error, e);
                None
            }
        };
        self.reconcile_age_rating(apply, app_info.as_deref(), r);
        self.reconcile_categories(apply, app_info.as_deref(), r);
        self.reconcile_content_rights(apply, &app_id, r);
        self.reconcile_price(apply, &app_id, r);

        // version: description / keywords / whatsNew / URL / 著作権表示 と審査情報。
        let found = match self.find_version(&app_id) {
            Ok(v) => v,
            Err(e) => {
                r.add("version", bundle, Status::Error, e);
                return Ok(());
            }
        };
        let (version_id, version_attrs) = match found {
            Pick::Found { id, attrs, note } => {
                r.app_store_version = Some(version_info(&attrs, false));
                if !note.is_empty() {
                    r.add("version", &self.cfg.app_store_version, Status::Exists, note);
                }
                (id, attrs)
            }
            Pick::Missing(note) => {
                r.add("version", bundle, Status::Skip, note);
                return Ok(());
            }
            Pick::Create => {
                if !apply {
                    self.plan_new_version(r);
                    return Ok(());
                }
                match self.create_version(&app_id) {
                    Ok(v) => {
                        r.add(
                            "version",
                            &self.cfg.app_store_version,
                            Status::Created,
                            "new",
                        );
                        r.app_store_version = Some(version_info(&v.1, true));
                        v
                    }
                    Err(e) => {
                        r.add("version", &self.cfg.app_store_version, Status::Error, e);
                        return Ok(());
                    }
                }
            }
            Pick::Blocked(note) => {
                r.add("version", &self.cfg.app_store_version, Status::Error, note);
                return Ok(());
            }
        };

        self.reconcile_version_localizations(apply, &version_id, r);
        self.reconcile_copyright(apply, &version_id, &version_attrs, r);
        self.reconcile_review_detail(apply, &version_id, r);
        self.reconcile_screenshots(apply, &version_id, r);
        self.reconcile_creatives(apply, &app_id, &version_id, r);
        Ok(())
    }
}

impl MetaAppStore {
    pub(super) fn find_app_id(&self) -> Result<Option<String>, String> {
        let resp = self.cli.r#do(
            "GET",
            &format!("/v1/apps?filter[bundleId]={}", self.cfg.bundle_id),
            None,
        )?;
        if !resp.ok() {
            return Err(format!("list apps: {}", resp.error()));
        }
        let (data, _) = parse_list(&resp)?;
        Ok(data.into_iter().next().map(|r| r.id))
    }

    /// 編集できる状態の appInfo。どれも編集できる状態でなければ最初のもの。
    fn find_editable_app_info(&self, app_id: &str) -> Result<Option<String>, String> {
        let resp = self
            .cli
            .r#do("GET", &format!("/v1/apps/{app_id}/appInfos?limit=50"), None)?;
        if !resp.ok() {
            return Err(format!("list app infos: {}", resp.error()));
        }
        let (data, _) = parse_list(&resp)?;
        Ok(data
            .iter()
            .find(|r| is_editable(state_of(r)))
            .or(data.first())
            .map(|r| r.id.clone()))
    }

    /// 編集する版を選ぶ。`appStoreVersion` を指定していれば versionString で、無ければ最初の編集できる版。
    fn find_version(&self, app_id: &str) -> Result<Pick, String> {
        let resp = self.cli.r#do(
            "GET",
            &format!("/v1/apps/{app_id}/appStoreVersions?limit=50"),
            None,
        )?;
        if !resp.ok() {
            return Err(format!("list versions: {}", resp.error()));
        }
        let (data, _) = parse_list(&resp)?;
        Ok(pick_version(
            &data,
            &self.cfg.app_store_version,
            self.cfg.create_app_store_version,
        ))
    }

    /// 版を作る（`POST /v1/appStoreVersions`）。作った版の (id, 属性) を返す。
    fn create_version(&self, app_id: &str) -> Result<(String, Value), String> {
        let body = version_create_body(app_id, &self.cfg.app_store_version);
        let resp = self.cli.r#do("POST", "/v1/appStoreVersions", Some(&body))?;
        if !resp.ok() {
            return Err(format!("create version: {}", resp.error()));
        }
        let v: Value = resp.json()?;
        let id = v
            .pointer("/data/id")
            .and_then(Value::as_str)
            .ok_or("create version: no id in the response")?
            .to_string();
        let attrs = v
            .pointer("/data/attributes")
            .cloned()
            .unwrap_or(Value::Null);
        Ok((id, attrs))
    }

    /// 版がまだ無い plan: 版の作成と、版の中で作るものを出す。版が無いので読めず、どれも `+` になる。
    /// スクリーンショット・クリエイティブはローカライズを作った後でないと比べられない（初めてのロケールと同じ）。
    fn plan_new_version(&self, r: &mut Report) {
        let want = self.cfg.app_store_version.as_str();
        // まだ無い版。state は空（plan で作る予定）。
        r.app_store_version = Some(AppStoreVersion {
            version_string: want.to_string(),
            state: String::new(),
            created: false,
        });
        r.add("version", want, Status::Create, "new");
        self.version_localizations_with(false, want, &Locs::new(), r);
        self.reconcile_copyright(false, want, &Value::Null, r);
        if self.cfg.review.is_some() {
            r.add("reviewDetail", want, Status::Create, "new");
        }
        if let Some(shots) = &self.cfg.screenshots
            && !(shots.appstore.is_empty() && shots.creatives.is_empty())
        {
            r.add(
                "screenshot",
                want,
                Status::Skip,
                format!("compared after version {want} is created"),
            );
        }
    }

    // --- ローカライズ ---

    fn reconcile_app_info_localizations(&self, apply: bool, app_info_id: &str, r: &mut Report) {
        let existing = match self.list_localizations(&format!(
            "/v1/appInfos/{app_info_id}/appInfoLocalizations?limit=50"
        )) {
            Ok(e) => e,
            Err(e) => {
                r.add("appInfoLoc", app_info_id, Status::Error, e);
                return;
            }
        };
        for loc in &self.cfg.locales {
            let mut desired = Map::new();
            put_if(&mut desired, "name", &loc.name);
            put_if(&mut desired, "subtitle", &loc.subtitle);
            put_if(&mut desired, "privacyPolicyUrl", &loc.privacy_policy_url);
            if desired.is_empty() {
                continue;
            }
            let rel = json!({"appInfo": {"data": {"type": "appInfos", "id": app_info_id}}});
            self.upsert_localization(
                apply,
                "appInfoLoc",
                "appInfoLocalizations",
                &existing,
                &loc.locale,
                desired,
                rel,
                r,
            );
        }
    }

    fn reconcile_version_localizations(&self, apply: bool, version_id: &str, r: &mut Report) {
        let existing = match self.version_localizations(version_id) {
            Ok(e) => e,
            Err(e) => {
                r.add("versionLoc", version_id, Status::Error, e);
                return;
            }
        };
        self.version_localizations_with(apply, version_id, &existing, r);
    }

    fn version_localizations_with(
        &self,
        apply: bool,
        version_id: &str,
        existing: &Locs,
        r: &mut Report,
    ) {
        for loc in &self.cfg.locales {
            let mut desired = Map::new();
            put_if(&mut desired, "description", &loc.description);
            put_if(&mut desired, "keywords", &loc.keywords);
            put_if(&mut desired, "promotionalText", &loc.promotional_text);
            put_if(&mut desired, "whatsNew", &loc.whats_new);
            put_if(&mut desired, "supportUrl", &loc.support_url);
            put_if(&mut desired, "marketingUrl", &loc.marketing_url);
            if desired.is_empty() {
                continue;
            }
            let rel = json!({"appStoreVersion": {"data": {"type": "appStoreVersions", "id": version_id}}});
            self.upsert_localization(
                apply,
                "versionLoc",
                "appStoreVersionLocalizations",
                existing,
                &loc.locale,
                desired,
                rel,
                r,
            );
        }
    }

    pub(super) fn version_localizations(&self, version_id: &str) -> Result<Locs, String> {
        self.list_localizations(&format!(
            "/v1/appStoreVersions/{version_id}/appStoreVersionLocalizations?limit=50"
        ))
    }

    pub(super) fn list_localizations(&self, path: &str) -> Result<Locs, String> {
        Ok(get_all(&self.cli, path, "list localizations")?
            .into_iter()
            .filter(|r| !attr(r, "locale").is_empty())
            .map(|r| (attr(&r, "locale").to_string(), (r.id, r.attributes)))
            .collect())
    }

    /// ローカライズを 1 つ作るか、違う項目だけ PATCH する。
    #[allow(clippy::too_many_arguments)]
    fn upsert_localization(
        &self,
        apply: bool,
        op: &str,
        ty: &str,
        existing: &Locs,
        locale: &str,
        desired: Map<String, Value>,
        rel: Value,
        r: &mut Report,
    ) {
        let (method, path, body, pending, done, what) = match existing.get(locale) {
            None => {
                let mut attrs = Map::new();
                attrs.insert("locale".into(), json!(locale));
                attrs.extend(desired);
                (
                    "POST",
                    format!("/v1/{ty}"),
                    json!({"data": {"type": ty, "attributes": attrs, "relationships": rel}}),
                    Status::Create,
                    Status::Created,
                    "new".to_string(),
                )
            }
            Some((id, cur)) => {
                let diff = changed_attrs(cur, &desired);
                if diff.is_empty() {
                    r.add(op, locale, Status::Exists, "");
                    return;
                }
                let what = keys(&diff);
                (
                    "PATCH",
                    format!("/v1/{ty}/{id}"),
                    json!({"data": {"type": ty, "id": id, "attributes": diff}}),
                    Status::Drift,
                    Status::Updated,
                    what,
                )
            }
        };
        if !apply {
            r.add(op, locale, pending, what);
            return;
        }
        match self.send_with_edit_retry(method, &path, body) {
            Err(e) => r.add(op, locale, Status::Error, e),
            Ok((None, dropped)) => r.add(
                op,
                locale,
                Status::Exists,
                format!("non-editable: {}", dropped.join(",")),
            ),
            Ok((Some(resp), _)) if !resp.ok() => r.add(op, locale, Status::Error, resp.error()),
            Ok((Some(_), dropped)) => r.add(op, locale, done, with_dropped(&what, &dropped)),
        }
    }

    /// JSON:API の書き込みを送り、「編集できない」属性で 409 なら、それを落として 1 回だけ送り直す
    /// （初回の版の whatsNew など）。落とした属性の名前も返す。全部落ちたら送らずに None。
    fn send_with_edit_retry(
        &self,
        method: &str,
        path: &str,
        mut body: Value,
    ) -> Result<(Option<Response>, Vec<String>), String> {
        let resp = self.cli.r#do(method, path, Some(&body))?;
        if resp.ok() {
            return Ok((Some(resp), Vec::new()));
        }
        let dropped = non_editable_attrs(&resp.body);
        if dropped.is_empty() {
            return Ok((Some(resp), Vec::new()));
        }
        if let Some(attrs) = body
            .pointer_mut("/data/attributes")
            .and_then(Value::as_object_mut)
        {
            for k in &dropped {
                attrs.remove(k);
            }
            if !attrs.keys().any(|k| k != "locale") {
                return Ok((None, dropped));
            }
        }
        let resp = self.cli.r#do(method, path, Some(&body))?;
        Ok((Some(resp), dropped))
    }

    // --- 審査情報（to-one） ---

    fn reconcile_review_detail(&self, apply: bool, version_id: &str, r: &mut Report) {
        let Some(rv) = &self.cfg.review else { return };
        let mut desired = Map::new();
        for (k, v) in [
            ("contactFirstName", &rv.contact_first_name),
            ("contactLastName", &rv.contact_last_name),
            ("contactPhone", &rv.contact_phone),
            ("contactEmail", &rv.contact_email),
            ("demoAccountName", &rv.demo_account_name),
            ("notes", &rv.notes),
        ] {
            desired.insert(k.into(), json!(v.trim_end()));
        }
        // 空のパスワードは「管理しない」（YAML に書かずに済むように。ストアの値を消さない）
        let password = rv.demo_account_password.trim_end();
        if !password.is_empty() {
            desired.insert("demoAccountPassword".into(), json!(password));
        }
        desired.insert(
            "demoAccountRequired".into(),
            json!(rv.demo_account_required),
        );

        let resp = match self.cli.r#do(
            "GET",
            &format!("/v1/appStoreVersions/{version_id}/appStoreReviewDetail"),
            None,
        ) {
            Ok(resp) => resp,
            Err(e) => {
                r.add("reviewDetail", version_id, Status::Error, e);
                return;
            }
        };
        // 404 か data が空なら「まだ無い」。それ以外の失敗は「無い」とみなさない。
        let current = if resp.status == 404 {
            None
        } else if !resp.ok() {
            r.add(
                "reviewDetail",
                version_id,
                Status::Error,
                format!("get review detail: {}", resp.error()),
            );
            return;
        } else {
            resp.json::<Value>().ok().and_then(|v| {
                let id = v.pointer("/data/id")?.as_str()?.to_string();
                Some((
                    id,
                    v.pointer("/data/attributes")
                        .cloned()
                        .unwrap_or(Value::Null),
                ))
            })
        };

        match current {
            None => {
                if !apply {
                    r.add("reviewDetail", version_id, Status::Create, "new");
                    return;
                }
                let body = json!({"data": {
                    "type": "appStoreReviewDetails",
                    "attributes": desired,
                    "relationships": {"appStoreVersion": {"data": {"type": "appStoreVersions", "id": version_id}}},
                }});
                self.report_write(
                    r,
                    "reviewDetail",
                    version_id,
                    "POST",
                    "/v1/appStoreReviewDetails",
                    &body,
                    Status::Created,
                    "new",
                );
            }
            Some((id, attrs)) => {
                let diff = changed_attrs(&attrs, &desired);
                if diff.is_empty() {
                    r.add("reviewDetail", version_id, Status::Exists, "");
                    return;
                }
                let what = keys(&diff);
                if !apply {
                    r.add("reviewDetail", version_id, Status::Drift, what);
                    return;
                }
                let body = json!({"data": {"type": "appStoreReviewDetails", "id": id, "attributes": diff}});
                self.report_write(
                    r,
                    "reviewDetail",
                    version_id,
                    "PATCH",
                    &format!("/v1/appStoreReviewDetails/{id}"),
                    &body,
                    Status::Updated,
                    &what,
                );
            }
        }
    }

    // --- 年齢区分 ---

    /// 年齢区分の申告は appInfo にぶら下がる（appStoreVersion ではない）。属性の組と編集できるかは
    /// API の版で変わるので、読めない・書けないは run 全体を失敗にせず skip にする（Go 版と同じ）。
    fn reconcile_age_rating(&self, apply: bool, app_info_id: Option<&str>, r: &mut Report) {
        match &self.cfg.age_rating {
            Some(a) if a.preset == "none" => {}
            _ => return,
        }
        let bundle = &self.cfg.bundle_id;
        let Some(app_info_id) = app_info_id else {
            r.add("ageRating", bundle, Status::Skip, "no editable app info");
            return;
        };
        let resp = match self.cli.r#do(
            "GET",
            &format!("/v1/appInfos/{app_info_id}/ageRatingDeclaration"),
            None,
        ) {
            Ok(resp) => resp,
            Err(e) => {
                r.add("ageRating", app_info_id, Status::Skip, e);
                return;
            }
        };
        let decl = resp
            .ok()
            .then(|| resp.json::<Value>().ok())
            .flatten()
            .and_then(|v| {
                let id = v.pointer("/data/id")?.as_str()?.to_string();
                Some((
                    id,
                    v.pointer("/data/attributes")
                        .cloned()
                        .unwrap_or(Value::Null),
                ))
            });
        let Some((id, attrs)) = decl else {
            r.add(
                "ageRating",
                app_info_id,
                Status::Skip,
                format!(
                    "age rating declaration not available via API ({})",
                    resp.error()
                ),
            );
            return;
        };
        let diff = changed_attrs(&attrs, &age_rating_none());
        if diff.is_empty() {
            r.add("ageRating", &id, Status::Exists, "none");
            return;
        }
        if !apply {
            r.add("ageRating", &id, Status::Drift, "set none");
            return;
        }
        let body = json!({"data": {"type": "ageRatingDeclarations", "id": id, "attributes": diff}});
        match self.cli.r#do(
            "PATCH",
            &format!("/v1/ageRatingDeclarations/{id}"),
            Some(&body),
        ) {
            Err(e) => r.add("ageRating", &id, Status::Skip, e),
            // 申告の PATCH は質問票の全部（parentalControls など）を要る。App Store Connect で一度
            // 済ませてもらう。run の他の項目は進める。
            Ok(resp) if !resp.ok() => r.add(
                "ageRating",
                &id,
                Status::Skip,
                "complete the age-rating questionnaire in App Store Connect",
            ),
            Ok(_) => r.add("ageRating", &id, Status::Updated, "none"),
        }
    }

    // --- 小さな道具 ---

    /// 書き込みを送って結果を報告する。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn report_write(
        &self,
        r: &mut Report,
        op: &str,
        target: &str,
        method: &str,
        path: &str,
        body: &Value,
        done: Status,
        detail: &str,
    ) -> bool {
        match self.cli.r#do(method, path, Some(body)) {
            Err(e) => {
                r.add(op, target, Status::Error, e);
                false
            }
            Ok(resp) if !resp.ok() => {
                r.add(op, target, Status::Error, resp.error());
                false
            }
            Ok(_) => {
                r.add(op, target, done, detail);
                true
            }
        }
    }

    /// to-one の relationship の id（無ければ None。404 も None）。
    pub(super) fn related_id(&self, path: &str) -> Result<Option<String>, String> {
        let resp = self.cli.r#do("GET", path, None)?;
        if resp.status == 404 {
            return Ok(None);
        }
        if !resp.ok() {
            return Err(format!("read {path}: {}", resp.error()));
        }
        Ok(created_id(&resp))
    }
}

/// 編集する版の選び方の結果。
#[derive(Debug, PartialEq)]
enum Pick {
    /// その版を編集する。note は「編集できない状態かもしれない」など
    Found {
        id: String,
        attrs: Value,
        note: String,
    },
    /// 編集する版が無い（skip）
    Missing(String),
    /// 指定の版が無く、作る（`--create-app-store-version`）
    Create,
    /// 作れない（別の番号の編集できる版がある）
    Blocked(String),
}

fn pick_version(data: &[Resource], want: &str, create: bool) -> Pick {
    let found = |r: &Resource, note: String| Pick::Found {
        id: r.id.clone(),
        attrs: r.attributes.clone(),
        note,
    };
    if want.is_empty() {
        return match data.iter().find(|r| is_editable(state_of(r))) {
            Some(r) => found(r, String::new()),
            None => Pick::Missing("no editable app version".into()),
        };
    }
    if let Some(r) = data.iter().find(|r| attr(r, "versionString") == want) {
        let state = state_of(r);
        let note = if is_editable(state) {
            String::new()
        } else {
            format!("version state {state} may not be editable")
        };
        return found(r, note);
    }
    if !create {
        return Pick::Missing(format!(
            "version {want} not found (create it in App Store Connect first, or pass --create-app-store-version)"
        ));
    }
    // App Store は編集できる版を同時に 1 つしか持たせない。番号を付け替えるか捨てるかは人が決める。
    match data.iter().find(|r| is_editable(state_of(r))) {
        Some(r) => Pick::Blocked(format!(
            "cannot create version {want}: version {} is still editable ({}); change or remove it in App Store Connect",
            attr(r, "versionString"),
            state_of(r)
        )),
        None => Pick::Create,
    }
}

/// JSON の appStoreVersion（対象にした版）。
fn version_info(attrs: &Value, created: bool) -> AppStoreVersion {
    let get = |k: &str| attrs.get(k).and_then(Value::as_str).unwrap_or("");
    let state = ["appStoreState", "state", "appVersionState"]
        .iter()
        .map(|k| get(k))
        .find(|s| !s.is_empty())
        .unwrap_or("");
    AppStoreVersion {
        version_string: get("versionString").to_string(),
        state: state.to_string(),
        created,
    }
}

fn version_create_body(app_id: &str, version: &str) -> Value {
    json!({"data": {
        "type": "appStoreVersions",
        "attributes": {"platform": "IOS", "versionString": version},
        "relationships": {"app": {"data": {"type": "apps", "id": app_id}}},
    }})
}

fn with_dropped(base: &str, dropped: &[String]) -> String {
    if dropped.is_empty() {
        base.to_string()
    } else {
        format!("{base} (skipped non-editable: {})", dropped.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_prefers_legacy_field() {
        let r = Resource {
            id: "1".into(),
            attributes: json!({"appStoreState": "READY_FOR_SALE", "state": "PREPARE_FOR_SUBMISSION"}),
        };
        assert_eq!(state_of(&r), "READY_FOR_SALE");
        let r = Resource {
            id: "1".into(),
            attributes: json!({"appVersionState": "PREPARE_FOR_SUBMISSION"}),
        };
        assert_eq!(state_of(&r), "PREPARE_FOR_SUBMISSION");
        assert!(is_editable(state_of(&r)));
    }

    #[test]
    fn age_rating_none_values() {
        let m = age_rating_none();
        assert_eq!(m["contests"], json!("NONE"));
        assert_eq!(m["gambling"], json!(false));
        assert_eq!(m.len(), 14);
    }

    fn ver(id: &str, version: &str, state: &str) -> Resource {
        Resource {
            id: id.into(),
            attributes: json!({"versionString": version, "appVersionState": state}),
        }
    }

    fn found_id(p: &Pick) -> Option<&str> {
        match p {
            Pick::Found { id, .. } => Some(id),
            _ => None,
        }
    }

    #[test]
    fn pick_first_editable_without_version() {
        let data = [
            ver("1", "1.0.0", "READY_FOR_DISTRIBUTION"),
            ver("2", "1.1.0", "PREPARE_FOR_SUBMISSION"),
        ];
        assert_eq!(found_id(&pick_version(&data, "", false)), Some("2"));
        assert_eq!(
            pick_version(&data[..1], "", true),
            Pick::Missing("no editable app version".into())
        );
    }

    #[test]
    fn pick_named_version() {
        let data = [
            ver("1", "1.0.0", "READY_FOR_DISTRIBUTION"),
            ver("2", "1.1.0", "PREPARE_FOR_SUBMISSION"),
        ];
        assert_eq!(found_id(&pick_version(&data, "1.1.0", true)), Some("2"));
        // 編集できない版も試す（注記付き）。作らない
        match pick_version(&data, "1.0.0", true) {
            Pick::Found { id, note, .. } => {
                assert_eq!(id, "1");
                assert!(note.contains("READY_FOR_DISTRIBUTION"), "{note}");
            }
            p => panic!("{p:?}"),
        }
    }

    #[test]
    fn pick_missing_version_creates_only_when_asked() {
        let live = [ver("1", "1.0.0", "READY_FOR_DISTRIBUTION")];
        match pick_version(&live, "1.1.0", false) {
            Pick::Missing(note) => assert!(note.contains("--create-app-store-version"), "{note}"),
            p => panic!("{p:?}"),
        }
        assert_eq!(pick_version(&live, "1.1.0", true), Pick::Create);
        assert_eq!(pick_version(&[], "1.0.0", true), Pick::Create);
    }

    #[test]
    fn pick_blocks_when_another_version_is_editable() {
        let data = [
            ver("1", "1.0.0", "READY_FOR_DISTRIBUTION"),
            ver("2", "1.0.1", "PREPARE_FOR_SUBMISSION"),
        ];
        match pick_version(&data, "1.1.0", true) {
            Pick::Blocked(note) => assert!(note.contains("1.0.1"), "{note}"),
            p => panic!("{p:?}"),
        }
    }

    #[test]
    fn create_body() {
        let b = version_create_body("123", "1.4.0");
        assert_eq!(b["data"]["attributes"]["platform"], "IOS");
        assert_eq!(b["data"]["attributes"]["versionString"], "1.4.0");
        assert_eq!(b["data"]["relationships"]["app"]["data"]["id"], "123");
    }

    #[test]
    fn dropped_note() {
        assert_eq!(with_dropped("new", &[]), "new");
        assert_eq!(
            with_dropped("description", &["whatsNew".into()]),
            "description (skipped non-editable: whatsNew)"
        );
    }
}
