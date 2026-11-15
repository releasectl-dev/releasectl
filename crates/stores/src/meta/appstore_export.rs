//! App Store の掲載情報の今の値（`releasectl meta export`、docs: 02_cli-architecture/07-export.md）。読むだけ。
//! キーの名前は metadata.yaml の項目名にそろえる（Cloud が YAML と比べ、取り込みで YAML に戻せるように）。

use super::appstore::{MetaAppStore, is_editable, state_of};
use super::{md5_hex, write_shot};
use crate::ExportOpts;
use crate::appstore::{Resource, attr, get_all, parse_list};
use releasectl_core::{AppStoreVersion, Report, Status};
use serde_json::{Map, Value, json};

/// 販売中の版の状態（旧 appStoreState と新しい appVersionState）。
const LIVE_STATES: [&str; 2] = ["READY_FOR_SALE", "READY_FOR_DISTRIBUTION"];

fn is_live(state: &str) -> bool {
    LIVE_STATES.contains(&state)
}

/// 出す版を選ぶ。"live" は販売中の版、番号ならその版、空なら編集できる版（無ければ販売中の版）。
fn pick_export_version<'a>(data: &'a [Resource], want: &str) -> Option<&'a Resource> {
    let live = || data.iter().find(|r| is_live(state_of(r)));
    match want {
        "live" => live(),
        "" => data.iter().find(|r| is_editable(state_of(r))).or_else(live),
        v => data.iter().find(|r| attr(r, "versionString") == v),
    }
}

/// 出す appInfo を選ぶ。版が販売中なら販売中の appInfo、そうでなければ編集できる appInfo。どちらも無ければ最初のもの。
fn pick_app_info(data: &[Resource], live: bool) -> Option<&Resource> {
    let want = |r: &&Resource| {
        let s = state_of(r);
        if live { is_live(s) } else { is_editable(s) }
    };
    data.iter().find(want).or(data.first())
}

fn pick_attrs(attrs: &Value, names: &[&str]) -> Map<String, Value> {
    names
        .iter()
        .filter_map(|n| {
            let v = attrs.get(*n)?.as_str()?;
            Some((n.to_string(), json!(v)))
        })
        .collect()
}

type JsonMap = Map<String, Value>;

const APP_INFO_FIELDS: [&str; 3] = ["name", "subtitle", "privacyPolicyUrl"];
const REVIEW_FIELDS: [&str; 6] = [
    "contactFirstName",
    "contactLastName",
    "contactPhone",
    "contactEmail",
    "demoAccountName",
    "notes",
];
const VERSION_FIELDS: [&str; 6] = [
    "description",
    "keywords",
    "promotionalText",
    "whatsNew",
    "supportUrl",
    "marketingUrl",
];

impl MetaAppStore {
    pub(super) fn export_store(&self, opts: &ExportOpts, r: &mut Report) -> Result<Value, String> {
        r.set_key_singleton(&["app", "appInfo", "contentRights", "version"]);
        let bundle = self.cfg.bundle_id.as_str();
        if bundle.is_empty() {
            r.add("app", &self.cfg.app_name, Status::Skip, "bundleId not set");
            return Ok(Value::Null);
        }
        let mut out = Map::new();
        out.insert("bundleId".into(), json!(bundle));
        let Some(app_id) = self.find_app_id()? else {
            r.add(
                "app",
                bundle,
                Status::Error,
                "no App Store app for this bundle id",
            );
            return Ok(Value::Object(out));
        };
        r.store_id("appId", &app_id);
        out.insert("appId".into(), json!(app_id));

        // 版
        let resp = self.cli.r#do(
            "GET",
            &format!("/v1/apps/{app_id}/appStoreVersions?limit=50"),
            None,
        )?;
        if !resp.ok() {
            return Err(format!("list versions: {}", resp.error()));
        }
        let (versions, _) = parse_list(&resp)?;
        let want = self.cfg.app_store_version.as_str();
        let version = pick_export_version(&versions, want);
        let live = version.is_some_and(|v| is_live(state_of(v)));
        let mut settings = Map::new();
        match version {
            None => r.add(
                "version",
                bundle,
                Status::Error,
                match want {
                    "live" => "no version on sale".to_string(),
                    "" => "no editable or on-sale version".to_string(),
                    v => format!("version {v} not found"),
                },
            ),
            Some(v) => {
                let info = AppStoreVersion {
                    version_string: attr(v, "versionString").to_string(),
                    state: state_of(v).to_string(),
                    created: false,
                };
                // 版はトップレベルの appStoreVersion に出す（plan / apply の JSON と同じ）。
                r.app_store_version = Some(info);
                if let Some(c) = v.attributes.get("copyright").and_then(Value::as_str) {
                    settings.insert("copyright".into(), json!(c));
                }
                self.export_version(&v.id, opts, &mut out, r);
                if opts.review {
                    self.export_review(&v.id, &mut out, r);
                }
            }
        }

        // appInfo（名前・サブタイトル・カテゴリ）
        match get_all(
            &self.cli,
            &format!("/v1/apps/{app_id}/appInfos?limit=50"),
            "list app infos",
        ) {
            Err(e) => r.add("appInfo", bundle, Status::Error, e),
            Ok(infos) => {
                if let Some(info) = pick_app_info(&infos, live) {
                    self.export_app_info(&info.id, &mut out, &mut settings, r);
                }
            }
        }

        // コンテンツの権利
        match self.cli.r#do(
            "GET",
            &format!("/v1/apps/{app_id}?fields[apps]=contentRightsDeclaration"),
            None,
        ) {
            Ok(resp) if resp.ok() => {
                let v: Value = resp.json().unwrap_or(Value::Null);
                if let Some(c) = v
                    .pointer("/data/attributes/contentRightsDeclaration")
                    .and_then(Value::as_str)
                {
                    // metadata.yaml の appstore.contentRights の値（none / thirdParty）で出す
                    if let Some(y) = releasectl_core::meta::content_rights_yaml(c) {
                        settings.insert("contentRights".into(), json!(y));
                    }
                }
            }
            Ok(resp) => r.add("contentRights", &app_id, Status::Error, resp.error()),
            Err(e) => r.add("contentRights", &app_id, Status::Error, e),
        }
        // metadata.yaml の appstore: と同じ名前
        out.insert("appstore".into(), Value::Object(settings));
        Ok(Value::Object(out))
    }

    /// 審査情報を metadata.yaml の `review` の名前で出す（`meta init` 用）。パスワードは出さない。
    fn export_review(&self, version_id: &str, out: &mut Map<String, Value>, r: &mut Report) {
        let resp = match self.cli.r#do(
            "GET",
            &format!("/v1/appStoreVersions/{version_id}/appStoreReviewDetail"),
            None,
        ) {
            Ok(resp) => resp,
            Err(e) => return r.add("reviewDetail", version_id, Status::Error, e),
        };
        if resp.status == 404 {
            return;
        }
        if !resp.ok() {
            return r.add("reviewDetail", version_id, Status::Error, resp.error());
        }
        let v: Value = resp.json().unwrap_or(Value::Null);
        let Some(attrs) = v.pointer("/data/attributes") else {
            return;
        };
        let mut m = pick_attrs(attrs, &REVIEW_FIELDS);
        m.retain(|_, v| v.as_str().is_some_and(|s| !s.trim().is_empty()));
        let required = attrs.get("demoAccountRequired").and_then(Value::as_bool);
        if m.is_empty() && required != Some(true) {
            return;
        }
        m.insert(
            "demoAccountRequired".into(),
            json!(required.unwrap_or(false)),
        );
        // ストアにパスワードがあることだけ伝える（init が「手で入れる」と案内する）
        if attrs
            .get("demoAccountPassword")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
        {
            m.insert("hasDemoAccountPassword".into(), json!(true));
        }
        out.insert("review".into(), Value::Object(m));
    }

    fn export_version(
        &self,
        version_id: &str,
        opts: &ExportOpts,
        out: &mut Map<String, Value>,
        r: &mut Report,
    ) {
        let locs = match self.version_localizations(version_id) {
            Ok(l) => l,
            Err(e) => {
                r.add("versionLoc", version_id, Status::Error, e);
                return;
            }
        };
        let mut texts = Map::new();
        let mut shots = Map::new();
        let mut files = Map::new();
        let mut locales: Vec<&String> = locs.keys().collect();
        locales.sort();
        for locale in locales {
            let (loc_id, attrs) = &locs[locale];
            texts.insert(
                locale.clone(),
                Value::Object(pick_attrs(attrs, &VERSION_FIELDS)),
            );
            match self.export_screenshots(loc_id, locale, opts, r) {
                Ok((m, f)) => {
                    if !m.is_empty() {
                        shots.insert(locale.clone(), Value::Object(m));
                    }
                    if !f.is_empty() {
                        files.insert(locale.clone(), Value::Object(f));
                    }
                }
                Err(e) => r.add("screenshot", locale, Status::Error, e),
            }
        }
        out.insert("versionLocalizations".into(), Value::Object(texts));
        out.insert("screenshots".into(), Value::Object(shots));
        if opts.images.is_some() {
            out.insert("screenshotFiles".into(), Value::Object(files));
        }
    }

    /// 表示タイプ → スクリーンショットの MD5（並び順）。処理中で MD5 がまだ無いものは空文字。
    /// `opts.images` があれば `<images>/appstore/<表示タイプ>/<ロケール>/NN.png` にダウンロードし、2 つ目に
    /// 表示タイプ → `{dir, files, sameAsStore}` を返す（sameAsStore: 置いたファイルの MD5 がストアの値と同じか）。
    fn export_screenshots(
        &self,
        loc_id: &str,
        locale: &str,
        opts: &ExportOpts,
        r: &mut Report,
    ) -> Result<(JsonMap, JsonMap), String> {
        let sets = get_all(
            &self.cli,
            &format!("/v1/appStoreVersionLocalizations/{loc_id}/appScreenshotSets?limit=50"),
            "list screenshot sets",
        )?;
        let mut m = Map::new();
        let mut files = Map::new();
        for set in &sets {
            let display_type = attr(set, "screenshotDisplayType");
            let list = self.list_screenshots(&set.id)?;
            if list.is_empty() {
                continue;
            }
            if let Some(base) = &opts.images {
                let rel = format!("appstore/{display_type}");
                let dir = base.join(&rel).join(locale);
                let mut names = Vec::new();
                let mut same = true;
                for (i, s) in list.iter().enumerate() {
                    let target = format!("{display_type}/{locale}");
                    if s.url.is_empty() {
                        r.add("screenshot", &target, Status::Error, "no image url yet");
                        continue;
                    }
                    match crate::http::download(&s.url).and_then(|d| {
                        same &= md5_hex(&d).eq_ignore_ascii_case(&s.checksum);
                        write_shot(&dir, i, &d)
                    }) {
                        Ok(name) => names.push(name),
                        Err(e) => r.add("screenshot", &target, Status::Error, e),
                    }
                }
                files.insert(
                    display_type.to_string(),
                    json!({"dir": rel, "files": names, "sameAsStore": same}),
                );
            }
            let md5s: Vec<String> = list.into_iter().map(|s| s.checksum).collect();
            m.insert(display_type.to_string(), json!(md5s));
        }
        Ok((m, files))
    }

    fn export_app_info(
        &self,
        app_info_id: &str,
        out: &mut Map<String, Value>,
        settings: &mut Map<String, Value>,
        r: &mut Report,
    ) {
        match self.list_localizations(&format!(
            "/v1/appInfos/{app_info_id}/appInfoLocalizations?limit=50"
        )) {
            Err(e) => r.add("appInfoLoc", app_info_id, Status::Error, e),
            Ok(locs) => {
                let mut m = Map::new();
                let mut locales: Vec<&String> = locs.keys().collect();
                locales.sort();
                for locale in locales {
                    m.insert(
                        locale.clone(),
                        Value::Object(pick_attrs(&locs[locale].1, &APP_INFO_FIELDS)),
                    );
                }
                out.insert("appInfo".into(), Value::Object(m));
            }
        }
        for rel in ["primaryCategory", "secondaryCategory"] {
            match self.related_id(&format!("/v1/appInfos/{app_info_id}/relationships/{rel}")) {
                Ok(Some(id)) => {
                    settings.insert(rel.into(), json!(id));
                }
                Ok(None) => {}
                Err(e) => r.add("category", rel, Status::Error, e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ver(id: &str, version: &str, state: &str) -> Resource {
        Resource {
            id: id.into(),
            attributes: json!({"versionString": version, "appVersionState": state}),
        }
    }

    #[test]
    fn picks_export_version() {
        let data = [
            ver("2", "1.1.0", "PREPARE_FOR_SUBMISSION"),
            ver("1", "1.0.0", "READY_FOR_DISTRIBUTION"),
        ];
        let id = |w: &str| pick_export_version(&data, w).map(|r| r.id.as_str());
        assert_eq!(id("live"), Some("1"));
        assert_eq!(id(""), Some("2"));
        assert_eq!(id("1.0.0"), Some("1"));
        assert_eq!(id("9.9.9"), None);
        // 編集できる版が無ければ空は販売中の版
        assert_eq!(
            pick_export_version(&data[1..], "").map(|r| r.id.as_str()),
            Some("1")
        );
        // 販売中の版が無ければ live は無し
        assert!(pick_export_version(&data[..1], "live").is_none());
    }

    #[test]
    fn picks_app_info() {
        let data = [
            Resource {
                id: "a".into(),
                attributes: json!({"state": "PREPARE_FOR_SUBMISSION"}),
            },
            Resource {
                id: "b".into(),
                attributes: json!({"state": "READY_FOR_DISTRIBUTION"}),
            },
        ];
        assert_eq!(pick_app_info(&data, true).unwrap().id, "b");
        assert_eq!(pick_app_info(&data, false).unwrap().id, "a");
        assert_eq!(pick_app_info(&data[1..], false).unwrap().id, "b");
    }

    #[test]
    fn picks_only_string_fields() {
        let m = pick_attrs(
            &json!({"description": "d", "keywords": null, "locale": "ja"}),
            &VERSION_FIELDS,
        );
        assert_eq!(Value::Object(m), json!({"description": "d"}));
    }
}
