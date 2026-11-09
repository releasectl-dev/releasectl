//! `iap.yaml`（desired-state）の読み込みと検証。
//!
//! 1 枚のファイルで課金カタログ全体を書き、各ストアの reconciler は自分が使う
//! フィールドだけを読む。Go 版 `internal/config` と同じ形・同じ検証。
//! Go 版にあった `entitlement` / `revenuecat` / `rcPackage` は RevenueCat 用で、
//! Rust 版は RevenueCat を扱わない（2026-11-09）。既存の iap.yaml をそのまま
//! 読めるよう、キーとしては受け付けて無視する。

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("read config: {0}")]
    Read(#[from] std::io::Error),
    #[error("parse {path}: {source}")]
    Parse {
        path: String,
        source: serde_yaml_ng::Error,
    },
    #[error("{0}")]
    Invalid(String),
}

/// パース済みの iap.yaml。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub app_name: String,
    /// App Store の bundle id
    #[serde(default)]
    pub bundle_id: String,
    /// Google Play の package name
    #[serde(default)]
    pub package_name: String,
    /// RevenueCat 用（Rust 版では使わない）
    #[serde(default)]
    pub entitlement: Option<Ignored>,
    #[serde(default)]
    pub pricing: Pricing,
    #[serde(default)]
    pub products: Vec<Product>,
    /// RevenueCat 用（Rust 版では使わない）
    #[serde(default)]
    pub revenuecat: Option<Ignored>,
    #[serde(default)]
    pub appstore: AppStore,
}

/// 中身を見ずに読み飛ばす値。
#[derive(Debug, Clone, Deserialize)]
pub struct Ignored(#[allow(dead_code)] serde_yaml_ng::Value);

/// 1 つのテリトリ / リージョン / 通貨（多地域は対象外）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pricing {
    /// App Store のテリトリ（ISO-3166 alpha-3）
    #[serde(default)]
    pub territory: String,
    /// Google Play のリージョン（ISO-3166 alpha-2）
    #[serde(default)]
    pub region: String,
    /// ISO-4217
    #[serde(default)]
    pub currency: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductType {
    Subscription,
    NonConsumable,
}

pub const TYPE_SUBSCRIPTION: &str = "subscription";
pub const TYPE_NON_CONSUMABLE: &str = "nonConsumable";

/// 販売する 1 商品。自動更新サブスク（既定）か買い切り（`type: nonConsumable`）。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Product {
    #[serde(default)]
    pub key: String,
    /// "" / "subscription"（既定）| "nonConsumable"
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub app_store_id: String,
    #[serde(default)]
    pub play_product_id: String,
    #[serde(default)]
    pub play_base_plan_id: String,
    /// サブスクのみ。ISO 8601 の P1W/P1M/P2M/P3M/P6M/P1Y
    #[serde(default)]
    pub period: String,
    /// 通貨の主単位。小数可（4.99 = $4.99、600 = ¥600）
    #[serde(default)]
    pub price: f64,
    /// RevenueCat 用（Rust 版では使わない）
    #[serde(default)]
    pub rc_package: String,
    #[serde(default)]
    pub localizations: Vec<Localization>,
}

impl Product {
    pub fn product_type(&self) -> ProductType {
        if self.kind == TYPE_NON_CONSUMABLE {
            ProductType::NonConsumable
        } else {
            ProductType::Subscription
        }
    }

    /// 買い切り（非消費型）なら true。
    pub fn is_one_time(&self) -> bool {
        self.product_type() == ProductType::NonConsumable
    }

    /// 価格の整数部（Google Play の money.units。4.99 → "4"、600 → "600"）。
    pub fn price_units(&self) -> String {
        ((self.price + 1e-9).floor() as i64).to_string()
    }

    /// 価格の小数部を nanos で（4.99 → 990000000、600 → 0）。
    pub fn price_nanos(&self) -> i64 {
        let frac = self.price - (self.price + 1e-9).floor();
        (frac * 1e9).round() as i64
    }

    /// 表示用の価格（"4.99"、"600"）。Go の `strconv.FormatFloat(f, 'f', -1, 64)` と同じ。
    pub fn price_label(&self) -> String {
        format_float(self.price)
    }
}

/// f64 を最短の 10 進表記にする（指数表記にしない）。
pub fn format_float(f: f64) -> String {
    // Rust の Display は最短の往復表記で、整数値は小数点を付けない。
    format!("{f}")
}

/// Google Play の money（units + nanos）を表示用にする（"4" + 990000000 → "4.99"、"600" + 0 → "600"）。
/// `Product::price_label` と同じ見た目になるので、現在値と希望値をそのまま並べられる。
pub fn format_money(units: &str, nanos: i64) -> String {
    if nanos == 0 {
        return units.to_string();
    }
    let frac = format!("{:09}", nanos.unsigned_abs());
    format!("{units}.{}", frac.trim_end_matches('0'))
}

/// 商品のロケールごとの表示名と説明。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Localization {
    /// BCP-47。App Store の形（"ja"）。Play は ja-JP に読み替える
    #[serde(default)]
    pub locale: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AppStore {
    /// サブスクリプショングループの referenceName
    #[serde(default)]
    pub subscription_group: String,
}

impl Config {
    /// path を読み、パースして検証する。
    pub fn load(path: impl AsRef<Path>) -> Result<Config, ConfigError> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)?;
        Self::parse(&raw, &path.display().to_string())
    }

    pub fn parse(raw: &str, path: &str) -> Result<Config, ConfigError> {
        let cfg: Config = serde_yaml_ng::from_str(raw).map_err(|source| ConfigError::Parse {
            path: path.to_string(),
            source,
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |m: String| Err(ConfigError::Invalid(m));
        if self.products.is_empty() {
            return invalid("at least one product is required".into());
        }
        if self.pricing.currency.is_empty() {
            return invalid("pricing.currency is required".into());
        }
        let mut seen = std::collections::HashSet::new();
        for (i, p) in self.products.iter().enumerate() {
            if p.key.is_empty() {
                return invalid(format!("products[{i}].key is required"));
            }
            if !seen.insert(p.key.as_str()) {
                return invalid(format!("duplicate product key {:?}", p.key));
            }
            match p.kind.as_str() {
                "" | TYPE_SUBSCRIPTION => {
                    if p.period.is_empty() {
                        return invalid(format!(
                            "products[{i}] ({}): period is required for subscriptions",
                            p.key
                        ));
                    }
                }
                TYPE_NON_CONSUMABLE => {}
                other => {
                    return invalid(format!(
                        "products[{i}] ({}): unknown type {other:?} (use {TYPE_SUBSCRIPTION:?} or {TYPE_NON_CONSUMABLE:?})",
                        p.key
                    ));
                }
            }
            if p.price <= 0.0 {
                return invalid(format!("products[{i}] ({}): price must be > 0", p.key));
            }
        }
        Ok(())
    }
}

/// ISO 8601 の期間を App Store Connect の subscriptionPeriod に読み替える。
pub fn asc_period(iso: &str) -> Result<&'static str, String> {
    Ok(match iso {
        "P1W" => "ONE_WEEK",
        "P1M" => "ONE_MONTH",
        "P2M" => "TWO_MONTHS",
        "P3M" => "THREE_MONTHS",
        "P6M" => "SIX_MONTHS",
        "P1Y" => "ONE_YEAR",
        _ => {
            return Err(format!(
                "unsupported period {iso:?} (use P1W/P1M/P2M/P3M/P6M/P1Y)"
            ));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"
appName: Demo
bundleId: com.example.demo
packageName: com.example.demo
entitlement: { key: pro, displayName: Pro }
pricing: { territory: USA, region: US, currency: USD }
products:
  - key: monthly
    appStoreId: demo.monthly
    playProductId: demo_monthly
    playBasePlanId: monthly
    period: P1M
    price: 4.99
    rcPackage: $rc_monthly
  - key: lifetime
    type: nonConsumable
    playProductId: demo_lifetime
    price: 600
revenuecat: { project: Demo, offering: default }
"#;

    fn product(price: f64) -> Product {
        let mut c = Config::parse(BASE, "t").unwrap();
        let mut p = c.products.remove(0);
        p.price = price;
        p
    }

    #[test]
    fn parses_go_era_file_with_revenuecat_keys() {
        let c = Config::parse(BASE, "t").unwrap();
        assert_eq!(c.products.len(), 2);
        assert!(c.products[1].is_one_time());
        assert!(!c.products[0].is_one_time());
    }

    #[test]
    fn money_split_matches_go() {
        for (price, units, nanos, label) in [
            (4.99, "4", 990_000_000, "4.99"),
            (44.99, "44", 990_000_000, "44.99"),
            (89.99, "89", 990_000_000, "89.99"),
            (600.0, "600", 0, "600"),
            (0.5, "0", 500_000_000, "0.5"),
        ] {
            let p = product(price);
            assert_eq!(p.price_units(), units, "{price}");
            assert_eq!(p.price_nanos(), nanos, "{price}");
            assert_eq!(p.price_label(), label, "{price}");
        }
    }

    #[test]
    fn format_money_matches_price_label() {
        assert_eq!(format_money("4", 990_000_000), "4.99");
        assert_eq!(format_money("44", 990_000_000), "44.99");
        assert_eq!(format_money("600", 0), "600");
        assert_eq!(format_money("0", 500_000_000), "0.5");
        assert_eq!(format_money("1", 5), "1.000000005");
        for price in [4.99, 44.99, 89.99, 600.0, 0.5] {
            let p = product(price);
            assert_eq!(
                format_money(&p.price_units(), p.price_nanos()),
                p.price_label()
            );
        }
    }

    #[test]
    fn rejects_unknown_keys() {
        let raw = BASE.replace("period: P1M", "perod: P1M");
        assert!(matches!(
            Config::parse(&raw, "t"),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn validation_messages() {
        let cases = [
            (
                BASE.replace("    period: P1M\n", ""),
                "products[0] (monthly): period is required for subscriptions",
            ),
            (
                BASE.replace("price: 600", "price: 0"),
                "products[1] (lifetime): price must be > 0",
            ),
            (
                BASE.replace("currency: USD", "currency: \"\""),
                "pricing.currency is required",
            ),
            (
                BASE.replace("key: lifetime", "key: monthly"),
                "duplicate product key \"monthly\"",
            ),
            (
                BASE.replace("type: nonConsumable", "type: consumable"),
                "products[1] (lifetime): unknown type \"consumable\" (use \"subscription\" or \"nonConsumable\")",
            ),
        ];
        for (raw, want) in cases {
            let err = Config::parse(&raw, "t").unwrap_err().to_string();
            assert_eq!(err, want);
        }
    }

    #[test]
    fn asc_periods() {
        assert_eq!(asc_period("P1Y").unwrap(), "ONE_YEAR");
        assert!(asc_period("P5D").is_err());
    }
}
