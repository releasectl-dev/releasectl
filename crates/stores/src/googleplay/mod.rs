//! Google Play（Android Publisher API v3）のサブスク（base plan 方式）と
//! 一回課金プロダクトの reconcile（Go 版 `internal/googleplay/googleplay.go`）。
//!
//! 認証（env）: GOOGLE_PLAY_SERVICE_ACCOUNT_JSON（インライン）/
//! GOOGLE_PLAY_SERVICE_ACCOUNT_PATH / GOOGLE_APPLICATION_CREDENTIALS（パス）のどれか。
//!
//! サブスクは base plan を内包した形で設定リージョンの価格つきで作り、DRAFT で
//! できる base plan を有効化する。作る前に productId で突合する（重複は 409）。

mod auth;
mod export;
mod retire;
pub mod upload;

use crate::http::{self, Client};
use crate::{Reconciler, env, go_path_err};
use releasectl_core::config::format_money;
use releasectl_core::{Config, Product, Report, Status};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::rc::Rc;

/// key にサブスクの ID（親）を入れる op（JSON v2 の key）。
pub(super) const BASE_PLAN_OPS: [&str; 4] = [
    "basePlan",
    "basePlan.activate",
    "basePlan.price",
    "basePlan.deactivate",
];

const API_BASE: &str = "https://androidpublisher.googleapis.com/androidpublisher/v3";
/// 現行の Google Play regions version
const REGIONS_VERSION: &str = "2025/03";
/// iapctl が商品ごとに管理する購入オプション id
const ONE_TIME_BUY_OPTION: &str = "buy";

pub struct Play {
    cfg: Config,
    cli: Client,
}

/// サービスアカウントの env から認証済みクライアントを作る。未設定なら `Err(理由)`。
pub fn new_client() -> Result<Client, String> {
    new_client_with(http::http_client())
}

/// HTTP クライアントを指定して作る（アップロードはタイムアウトを延ばす）。
pub fn new_client_with(hc: reqwest::blocking::Client) -> Result<Client, String> {
    let sa_json = load_service_account()?;
    let ts = Rc::new(auth::TokenSource::new(&sa_json, hc.clone())?);
    let auth: http::AuthFn = Box::new(move || ts.token().map(|t| format!("Bearer {t}")));
    Ok(Client::new(hc, API_BASE, Some(auth)))
}

pub fn build(cfg: &Config) -> Result<Box<dyn Reconciler>, String> {
    let cli = new_client()?;
    Ok(Box::new(Play {
        cfg: cfg.clone(),
        cli,
    }))
}

fn load_service_account() -> Result<Vec<u8>, String> {
    if let Some(inline) = env("GOOGLE_PLAY_SERVICE_ACCOUNT_JSON") {
        return Ok(inline.into_bytes());
    }
    for name in [
        "GOOGLE_PLAY_SERVICE_ACCOUNT_PATH",
        "GOOGLE_APPLICATION_CREDENTIALS",
    ] {
        if let Some(path) = env(name) {
            return std::fs::read(&path)
                .map_err(|e| format!("read {name}: {}", go_path_err(&path, &e)));
        }
    }
    Err("GOOGLE_PLAY_SERVICE_ACCOUNT_JSON / _PATH / GOOGLE_APPLICATION_CREDENTIALS not set".into())
}

// --- API の形 ---

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Money {
    #[serde(default)]
    currency_code: String,
    /// Play は 0 の項目を省いて返す（$0.99 は units 無し）。比較のため "0" に揃える。
    #[serde(default = "zero_units", deserialize_with = "units_or_zero")]
    units: String,
    #[serde(default)]
    nanos: i64,
}

fn zero_units() -> String {
    "0".to_string()
}

fn units_or_zero<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let s = String::deserialize(d)?;
    Ok(if s.is_empty() { zero_units() } else { s })
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegionalConfig {
    #[serde(default)]
    region_code: String,
    #[serde(default)]
    price: Money,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BasePlan {
    #[serde(default)]
    base_plan_id: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    regional_configs: Vec<RegionalConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaySubscription {
    #[serde(default)]
    base_plans: Vec<BasePlan>,
    #[serde(default)]
    listings: Vec<PlayListing>,
}

/// 言語ごとのストア掲載（サブスク / 一回課金で共通）。
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayListing {
    #[serde(default)]
    language_code: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
}

/// iap.yaml のロケール（App Store の形）を Play の languageCode にする。
/// App Store は "ja"、Play は "ja-JP"。en-US/de-DE/fr-FR はそのまま。
fn play_lang(locale: &str) -> String {
    match locale {
        "ja" => "ja-JP",
        "en" => "en-US",
        "de" => "de-DE",
        "fr" => "fr-FR",
        other => other,
    }
    .to_string()
}

fn desired_listings(prod: &Product) -> Value {
    Value::Array(
        prod.localizations
            .iter()
            .map(|loc| {
                let mut l = json!({"languageCode": play_lang(&loc.locale), "title": loc.name});
                if !loc.description.is_empty() {
                    l["description"] = json!(loc.description);
                }
                l
            })
            .collect(),
    )
}

fn want_listings(prod: &Product) -> HashMap<String, PlayListing> {
    prod.localizations
        .iter()
        .map(|loc| {
            let lc = play_lang(&loc.locale);
            (
                lc.clone(),
                PlayListing {
                    language_code: lc,
                    title: loc.name.clone(),
                    description: loc.description.clone(),
                },
            )
        })
        .collect()
}

fn price_json(currency: &str, prod: &Product) -> Value {
    json!({"currencyCode": currency, "units": prod.price_units(), "nanos": prod.price_nanos()})
}

/// base plan の設定リージョンの価格（units, nanos）。無ければ None。
fn base_plan_price<'a>(
    sub: &'a PlaySubscription,
    base_plan_id: &str,
    region: &str,
) -> Option<(&'a str, i64)> {
    sub.base_plans
        .iter()
        .filter(|bp| bp.base_plan_id == base_plan_id)
        .flat_map(|bp| bp.regional_configs.iter())
        .find(|rc| rc.region_code == region)
        .map(|rc| (rc.price.units.as_str(), rc.price.nanos))
}

fn base_plan_state<'a>(sub: &'a PlaySubscription, base_plan_id: &str) -> &'a str {
    sub.base_plans
        .iter()
        .find(|bp| bp.base_plan_id == base_plan_id)
        .map(|bp| bp.state.as_str())
        .unwrap_or("")
}

fn q(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

impl Reconciler for Play {
    fn name(&self) -> &'static str {
        "googleplay"
    }

    fn export(&self, _opts: &crate::ExportOpts, r: &mut Report) -> Result<Value, String> {
        self.export_catalog(r)
    }

    fn retire(&self, apply: bool, r: &mut Report) -> Result<(), String> {
        self.retire_products(apply, r)
    }

    fn reconcile(&self, apply: bool, r: &mut Report) -> Result<(), String> {
        let pkg = self.cfg.package_name.as_str();
        if pkg.is_empty() {
            return Err("packageName is required for the googleplay provider".into());
        }
        let region = self.cfg.pricing.region.as_str();
        let currency = self.cfg.pricing.currency.as_str();
        r.store_id("packageName", pkg);
        r.set_key_suffix("basePlan.price", region);

        for prod in &self.cfg.products {
            if prod.play_product_id.is_empty() {
                continue;
            }
            if prod.is_one_time() {
                self.reconcile_one_time_product(apply, pkg, prod, r);
                continue;
            }
            let pid = prod.play_product_id.as_str();
            let bpid = prod.play_base_plan_id.as_str();
            // base plan の ID はサブスクの中でだけ一意なので、key にサブスクの ID を入れる。
            r.set_key_parent(&BASE_PLAN_OPS, Some(pid));
            let sub = match self.get_subscription(pkg, pid) {
                Ok(s) => s,
                Err(e) => {
                    r.add("subscription", pid, Status::Error, e);
                    continue;
                }
            };

            // 今回作ったなら、続く項目は「作った」、既存なら「直した」として報告する。
            let just_created = sub.is_none();
            let sub = match sub {
                Some(s) => {
                    r.add("subscription", pid, Status::Exists, "");
                    s
                }
                None if !apply => {
                    r.add("subscription", pid, Status::Create, "");
                    r.add("basePlan.activate", bpid, Status::Create, "");
                    continue;
                }
                None => match self.create_subscription(pkg, prod) {
                    Ok(s) => {
                        r.add("subscription", pid, Status::Created, "");
                        s
                    }
                    Err(e) => {
                        r.add("subscription", pid, Status::Error, e);
                        continue;
                    }
                },
            };

            // 設定した base plan が存在し ACTIVE であること。
            match base_plan_state(&sub, bpid) {
                "" => r.add(
                    "basePlan",
                    bpid,
                    Status::Skip,
                    "base plan absent on existing subscription; patch not supported",
                ),
                "ACTIVE" => r.add("basePlan.activate", bpid, Status::Exists, ""),
                // 既存のサブスクの base plan が ACTIVE でない = 販売停止中などのドリフト。
                state if !apply => {
                    r.add_diff("basePlan.activate", bpid, Status::Drift, state, "ACTIVE")
                }
                state => match self.activate_base_plan(pkg, pid, bpid) {
                    Ok(()) if just_created => r.add("basePlan.activate", bpid, Status::Created, ""),
                    Ok(()) => {
                        r.add_diff("basePlan.activate", bpid, Status::Updated, state, "ACTIVE")
                    }
                    Err(e) => r.add("basePlan.activate", bpid, Status::Error, e),
                },
            }

            // 設定リージョンの base plan 価格が違えばその場で更新する（他リージョンは保つ）。
            if let Some((cu, cn)) = base_plan_price(&sub, bpid, region) {
                let current = format!("{currency} {}", format_money(cu, cn));
                let desired = format!("{currency} {}", prod.price_label());
                if cu == prod.price_units() && cn == prod.price_nanos() {
                    r.add("basePlan.price", bpid, Status::Exists, "");
                } else if !apply {
                    r.add_diff("basePlan.price", bpid, Status::Drift, current, desired);
                } else {
                    match self.update_base_plan_price(pkg, prod) {
                        Ok(()) => {
                            r.add_diff("basePlan.price", bpid, Status::Updated, current, desired)
                        }
                        Err(e) => r.add("basePlan.price", bpid, Status::Error, e),
                    }
                }
            }

            self.reconcile_subscription_listings(apply, pkg, prod, &sub, r);
        }
        Ok(())
    }
}

impl Play {
    /// サブスクのストア掲載を localizations に合わせる（足りない言語を作り、
    /// 変わった文言を更新）。正は iap.yaml。patch では listings を丸ごと置き換える。
    fn reconcile_subscription_listings(
        &self,
        apply: bool,
        pkg: &str,
        prod: &Product,
        sub: &PlaySubscription,
        r: &mut Report,
    ) {
        if prod.localizations.is_empty() {
            return;
        }
        let pid = prod.play_product_id.as_str();
        let want = want_listings(prod);
        let cur: HashMap<&str, &PlayListing> = sub
            .listings
            .iter()
            .map(|l| (l.language_code.as_str(), l))
            .collect();
        let matches = cur.len() == want.len()
            && want.iter().all(|(lc, w)| {
                cur.get(lc.as_str())
                    .is_some_and(|c| c.title == w.title && c.description == w.description)
            });
        if matches {
            r.add("listing", pid, Status::Exists, "");
            return;
        }
        // 掲載が 1 つも無ければ作成、あれば違う言語を挙げてドリフトとして報告する。
        let (pending, done, detail) = if cur.is_empty() {
            (
                Status::Create,
                Status::Created,
                format!("{} langs", want.len()),
            )
        } else {
            let mut diffs: Vec<String> = want
                .iter()
                .filter_map(|(lc, w)| match cur.get(lc.as_str()) {
                    None => Some(format!("+{lc}")),
                    Some(c) if c.title != w.title || c.description != w.description => {
                        Some(format!("~{lc}"))
                    }
                    _ => None,
                })
                .chain(
                    cur.keys()
                        .filter(|lc| !want.contains_key(**lc))
                        .map(|lc| format!("-{lc}")),
                )
                .collect();
            diffs.sort();
            (Status::Drift, Status::Updated, diffs.join(" "))
        };
        if !apply {
            r.add("listing", pid, pending, detail);
            return;
        }
        let body =
            json!({"packageName": pkg, "productId": pid, "listings": desired_listings(prod)});
        let path = format!(
            "/applications/{pkg}/subscriptions/{pid}?updateMask=listings&regionsVersion.version={}&latencyTolerance=PRODUCT_UPDATE_LATENCY_TOLERANCE_LATENCY_SENSITIVE",
            q(REGIONS_VERSION)
        );
        match self.cli.r#do("PATCH", &path, Some(&body)) {
            Err(e) => r.add("listing", pid, Status::Error, e),
            Ok(resp) if !resp.ok() => r.add("listing", pid, Status::Error, resp.error()),
            Ok(_) => r.add("listing", pid, done, detail),
        }
    }

    /// 404 なら `Ok(None)`。
    fn get_subscription(&self, pkg: &str, pid: &str) -> Result<Option<PlaySubscription>, String> {
        let resp = self.cli.r#do(
            "GET",
            &format!("/applications/{pkg}/subscriptions/{pid}"),
            None,
        )?;
        if resp.status == 404 {
            return Ok(None);
        }
        if !resp.ok() {
            return Err(format!("get subscription: {}", resp.error()));
        }
        resp.json().map(Some)
    }

    fn create_subscription(&self, pkg: &str, prod: &Product) -> Result<PlaySubscription, String> {
        let currency = &self.cfg.pricing.currency;
        let body = json!({
            "packageName": pkg,
            "productId": prod.play_product_id,
            "listings": desired_listings(prod),
            "basePlans": [{
                "basePlanId": prod.play_base_plan_id,
                "autoRenewingBasePlanType": {"billingPeriodDuration": prod.period},
                "regionalConfigs": [{
                    "regionCode": self.cfg.pricing.region,
                    "newSubscriberAvailability": true,
                    // 金額は整数部 + nanos に分ける（$4.99 → 4 / 990000000、¥600 → 600 / 0）
                    "price": price_json(currency, prod),
                }],
            }],
        });
        let path = format!(
            "/applications/{pkg}/subscriptions?productId={}&regionsVersion.version={}",
            q(&prod.play_product_id),
            q(REGIONS_VERSION)
        );
        let resp = self.cli.r#do("POST", &path, Some(&body))?;
        if !resp.ok() {
            return Err(resp.error());
        }
        resp.json()
    }

    fn activate_base_plan(&self, pkg: &str, pid: &str, bpid: &str) -> Result<(), String> {
        let path = format!("/applications/{pkg}/subscriptions/{pid}/basePlans/{bpid}:activate");
        let resp = self.cli.r#do("POST", &path, Some(&json!({})))?;
        if !resp.ok() {
            return Err(resp.error());
        }
        Ok(())
    }

    /// 対象 base plan の設定リージョンの価格だけを差し替える。base plan の
    /// フィールドを落とさないよう、サブスクを生の JSON で取り直してから patch する。
    fn update_base_plan_price(&self, pkg: &str, prod: &Product) -> Result<(), String> {
        let pid = &prod.play_product_id;
        let resp = self.cli.r#do(
            "GET",
            &format!("/applications/{pkg}/subscriptions/{pid}"),
            None,
        )?;
        if !resp.ok() {
            return Err(resp.error());
        }
        let mut full: Value = serde_json::from_slice(&resp.body).map_err(|e| e.to_string())?;
        let mut base_plans = full
            .get_mut("basePlans")
            .map(Value::take)
            .unwrap_or(Value::Null);
        if let Some(bps) = base_plans.as_array_mut() {
            for bp in bps {
                if bp.get("basePlanId").and_then(Value::as_str)
                    != Some(prod.play_base_plan_id.as_str())
                {
                    continue;
                }
                if let Some(rcs) = bp.get_mut("regionalConfigs").and_then(Value::as_array_mut) {
                    for rc in rcs {
                        if rc.get("regionCode").and_then(Value::as_str)
                            == Some(self.cfg.pricing.region.as_str())
                        {
                            rc["price"] = price_json(&self.cfg.pricing.currency, prod);
                        }
                    }
                }
            }
        }
        let body = json!({"packageName": pkg, "productId": pid, "basePlans": base_plans});
        let path = format!(
            "/applications/{pkg}/subscriptions/{pid}?updateMask=basePlans&regionsVersion.version={}&latencyTolerance=PRODUCT_UPDATE_LATENCY_TOLERANCE_LATENCY_SENSITIVE",
            q(REGIONS_VERSION)
        );
        let r = self.cli.r#do("PATCH", &path, Some(&body))?;
        if !r.ok() {
            return Err(r.error());
        }
        Ok(())
    }

    // --- 買い切り（一回課金プロダクト）---
    //
    // 新しい monetization API の one-time product として扱う（旧 inappproducts は
    // 403 "Please migrate to the new publishing API"）。legacy 互換の "buy" 購入
    // オプションを 1 つ持たせ、設定リージョンの価格を付ける。upsert は
    // allowMissing=true の PATCH。作る前に GET で productId を突合する（404 = 無い）。

    fn reconcile_one_time_product(&self, apply: bool, pkg: &str, prod: &Product, r: &mut Report) {
        let pid = prod.play_product_id.as_str();
        let opt = match self.one_time_option(pkg, pid, ONE_TIME_BUY_OPTION) {
            Ok(o) => o,
            Err(e) => {
                r.add("oneTimeProduct", pid, Status::Error, e);
                return;
            }
        };
        let region = self.cfg.pricing.region.as_str();
        let price_ok = opt.configs.iter().any(|c| {
            c.region_code == region
                && c.units == prod.price_units()
                && c.nanos == prod.price_nanos()
        });
        let listings_ok = listings_match(&opt.listings, prod);
        if opt.found && opt.state == "ACTIVE" && price_ok && listings_ok {
            r.add("oneTimeProduct", pid, Status::Exists, "");
            return;
        }
        let currency = self.cfg.pricing.currency.as_str();
        let desired_price = format!("{currency} {}", prod.price_label());
        // 既にある商品は、違うところ（価格・状態・掲載）を挙げてドリフトとして報告する。
        let current_price = opt
            .configs
            .iter()
            .find(|c| c.region_code == region)
            .map(|c| format!("{} {}", c.currency_code, format_money(&c.units, c.nanos)))
            .unwrap_or_else(|| "none".to_string());
        let state = if opt.state.is_empty() {
            "none"
        } else {
            opt.state.as_str()
        };
        let mut diffs = Vec::new();
        if !price_ok {
            diffs.push(format!("price {current_price}→{desired_price}"));
        }
        if opt.state != "ACTIVE" {
            diffs.push(format!("state {state}→ACTIVE"));
        }
        if !listings_ok {
            diffs.push("listing".to_string());
        }
        let report = |r: &mut Report, status: Status| {
            if !opt.found {
                let detail = if apply {
                    desired_price.clone()
                } else {
                    "create + activate".into()
                };
                r.add("oneTimeProduct", pid, status, detail);
            } else if !price_ok {
                // 価格が違うときは current / desired に価格を入れる（機械が読むのは価格）
                r.add_with(
                    "oneTimeProduct",
                    pid,
                    status,
                    diffs.join(", "),
                    current_price.clone(),
                    desired_price.clone(),
                );
            } else {
                r.add("oneTimeProduct", pid, status, diffs.join(", "));
            }
        };
        if !apply {
            report(
                r,
                if opt.found {
                    Status::Drift
                } else {
                    Status::Create
                },
            );
            return;
        }
        let res = (|| {
            if !opt.found {
                self.upsert_one_time_product(pkg, prod)?;
            } else {
                if !price_ok {
                    self.update_one_time_product_price(pkg, prod, &opt.configs)?;
                }
                if !listings_ok {
                    self.patch_one_time_listings(pkg, prod)?;
                }
            }
            self.activate_one_time_option(pkg, pid, ONE_TIME_BUY_OPTION)
        })();
        match res {
            Ok(()) => report(
                r,
                if opt.found {
                    Status::Updated
                } else {
                    Status::Created
                },
            ),
            Err(e) => r.add("oneTimeProduct", pid, Status::Error, e),
        }
    }

    /// 掲載だけを iap.yaml から置き換える（updateMask=listings。価格には触れない）。
    fn patch_one_time_listings(&self, pkg: &str, prod: &Product) -> Result<(), String> {
        let pid = &prod.play_product_id;
        let body =
            json!({"packageName": pkg, "productId": pid, "listings": desired_listings(prod)});
        let path = format!(
            "/applications/{pkg}/onetimeproducts/{}?updateMask=listings&regionsVersion.version={}",
            q(pid),
            q(REGIONS_VERSION)
        );
        let r = self.cli.r#do("PATCH", &path, Some(&body))?;
        if !r.ok() {
            return Err(r.error());
        }
        Ok(())
    }

    /// 購入オプションの状態・リージョン設定と、商品が存在するか。
    /// NOTE: GET は camelCase の `oneTimeProducts` でないと 404 になる（小文字は書き込み系だけ）。
    fn one_time_option(
        &self,
        pkg: &str,
        pid: &str,
        option_id: &str,
    ) -> Result<OneTimeOption, String> {
        let resp = self.cli.r#do(
            "GET",
            &format!("/applications/{pkg}/oneTimeProducts/{pid}"),
            None,
        )?;
        if resp.status == 404 {
            return Ok(OneTimeOption::default());
        }
        if !resp.ok() {
            return Err(format!("get one-time product: {}", resp.error()));
        }

        #[derive(Default, Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Cfg {
            #[serde(default)]
            region_code: String,
            #[serde(default)]
            price: Money,
            #[serde(default)]
            availability: String,
        }
        #[derive(Default, Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Po {
            #[serde(default)]
            purchase_option_id: String,
            #[serde(default)]
            state: String,
            #[serde(default, rename = "regionalPricingAndAvailabilityConfigs")]
            configs: Vec<Cfg>,
        }
        #[derive(Default, Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct D {
            #[serde(default)]
            listings: Vec<PlayListing>,
            #[serde(default)]
            purchase_options: Vec<Po>,
        }
        let d: D = resp.json()?;
        let mut out = OneTimeOption {
            found: true,
            listings: d.listings,
            ..Default::default()
        };
        if let Some(po) = d
            .purchase_options
            .into_iter()
            .find(|po| po.purchase_option_id == option_id)
        {
            out.state = po.state;
            out.configs = po
                .configs
                .into_iter()
                .map(|c| RegionalPrice {
                    region_code: c.region_code,
                    currency_code: c.price.currency_code,
                    units: c.price.units,
                    nanos: c.price.nanos,
                    availability: c.availability,
                })
                .collect();
        }
        Ok(out)
    }

    /// 購入オプションのリージョン設定のうち、設定リージョンの価格だけを差し替える。
    fn update_one_time_product_price(
        &self,
        pkg: &str,
        prod: &Product,
        current: &[RegionalPrice],
    ) -> Result<(), String> {
        let region = self.cfg.pricing.region.as_str();
        let currency = self.cfg.pricing.currency.as_str();
        let mut have = false;
        let mut out: Vec<Value> = current
            .iter()
            .map(|c| {
                if c.region_code == region {
                    have = true;
                    json!({"regionCode": c.region_code, "price": price_json(currency, prod), "availability": "AVAILABLE"})
                } else {
                    json!({
                        "regionCode": c.region_code,
                        "price": {"currencyCode": c.currency_code, "units": c.units, "nanos": c.nanos},
                        "availability": c.availability,
                    })
                }
            })
            .collect();
        if !have {
            out.push(json!({"regionCode": region, "price": price_json(currency, prod), "availability": "AVAILABLE"}));
        }
        let pid = &prod.play_product_id;
        let body = json!({
            "packageName": pkg,
            "productId": pid,
            "purchaseOptions": [{
                "purchaseOptionId": ONE_TIME_BUY_OPTION,
                "buyOption": {"legacyCompatible": true, "multiQuantityEnabled": false},
                "regionalPricingAndAvailabilityConfigs": out,
            }],
        });
        let path = format!(
            "/applications/{pkg}/onetimeproducts/{}?updateMask=purchaseOptions&regionsVersion.version={}",
            q(pid),
            q(REGIONS_VERSION)
        );
        let resp = self.cli.r#do("PATCH", &path, Some(&body))?;
        if !resp.ok() {
            return Err(resp.error());
        }
        Ok(())
    }

    /// legacy 互換の "buy" オプション 1 つで作成 / 更新する。オプションは DRAFT で
    /// できるので、activate_one_time_option で公開する。
    fn upsert_one_time_product(&self, pkg: &str, prod: &Product) -> Result<(), String> {
        let pid = &prod.play_product_id;
        let body = json!({
            "packageName": pkg,
            "productId": pid,
            "listings": desired_listings(prod),
            "purchaseOptions": [{
                "purchaseOptionId": ONE_TIME_BUY_OPTION,
                // legacyCompatible で旧 Billing API からも見える。legacy 互換は 1 商品に 1 つだけ。
                "buyOption": {"legacyCompatible": true, "multiQuantityEnabled": false},
                "regionalPricingAndAvailabilityConfigs": [{
                    "regionCode": self.cfg.pricing.region,
                    // JPY は小数なし: units に全額、nanos は 0
                    "price": price_json(&self.cfg.pricing.currency, prod),
                    "availability": "AVAILABLE",
                }],
            }],
        });
        // allowMissing=true の PATCH が upsert。updateMask は明示（"*" は INVALID_ARGUMENT）。
        // 大文字小文字の癖（実 API で確認済み）: PATCH は小文字 `onetimeproducts`、
        // GET と batchUpdateStates は camelCase `oneTimeProducts`。揃えないこと。
        let path = format!(
            "/applications/{pkg}/onetimeproducts/{}?updateMask=listings,purchaseOptions&allowMissing=true&regionsVersion.version={}",
            q(pid),
            q(REGIONS_VERSION)
        );
        let resp = self.cli.r#do("PATCH", &path, Some(&body))?;
        if !resp.ok() {
            return Err(resp.error());
        }
        Ok(())
    }

    /// 購入オプションを公開する（DRAFT → ACTIVE）。公開済みでも成功する。
    fn activate_one_time_option(
        &self,
        pkg: &str,
        pid: &str,
        option_id: &str,
    ) -> Result<(), String> {
        let body = json!({"requests": [{
            "activatePurchaseOptionRequest": {
                "packageName": pkg,
                "productId": pid,
                "purchaseOptionId": option_id,
            },
        }]});
        let path =
            format!("/applications/{pkg}/oneTimeProducts/{pid}/purchaseOptions:batchUpdateStates");
        let resp = self.cli.r#do("POST", &path, Some(&body))?;
        if !resp.ok() {
            return Err(resp.error());
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
struct OneTimeOption {
    state: String,
    configs: Vec<RegionalPrice>,
    listings: Vec<PlayListing>,
    found: bool,
}

#[derive(Debug, Clone)]
struct RegionalPrice {
    region_code: String,
    currency_code: String,
    units: String,
    nanos: i64,
    availability: String,
}

/// 現在の掲載が localizations（言語 + タイトル + 説明）と一致するか。
fn listings_match(current: &[PlayListing], prod: &Product) -> bool {
    let want = want_listings(prod);
    current.len() == want.len()
        && current.iter().all(|c| {
            want.get(&c.language_code)
                .is_some_and(|w| c.title == w.title && c.description == w.description)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::parse(
            r#"
packageName: com.example
pricing: { region: US, currency: USD }
products:
  - key: m
    playProductId: m
    playBasePlanId: monthly
    period: P1M
    price: 4.99
    localizations:
      - { locale: ja, name: "月額", description: "説明" }
      - { locale: en-US, name: "Monthly" }
"#,
            "t",
        )
        .unwrap()
    }

    #[test]
    fn listings_map_locales_and_omit_empty_description() {
        let p = &cfg().products[0];
        assert_eq!(
            desired_listings(p),
            json!([
                {"languageCode": "ja-JP", "title": "月額", "description": "説明"},
                {"languageCode": "en-US", "title": "Monthly"},
            ])
        );
    }

    #[test]
    fn listings_match_requires_same_set() {
        let p = &cfg().products[0];
        let l = |lc: &str, t: &str, d: &str| PlayListing {
            language_code: lc.into(),
            title: t.into(),
            description: d.into(),
        };
        assert!(listings_match(
            &[l("ja-JP", "月額", "説明"), l("en-US", "Monthly", "")],
            p
        ));
        assert!(!listings_match(&[l("ja-JP", "月額", "説明")], p));
        assert!(!listings_match(
            &[l("ja-JP", "月額", "x"), l("en-US", "Monthly", "")],
            p
        ));
    }

    #[test]
    fn base_plan_lookup() {
        let sub: PlaySubscription = serde_json::from_value(json!({
            "basePlans": [{"basePlanId": "monthly", "state": "ACTIVE",
                "regionalConfigs": [{"regionCode": "US", "price": {"currencyCode": "USD", "units": "4", "nanos": 990000000}}]}]
        }))
        .unwrap();
        assert_eq!(
            base_plan_price(&sub, "monthly", "US"),
            Some(("4", 990_000_000))
        );
        assert_eq!(base_plan_price(&sub, "monthly", "JP"), None);
        assert_eq!(base_plan_state(&sub, "monthly"), "ACTIVE");
        assert_eq!(base_plan_state(&sub, "yearly"), "");
    }

    #[test]
    fn omitted_units_read_as_zero() {
        // $0.99: Play は units を省いて返す
        let sub: PlaySubscription = serde_json::from_value(json!({
            "basePlans": [{"basePlanId": "weekly", "state": "ACTIVE",
                "regionalConfigs": [{"regionCode": "US", "price": {"currencyCode": "USD", "nanos": 990000000}}]}]
        }))
        .unwrap();
        assert_eq!(
            base_plan_price(&sub, "weekly", "US"),
            Some(("0", 990_000_000))
        );
        let p: PlaySubscription = serde_json::from_value(json!({
            "basePlans": [{"basePlanId": "w", "regionalConfigs": [{"regionCode": "US", "price": {"units": ""}}]}]
        }))
        .unwrap();
        assert_eq!(base_plan_price(&p, "w", "US"), Some(("0", 0)));
    }
}
