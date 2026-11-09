//! 1 回の plan / apply が何をした（する）かの報告。CLI の表示と、Cloud へ送る JSON
//! （docs/02_cli-architecture/03-output.md）の両方の元になる。鍵や認証情報はここに入れない。

use serde::Serialize;

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

/// plan / apply の報告の 1 行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Action {
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

/// provider が Action を積むための小さな入れ物。
#[derive(Debug)]
pub struct Report {
    provider: &'static str,
    pub actions: Vec<Action>,
}

impl Report {
    pub fn new(provider: &'static str) -> Self {
        Report {
            provider,
            actions: Vec::new(),
        }
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
            provider: self.provider,
            op: op.to_string(),
            target: target.to_string(),
            status,
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
    pub app: &'a str,
    /// "plan" | "apply" | "retire-plan" | "retire"
    pub command: &'a str,
    pub summary: Summary,
    pub actions: &'a [Action],
}

pub const JSON_VERSION: u32 = 1;

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
            app: "demo",
            command: "plan",
            summary: Summary::of(&r.actions),
            actions: &r.actions,
        };
        let v = serde_json::to_value(&rep).unwrap();
        assert_eq!(v["version"], 1);
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
