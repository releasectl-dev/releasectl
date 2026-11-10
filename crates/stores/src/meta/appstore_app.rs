//! テキスト以外で「審査に追加」を止める App Store の設定（Go 版 `appstore_app.go`）: カテゴリ（appInfo の
//! relationship）、コンテンツの権利（app の属性）、著作権表示（version の属性）、価格（app の価格スケジュール）。
//! App Privacy は API が無いので手作業。

use super::appstore::MetaAppStore;
use crate::appstore::get_all;
use releasectl_core::{Report, Status};
use serde_json::{Value, json};

/// 無料の価格スケジュールを作るときの基準テリトリ。無料なら全テリトリが無料の段になる。
const FREE_BASE_TERRITORY: &str = "USA";

fn or_unset(s: &str) -> &str {
    if s.is_empty() { "(unset)" } else { s }
}

fn is_zero_price(v: &Value) -> bool {
    v.get("customerPrice")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<f64>().ok())
        .is_some_and(|p| p == 0.0)
}

impl MetaAppStore {
    // --- カテゴリ ---

    pub(super) fn reconcile_categories(
        &self,
        apply: bool,
        app_info_id: Option<&str>,
        r: &mut Report,
    ) {
        let Some(s) = &self.cfg.app_store_app else {
            return;
        };
        if s.primary_category.is_empty() && s.secondary_category.is_empty() {
            return;
        }
        let Some(app_info_id) = app_info_id else {
            r.add(
                "category",
                &self.cfg.bundle_id,
                Status::Skip,
                "no editable app info",
            );
            return;
        };
        let mut rels = serde_json::Map::new();
        let mut changed: Vec<(&str, String, &str)> = Vec::new(); // (rel, current, want)
        for (rel, want) in [
            ("primaryCategory", s.primary_category.as_str()),
            ("secondaryCategory", s.secondary_category.as_str()),
        ] {
            if want.is_empty() {
                continue;
            }
            let cur =
                match self.related_id(&format!("/v1/appInfos/{app_info_id}/relationships/{rel}")) {
                    Ok(c) => c.unwrap_or_default(),
                    Err(e) => {
                        r.add("category", rel, Status::Error, e);
                        return;
                    }
                };
            if cur == want {
                r.add("category", rel, Status::Exists, want);
                continue;
            }
            rels.insert(
                rel.into(),
                json!({"data": {"type": "appCategories", "id": want}}),
            );
            if !apply {
                if cur.is_empty() {
                    r.add("category", rel, Status::Create, want);
                } else {
                    r.add_diff("category", rel, Status::Drift, cur.clone(), want);
                }
            }
            changed.push((rel, cur, want));
        }
        if !apply || rels.is_empty() {
            return;
        }
        let body = json!({"data": {"type": "appInfos", "id": app_info_id, "relationships": rels}});
        let res = self
            .cli
            .r#do("PATCH", &format!("/v1/appInfos/{app_info_id}"), Some(&body));
        for (rel, cur, want) in changed {
            match &res {
                Err(e) => r.add("category", rel, Status::Error, e.clone()),
                Ok(resp) if !resp.ok() => r.add("category", rel, Status::Error, resp.error()),
                Ok(_) if cur.is_empty() => r.add("category", rel, Status::Created, want),
                Ok(_) => r.add_diff("category", rel, Status::Updated, cur, want),
            }
        }
    }

    // --- コンテンツの権利 ---

    pub(super) fn reconcile_content_rights(&self, apply: bool, app_id: &str, r: &mut Report) {
        let Some(s) = &self.cfg.app_store_app else {
            return;
        };
        let Some(want) = s.content_rights_value() else {
            return;
        };
        let resp = match self.cli.r#do(
            "GET",
            &format!("/v1/apps/{app_id}?fields[apps]=contentRightsDeclaration"),
            None,
        ) {
            Ok(resp) if resp.ok() => resp,
            Ok(resp) => {
                r.add("contentRights", app_id, Status::Error, resp.error());
                return;
            }
            Err(e) => {
                r.add("contentRights", app_id, Status::Error, e);
                return;
            }
        };
        let cur = resp
            .json::<Value>()
            .ok()
            .and_then(|v| {
                v.pointer("/data/attributes/contentRightsDeclaration")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        if cur == want {
            r.add("contentRights", app_id, Status::Exists, want);
            return;
        }
        if !apply {
            if cur.is_empty() {
                r.add("contentRights", app_id, Status::Create, want);
            } else {
                r.add_diff("contentRights", app_id, Status::Drift, cur, want);
            }
            return;
        }
        let body = json!({"data": {"type": "apps", "id": app_id, "attributes": {"contentRightsDeclaration": want}}});
        let done = if cur.is_empty() {
            Status::Created
        } else {
            Status::Updated
        };
        let detail = format!("{}→{want}", or_unset(&cur));
        self.report_write(
            r,
            "contentRights",
            app_id,
            "PATCH",
            &format!("/v1/apps/{app_id}"),
            &body,
            done,
            &detail,
        );
    }

    // --- 著作権表示 ---

    pub(super) fn reconcile_copyright(
        &self,
        apply: bool,
        version_id: &str,
        version_attrs: &Value,
        r: &mut Report,
    ) {
        let Some(s) = &self.cfg.app_store_app else {
            return;
        };
        let want = s.copyright.trim_end();
        if want.is_empty() {
            return;
        }
        let cur = version_attrs
            .get("copyright")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim_end()
            .to_string();
        if cur == want {
            r.add("copyright", version_id, Status::Exists, want);
            return;
        }
        if !apply {
            if cur.is_empty() {
                r.add("copyright", version_id, Status::Create, format!("{want:?}"));
            } else {
                r.add_diff("copyright", version_id, Status::Drift, cur, want);
            }
            return;
        }
        let body = json!({"data": {"type": "appStoreVersions", "id": version_id, "attributes": {"copyright": want}}});
        let done = if cur.is_empty() {
            Status::Created
        } else {
            Status::Updated
        };
        self.report_write(
            r,
            "copyright",
            version_id,
            "PATCH",
            &format!("/v1/appStoreVersions/{version_id}"),
            &body,
            done,
            &format!("{want:?}"),
        );
    }

    // --- 価格（無料だけ） ---

    /// 無料にする。手動の価格が 1 つ以上あり、全部が 0 の price-point なら無料。スケジュールの POST は
    /// 丸ごと置き換えるので、無料でないときだけ送る。
    pub(super) fn reconcile_price(&self, apply: bool, app_id: &str, r: &mut Report) {
        let Some(s) = &self.cfg.app_store_app else {
            return;
        };
        if s.price.is_empty() {
            return;
        }
        let (free, set) = match self.app_is_free(app_id) {
            Ok(v) => v,
            Err(e) => {
                r.add("price", app_id, Status::Error, e);
                return;
            }
        };
        if free {
            r.add("price", app_id, Status::Exists, "free");
            return;
        }
        let (pending, done, detail) = if set {
            (
                Status::Drift,
                Status::Updated,
                "free (replaces the current schedule)",
            )
        } else {
            (Status::Create, Status::Created, "free")
        };
        if !apply {
            r.add("price", app_id, pending, detail);
            return;
        }
        let point = match self.free_price_point(app_id) {
            Ok(p) => p,
            Err(e) => {
                r.add("price", app_id, Status::Error, e);
                return;
            }
        };
        let body = json!({
            "data": {
                "type": "appPriceSchedules",
                "relationships": {
                    "app": {"data": {"type": "apps", "id": app_id}},
                    "baseTerritory": {"data": {"type": "territories", "id": FREE_BASE_TERRITORY}},
                    "manualPrices": {"data": [{"type": "appPrices", "id": "${price1}"}]},
                },
            },
            "included": [{
                "type": "appPrices",
                "id": "${price1}",
                "attributes": {"startDate": null},
                "relationships": {"appPricePoint": {"data": {"type": "appPricePoints", "id": point}}},
            }],
        });
        self.report_write(
            r,
            "price",
            app_id,
            "POST",
            "/v1/appPriceSchedules",
            &body,
            done,
            "free",
        );
    }

    /// (無料か, スケジュールがあるか)。
    fn app_is_free(&self, app_id: &str) -> Result<(bool, bool), String> {
        let resp = self
            .cli
            .r#do("GET", &format!("/v1/apps/{app_id}/appPriceSchedule"), None)?;
        if resp.status == 404 {
            return Ok((false, false));
        }
        if !resp.ok() {
            return Err(format!("read price schedule: {}", resp.error()));
        }
        let Some(schedule) = crate::appstore::created_id(&resp) else {
            return Ok((false, false));
        };
        let resp = self.cli.r#do(
            "GET",
            &format!("/v1/appPriceSchedules/{schedule}/manualPrices?include=appPricePoint&fields[appPricePoints]=customerPrice&limit=200"),
            None,
        )?;
        if resp.status == 404 {
            return Ok((false, false));
        }
        if !resp.ok() {
            return Err(format!("read manual prices: {}", resp.error()));
        }
        let v: Value = resp.json()?;
        let data = v
            .get("data")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        if data == 0 {
            return Ok((false, false));
        }
        let included: Vec<&Value> = v
            .get("included")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_default();
        let all_zero = included
            .iter()
            .all(|pp| pp.get("attributes").is_some_and(is_zero_price));
        Ok((all_zero && !included.is_empty(), true))
    }

    /// 基準テリトリの 0 の price-point。
    fn free_price_point(&self, app_id: &str) -> Result<String, String> {
        let points = get_all(
            &self.cli,
            &format!(
                "/v1/apps/{app_id}/appPricePoints?filter[territory]={FREE_BASE_TERRITORY}&limit=200"
            ),
            "list app price points",
        )?;
        points
            .iter()
            .find(|pp| is_zero_price(&pp.attributes))
            .map(|pp| pp.id.clone())
            .ok_or_else(|| format!("no free price point in {FREE_BASE_TERRITORY}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_price() {
        assert!(is_zero_price(&json!({"customerPrice": "0.0"})));
        assert!(is_zero_price(&json!({"customerPrice": "0"})));
        assert!(!is_zero_price(&json!({"customerPrice": "0.99"})));
        assert!(!is_zero_price(&json!({})));
    }

    #[test]
    fn unset_label() {
        assert_eq!(or_unset(""), "(unset)");
        assert_eq!(or_unset("MUSIC"), "MUSIC");
    }
}
