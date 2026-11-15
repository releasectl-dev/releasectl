//! App Store Connect（REST / JSON:API）の自動更新サブスクと非消費型 IAP の reconcile
//! （Go 版 `internal/appstore/appstore.go`）。
//!
//! 認証（env）: ASC_KEY_ID、ASC_ISSUER_ID、ASC_PRIVATE_KEY（.p8 の中身）か
//! ASC_PRIVATE_KEY_PATH（.p8 のパス）。
//!
//! サブスクごとに: サブスクリプショングループ、サブスク、設定テリトリの価格（Apple の
//! price-point カタログから解決）、ロケールごとのローカライズ、販売地域。どれも作る前に
//! 突合する（productId / referenceName は一意で、重複は 409）。

mod auth;
mod export;

use crate::http::{self, Client, Response};
use crate::{Reconciler, env, go_path_err};
use releasectl_core::config::asc_period;
use releasectl_core::{Config, Product, Report, Status};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::rc::Rc;

const API_BASE: &str = "https://api.appstoreconnect.apple.com";

pub struct AppStore {
    cfg: Config,
    cli: Client,
}

/// ASC_* の env から認証済みクライアントを作る。鍵が無ければ `Err(理由)`。
pub fn new_client() -> Result<Client, String> {
    new_client_with(http::http_client())
}

/// HTTP クライアントを指定して作る（掲載画像・動画のアップロードはタイムアウトを延ばす）。
pub fn new_client_with(hc: reqwest::blocking::Client) -> Result<Client, String> {
    let (Some(key_id), Some(issuer_id)) = (env("ASC_KEY_ID"), env("ASC_ISSUER_ID")) else {
        return Err("ASC_KEY_ID / ASC_ISSUER_ID not set".into());
    };
    let pem = match env("ASC_PRIVATE_KEY") {
        Some(k) => k.into_bytes(),
        None => {
            let Some(path) = env("ASC_PRIVATE_KEY_PATH") else {
                return Err("ASC_PRIVATE_KEY / ASC_PRIVATE_KEY_PATH not set".into());
            };
            std::fs::read(&path)
                .map_err(|e| format!("read ASC_PRIVATE_KEY_PATH: {}", go_path_err(&path, &e)))?
        }
    };
    let ts = Rc::new(auth::TokenSource::new(key_id, issuer_id, &pem)?);
    let auth: http::AuthFn = Box::new(move || ts.token().map(|t| format!("Bearer {t}")));
    Ok(Client::new(hc, API_BASE, Some(auth)))
}

pub fn build(cfg: &Config) -> Result<Box<dyn Reconciler>, String> {
    let cli = new_client()?;
    Ok(Box::new(AppStore {
        cfg: cfg.clone(),
        cli,
    }))
}

// --- JSON:API ---

/// 一覧を links.next をたどって全部取る。
pub(crate) fn get_all(cli: &Client, first: &str, what: &str) -> Result<Vec<Resource>, String> {
    let mut out = Vec::new();
    let mut path = first.to_string();
    while !path.is_empty() {
        let resp = cli.r#do("GET", &path, None)?;
        if !resp.ok() {
            return Err(format!("{what}: {}", resp.error()));
        }
        let (data, next) = parse_list(&resp)?;
        out.extend(data);
        path = next;
    }
    Ok(out)
}

/// 一覧の 1 件（id と attributes）。
pub(crate) struct Resource {
    pub(crate) id: String,
    pub(crate) attributes: Value,
}

pub(crate) fn attr<'a>(r: &'a Resource, name: &str) -> &'a str {
    r.attributes.get(name).and_then(Value::as_str).unwrap_or("")
}

pub(crate) fn parse_list(resp: &Response) -> Result<(Vec<Resource>, String), String> {
    let v: Value = resp.json()?;
    let data = v
        .get("data")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|r| Resource {
                    id: r
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    attributes: r.get("attributes").cloned().unwrap_or(Value::Null),
                })
                .collect()
        })
        .unwrap_or_default();
    let next = v
        .pointer("/links/next")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok((data, next))
}

/// 作成のレスポンスの data.id。
pub(crate) fn created_id(resp: &Response) -> Option<String> {
    let v: Value = resp.json().ok()?;
    v.pointer("/data/id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// 価格の比較（Go 版と同じく 0.005 未満の差は同じ価格とみなす）。
fn price_eq(customer_price: &str, want: f64) -> bool {
    customer_price
        .parse::<f64>()
        .is_ok_and(|v| (v - want).abs() < 0.005)
}

/// 表示名: 最初のローカライズの名前、無ければ key。
fn display_name(p: &Product) -> &str {
    match p.localizations.first() {
        Some(l) if !l.name.is_empty() => &l.name,
        _ => &p.key,
    }
}

impl Reconciler for AppStore {
    fn name(&self) -> &'static str {
        "appstore"
    }

    fn export(&self, _opts: &crate::ExportOpts, r: &mut Report) -> Result<Value, String> {
        self.export_catalog(r)
    }

    /// App Store には可逆な「販売停止」の書き込みが無いので、手作業を案内するだけ（書き込みなし）。
    ///
    /// - サブスク: 販売地域は作成時にしか決められない。`availableTerritories` は作成後は読み取り専用で、
    ///   空にする API が無い。非消費型（`inAppPurchaseAvailabilities`）も同じ。
    /// - `DELETE /v1/subscriptions/{id}` などはあるが、承認・販売済みの商品には拒否され、消した
    ///   識別子は再利用できない。販売停止ではなく切断になる。
    ///
    /// 推測で未確認の書き込みをするより、Console で 1 商品 1 クリック（そこで戻せる）の方が安い。
    fn retire(&self, _apply: bool, r: &mut Report) -> Result<(), String> {
        for p in self
            .cfg
            .products
            .iter()
            .filter(|p| !p.app_store_id.is_empty())
        {
            r.add(
                "removeFromSale",
                &p.app_store_id,
                Status::Skip,
                "console only — App Store Connect > Monetization > 対象商品 > Availability を削除",
            );
        }
        Ok(())
    }

    fn reconcile(&self, apply: bool, r: &mut Report) -> Result<(), String> {
        r.set_key_suffix("price", &self.cfg.pricing.territory);
        r.set_key_singleton(&["app"]);
        // 1. bundle id から app id を引く。
        let Some(app_id) = self.find_app_id()? else {
            r.add(
                "app",
                &self.cfg.bundle_id,
                Status::Skip,
                "no App Store app for this bundle id",
            );
            return Ok(());
        };
        r.store_id("appId", &app_id);

        // サブスクはグループの流れ、非消費型（買い切り）は in-app purchase v2 の流れ。
        let (one_times, subs): (Vec<&Product>, Vec<&Product>) = self
            .cfg
            .products
            .iter()
            .filter(|p| !p.app_store_id.is_empty())
            .partition(|p| p.is_one_time());

        // 2-3. サブスク: グループ、それから商品ごとにサブスク / 価格 / ローカライズ / 販売地域。
        if !subs.is_empty() {
            let group_name = if self.cfg.appstore.subscription_group.is_empty() {
                format!("{} Subscriptions", self.cfg.app_name)
            } else {
                self.cfg.appstore.subscription_group.clone()
            };
            match self.ensure_group(apply, &app_id, &group_name, r)? {
                None => {
                    // plan でグループを作る予定: サブスクも作成予定として報告する。
                    for p in &subs {
                        r.add(
                            "subscription",
                            &p.app_store_id,
                            Status::Create,
                            "after group",
                        );
                    }
                }
                Some(group_id) => {
                    let existing = self.list_subscriptions(&group_id)?;
                    for p in subs {
                        let sub_id = match existing.get(&p.app_store_id) {
                            Some(id) => {
                                r.add("subscription", &p.app_store_id, Status::Exists, "");
                                id.clone()
                            }
                            None if !apply => {
                                r.add("subscription", &p.app_store_id, Status::Create, "");
                                continue;
                            }
                            None => match self.create_subscription(&group_id, p) {
                                Ok(id) => {
                                    r.add("subscription", &p.app_store_id, Status::Created, "");
                                    id
                                }
                                Err(e) => {
                                    r.add("subscription", &p.app_store_id, Status::Error, e);
                                    continue;
                                }
                            },
                        };
                        // 販売地域を先に付ける。無いテリトリに価格を付けると
                        // 409 UNSUPPORTED_TERRITORY になる（2026-11-09、テストアプリで確認）。
                        self.reconcile_availability(apply, Kind::Subscription, &sub_id, p, r);
                        self.reconcile_price(apply, &sub_id, p, r);
                        self.reconcile_localizations(apply, Kind::Subscription, &sub_id, p, r);
                    }
                }
            }
        }

        // 4. 非消費型（買い切り）。
        if !one_times.is_empty() {
            self.reconcile_non_consumables(apply, &app_id, &one_times, r)?;
        }
        Ok(())
    }
}

/// サブスクと非消費型 IAP で形だけが違う処理（ローカライズ・販売地域）の切り替え。
#[derive(Clone, Copy)]
enum Kind {
    Subscription,
    InAppPurchase,
}

impl AppStore {
    /// 一覧を links.next をたどって全部取る。
    fn get_all(&self, first: &str, what: &str) -> Result<Vec<Resource>, String> {
        get_all(&self.cli, first, what)
    }

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

    /// グループの id。plan で作る予定のとき、または作成に失敗したときは None。
    fn ensure_group(
        &self,
        apply: bool,
        app_id: &str,
        name: &str,
        r: &mut Report,
    ) -> Result<Option<String>, String> {
        let resp = self.cli.r#do(
            "GET",
            &format!("/v1/apps/{app_id}/subscriptionGroups?limit=200"),
            None,
        )?;
        if !resp.ok() {
            return Err(format!("list subscription groups: {}", resp.error()));
        }
        let (groups, _) = parse_list(&resp)?;
        if let Some(g) = groups.iter().find(|g| attr(g, "referenceName") == name) {
            r.add("group", name, Status::Exists, "");
            return Ok(Some(g.id.clone()));
        }
        if !apply {
            r.add("group", name, Status::Create, "");
            return Ok(None);
        }
        let body = json!({"data": {
            "type": "subscriptionGroups",
            "attributes": {"referenceName": name},
            "relationships": {"app": {"data": {"type": "apps", "id": app_id}}},
        }});
        let resp = self
            .cli
            .r#do("POST", "/v1/subscriptionGroups", Some(&body))?;
        if !resp.ok() {
            r.add("group", name, Status::Error, resp.error());
            return Ok(None);
        }
        let id = created_id(&resp).ok_or("create group: empty response")?;
        r.add("group", name, Status::Created, "");
        Ok(Some(id))
    }

    /// productId → サブスクの id。
    fn list_subscriptions(&self, group_id: &str) -> Result<HashMap<String, String>, String> {
        Ok(self
            .get_all(
                &format!("/v1/subscriptionGroups/{group_id}/subscriptions?limit=200"),
                "list subscriptions",
            )?
            .into_iter()
            .map(|s| (attr(&s, "productId").to_string(), s.id))
            .collect())
    }

    fn create_subscription(&self, group_id: &str, p: &Product) -> Result<String, String> {
        let period = asc_period(&p.period)?;
        let body = json!({"data": {
            "type": "subscriptions",
            "attributes": {
                "name": display_name(p),
                "productId": p.app_store_id,
                "subscriptionPeriod": period,
                "familySharable": false,
                "groupLevel": 1,
            },
            // NOTE: relationship のキーは "group"（type は subscriptionGroups）。"subscriptionGroup" ではない。
            "relationships": {"group": {"data": {"type": "subscriptionGroups", "id": group_id}}},
        }});
        let resp = self.cli.r#do("POST", "/v1/subscriptions", Some(&body))?;
        if !resp.ok() {
            return Err(resp.error());
        }
        created_id(&resp).ok_or_else(|| "create subscription: empty response".to_string())
    }

    fn no_price_point(&self, p: &Product) -> String {
        format!(
            "no {} price point matching {} in {}",
            self.cfg.pricing.currency,
            p.price_label(),
            self.cfg.pricing.territory
        )
    }

    fn reconcile_price(&self, apply: bool, sub_id: &str, p: &Product, r: &mut Report) {
        // 設定テリトリの price-point カタログ（id → customerPrice）と、サブスクが今付いている
        // price-point の id。ここから「希望の価格が既にあるか」「別の価格があるか（更新か）」が
        // わかる。App Store の価格は追記型で、新しい価格を POST すると古いものに取って代わる。
        let points = match self.territory_price_points(sub_id) {
            Ok(m) => m,
            Err(e) => {
                r.add("price", &p.app_store_id, Status::Error, e);
                return;
            }
        };
        let want_point = points
            .iter()
            .find(|(_, cp)| price_eq(cp, p.price))
            .map(|(id, _)| id.clone());
        let current_ids = match self.subscription_price_point_ids(sub_id) {
            Ok(ids) => ids,
            Err(e) => {
                r.add("price", &p.app_store_id, Status::Error, e);
                return;
            }
        };
        // 設定テリトリの価格だけを見る（別テリトリの価格は無視）。
        let in_territory: Vec<&String> = current_ids
            .iter()
            .filter(|pid| points.contains_key(*pid))
            .collect();
        if in_territory
            .iter()
            .any(|pid| want_point.as_deref() == Some(pid.as_str()))
        {
            r.add("price", &p.app_store_id, Status::Exists, "");
            return;
        }
        let currency = &self.cfg.pricing.currency;
        let desired = format!("{currency} {}", p.price_label());
        // そのテリトリに別の価格があればドリフト（現在値はその price-point の customerPrice）。
        let current = in_territory
            .first()
            .map(|pid| format!("{currency} {}", points[*pid]));
        let report = |r: &mut Report, done: bool| match &current {
            Some(c) => r.add_diff(
                "price",
                &p.app_store_id,
                if done { Status::Updated } else { Status::Drift },
                c.clone(),
                desired.clone(),
            ),
            None => r.add(
                "price",
                &p.app_store_id,
                if done {
                    Status::Created
                } else {
                    Status::Create
                },
                desired.clone(),
            ),
        };
        if !apply {
            report(r, false);
            return;
        }
        let Some(want_point) = want_point else {
            r.add(
                "price",
                &p.app_store_id,
                Status::Error,
                self.no_price_point(p),
            );
            return;
        };
        // preserveCurrentPrice:false で更新時の価格にも新価格を当てる。startDate 省略 = 即時。
        // 値上げ時の既存購読者の同意は App Store 側が扱う。
        let body = json!({"data": {
            "type": "subscriptionPrices",
            "attributes": {"preserveCurrentPrice": false},
            "relationships": {
                "subscription": {"data": {"type": "subscriptions", "id": sub_id}},
                "subscriptionPricePoint": {"data": {"type": "subscriptionPricePoints", "id": want_point}},
            },
        }});
        match self.cli.r#do("POST", "/v1/subscriptionPrices", Some(&body)) {
            Err(e) => r.add("price", &p.app_store_id, Status::Error, e),
            Ok(resp) if !resp.ok() => r.add("price", &p.app_store_id, Status::Error, resp.error()),
            Ok(_) => report(r, true),
        }
    }

    /// 設定テリトリの price-point（id → customerPrice）。
    fn territory_price_points(&self, sub_id: &str) -> Result<HashMap<String, String>, String> {
        Ok(self
            .get_all(
                &format!(
                    "/v1/subscriptions/{sub_id}/pricePoints?filter[territory]={}&limit=8000",
                    self.cfg.pricing.territory
                ),
                "list price points",
            )?
            .into_iter()
            .map(|pp| (pp.id.clone(), attr(&pp, "customerPrice").to_string()))
            .collect())
    }

    /// サブスクが今付いている price-point の id（全テリトリ）。
    /// 読めなければエラー（「価格が無い」とはみなさない）。
    fn subscription_price_point_ids(&self, sub_id: &str) -> Result<Vec<String>, String> {
        // include=subscriptionPricePoint を付けないと relationship の data.id が返らない。
        let resp = self.cli.r#do(
            "GET",
            &format!("/v1/subscriptions/{sub_id}/prices?include=subscriptionPricePoint&limit=200"),
            None,
        )?;
        if !resp.ok() {
            return Err(format!("list subscription prices: {}", resp.error()));
        }
        let v: Value = resp.json()?;
        Ok(v.get("data")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|d| {
                        d.pointer("/relationships/subscriptionPricePoint/data/id")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// ロケールごとに、無ければ作り、文言が違えば更新する（正は iap.yaml）。
    fn reconcile_localizations(
        &self,
        apply: bool,
        kind: Kind,
        id: &str,
        p: &Product,
        r: &mut Report,
    ) {
        let (list_path, ty, write_path) = match kind {
            Kind::Subscription => (
                format!("/v1/subscriptions/{id}/subscriptionLocalizations?limit=50"),
                "subscriptionLocalizations",
                "/v1/subscriptionLocalizations",
            ),
            Kind::InAppPurchase => (
                format!("/v2/inAppPurchases/{id}/inAppPurchaseLocalizations?limit=50"),
                "inAppPurchaseLocalizations",
                "/v1/inAppPurchaseLocalizations",
            ),
        };
        // 既存の id と今の文言（ロケールごと）。読めなければエラーにして、書き込まない
        // （Go 版は「無い」とみなして作成を送っていた）。
        let data = match self.get_all(&list_path, "list localizations") {
            Ok(d) => d,
            Err(e) => {
                r.add("localization", &p.app_store_id, Status::Error, e);
                return;
            }
        };
        let have: HashMap<String, (String, String, String)> = data
            .into_iter()
            .map(|loc| {
                (
                    attr(&loc, "locale").to_string(),
                    (
                        loc.id.clone(),
                        attr(&loc, "name").to_string(),
                        attr(&loc, "description").to_string(),
                    ),
                )
            })
            .collect();
        for loc in &p.localizations {
            let target = format!("{} [{}]", p.app_store_id, loc.locale);
            let cur = have.get(&loc.locale);
            if let Some((_, name, desc)) = cur
                && *name == loc.name
                && *desc == loc.description
            {
                r.add("localization", &target, Status::Exists, "");
                continue;
            }
            // そのロケールがあれば、違う項目を挙げてドリフトとして報告する。
            let (pending, done, detail) = match cur {
                None => (Status::Create, Status::Created, String::new()),
                Some((_, name, desc)) => {
                    let mut d = Vec::new();
                    if *name != loc.name {
                        d.push("name");
                    }
                    if *desc != loc.description {
                        d.push("description");
                    }
                    (Status::Drift, Status::Updated, d.join(", "))
                }
            };
            if !apply {
                r.add("localization", &target, pending, detail);
                continue;
            }
            let mut attrs = json!({"name": loc.name});
            if !loc.description.is_empty() {
                attrs["description"] = json!(loc.description);
            }
            let res = match cur {
                // 既存の文言を更新（locale 自体は変えられない）。
                Some((loc_id, _, _)) => {
                    let body = json!({"data": {"type": ty, "id": loc_id, "attributes": attrs}});
                    self.cli
                        .r#do("PATCH", &format!("{write_path}/{loc_id}"), Some(&body))
                }
                None => {
                    attrs["locale"] = json!(loc.locale);
                    let rel = match kind {
                        Kind::Subscription => {
                            json!({"subscription": {"data": {"type": "subscriptions", "id": id}}})
                        }
                        // v2 IAP の relationship は "inAppPurchaseV2"（"inAppPurchase" ではない）。
                        Kind::InAppPurchase => {
                            json!({"inAppPurchaseV2": {"data": {"type": "inAppPurchases", "id": id}}})
                        }
                    };
                    let body =
                        json!({"data": {"type": ty, "attributes": attrs, "relationships": rel}});
                    self.cli.r#do("POST", write_path, Some(&body))
                }
            };
            match res {
                Err(e) => r.add("localization", &target, Status::Error, e),
                Ok(resp) if !resp.ok() => {
                    r.add("localization", &target, Status::Error, resp.error())
                }
                Ok(_) => r.add("localization", &target, done, detail),
            }
        }
    }

    /// 販売地域は to-one。既に設定されていれば触らない。
    fn reconcile_availability(
        &self,
        apply: bool,
        kind: Kind,
        id: &str,
        p: &Product,
        r: &mut Report,
    ) {
        let (get_path, ty, rel_key, rel_ty, post_path) = match kind {
            Kind::Subscription => (
                format!("/v1/subscriptions/{id}/subscriptionAvailability"),
                "subscriptionAvailabilities",
                "subscription",
                "subscriptions",
                "/v1/subscriptionAvailabilities",
            ),
            Kind::InAppPurchase => (
                format!("/v2/inAppPurchases/{id}/inAppPurchaseAvailability"),
                "inAppPurchaseAvailabilities",
                "inAppPurchase",
                "inAppPurchases",
                "/v1/inAppPurchaseAvailabilities",
            ),
        };
        // 読めなければエラーにして、書き込まない（Go 版は「無い」とみなして作成を送っていた）。
        // 404 か data が空なら「まだ無い」。
        match self.cli.r#do("GET", &get_path, None) {
            Err(e) => {
                r.add("availability", &p.app_store_id, Status::Error, e);
                return;
            }
            Ok(resp) if resp.status == 404 => {}
            Ok(resp) if !resp.ok() => {
                r.add(
                    "availability",
                    &p.app_store_id,
                    Status::Error,
                    format!("get availability: {}", resp.error()),
                );
                return;
            }
            Ok(resp) => {
                if created_id(&resp).is_some_and(|i| !i.is_empty()) {
                    r.add("availability", &p.app_store_id, Status::Exists, "");
                    return;
                }
            }
        }
        let territory = self.cfg.pricing.territory.as_str();
        if !apply {
            r.add("availability", &p.app_store_id, Status::Create, territory);
            return;
        }
        let body = json!({"data": {
            "type": ty,
            "attributes": {"availableInNewTerritories": true},
            "relationships": {
                rel_key: {"data": {"type": rel_ty, "id": id}},
                "availableTerritories": {"data": [{"type": "territories", "id": territory}]},
            },
        }});
        match self.cli.r#do("POST", post_path, Some(&body)) {
            Err(e) => r.add("availability", &p.app_store_id, Status::Error, e),
            Ok(resp) if !resp.ok() && resp.status != 409 => {
                r.add("availability", &p.app_store_id, Status::Error, resp.error())
            }
            Ok(_) => r.add("availability", &p.app_store_id, Status::Created, territory),
        }
    }

    // --- 非消費型（買い切り）の in-app purchase ---
    //
    // サブスクとは別のリソース木: `inAppPurchases`（v2）、手動の価格スケジュール（price-point は
    // 固定カタログ）、ロケールごとのローカライズ、販売地域。

    fn reconcile_non_consumables(
        &self,
        apply: bool,
        app_id: &str,
        products: &[&Product],
        r: &mut Report,
    ) -> Result<(), String> {
        let existing = self.list_iaps(app_id)?;
        for p in products {
            let iap_id = match existing.get(&p.app_store_id) {
                Some(id) => {
                    r.add("iap", &p.app_store_id, Status::Exists, "");
                    id.clone()
                }
                None if !apply => {
                    r.add("iap", &p.app_store_id, Status::Create, "non-consumable");
                    continue;
                }
                None => match self.create_iap(app_id, p) {
                    Ok(id) => {
                        r.add("iap", &p.app_store_id, Status::Created, "non-consumable");
                        id
                    }
                    Err(e) => {
                        r.add("iap", &p.app_store_id, Status::Error, e);
                        continue;
                    }
                },
            };
            self.reconcile_iap_price(apply, &iap_id, p, r);
            self.reconcile_localizations(apply, Kind::InAppPurchase, &iap_id, p, r);
            self.reconcile_availability(apply, Kind::InAppPurchase, &iap_id, p, r);
        }
        Ok(())
    }

    /// productId → inAppPurchase の id。
    fn list_iaps(&self, app_id: &str) -> Result<HashMap<String, String>, String> {
        Ok(self
            .get_all(
                &format!("/v1/apps/{app_id}/inAppPurchasesV2?limit=200"),
                "list in-app purchases",
            )?
            .into_iter()
            .map(|s| (attr(&s, "productId").to_string(), s.id))
            .collect())
    }

    fn create_iap(&self, app_id: &str, p: &Product) -> Result<String, String> {
        let body = json!({"data": {
            "type": "inAppPurchases",
            "attributes": {
                "name": display_name(p),
                "productId": p.app_store_id,
                "inAppPurchaseType": "NON_CONSUMABLE",
            },
            "relationships": {"app": {"data": {"type": "apps", "id": app_id}}},
        }});
        let resp = self.cli.r#do("POST", "/v2/inAppPurchases", Some(&body))?;
        if !resp.ok() {
            return Err(resp.error());
        }
        created_id(&resp).ok_or_else(|| "create in-app purchase: empty response".to_string())
    }

    fn reconcile_iap_price(&self, apply: bool, iap_id: &str, p: &Product, r: &mut Report) {
        let points = match self.iap_price_points(iap_id) {
            Ok(pts) => pts,
            Err(e) => {
                r.add("price", &p.app_store_id, Status::Error, e);
                return;
            }
        };
        let point_id = match points
            .iter()
            .find(|(_, cp)| price_eq(cp, p.price))
            .map(|(id, _)| id.clone())
        {
            Some(id) => id,
            None => {
                r.add(
                    "price",
                    &p.app_store_id,
                    Status::Error,
                    self.no_price_point(p),
                );
                return;
            }
        };
        // 設定テリトリをスケジュールの baseTerritory にし、他のテリトリはそこから自動換算させる。
        // base が設定テリトリで、その手動価格が希望の price-point なら一致。
        // （手動価格の id と price-point の id はどちらも {territory, tier} を base64 にしたものなので、
        //  不透明な id 文字列ではなく、デコードしたキーで比べる）
        let territory = self.cfg.pricing.territory.as_str();
        let (base, manual) = match self.read_iap_schedule(iap_id) {
            Ok(s) => s,
            Err(e) => {
                r.add("price", &p.app_store_id, Status::Error, e);
                return;
            }
        };
        let current_key = manual.get(territory);
        if base == territory
            && current_key.map(String::as_str) == iap_price_key(&point_id).as_deref()
        {
            r.add("price", &p.app_store_id, Status::Exists, "");
            return;
        }
        let currency = &self.cfg.pricing.currency;
        let desired = format!("{currency} {}", p.price_label());
        // スケジュールがあればドリフト。現在値は、手動価格のキーに一致する price-point の customerPrice。
        let current = (!base.is_empty()).then(|| {
            current_key
                .and_then(|k| {
                    points
                        .iter()
                        .find(|(id, _)| iap_price_key(id).as_ref() == Some(k))
                })
                .map(|(_, cp)| format!("{currency} {cp}"))
                .unwrap_or_else(|| format!("base {base}"))
        });
        let report = |r: &mut Report, done: bool| match &current {
            Some(c) => r.add_diff(
                "price",
                &p.app_store_id,
                if done { Status::Updated } else { Status::Drift },
                c.clone(),
                desired.clone(),
            ),
            None => r.add(
                "price",
                &p.app_store_id,
                if done {
                    Status::Created
                } else {
                    Status::Create
                },
                desired.clone(),
            ),
        };
        if !apply {
            report(r, false);
            return;
        }
        // 手動の価格スケジュール = スケジュール + そのテリトリの price-point を指す価格 1 つ。
        // 価格は JSON:API の "included" で作り、仮 id でスケジュールの manualPrices に結ぶ。
        // 注意: このスケジュールの POST は既存のスケジュールを丸ごと置き換える（他テリトリの手動価格も消える）。
        let body = json!({
            "data": {
                "type": "inAppPurchasePriceSchedules",
                "relationships": {
                    "inAppPurchase": {"data": {"type": "inAppPurchases", "id": iap_id}},
                    "baseTerritory": {"data": {"type": "territories", "id": territory}},
                    "manualPrices": {"data": [{"type": "inAppPurchasePrices", "id": "${price1}"}]},
                },
            },
            "included": [{
                "type": "inAppPurchasePrices",
                "id": "${price1}",
                "attributes": {"startDate": null},
                "relationships": {
                    "inAppPurchasePricePoint": {"data": {"type": "inAppPurchasePricePoints", "id": point_id}},
                },
            }],
        });
        match self
            .cli
            .r#do("POST", "/v1/inAppPurchasePriceSchedules", Some(&body))
        {
            Err(e) => r.add("price", &p.app_store_id, Status::Error, e),
            Ok(resp) if !resp.ok() => r.add("price", &p.app_store_id, Status::Error, resp.error()),
            Ok(_) => report(r, true),
        }
    }

    /// 価格スケジュールの baseTerritory と、テリトリ → 価格キー（"territory|tier"）の手動価格。
    /// 手動価格の id に {territory, tier} が入っているので、1 件ずつ引かずにデコードする。
    /// スケジュールが無ければ（404）空。読めなければエラー。
    fn read_iap_schedule(&self, iap_id: &str) -> Result<(String, HashMap<String, String>), String> {
        let mut manual = HashMap::new();
        let resp = self.cli.r#do(
            "GET",
            &format!(
                "/v2/inAppPurchases/{iap_id}/iapPriceSchedule?include=manualPrices,baseTerritory"
            ),
            None,
        )?;
        if resp.status == 404 {
            return Ok((String::new(), manual));
        }
        if !resp.ok() {
            return Err(format!("get price schedule: {}", resp.error()));
        }
        let v: Value = resp.json()?;
        if let Some(mps) = v
            .pointer("/data/relationships/manualPrices/data")
            .and_then(Value::as_array)
        {
            for mp in mps {
                if let Some((t, key)) = mp
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(iap_price_territory_key)
                {
                    manual.insert(t, key);
                }
            }
        }
        let base = v
            .pointer("/data/relationships/baseTerritory/data/id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        Ok((base, manual))
    }

    /// 設定テリトリの IAP の price-point（id, customerPrice）。
    fn iap_price_points(&self, iap_id: &str) -> Result<Vec<(String, String)>, String> {
        Ok(self
            .get_all(
                &format!(
                    "/v2/inAppPurchases/{iap_id}/pricePoints?filter[territory]={}&limit=8000",
                    self.cfg.pricing.territory
                ),
                "list iap price points",
            )?
            .into_iter()
            .map(|pp| (pp.id.clone(), attr(&pp, "customerPrice").to_string()))
            .collect())
    }
}

/// App Store の IAP の price-point / 手動価格の id を、比べられるキー "territory|tier" にする。
fn iap_price_key(id: &str) -> Option<String> {
    iap_price_territory_key(id).map(|(_, k)| k)
}

fn iap_price_territory_key(id: &str) -> Option<(String, String)> {
    use base64::Engine;
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    // Go 版と同じ順で試し、最初にデコードできたものを使う。
    let raw = [STANDARD_NO_PAD, URL_SAFE_NO_PAD, STANDARD, URL_SAFE]
        .iter()
        .find_map(|e| e.decode(id).ok())?;
    let v: Value = serde_json::from_slice(&raw).ok()?;
    let t = v
        .get("t")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())?;
    let p = v.get("p").and_then(Value::as_str).unwrap_or("");
    Some((t.to_string(), format!("{t}|{p}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn decodes_price_ids_to_territory_and_tier() {
        let id = base64::engine::general_purpose::STANDARD_NO_PAD
            .encode(r#"{"s":"6450000000","t":"USA","p":"10030"}"#);
        assert_eq!(
            iap_price_territory_key(&id),
            Some(("USA".to_string(), "USA|10030".to_string()))
        );
        // パディング付きでも読める
        let padded = base64::engine::general_purpose::STANDARD.encode(r#"{"t":"JPN","p":"1"}"#);
        assert_eq!(iap_price_key(&padded).as_deref(), Some("JPN|1"));
        assert_eq!(iap_price_key("not-base64!"), None);
        let no_t = base64::engine::general_purpose::STANDARD_NO_PAD.encode(r#"{"p":"1"}"#);
        assert_eq!(iap_price_key(&no_t), None);
    }

    #[test]
    fn price_match_tolerance() {
        assert!(price_eq("4.99", 4.99));
        assert!(price_eq("29.990", 29.99));
        assert!(!price_eq("5.99", 4.99));
        assert!(!price_eq("", 4.99));
    }
}
