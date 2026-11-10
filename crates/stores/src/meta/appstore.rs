//! App Store Connect の掲載情報（Go 版 `internal/storemeta/appstore.go`）: ロケールごとの appInfo と
//! version のローカライズ、審査情報、年齢区分。アプリの設定は appstore_app.rs、画像は
//! appstore_shots.rs / appstore_creatives.rs。

use super::{changed_attrs, keys, non_editable_attrs, put_if};
use crate::Reconciler;
use crate::appstore::{Resource, attr, created_id, get_all, parse_list};
use crate::http::{Client, Response};
use releasectl_core::meta::MetaConfig;
use releasectl_core::{Report, Status};
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

fn is_editable(state: &str) -> bool {
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

    fn reconcile(&self, apply: bool, r: &mut Report) -> Result<(), String> {
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
        let (version_id, version_attrs, note) = match self.find_version(&app_id) {
            Ok(v) => v,
            Err(e) => {
                r.add("version", bundle, Status::Error, e);
                return Ok(());
            }
        };
        let Some(version_id) = version_id else {
            r.add("version", bundle, Status::Skip, note);
            return Ok(());
        };
        if !note.is_empty() {
            r.add("version", &self.cfg.app_store_version, Status::Exists, note);
        }

        self.reconcile_version_localizations(apply, &version_id, r);
        self.reconcile_copyright(apply, &version_id, &version_attrs, r);
        self.reconcile_review_detail(apply, &version_id, r);
        self.reconcile_screenshots(apply, &version_id, r);
        self.reconcile_creatives(apply, &app_id, &version_id, r);
        Ok(())
    }
}

impl MetaAppStore {
    fn find_app_id(&self) -> Result<Option<String>, String> {
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

    /// 編集する版: (id, 属性, 補足)。appStoreVersion を指定していれば versionString で、
    /// 無ければ最初の編集できる版。
    fn find_version(&self, app_id: &str) -> Result<(Option<String>, Value, String), String> {
        let resp = self.cli.r#do(
            "GET",
            &format!("/v1/apps/{app_id}/appStoreVersions?limit=50"),
            None,
        )?;
        if !resp.ok() {
            return Err(format!("list versions: {}", resp.error()));
        }
        let (data, _) = parse_list(&resp)?;
        let want = self.cfg.app_store_version.as_str();
        for r in data {
            let state = state_of(&r).to_string();
            if !want.is_empty() {
                if attr(&r, "versionString") == want {
                    let note = if is_editable(&state) {
                        String::new()
                    } else {
                        format!("version state {state} may not be editable")
                    };
                    return Ok((Some(r.id), r.attributes, note));
                }
                continue;
            }
            if is_editable(&state) {
                return Ok((Some(r.id), r.attributes, String::new()));
            }
        }
        let note = if want.is_empty() {
            "no editable app version".to_string()
        } else {
            format!("version {want} not found (create it in App Store Connect first)")
        };
        Ok((None, Value::Null, note))
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
                &existing,
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

    fn list_localizations(&self, path: &str) -> Result<Locs, String> {
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
            ("demoAccountPassword", &rv.demo_account_password),
            ("notes", &rv.notes),
        ] {
            desired.insert(k.into(), json!(v.trim_end()));
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

    #[test]
    fn dropped_note() {
        assert_eq!(with_dropped("new", &[]), "new");
        assert_eq!(
            with_dropped("description", &["whatsNew".into()]),
            "description (skipped non-editable: whatsNew)"
        );
    }
}
