//! App Store の課金カタログの今の値（`releasectl iap init` が読む。docs: 02_cli-architecture/08-init.md）。読むだけ。
//! 価格は設定テリトリ（`pricing.territory`）のものを、plan と同じ読み方で出す。

use super::{AppStore, Resource, attr, iap_price_key};
use releasectl_core::config::iso_period;
use releasectl_core::{Report, Status};
use serde_json::{Map, Value, json};

impl AppStore {
    pub(super) fn export_catalog(&self, r: &mut Report) -> Result<Value, String> {
        r.set_key_singleton(&["app"]);
        let bundle = self.cfg.bundle_id.as_str();
        if bundle.is_empty() {
            r.add("app", &self.cfg.app_name, Status::Skip, "bundleId not set");
            return Ok(Value::Null);
        }
        let Some(app_id) = self.find_app_id()? else {
            r.add(
                "app",
                bundle,
                Status::Error,
                "no App Store app for this bundle id",
            );
            return Ok(Value::Null);
        };
        r.store_id("appId", &app_id);

        let mut groups = Vec::new();
        for g in self.get_all(
            &format!("/v1/apps/{app_id}/subscriptionGroups?limit=200"),
            "list subscription groups",
        )? {
            let subs = self.get_all(
                &format!("/v1/subscriptionGroups/{}/subscriptions?limit=200", g.id),
                "list subscriptions",
            )?;
            let subs: Vec<Value> = subs
                .iter()
                .map(|s| self.export_subscription(s, r))
                .collect();
            groups.push(json!({"name": attr(&g, "referenceName"), "subscriptions": subs}));
        }

        let iaps: Vec<Value> = self
            .get_all(
                &format!("/v1/apps/{app_id}/inAppPurchasesV2?limit=200"),
                "list in-app purchases",
            )?
            .iter()
            .map(|p| self.export_iap(p, r))
            .collect();

        Ok(json!({
            "bundleId": bundle,
            "appId": app_id,
            "territory": self.cfg.pricing.territory,
            "groups": groups,
            "inAppPurchases": iaps,
        }))
    }

    fn export_subscription(&self, s: &Resource, r: &mut Report) -> Value {
        let pid = attr(s, "productId");
        let mut out = Map::new();
        out.insert("productId".into(), json!(pid));
        out.insert("state".into(), json!(attr(s, "state")));
        if let Some(p) = iso_period(attr(s, "subscriptionPeriod")) {
            out.insert("period".into(), json!(p));
        }
        // 設定テリトリの今の価格（plan と同じ: そのテリトリの price-point のうち、今付いているもの）
        match self
            .territory_price_points(&s.id)
            .and_then(|points| Ok((points, self.subscription_price_point_ids(&s.id)?)))
        {
            Err(e) => r.add("price", pid, Status::Error, e),
            Ok((points, ids)) => {
                if let Some(cp) = ids.iter().find_map(|id| points.get(id)) {
                    out.insert("price".into(), json!(cp));
                }
            }
        }
        self.export_localizations(
            &format!(
                "/v1/subscriptions/{}/subscriptionLocalizations?limit=50",
                s.id
            ),
            pid,
            &mut out,
            r,
        );
        Value::Object(out)
    }

    fn export_iap(&self, p: &Resource, r: &mut Report) -> Value {
        let pid = attr(p, "productId");
        let ty = attr(p, "inAppPurchaseType");
        let mut out = Map::new();
        out.insert("productId".into(), json!(pid));
        out.insert("type".into(), json!(ty));
        out.insert("state".into(), json!(attr(p, "state")));
        // releasectl が扱うのは非消費型だけ。ほかの種類は一覧に出すだけにする
        if ty != "NON_CONSUMABLE" {
            return Value::Object(out);
        }
        let territory = self.cfg.pricing.territory.as_str();
        match self
            .iap_price_points(&p.id)
            .and_then(|points| Ok((points, self.read_iap_schedule(&p.id)?)))
        {
            Err(e) => r.add("price", pid, Status::Error, e),
            Ok((points, (base, manual))) => {
                // plan が「一致」とみなすのは、基準のテリトリが設定テリトリで、その手動価格があるときだけ
                if base == territory
                    && let Some(key) = manual.get(territory)
                    && let Some((_, cp)) = points
                        .iter()
                        .find(|(id, _)| iap_price_key(id).as_ref() == Some(key))
                {
                    out.insert("price".into(), json!(cp));
                }
                // 基準以外のテリトリの手動価格（価格スケジュールを置き換えると消える）
                let others = manual.keys().filter(|t| **t != base).count();
                out.insert("manualTerritories".into(), json!(others));
                out.insert("baseTerritory".into(), json!(base));
            }
        }
        self.export_localizations(
            &format!(
                "/v2/inAppPurchases/{}/inAppPurchaseLocalizations?limit=50",
                p.id
            ),
            pid,
            &mut out,
            r,
        );
        Value::Object(out)
    }

    /// ロケール → {name, description}。
    fn export_localizations(
        &self,
        path: &str,
        pid: &str,
        out: &mut Map<String, Value>,
        r: &mut Report,
    ) {
        match self.get_all(path, "list localizations") {
            Err(e) => r.add("localization", pid, Status::Error, e),
            Ok(locs) => {
                let m: Map<String, Value> = locs
                    .iter()
                    .map(|l| {
                        (
                            attr(l, "locale").to_string(),
                            json!({"name": attr(l, "name"), "description": attr(l, "description")}),
                        )
                    })
                    .collect();
                out.insert("localizations".into(), Value::Object(m));
            }
        }
    }
}
