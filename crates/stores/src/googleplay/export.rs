//! Google Play の課金カタログの今の値（`releasectl iap init` が読む。docs: 02_cli-architecture/08-init.md）。読むだけ。
//! 価格は設定リージョン（`pricing.region`）のものを出す。

use super::{Money, Play};
use releasectl_core::config::format_money;
use releasectl_core::{Report, Status};
use serde_json::{Map, Value, json};

/// regionalConfigs（か regionalPricingAndAvailabilityConfigs）から、リージョンの価格を `price` / `currency` に入れる。
fn put_region_price(out: &mut Map<String, Value>, configs: Option<&Value>, region: &str) {
    let configs = configs.and_then(Value::as_array);
    out.insert("regions".into(), json!(configs.map(Vec::len).unwrap_or(0)));
    let Some(c) = configs.and_then(|a| {
        a.iter()
            .find(|c| c.get("regionCode").and_then(Value::as_str) == Some(region))
    }) else {
        return;
    };
    if let Some(a) = c.get("availability").and_then(Value::as_str) {
        out.insert("availability".into(), json!(a));
    }
    // Play は 0 の項目を省く（$0.99 は units 無し）。plan と同じ型で読んでそろえる
    if let Some(m) = c
        .get("price")
        .and_then(|p| serde_json::from_value::<Money>(p.clone()).ok())
    {
        out.insert("price".into(), json!(format_money(&m.units, m.nanos)));
        out.insert("currency".into(), json!(m.currency_code));
    }
}

fn listings(v: &Value) -> Value {
    Value::Object(
        v.get("listings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|l| {
                let s = |k: &str| l.get(k).and_then(Value::as_str).unwrap_or("");
                Some((
                    l.get("languageCode")?.as_str()?.to_string(),
                    json!({"title": s("title"), "description": s("description")}),
                ))
            })
            .collect(),
    )
}

fn subscription(v: &Value, region: &str) -> Value {
    let plans: Vec<Value> = v
        .get("basePlans")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|bp| {
            let s = |k: &str| bp.get(k).and_then(Value::as_str).unwrap_or("");
            let mut out = Map::new();
            out.insert("basePlanId".into(), json!(s("basePlanId")));
            out.insert("state".into(), json!(s("state")));
            let ty = ["autoRenewing", "prepaid", "installments"]
                .into_iter()
                .find(|t| bp.get(format!("{t}BasePlanType")).is_some())
                .unwrap_or("");
            out.insert("type".into(), json!(ty));
            if let Some(p) = bp
                .pointer("/autoRenewingBasePlanType/billingPeriodDuration")
                .and_then(Value::as_str)
            {
                out.insert("period".into(), json!(p));
            }
            put_region_price(&mut out, bp.get("regionalConfigs"), region);
            Value::Object(out)
        })
        .collect();
    json!({
        "productId": v.get("productId").and_then(Value::as_str).unwrap_or(""),
        "listings": listings(v),
        "basePlans": plans,
    })
}

fn one_time_product(v: &Value, region: &str) -> Value {
    let options: Vec<Value> = v
        .get("purchaseOptions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|po| {
            let s = |k: &str| po.get(k).and_then(Value::as_str).unwrap_or("");
            let mut out = Map::new();
            out.insert("id".into(), json!(s("purchaseOptionId")));
            out.insert("state".into(), json!(s("state")));
            let ty = ["buy", "rent"]
                .into_iter()
                .find(|t| po.get(format!("{t}Option")).is_some())
                .unwrap_or("");
            out.insert("type".into(), json!(ty));
            put_region_price(
                &mut out,
                po.get("regionalPricingAndAvailabilityConfigs"),
                region,
            );
            Value::Object(out)
        })
        .collect();
    json!({
        "productId": v.get("productId").and_then(Value::as_str).unwrap_or(""),
        "listings": listings(v),
        "purchaseOptions": options,
    })
}

impl Play {
    pub(super) fn export_catalog(&self, r: &mut Report) -> Result<Value, String> {
        r.set_key_singleton(&["app"]);
        let pkg = self.cfg.package_name.as_str();
        if pkg.is_empty() {
            r.add(
                "app",
                &self.cfg.app_name,
                Status::Skip,
                "packageName not set",
            );
            return Ok(Value::Null);
        }
        r.store_id("packageName", pkg);
        let region = self.cfg.pricing.region.as_str();
        let subs = self.list_all(
            &format!("/applications/{pkg}/subscriptions"),
            "subscriptions",
        )?;
        // NOTE: 一回課金の GET は camelCase の `oneTimeProducts`（小文字は書き込み系だけ）
        let one_times = self.list_all(
            &format!("/applications/{pkg}/oneTimeProducts"),
            "oneTimeProducts",
        )?;
        Ok(json!({
            "packageName": pkg,
            "region": region,
            "subscriptions": subs.iter().map(|s| subscription(s, region)).collect::<Vec<_>>(),
            "oneTimeProducts": one_times.iter().map(|p| one_time_product(p, region)).collect::<Vec<_>>(),
        }))
    }

    /// 一覧を nextPageToken をたどって全部取る。
    fn list_all(&self, path: &str, field: &str) -> Result<Vec<Value>, String> {
        let mut out = Vec::new();
        let mut token = String::new();
        loop {
            let mut url = format!("{path}?pageSize=100");
            if !token.is_empty() {
                url.push_str(&format!("&pageToken={}", super::q(&token)));
            }
            let resp = self.cli.r#do("GET", &url, None)?;
            if !resp.ok() {
                return Err(format!("list {field}: {}", resp.error()));
            }
            let mut v: Value = resp.json()?;
            if let Some(a) = v.get_mut(field).and_then(Value::as_array_mut) {
                out.append(a);
            }
            token = v
                .get("nextPageToken")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if token.is_empty() {
                return Ok(out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_base_plans_and_region_price() {
        let v = json!({
            "productId": "pro",
            "listings": [{"languageCode": "ja-JP", "title": "月額", "description": "説明"}, {"languageCode": "en-US", "title": "Monthly"}],
            "basePlans": [
                {"basePlanId": "monthly", "state": "ACTIVE", "autoRenewingBasePlanType": {"billingPeriodDuration": "P1M"},
                 "regionalConfigs": [{"regionCode": "JP", "price": {"currencyCode": "JPY", "units": "600"}},
                                     {"regionCode": "US", "price": {"currencyCode": "USD", "nanos": 990000000}}]},
                {"basePlanId": "pass", "state": "DRAFT", "prepaidBasePlanType": {"billingPeriodDuration": "P1M"}, "regionalConfigs": []}
            ]
        });
        assert_eq!(
            subscription(&v, "US"),
            json!({
                "productId": "pro",
                "listings": {"ja-JP": {"title": "月額", "description": "説明"}, "en-US": {"title": "Monthly", "description": ""}},
                "basePlans": [
                    {"basePlanId": "monthly", "state": "ACTIVE", "type": "autoRenewing", "period": "P1M", "regions": 2, "price": "0.99", "currency": "USD"},
                    {"basePlanId": "pass", "state": "DRAFT", "type": "prepaid", "regions": 0}
                ]
            })
        );
    }

    #[test]
    fn one_time_purchase_options() {
        let v = json!({
            "productId": "lifetime",
            "listings": [],
            "purchaseOptions": [{
                "purchaseOptionId": "buy", "state": "ACTIVE", "buyOption": {"legacyCompatible": true},
                "regionalPricingAndAvailabilityConfigs": [{"regionCode": "US", "availability": "AVAILABLE", "price": {"currencyCode": "USD", "units": "29", "nanos": 990000000}}]
            }]
        });
        assert_eq!(
            one_time_product(&v, "US"),
            json!({
                "productId": "lifetime",
                "listings": {},
                "purchaseOptions": [{"id": "buy", "state": "ACTIVE", "type": "buy", "regions": 1, "availability": "AVAILABLE", "price": "29.99", "currency": "USD"}]
            })
        );
    }
}
