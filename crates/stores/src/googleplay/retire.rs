//! Google Play の販売停止（Go 版 `internal/googleplay/retire.go`）。
//!
//! サブスクは base plan を `basePlans:deactivate`、一回課金は購入オプションを
//! `purchaseOptions:batchUpdateStates`（deactivate）で INACTIVE にする。どちらも作成時の
//! activate のちょうど逆なので、apply を流せば販売に戻る。消さないのはこの可逆性のため。
//! 商品・掲載・価格の履歴は触らず、購入済みの人の権利も残る。

use super::{ONE_TIME_BUY_OPTION, Play, base_plan_state};
use releasectl_core::{Report, Status};
use serde_json::json;

impl Play {
    pub(super) fn retire_products(&self, apply: bool, r: &mut Report) -> Result<(), String> {
        let pkg = self.cfg.package_name.as_str();
        if pkg.is_empty() {
            return Err("packageName is required for the googleplay provider".into());
        }
        for prod in &self.cfg.products {
            if prod.play_product_id.is_empty() {
                continue;
            }
            let pid = prod.play_product_id.as_str();
            if prod.is_one_time() {
                self.retire_one_time_product(apply, pkg, pid, r);
                continue;
            }
            let bpid = prod.play_base_plan_id.as_str();
            r.set_key_parent(&super::BASE_PLAN_OPS, Some(pid));
            let sub = match self.get_subscription(pkg, pid) {
                Ok(Some(s)) => s,
                Ok(None) => {
                    r.add(
                        "basePlan.deactivate",
                        bpid,
                        Status::Skip,
                        "subscription not on Play",
                    );
                    continue;
                }
                Err(e) => {
                    r.add("subscription", pid, Status::Error, e);
                    continue;
                }
            };
            match base_plan_state(&sub, bpid) {
                "" => r.add(
                    "basePlan.deactivate",
                    bpid,
                    Status::Skip,
                    "base plan not found",
                ),
                "ACTIVE" if !apply => r.add(
                    "basePlan.deactivate",
                    bpid,
                    Status::Retire,
                    "ACTIVE -> INACTIVE",
                ),
                "ACTIVE" => match self.post_ok(
                    &format!("/applications/{pkg}/subscriptions/{pid}/basePlans/{bpid}:deactivate"),
                    &json!({}),
                ) {
                    Ok(()) => r.add("basePlan.deactivate", bpid, Status::Retired, "INACTIVE"),
                    Err(e) => r.add("basePlan.deactivate", bpid, Status::Error, e),
                },
                _ => r.add(
                    "basePlan.deactivate",
                    bpid,
                    Status::Exists,
                    "already off sale",
                ),
            }
        }
        Ok(())
    }

    fn retire_one_time_product(&self, apply: bool, pkg: &str, pid: &str, r: &mut Report) {
        // 作成の流れが管理しているのと同じ購入オプション 1 つ。
        let opt = match self.one_time_option(pkg, pid, ONE_TIME_BUY_OPTION) {
            Ok(o) => o,
            Err(e) => {
                r.add("oneTimeProduct", pid, Status::Error, e);
                return;
            }
        };
        if !opt.found {
            r.add(
                "purchaseOption.deactivate",
                pid,
                Status::Skip,
                "product not on Play",
            );
            return;
        }
        match opt.state.as_str() {
            "" => r.add(
                "purchaseOption.deactivate",
                pid,
                Status::Skip,
                "purchase option not found",
            ),
            "ACTIVE" if !apply => r.add(
                "purchaseOption.deactivate",
                pid,
                Status::Retire,
                "ACTIVE -> INACTIVE",
            ),
            "ACTIVE" => {
                let body = json!({"requests": [{
                    "deactivatePurchaseOptionRequest": {
                        "packageName": pkg,
                        "productId": pid,
                        "purchaseOptionId": ONE_TIME_BUY_OPTION,
                    },
                }]});
                match self.post_ok(
                    &format!("/applications/{pkg}/oneTimeProducts/{pid}/purchaseOptions:batchUpdateStates"),
                    &body,
                ) {
                    Ok(()) => r.add("purchaseOption.deactivate", pid, Status::Retired, "INACTIVE"),
                    Err(e) => r.add("purchaseOption.deactivate", pid, Status::Error, e),
                }
            }
            _ => r.add(
                "purchaseOption.deactivate",
                pid,
                Status::Exists,
                "already off sale",
            ),
        }
    }

    /// POST して 2xx でなければエラー文を返す。
    fn post_ok(&self, path: &str, body: &serde_json::Value) -> Result<(), String> {
        let resp = self.cli.r#do("POST", path, Some(body))?;
        if !resp.ok() {
            return Err(resp.error());
        }
        Ok(())
    }
}
