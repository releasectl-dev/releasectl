//! `releasectl iap init`: ストアの今の課金カタログを `iap.yaml` の形に畳む
//! （docs: 02_cli-architecture/08-init.md）。I/O は持たない。
//!
//! 契約: 書き出した YAML をそのまま `iap plan` に通すと差分が出ない。iap.yaml の形（1 つの商品 = 両ストアで
//! 同じ期間・価格・文言、サブスクリプショングループは 1 つ）に入らないものは書かず、`notes` で人に伝える。

use crate::config::Pricing;
use crate::yaml_out::{Yaml, scalar};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

pub struct IapInitInput<'a> {
    pub app_name: &'a str,
    /// YAML の先頭に入れるコメント
    pub header: &'a [String],
    pub pricing: &'a Pricing,
    /// 書き出すサブスクリプショングループ（空なら、サブスクが一番多いグループ）
    pub subscription_group: &'a str,
    /// App Store の今のカタログ（流さなかったら None）
    pub appstore: Option<&'a Value>,
    /// Google Play の今のカタログ
    pub googleplay: Option<&'a Value>,
}

pub struct IapInit {
    pub yaml: String,
    /// 書いた商品の数（0 なら YAML は使えない）
    pub products: usize,
    /// YAML に書けなかったもの・人が確かめること
    pub notes: Vec<String>,
}

/// ロケール → (名前, 説明)。
type Locs = BTreeMap<String, (String, String)>;

/// YAML に書ける 1 商品（片方のストアの分）。
#[derive(Debug, Clone, PartialEq)]
struct Cand {
    id: String,
    /// Google Play のサブスクの base plan
    base_plan: String,
    one_time: bool,
    period: String,
    price: String,
    locs: Locs,
    /// 商品の上に入れるコメント
    comment: Option<String>,
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn list<'a>(v: Option<&'a Value>, key: &str) -> &'a [Value] {
    v.and_then(|v| v.get(key))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn removed(state: &str) -> bool {
    state.contains("REMOVED_FROM_SALE")
}

fn asc_locs(v: &Value) -> Locs {
    v.get("localizations")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(locale, l)| {
            (
                locale.clone(),
                (s(l, "name").to_string(), s(l, "description").to_string()),
            )
        })
        .collect()
}

/// Play の languageCode → iap.yaml のロケール（Play の reconciler が "ja" を "ja-JP" に読み替えるのの逆）。
fn play_locs(v: &Value) -> Locs {
    v.get("listings")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(lang, l)| {
            let locale = if lang == "ja-JP" { "ja" } else { lang.as_str() };
            (
                locale.to_string(),
                (s(l, "title").to_string(), s(l, "description").to_string()),
            )
        })
        .collect()
}

fn same_price(a: &str, b: &str) -> bool {
    matches!((a.parse::<f64>(), b.parse::<f64>()), (Ok(x), Ok(y)) if (x - y).abs() < 1e-9)
}

/// App Store の書ける商品。書けないものは notes に入れる。2 つ目は書き出したグループの名前。
fn appstore_cands(input: &IapInitInput, notes: &mut Vec<String>) -> (Vec<Cand>, String) {
    let territory = input.pricing.territory.as_str();
    let mut out = Vec::new();
    let groups = list(input.appstore, "groups");
    // グループは 1 つしか書けない: 指定があればそれ、無ければサブスクが一番多いもの
    let count = |g: &Value| list(Some(g), "subscriptions").len();
    let chosen = if input.subscription_group.is_empty() {
        groups
            .iter()
            .filter(|g| count(g) > 0)
            .max_by(|a, b| count(a).cmp(&count(b)).then(s(b, "name").cmp(s(a, "name"))))
    } else {
        let g = groups
            .iter()
            .find(|g| s(g, "name") == input.subscription_group);
        if g.is_none() && input.appstore.is_some() {
            notes.push(format!(
                "App Store: subscription group {:?} not found; no subscriptions written.",
                input.subscription_group
            ));
        }
        g
    };
    let group_name = chosen.map(|g| s(g, "name").to_string()).unwrap_or_default();
    for g in groups {
        if s(g, "name") != group_name && count(g) > 0 {
            notes.push(format!(
                "App Store: subscription group {:?} ({} subscriptions) not written — iap.yaml holds one group ({:?}). Pass --subscription-group to pick another.",
                s(g, "name"),
                count(g),
                group_name
            ));
        }
    }
    for sub in chosen
        .map(|g| list(Some(g), "subscriptions"))
        .unwrap_or(&[])
    {
        let id = s(sub, "productId");
        let skip = if removed(s(sub, "state")) {
            Some(format!("removed from sale ({})", s(sub, "state")))
        } else if s(sub, "period").is_empty() {
            Some("unsupported subscription period".to_string())
        } else if s(sub, "price").is_empty() {
            Some(format!("no price in {territory}"))
        } else {
            None
        };
        if let Some(why) = skip {
            notes.push(format!("App Store: {id} not written — {why}."));
            continue;
        }
        out.push(Cand {
            id: id.to_string(),
            base_plan: String::new(),
            one_time: false,
            period: s(sub, "period").to_string(),
            price: s(sub, "price").to_string(),
            locs: asc_locs(sub),
            comment: None,
        });
    }
    for p in list(input.appstore, "inAppPurchases") {
        let id = s(p, "productId");
        let base = s(p, "baseTerritory");
        let skip = if s(p, "type") != "NON_CONSUMABLE" {
            Some(format!(
                "{} (releasectl manages subscriptions and non-consumables)",
                s(p, "type")
            ))
        } else if removed(s(p, "state")) {
            Some(format!("removed from sale ({})", s(p, "state")))
        } else if !base.is_empty() && base != territory {
            Some(format!(
                "its price schedule is based on {base}, not {territory}. Run init again with --territory {base} (and the matching --region / --currency)"
            ))
        } else if s(p, "price").is_empty() {
            Some(format!("no price in {territory}"))
        } else {
            None
        };
        if let Some(why) = skip {
            notes.push(format!("App Store: {id} not written — {why}."));
            continue;
        }
        let manual = p
            .get("manualTerritories")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let comment = (manual > 0).then(|| {
            notes.push(format!(
                "App Store: {id} has manual prices in {manual} other territories. Changing its `price` replaces the price schedule and resets them."
            ));
            format!(
                "releasectl init: {manual} other territories have manual prices on the App Store. Changing `price` resets them."
            )
        });
        out.push(Cand {
            id: id.to_string(),
            base_plan: String::new(),
            one_time: true,
            period: String::new(),
            price: s(p, "price").to_string(),
            locs: asc_locs(p),
            comment,
        });
    }
    (out, group_name)
}

/// Google Play の書ける商品。
fn play_cands(input: &IapInitInput, notes: &mut Vec<String>) -> Vec<Cand> {
    let (region, currency) = (
        input.pricing.region.as_str(),
        input.pricing.currency.as_str(),
    );
    // そのリージョンの価格が書けるか（無い・通貨が違う）
    let price_problem = |v: &Value| {
        if s(v, "price").is_empty() {
            Some(format!("no price in {region}"))
        } else if s(v, "currency") != currency {
            Some(format!(
                "its {region} price is in {}, not {currency}",
                s(v, "currency")
            ))
        } else {
            None
        }
    };
    let mut out = Vec::new();
    for sub in list(input.googleplay, "subscriptions") {
        let id = s(sub, "productId");
        for bp in list(Some(sub), "basePlans") {
            let bpid = s(bp, "basePlanId");
            let skip = if s(bp, "type") != "autoRenewing" {
                Some(format!("{} base plan", s(bp, "type")))
            } else if s(bp, "state") != "ACTIVE" {
                // 書くと apply が有効化する
                Some(format!("base plan is {}", s(bp, "state")))
            } else {
                price_problem(bp)
            };
            if let Some(why) = skip {
                notes.push(format!("Google Play: {id} / {bpid} not written — {why}."));
                continue;
            }
            out.push(Cand {
                id: id.to_string(),
                base_plan: bpid.to_string(),
                one_time: false,
                period: s(bp, "period").to_string(),
                price: s(bp, "price").to_string(),
                locs: play_locs(sub),
                comment: None,
            });
        }
    }
    for p in list(input.googleplay, "oneTimeProducts") {
        let id = s(p, "productId");
        let options = list(Some(p), "purchaseOptions");
        // releasectl が管理するのは、購入オプションが "buy" 1 つだけの商品
        let skip = match options {
            [o] if s(o, "id") == "buy" && s(o, "type") == "buy" => {
                if s(o, "state") != "ACTIVE" {
                    Some(format!("purchase option is {}", s(o, "state")))
                } else {
                    price_problem(o)
                }
            }
            _ => Some(format!(
                "its purchase options are [{}]; releasectl manages products with a single `buy` option",
                options
                    .iter()
                    .map(|o| s(o, "id"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        };
        if let Some(why) = skip {
            notes.push(format!("Google Play: {id} not written — {why}."));
            continue;
        }
        out.push(Cand {
            id: id.to_string(),
            base_plan: String::new(),
            one_time: true,
            period: String::new(),
            price: s(&options[0], "price").to_string(),
            locs: play_locs(p),
            comment: None,
        });
    }
    out
}

/// YAML の 1 商品（片方か両方のストア）。
struct Entry {
    asc: Option<Cand>,
    play: Option<Cand>,
}

impl Entry {
    fn any(&self) -> &Cand {
        self.asc.as_ref().or(self.play.as_ref()).expect("one side")
    }
}

/// ID の最後の部分を key にする（dev.example.app.monthly → monthly）。
fn key_base(id: &str) -> String {
    let last = id.rsplit('.').next().unwrap_or(id);
    let k: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if k.is_empty() { "product".into() } else { k }
}

pub fn iap_yaml(input: &IapInitInput) -> IapInit {
    let mut notes = Vec::new();
    let (asc, group_name) = appstore_cands(input, &mut notes);
    let mut play = play_cands(input, &mut notes);

    // 同じ ID の商品は、期間・価格・文言が全部同じときだけ 1 つにまとめる（違えば plan に差分が出る）
    let mut entries: Vec<Entry> = Vec::new();
    for a in asc {
        let same_id: Vec<usize> = (0..play.len()).filter(|i| play[*i].id == a.id).collect();
        let merged = match same_id.as_slice() {
            [i] => {
                let p = &play[*i];
                let mut differ = Vec::new();
                if p.one_time != a.one_time {
                    differ.push("type");
                }
                if p.period != a.period {
                    differ.push("period");
                }
                if !same_price(&p.price, &a.price) {
                    differ.push("price");
                }
                if p.locs != a.locs {
                    differ.push("localizations");
                }
                if differ.is_empty() {
                    Some(play.remove(*i))
                } else {
                    notes.push(format!(
                        "{}: on both stores, but {} differ — written as two products. Make them the same in the stores to merge them.",
                        a.id,
                        differ.join(" / ")
                    ));
                    None
                }
            }
            _ => None,
        };
        entries.push(Entry {
            asc: Some(a),
            play: merged,
        });
    }
    entries.extend(play.into_iter().map(|p| Entry {
        asc: None,
        play: Some(p),
    }));

    // key を決める（重なったらストアの名前、まだ重なれば番号を足す）
    let bases: Vec<String> = entries
        .iter()
        .map(|e| match (&e.asc, &e.play) {
            (Some(a), _) => key_base(&a.id),
            (None, Some(p)) => {
                let multi = entries
                    .iter()
                    .filter(|x| x.play.as_ref().is_some_and(|q| q.id == p.id))
                    .count()
                    > 1;
                if multi {
                    format!("{}_{}", key_base(&p.id), key_base(&p.base_plan))
                } else {
                    key_base(&p.id)
                }
            }
            (None, None) => unreachable!("an entry has one side"),
        })
        .collect();
    let mut seen: HashSet<String> = HashSet::new();
    let keys: Vec<String> = entries
        .iter()
        .zip(&bases)
        .map(|(e, base)| {
            let clash = bases.iter().filter(|b| *b == base).count() > 1;
            let mut key = match (clash, e.asc.is_some(), e.play.is_some()) {
                (true, true, false) => format!("{base}_appstore"),
                (true, false, true) => format!("{base}_play"),
                _ => base.clone(),
            };
            let stem = key.clone();
            let mut n = 2;
            while !seen.insert(key.clone()) {
                key = format!("{stem}_{n}");
                n += 1;
            }
            key
        })
        .collect();

    let mut y = Yaml::default();
    for h in input.header {
        y.comment(0, h);
    }
    y.str(0, "appName", input.app_name);
    y.str(
        0,
        "bundleId",
        input.appstore.map(|a| s(a, "bundleId")).unwrap_or(""),
    );
    y.str(
        0,
        "packageName",
        input.googleplay.map(|p| s(p, "packageName")).unwrap_or(""),
    );
    y.blank();
    y.line(0, "pricing:");
    y.str(2, "territory", &input.pricing.territory);
    y.str(2, "region", &input.pricing.region);
    y.str(2, "currency", &input.pricing.currency);
    y.blank();
    y.line(0, "products:");
    for (i, (e, key)) in entries.iter().zip(&keys).enumerate() {
        if i > 0 {
            y.blank();
        }
        let c = e.any();
        if let Some(comment) = e.asc.as_ref().and_then(|a| a.comment.as_deref()) {
            y.comment(2, comment);
        }
        y.line(2, &format!("- key: {}", scalar(key)));
        if c.one_time {
            y.line(4, "type: nonConsumable");
        }
        if let Some(a) = &e.asc {
            y.str(4, "appStoreId", &a.id);
        }
        if let Some(p) = &e.play {
            y.str(4, "playProductId", &p.id);
            y.str(4, "playBasePlanId", &p.base_plan);
        }
        y.str(4, "period", &c.period);
        y.num(4, "price", &c.price);
        if !c.locs.is_empty() {
            y.line(4, "localizations:");
            for (locale, (name, description)) in &c.locs {
                y.line(6, &format!("- locale: {}", scalar(locale)));
                // 文言は 1 文字でも違うと差分になるので、末尾の空白も落とさずに引用符で書く
                y.line(8, &format!("name: {}", scalar(name)));
                if !description.is_empty() {
                    y.line(8, &format!("description: {}", scalar(description)));
                }
            }
        }
    }
    if entries
        .iter()
        .any(|e| e.asc.as_ref().is_some_and(|a| !a.one_time))
    {
        y.blank();
        y.line(0, "appstore:");
        y.str(2, "subscriptionGroup", &group_name);
    }
    IapInit {
        yaml: y.finish(),
        products: entries.len(),
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use serde_json::json;

    fn pricing() -> Pricing {
        Pricing {
            territory: "USA".into(),
            region: "US".into(),
            currency: "USD".into(),
        }
    }

    fn run(asc: Option<&Value>, play: Option<&Value>, group: &str) -> (Config, IapInit) {
        let out = iap_yaml(&IapInitInput {
            app_name: "demo",
            header: &["Generated.".to_string()],
            pricing: &pricing(),
            subscription_group: group,
            appstore: asc,
            googleplay: play,
        });
        let cfg = Config::parse(&out.yaml, "t").unwrap_or_else(|e| panic!("{e}\n{}", out.yaml));
        (cfg, out)
    }

    fn loc(name: &str, description: &str) -> Value {
        json!({"name": name, "description": description})
    }

    fn asc() -> Value {
        json!({
            "bundleId": "com.example.demo",
            "groups": [
                {"name": "Pro", "subscriptions": [
                    {"productId": "com.example.demo.monthly", "state": "APPROVED", "period": "P1M", "price": "4.99",
                     "localizations": {"ja": loc("月額", "説明: 1"), "en-US": loc("Monthly", "")}},
                    {"productId": "pro_yearly", "state": "APPROVED", "period": "P1Y", "price": "39.99",
                     "localizations": {"en-US": loc("Yearly", "Yearly plan")}},
                    {"productId": "com.example.demo.old", "state": "DEVELOPER_REMOVED_FROM_SALE", "period": "P1M", "price": "1.99"},
                    {"productId": "com.example.demo.nousa", "state": "APPROVED", "period": "P1M"}
                ]},
                {"name": "Legacy", "subscriptions": [{"productId": "legacy.monthly", "state": "APPROVED", "period": "P1M", "price": "0.99"}]},
                {"name": "Empty", "subscriptions": []}
            ],
            "inAppPurchases": [
                {"productId": "com.example.demo.lifetime", "type": "NON_CONSUMABLE", "state": "APPROVED", "baseTerritory": "USA",
                 "price": "29.99", "manualTerritories": 3, "localizations": {"en-US": loc("Lifetime", "One-time")}},
                {"productId": "com.example.demo.coins", "type": "CONSUMABLE", "state": "APPROVED"},
                {"productId": "com.example.demo.jp", "type": "NON_CONSUMABLE", "state": "APPROVED", "baseTerritory": "JPN", "manualTerritories": 0}
            ]
        })
    }

    fn play() -> Value {
        let bp = |id: &str, state: &str, ty: &str, period: &str, price: &str, currency: &str| json!({"basePlanId": id, "state": state, "type": ty, "period": period, "price": price, "currency": currency});
        json!({
            "packageName": "com.example.demo",
            "subscriptions": [
                // App Store の pro_yearly と同じ ID・期間・価格・文言 → 1 つにまとまる
                {"productId": "pro_yearly", "listings": {"en-US": {"title": "Yearly", "description": "Yearly plan"}},
                 "basePlans": [bp("yearly", "ACTIVE", "autoRenewing", "P1Y", "39.99", "USD")]},
                {"productId": "pro", "listings": {"ja-JP": {"title": "プロ", "description": ""}},
                 "basePlans": [
                    bp("monthly", "ACTIVE", "autoRenewing", "P1M", "4.99", "USD"),
                    bp("weekly", "ACTIVE", "autoRenewing", "P1W", "0.99", "USD"),
                    bp("draft", "DRAFT", "autoRenewing", "P1M", "4.99", "USD"),
                    bp("pass", "ACTIVE", "prepaid", "", "4.99", "USD"),
                    bp("eur", "ACTIVE", "autoRenewing", "P1M", "4.99", "EUR"),
                    json!({"basePlanId": "nous", "state": "ACTIVE", "type": "autoRenewing", "period": "P1M"})
                 ]}
            ],
            "oneTimeProducts": [
                {"productId": "lifetime", "listings": {"en-US": {"title": "Lifetime", "description": "One-time"}},
                 "purchaseOptions": [{"id": "buy", "state": "ACTIVE", "type": "buy", "price": "29.99", "currency": "USD"}]},
                {"productId": "legacy", "listings": {},
                 "purchaseOptions": [{"id": "legacy-base", "state": "ACTIVE", "type": "buy", "price": "1", "currency": "USD"}]}
            ]
        })
    }

    #[test]
    fn folds_both_stores() {
        let (c, out) = run(Some(&asc()), Some(&play()), "");
        let keys: Vec<&str> = c.products.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "monthly",
                "pro_yearly",
                "lifetime_appstore",
                "pro_monthly",
                "pro_weekly",
                "lifetime_play"
            ]
        );
        assert_eq!(out.products, 6);
        assert_eq!(c.appstore.subscription_group, "Pro");
        assert_eq!(c.pricing.territory, "USA");

        let m = &c.products[0];
        assert_eq!(
            (m.app_store_id.as_str(), m.play_product_id.as_str()),
            ("com.example.demo.monthly", "")
        );
        assert_eq!((m.period.as_str(), m.price), ("P1M", 4.99));
        // ロケールは辞書順。空の説明は書かない
        assert_eq!(m.localizations[0].locale, "en-US");
        assert!(m.localizations[0].description.is_empty());
        assert_eq!(m.localizations[1].description, "説明: 1");

        // 両ストアで同じ → 1 つ
        let yr = &c.products[1];
        assert_eq!(
            (
                yr.app_store_id.as_str(),
                yr.play_product_id.as_str(),
                yr.play_base_plan_id.as_str()
            ),
            ("pro_yearly", "pro_yearly", "yearly")
        );

        let life = &c.products[2];
        assert!(life.is_one_time() && life.play_product_id.is_empty());
        assert!(
            out.yaml
                .contains("  # releasectl init: 3 other territories have manual prices")
        );

        // Play の 1 つのサブスクに base plan が 2 つ → 2 商品
        let (pm, pw) = (&c.products[3], &c.products[4]);
        assert_eq!(
            (pm.play_product_id.as_str(), pm.play_base_plan_id.as_str()),
            ("pro", "monthly")
        );
        assert_eq!(
            (pw.play_base_plan_id.as_str(), pw.period.as_str(), pw.price),
            ("weekly", "P1W", 0.99)
        );
        assert_eq!(pm.localizations[0].locale, "ja");
        assert!(c.products[5].is_one_time());

        let notes = out.notes.join("\n");
        for want in [
            "subscription group \"Legacy\" (1 subscriptions) not written",
            "com.example.demo.old not written — removed from sale",
            "com.example.demo.nousa not written — no price in USA",
            "com.example.demo.coins not written — CONSUMABLE",
            "com.example.demo.jp not written — its price schedule is based on JPN",
            "com.example.demo.lifetime has manual prices in 3 other territories",
            "pro / draft not written — base plan is DRAFT",
            "pro / pass not written — prepaid base plan",
            "pro / eur not written — its US price is in EUR, not USD",
            "pro / nous not written — no price in US",
            "legacy not written — its purchase options are [legacy-base]",
        ] {
            assert!(notes.contains(want), "missing {want:?} in\n{notes}");
        }
        assert!(!notes.contains("\"Empty\""), "{notes}");
    }

    #[test]
    fn same_id_but_different_stays_apart() {
        let mut p = play();
        p["subscriptions"][0]["basePlans"][0]["price"] = json!("34.99");
        let (c, out) = run(Some(&asc()), Some(&p), "");
        let keys: Vec<&str> = c.products.iter().map(|p| p.key.as_str()).collect();
        assert!(
            keys.contains(&"pro_yearly_appstore") && keys.contains(&"pro_yearly_play"),
            "{keys:?}"
        );
        assert!(
            out.notes
                .iter()
                .any(|n| n.contains("pro_yearly: on both stores, but price differ"))
        );
    }

    #[test]
    fn group_flag_and_single_store() {
        let (c, out) = run(Some(&asc()), None, "Legacy");
        assert_eq!(c.appstore.subscription_group, "Legacy");
        assert_eq!(c.products[0].app_store_id, "legacy.monthly");
        assert!(c.package_name.is_empty());
        assert!(
            out.notes
                .iter()
                .any(|n| n.contains("group \"Pro\" (4 subscriptions) not written"))
        );

        let (c, _) = run(None, Some(&play()), "");
        assert!(c.bundle_id.is_empty());
        assert!(c.appstore.subscription_group.is_empty());
        assert_eq!(c.products.len(), 4);
        assert_eq!(c.products[0].key, "pro_yearly");
    }

    #[test]
    fn nothing_writable() {
        let out = iap_yaml(&IapInitInput {
            app_name: "demo",
            header: &[],
            pricing: &pricing(),
            subscription_group: "",
            appstore: Some(&json!({"bundleId": "x", "groups": [], "inAppPurchases": []})),
            googleplay: None,
        });
        assert_eq!(out.products, 0);
    }
}
