//! 1 回の plan / apply が何をした（する）かの報告。CLI の表示と、
//! 将来 Cloud へ送る JSON（docs: CLI → Cloud の契約）の両方の元になる。
//! 鍵や認証情報はここに入れない。

/// 1 つの希望項目を reconcile した結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// 無い → 作る予定（plan）
    Create,
    /// 無かったので作った（apply）
    Created,
    /// 希望どおり。変更なし
    Exists,
    /// 作成時にしか決められない属性がずれていて、作り直しが要る（plan）
    Replace,
    /// 消して作り直した（apply）
    Replaced,
    /// 意図的に飛ばした（前提が足りない等）
    Skip,
    /// 販売中 → 販売停止にする予定（retire plan）
    Retire,
    /// 販売停止にした（retire）
    Retired,
    /// 失敗。detail に理由
    Error,
}

impl Status {
    /// 人間向け表示の記号（Go 版 `symbol` と同じ）。
    pub fn symbol(self) -> &'static str {
        match self {
            Status::Create => "+",
            Status::Created => "✓",
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
            Status::Created => "created",
            Status::Exists => "exists",
            Status::Replace => "replace",
            Status::Replaced => "replaced",
            Status::Skip => "skip",
            Status::Retire => "retire",
            Status::Retired => "retired",
            Status::Error => "error",
        }
    }
}

/// plan / apply の報告の 1 行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    /// "appstore" | "googleplay"
    pub provider: &'static str,
    /// 短い動詞（"subscription"、"basePlan.price" など）
    pub op: String,
    /// 対象の識別子
    pub target: String,
    pub status: Status,
    /// 人間向けの補足（エラー文など）
    pub detail: String,
}

impl Action {
    /// Go 版 `printActions` の 1 行と同じ形。
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
        self.actions.push(Action {
            provider: self.provider,
            op: op.to_string(),
            target: target.to_string(),
            status,
            detail: detail.into(),
        });
    }

    pub fn has_error(&self) -> bool {
        self.actions.iter().any(|a| a.status == Status::Error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_matches_go_layout() {
        let mut r = Report::new("googleplay");
        r.add("basePlan.price", "monthly", Status::Create, "USD 4→5.99");
        r.add("subscription", "sonir_pro_monthly", Status::Exists, "");
        assert_eq!(
            r.actions[0].render(),
            "  + basePlan.price       monthly  — USD 4→5.99"
        );
        assert_eq!(
            r.actions[1].render(),
            "  = subscription         sonir_pro_monthly"
        );
    }
}
