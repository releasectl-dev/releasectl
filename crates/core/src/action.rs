//! 1 回の plan / apply が何をした（する）かの報告。CLI の表示と、Cloud へ送る JSON
//! （docs/02_cli-architecture/03-output.md）の両方の元になる。鍵や認証情報はここに入れない。

use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};

/// 1 つの希望項目を reconcile した結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// 無い → 作る予定（plan）
    Create,
    /// あるが希望値と違う → 直す予定（plan）。コンソールでの手変更・販売停止中など
    Drift,
    /// 無かったので作った（apply）
    Created,
    /// 違っていたので直した（apply）
    Updated,
    /// 希望どおり。変更なし
    Exists,
    /// 作成時にしか決められない属性がずれていて、作り直しが要る（plan）
    Replace,
    /// 消して作り直した（apply）
    Replaced,
    /// 意図的に飛ばした（前提が足りない・手作業の案内）
    Skip,
    /// 販売中 → 販売停止にする予定（retire plan）
    Retire,
    /// 販売停止にした（retire）
    Retired,
    /// 失敗。detail に理由。読み取りの失敗もここ（「無い」とはみなさない）
    Error,
}

impl Status {
    /// JSON の summary に並べる順。
    pub const ALL: [Status; 11] = [
        Status::Create,
        Status::Drift,
        Status::Created,
        Status::Updated,
        Status::Exists,
        Status::Replace,
        Status::Replaced,
        Status::Skip,
        Status::Retire,
        Status::Retired,
        Status::Error,
    ];

    /// 人間向け表示の記号。
    pub fn symbol(self) -> &'static str {
        match self {
            Status::Create => "+",
            Status::Drift => "~",
            Status::Created | Status::Updated => "✓",
            Status::Exists => "=",
            Status::Replace => "-/+",
            Status::Replaced => "±",
            Status::Retire => "→",
            Status::Retired => "⊘",
            Status::Skip => "-",
            Status::Error => "✗",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Create => "create",
            Status::Drift => "drift",
            Status::Created => "created",
            Status::Updated => "updated",
            Status::Exists => "exists",
            Status::Replace => "replace",
            Status::Replaced => "replaced",
            Status::Skip => "skip",
            Status::Retire => "retire",
            Status::Retired => "retired",
            Status::Error => "error",
        }
    }

    /// まだ適用していない差分（`--detailed-exitcode` で 2 にするもの）。
    pub fn is_pending_change(self) -> bool {
        matches!(
            self,
            Status::Create | Status::Drift | Status::Replace | Status::Retire
        )
    }
}

/// 金額（JSON v2 の `current_value` / `desired_value`）。小数の誤差を避けるため amount は文字列。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Money {
    pub currency: String,
    pub amount: String,
}

impl Money {
    /// 表示用の値（"USD 4.99"）から読む。金額でなければ None（"INACTIVE"、"base 1.99" など）。
    pub fn parse(s: &str) -> Option<Money> {
        let (cur, amount) = s.split_once(' ')?;
        let is_amount = !amount.is_empty()
            && amount.chars().all(|c| c.is_ascii_digit() || c == '.')
            && amount.chars().filter(|c| *c == '.').count() <= 1
            && amount.chars().next().is_some_and(|c| c.is_ascii_digit());
        (cur.len() == 3 && cur.chars().all(|c| c.is_ascii_uppercase()) && is_amount).then(|| {
            Money {
                currency: cur.to_string(),
                amount: amount.to_string(),
            }
        })
    }
}

/// plan / apply の報告の 1 行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Action {
    /// 項目を一意に指す固定の識別子（JSON v2）。`provider/op[/親]/target[/副キー]`。Cloud の履歴はこれに乗るので、
    /// 一度決めた形は変えない（docs/02_cli-architecture/03-output.md）
    pub key: String,
    /// "appstore" | "googleplay"
    pub provider: &'static str,
    /// 短い動詞（"subscription"、"basePlan.price" など）。provider 全体の skip / error は "provider"
    pub op: String,
    /// 対象の識別子
    #[serde(skip_serializing_if = "String::is_empty")]
    pub target: String,
    pub status: Status,
    /// drift / update のときの現在値（機械が読む。例 "USD 4.99"）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// drift / update のときの希望値
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desired: Option<String>,
    /// `current` が金額なら構造で（JSON v2）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_value: Option<Money>,
    /// `desired` が金額なら構造で（JSON v2）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desired_value: Option<Money>,
    /// 人間向けの補足（エラー文など）
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

impl Action {
    /// 表示の 1 行（Go 版 `printActions` と同じレイアウト）。
    pub fn render(&self) -> String {
        let mut line = format!("  {} {:<20} {}", self.status.symbol(), self.op, self.target);
        if !self.detail.is_empty() {
            line.push_str("  — ");
            line.push_str(&self.detail);
        }
        line
    }
}

/// meta の App Store で対象にした版（JSON v2 の `appStoreVersion`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStoreVersion {
    pub version_string: String,
    pub state: String,
    /// この実行で作った
    pub created: bool,
}

/// provider が Action を積むための小さな入れ物。
#[derive(Debug)]
pub struct Report {
    provider: &'static str,
    pub actions: Vec<Action>,
    /// ストアの ID（JSON v2 の `stores.<provider>`。例 "appId"）。読めたものだけ
    pub store_ids: BTreeMap<String, String>,
    /// meta の App Store で対象にした版
    pub app_store_version: Option<AppStoreVersion>,
    /// op ごとに key に入れる親（Play の base plan の ID はサブスクの中でだけ一意）
    key_parent: HashMap<String, String>,
    /// op ごとに key の末尾に足す副キー（価格のテリトリ / リージョン）
    key_suffix: HashMap<String, String>,
    /// key に target を入れない op（target が API の内部 ID で、アプリ / 版に 1 つしかないもの）
    key_singleton: HashSet<String>,
}

impl Report {
    pub fn new(provider: &'static str) -> Self {
        Report {
            provider,
            actions: Vec::new(),
            store_ids: BTreeMap::new(),
            app_store_version: None,
            key_parent: HashMap::new(),
            key_suffix: HashMap::new(),
            key_singleton: HashSet::new(),
        }
    }

    /// 続けて積むその op の行の key に親を入れる（None で外す）。
    pub fn set_key_parent(&mut self, ops: &[&str], parent: Option<&str>) {
        for op in ops {
            match parent {
                Some(p) => self.key_parent.insert(op.to_string(), p.to_string()),
                None => self.key_parent.remove(*op),
            };
        }
    }

    /// その op の key の末尾に副キーを足す（例: App Store の price にテリトリ）。
    pub fn set_key_suffix(&mut self, op: &str, suffix: &str) {
        self.key_suffix.insert(op.to_string(), suffix.to_string());
    }

    /// その op の key に target を入れない。
    pub fn set_key_singleton(&mut self, ops: &[&str]) {
        self.key_singleton.extend(ops.iter().map(|s| s.to_string()));
    }

    pub fn store_id(&mut self, name: &str, value: &str) {
        if !value.is_empty() {
            self.store_ids.insert(name.to_string(), value.to_string());
        }
    }

    fn key(&self, op: &str, target: &str) -> String {
        let mut parts = vec![self.provider.to_string(), op.to_string()];
        if !self.key_singleton.contains(op) && !target.is_empty() {
            if let Some(p) = self.key_parent.get(op) {
                parts.push(p.clone());
            }
            // "dev.x.monthly [en-US]"（App Store のローカライズの表示）→ "dev.x.monthly/en-US"
            match target.strip_suffix(']').and_then(|t| t.split_once(" [")) {
                Some((id, loc)) => {
                    parts.push(id.to_string());
                    parts.push(loc.to_string());
                }
                None => parts.push(target.to_string()),
            }
        }
        if let Some(sfx) = self.key_suffix.get(op).filter(|s| !s.is_empty()) {
            parts.push(sfx.clone());
        }
        parts.join("/")
    }

    pub fn add(&mut self, op: &str, target: &str, status: Status, detail: impl Into<String>) {
        self.push(op, target, status, detail.into(), None, None);
    }

    /// 現在値と希望値つきで積む（drift / updated）。detail は "current→desired" にする。
    pub fn add_diff(
        &mut self,
        op: &str,
        target: &str,
        status: Status,
        current: impl Into<String>,
        desired: impl Into<String>,
    ) {
        let (c, d) = (current.into(), desired.into());
        let detail = format!("{c}→{d}");
        self.push(op, target, status, detail, Some(c), Some(d));
    }

    /// detail を別に指定して、現在値と希望値つきで積む。
    pub fn add_with(
        &mut self,
        op: &str,
        target: &str,
        status: Status,
        detail: impl Into<String>,
        current: impl Into<String>,
        desired: impl Into<String>,
    ) {
        self.push(
            op,
            target,
            status,
            detail.into(),
            Some(current.into()),
            Some(desired.into()),
        );
    }

    fn push(
        &mut self,
        op: &str,
        target: &str,
        status: Status,
        detail: String,
        current: Option<String>,
        desired: Option<String>,
    ) {
        self.actions.push(Action {
            key: self.key(op, target),
            provider: self.provider,
            op: op.to_string(),
            target: target.to_string(),
            status,
            current_value: current.as_deref().and_then(Money::parse),
            desired_value: desired.as_deref().and_then(Money::parse),
            current,
            desired,
            detail,
        });
    }

    pub fn has_error(&self) -> bool {
        self.actions.iter().any(|a| a.status == Status::Error)
    }
}

/// `--output json` で出す 1 回分の結果（docs/02_cli-architecture/03-output.md）。
#[derive(Debug, Serialize)]
pub struct RunReport<'a> {
    /// JSON の形の版。互換の無い変更をしたら上げる
    pub version: u32,
    /// CLI のバージョン（v2）
    pub releasectl_version: &'a str,
    pub app: &'a str,
    /// "iap-plan" | "iap-apply" | "iap-retire-plan" | "iap-retire" | "meta-plan" | "meta-apply"（GitHub Action の command と同じ）
    pub command: &'a str,
    /// ストアの ID（v2）。provider → {"bundleId" / "appId" / "packageName"}。Cloud はアプリをこれで見分ける
    pub stores: BTreeMap<&'static str, BTreeMap<String, String>>,
    /// meta の App Store で対象にした版（v2）。App Store を流さなかったときは出さない
    #[serde(rename = "appStoreVersion", skip_serializing_if = "Option::is_none")]
    pub app_store_version: Option<AppStoreVersion>,
    pub summary: Summary,
    pub actions: &'a [Action],
}

/// v2（2026-11-12）: releasectl_version・stores・appStoreVersion・Action の key・current_value / desired_value を足した。
/// v1 の項目はそのまま。
pub const JSON_VERSION: u32 = 2;

/// status ごとの件数（0 件の status も出す。並びは Status::ALL）。
#[derive(Debug, PartialEq, Eq)]
pub struct Summary(pub Vec<(Status, usize)>);

impl Summary {
    pub fn of(actions: &[Action]) -> Summary {
        Summary(
            Status::ALL
                .iter()
                .map(|s| (*s, actions.iter().filter(|a| a.status == *s).count()))
                .collect(),
        )
    }
}

impl Serialize for Summary {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (st, n) in &self.0 {
            m.serialize_entry(st.as_str(), n)?;
        }
        m.end()
    }
}

/// 終了コード（docs: 出力と終了コード）。エラーがあれば 1。`detailed` なら差分ありで 2。
pub fn exit_code(actions: &[Action], had_fatal: bool, detailed: bool) -> u8 {
    if had_fatal || actions.iter().any(|a| a.status == Status::Error) {
        1
    } else if detailed && actions.iter().any(|a| a.status.is_pending_change()) {
        2
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_layout() {
        let mut r = Report::new("googleplay");
        r.add_diff(
            "basePlan.price",
            "monthly",
            Status::Drift,
            "USD 4.99",
            "USD 5.99",
        );
        r.add("subscription", "sonir_pro_monthly", Status::Exists, "");
        assert_eq!(
            r.actions[0].render(),
            "  ~ basePlan.price       monthly  — USD 4.99→USD 5.99"
        );
        assert_eq!(
            r.actions[1].render(),
            "  = subscription         sonir_pro_monthly"
        );
    }

    #[test]
    fn json_shape() {
        let mut r = Report::new("appstore");
        r.add_diff("price", "a.monthly", Status::Drift, "USD 1.99", "USD 2.99");
        r.add("group", "Pro", Status::Exists, "");
        let rep = RunReport {
            version: JSON_VERSION,
            releasectl_version: "0.7.0",
            app: "demo",
            command: "iap-plan",
            stores: BTreeMap::new(),
            app_store_version: None,
            summary: Summary::of(&r.actions),
            actions: &r.actions,
        };
        let v = serde_json::to_value(&rep).unwrap();
        assert_eq!(v["version"], 2);
        assert_eq!(v["releasectl_version"], "0.7.0");
        assert!(v.get("appStoreVersion").is_none());
        assert_eq!(v["actions"][0]["key"], "appstore/price/a.monthly");
        assert_eq!(v["actions"][0]["current_value"]["amount"], "1.99");
        assert_eq!(v["actions"][0]["desired_value"]["currency"], "USD");
        assert!(v["actions"][1].get("current_value").is_none());
        assert_eq!(v["summary"]["drift"], 1);
        assert_eq!(v["summary"]["exists"], 1);
        assert_eq!(v["summary"]["error"], 0);
        assert_eq!(v["actions"][0]["status"], "drift");
        assert_eq!(v["actions"][0]["current"], "USD 1.99");
        assert_eq!(v["actions"][0]["desired"], "USD 2.99");
        // 無いものは省く
        assert!(v["actions"][1].get("current").is_none());
        assert!(v["actions"][1].get("detail").is_none());
        // summary の並びは Status::ALL
        let keys: Vec<_> = v["summary"].as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys.len(), Status::ALL.len());
    }

    #[test]
    fn keys() {
        let mut r = Report::new("appstore");
        r.set_key_suffix("price", "USA");
        r.set_key_singleton(&["copyright"]);
        r.add("price", "dev.x.monthly", Status::Exists, "");
        r.add("localization", "dev.x.monthly [en-US]", Status::Exists, "");
        r.add("copyright", "80e3526a-version-id", Status::Exists, "");
        r.add("provider", "", Status::Skip, "no keys");
        let keys: Vec<_> = r.actions.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "appstore/price/dev.x.monthly/USA",
                "appstore/localization/dev.x.monthly/en-US",
                "appstore/copyright",
                "appstore/provider",
            ]
        );

        let mut g = Report::new("googleplay");
        g.set_key_parent(&["basePlan.price"], Some("pro_monthly"));
        g.add("subscription", "pro_monthly", Status::Exists, "");
        g.add("basePlan.price", "monthly", Status::Exists, "");
        g.set_key_parent(&["basePlan.price"], None);
        g.add("subscription", "pro_yearly", Status::Exists, "");
        assert_eq!(g.actions[0].key, "googleplay/subscription/pro_monthly");
        assert_eq!(
            g.actions[1].key,
            "googleplay/basePlan.price/pro_monthly/monthly"
        );
        assert_eq!(g.actions[2].key, "googleplay/subscription/pro_yearly");
    }

    #[test]
    fn money() {
        let m = Money::parse("USD 4.99").unwrap();
        assert_eq!((m.currency.as_str(), m.amount.as_str()), ("USD", "4.99"));
        assert_eq!(Money::parse("JPY 300").unwrap().amount, "300");
        for s in [
            "INACTIVE",
            "base 1.99",
            "USD",
            "USD .5",
            "USD 1.2.3",
            "usd 1",
            "USD 4.99→5.99",
        ] {
            assert_eq!(Money::parse(s), None, "{s}");
        }
    }

    #[test]
    fn exit_codes() {
        let mut r = Report::new("googleplay");
        r.add("subscription", "x", Status::Exists, "");
        assert_eq!(exit_code(&r.actions, false, true), 0);
        r.add("basePlan.price", "x", Status::Drift, "");
        assert_eq!(exit_code(&r.actions, false, true), 2);
        assert_eq!(exit_code(&r.actions, false, false), 0);
        assert_eq!(exit_code(&r.actions, true, true), 1);
        r.add("listing", "x", Status::Error, "boom");
        assert_eq!(exit_code(&r.actions, false, true), 1);
        // apply の結果（created / updated）は差分ではない
        let mut a = Report::new("googleplay");
        a.add("subscription", "x", Status::Created, "");
        a.add("basePlan.price", "x", Status::Updated, "");
        assert_eq!(exit_code(&a.actions, false, true), 0);
    }
}
