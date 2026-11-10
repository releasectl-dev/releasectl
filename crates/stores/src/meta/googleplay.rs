//! Google Play の掲載情報（Go 版 `internal/storemeta/googleplay.go`）。Android Publisher v3 の edit の中で、
//! 言語ごとの listing・連絡先（details）・画像を揃え、apply なら validate → commit、plan なら捨てる。
//! 扱うのは title / shortDescription / fullDescription。レーティングとデータ セーフティは API が無い。

use super::{changed_attrs, keys, put_if};
use crate::Reconciler;
use crate::http::Client;
use releasectl_core::meta::{MetaConfig, play_lang};
use releasectl_core::{Report, Status};
use serde_json::{Map, Value, json};

pub(super) struct MetaPlay {
    pub(super) cfg: MetaConfig,
    pub(super) cli: Client,
}

pub(super) fn build(cfg: &MetaConfig) -> Result<Box<dyn Reconciler>, String> {
    // 画像・動画は do_raw（アップロード用の長いタイムアウト）で送る。
    let cli = crate::googleplay::new_client()?;
    Ok(Box::new(MetaPlay {
        cfg: cfg.clone(),
        cli,
    }))
}

impl Reconciler for MetaPlay {
    fn name(&self) -> &'static str {
        "googleplay"
    }

    fn reconcile(&self, apply: bool, r: &mut Report) -> Result<(), String> {
        let pkg = self.cfg.package_name.as_str();
        if pkg.is_empty() {
            r.add(
                "app",
                &self.cfg.app_name,
                Status::Skip,
                "packageName not set",
            );
            return Ok(());
        }
        let edit = match self.insert_edit(pkg) {
            Ok(id) => id,
            Err(e) => {
                r.add("edit", pkg, Status::Error, e);
                return Ok(());
            }
        };
        let edit_path = format!("/applications/{pkg}/edits/{edit}");
        let committed = self.reconcile_in_edit(apply, pkg, &edit_path, r);
        // commit しなかった edit は何も公開しない。いつでも片付ける。
        if !committed {
            let _ = self.cli.r#do("DELETE", &edit_path, None);
        }
        Ok(())
    }
}

impl MetaPlay {
    /// edit の中で揃える。commit したら true。
    fn reconcile_in_edit(&self, apply: bool, pkg: &str, edit_path: &str, r: &mut Report) -> bool {
        let mut changed = false;
        for loc in &self.cfg.locales {
            let mut desired = Map::new();
            put_if(&mut desired, "title", &loc.name);
            put_if(&mut desired, "shortDescription", &loc.short_description);
            put_if(&mut desired, "fullDescription", &loc.description);
            if desired.is_empty() {
                continue;
            }
            changed |=
                self.reconcile_listing(apply, edit_path, &play_lang(&loc.locale), desired, r);
        }
        changed |= self.reconcile_details(apply, pkg, edit_path, r);
        changed |= self.reconcile_images(apply, edit_path, r);

        if !apply {
            r.add(
                "commit",
                pkg,
                Status::Skip,
                "plan: edit discarded (no write)",
            );
            return false;
        }
        if !changed {
            r.add("commit", pkg, Status::Exists, "nothing to commit");
            return false;
        }
        for (step, op) in [("validate", ":validate"), ("commit", ":commit")] {
            match self.cli.r#do("POST", &format!("{edit_path}{op}"), None) {
                Err(e) => {
                    r.add(step, pkg, Status::Error, e);
                    return false;
                }
                Ok(resp) if !resp.ok() => {
                    r.add(step, pkg, Status::Error, resp.error());
                    return false;
                }
                Ok(_) => {}
            }
        }
        r.add("commit", pkg, Status::Updated, "committed");
        true
    }

    /// 言語の listing を作るか、違う項目だけ PATCH する。変わった（plan なら変わる）なら true。
    fn reconcile_listing(
        &self,
        apply: bool,
        edit_path: &str,
        lang: &str,
        desired: Map<String, Value>,
        r: &mut Report,
    ) -> bool {
        let path = format!("{edit_path}/listings/{lang}");
        let cur = match self.cli.r#do("GET", &path, None) {
            Err(e) => {
                r.add("listing", lang, Status::Error, e);
                return false;
            }
            Ok(resp) if resp.status == 404 => None,
            Ok(resp) if !resp.ok() => {
                r.add(
                    "listing",
                    lang,
                    Status::Error,
                    format!("get listing {lang}: {}", resp.error()),
                );
                return false;
            }
            Ok(resp) => match resp.json::<Value>() {
                Ok(v) => Some(v),
                Err(e) => {
                    r.add("listing", lang, Status::Error, e);
                    return false;
                }
            },
        };
        let (method, body, pending, done, what) = match cur {
            None => {
                let mut body = desired;
                body.insert("language".into(), json!(lang));
                (
                    "PUT",
                    Value::Object(body),
                    Status::Create,
                    Status::Created,
                    "new".to_string(),
                )
            }
            Some(cur) => {
                let diff = changed_attrs(&cur, &desired);
                if diff.is_empty() {
                    r.add("listing", lang, Status::Exists, "");
                    return false;
                }
                let what = keys(&diff);
                (
                    "PATCH",
                    Value::Object(diff),
                    Status::Drift,
                    Status::Updated,
                    what,
                )
            }
        };
        if !apply {
            r.add("listing", lang, pending, what);
            return true;
        }
        match self.cli.r#do(method, &path, Some(&body)) {
            Err(e) => r.add("listing", lang, Status::Error, e),
            Ok(resp) if !resp.ok() => r.add("listing", lang, Status::Error, resp.error()),
            Ok(_) => r.add("listing", lang, done, what),
        }
        true
    }

    /// 掲載の連絡先と既定の言語（edits.details）。
    fn reconcile_details(&self, apply: bool, pkg: &str, edit_path: &str, r: &mut Report) -> bool {
        let mut desired = Map::new();
        put_if(&mut desired, "defaultLanguage", &self.cfg.default_language);
        if let Some(c) = &self.cfg.play_contact {
            put_if(&mut desired, "contactEmail", &c.email);
            put_if(&mut desired, "contactPhone", &c.phone);
            put_if(&mut desired, "contactWebsite", &c.website);
        }
        if desired.is_empty() {
            return false;
        }
        let path = format!("{edit_path}/details");
        let cur = match self.cli.r#do("GET", &path, None) {
            Err(e) => {
                r.add("details", pkg, Status::Error, e);
                return false;
            }
            Ok(resp) if !resp.ok() => {
                r.add(
                    "details",
                    pkg,
                    Status::Error,
                    format!("get details: {}", resp.error()),
                );
                return false;
            }
            Ok(resp) => resp.json::<Value>().unwrap_or(Value::Null),
        };
        let diff = changed_attrs(&cur, &desired);
        if diff.is_empty() {
            r.add("details", pkg, Status::Exists, "");
            return false;
        }
        let what = keys(&diff);
        if !apply {
            r.add("details", pkg, Status::Drift, what);
            return true;
        }
        match self.cli.r#do("PATCH", &path, Some(&Value::Object(diff))) {
            Err(e) => r.add("details", pkg, Status::Error, e),
            Ok(resp) if !resp.ok() => r.add("details", pkg, Status::Error, resp.error()),
            Ok(_) => r.add("details", pkg, Status::Updated, what),
        }
        true
    }

    fn insert_edit(&self, pkg: &str) -> Result<String, String> {
        let resp = self.cli.r#do(
            "POST",
            &format!("/applications/{pkg}/edits"),
            Some(&json!({})),
        )?;
        if !resp.ok() {
            return Err(format!("insert edit: {}", resp.error()));
        }
        let v: Value = resp.json()?;
        v.get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| "insert edit: empty edit id".to_string())
    }
}
