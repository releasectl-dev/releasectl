//! PR コメントと GitHub Actions の job summary 用の Markdown（docs: GitHub Actions）。

use crate::action::{Action, RunReport, Status};

/// PR の sticky コメントを見つけるための印（コマンドごと）。
pub fn marker(command: &str) -> String {
    format!("<!-- releasectl:{command} -->")
}

/// 表に出す行か（変更・失敗。一致と skip は畳んだ全件にだけ出す）。
fn is_notable(a: &Action) -> bool {
    !matches!(a.status, Status::Exists | Status::Skip)
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn row(a: &Action) -> String {
    let change = match (&a.current, &a.desired) {
        (Some(c), Some(d)) => format!("{} → {}", cell(c), cell(d)),
        _ => cell(&a.detail),
    };
    let target = if a.target.is_empty() {
        "—".to_string()
    } else {
        format!("`{}`", cell(&a.target))
    };
    format!(
        "| {} | {} | `{}` | {} | {} |",
        a.status.symbol(),
        a.provider,
        cell(&a.op),
        target,
        change
    )
}

const HEADER: &str = "| | provider | op | target | change |\n|---|---|---|---|---|";

pub fn render(r: &RunReport) -> String {
    let mut out = String::new();
    out.push_str(&marker(r.command));
    out.push('\n');
    out.push_str(&format!("### releasectl {} — {}\n\n", r.command, r.app));

    let counts: Vec<String> = r
        .summary
        .0
        .iter()
        .filter(|(_, n)| *n > 0)
        .map(|(s, n)| format!("{} {n}", s.as_str()))
        .collect();
    let notable: Vec<&Action> = r.actions.iter().filter(|a| is_notable(a)).collect();
    // 鍵が無くて飛ばした provider（確認していないので「差分なし」とは言えない）
    let skipped: Vec<&str> = r
        .actions
        .iter()
        .filter(|a| a.op == "provider" && a.status == Status::Skip)
        .map(|a| a.provider)
        .collect();
    let checked_any = r
        .actions
        .iter()
        .any(|a| !(a.op == "provider" && a.status == Status::Skip));

    if !skipped.is_empty() {
        out.push_str(&format!(
            "⚠️ **確認していない provider: {}**（鍵の env が無い。詳細は下の全件）\n\n",
            skipped.join(", ")
        ));
    }
    if notable.is_empty() {
        if checked_any {
            out.push_str("✅ **差分なし** — ストアは desired-state と一致しています。\n\n");
        }
    } else {
        let has_error = notable.iter().any(|a| a.status == Status::Error);
        out.push_str(if has_error { "❌ " } else { "📝 " });
        out.push_str(&format!("**{}**\n\n", counts.join(" · ")));
        out.push_str(HEADER);
        out.push('\n');
        for a in &notable {
            out.push_str(&row(a));
            out.push('\n');
        }
        out.push('\n');
    }

    if !r.actions.is_empty() {
        out.push_str(&format!(
            "<details><summary>全 {} 件（{}）</summary>\n\n{HEADER}\n",
            r.actions.len(),
            counts.join(" · ")
        ));
        for a in r.actions {
            out.push_str(&row(a));
            out.push('\n');
        }
        out.push_str("\n</details>\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{JSON_VERSION, Report, Summary};

    fn rep<'a>(command: &'a str, actions: &'a [Action]) -> RunReport<'a> {
        RunReport {
            version: JSON_VERSION,
            releasectl_version: "0.0.0",
            app: "demo",
            command,
            stores: Default::default(),
            app_store_version: None,
            summary: Summary::of(actions),
            actions,
        }
    }

    #[test]
    fn no_changes() {
        let mut r = Report::new("googleplay");
        r.add("subscription", "m", Status::Exists, "");
        let md = render(&rep("iap-plan", &r.actions));
        assert!(md.starts_with("<!-- releasectl:iap-plan -->\n### releasectl iap-plan — demo"));
        assert!(md.contains("差分なし"));
        assert!(md.contains("<details><summary>全 1 件（exists 1）</summary>"));
    }

    #[test]
    fn drift_table() {
        let mut r = Report::new("appstore");
        r.add_diff("price", "a.monthly", Status::Drift, "USD 1.99", "USD 2.99");
        r.add("localization", "a|b [en-US]", Status::Drift, "description");
        r.add("group", "Pro", Status::Exists, "");
        let md = render(&rep("iap-plan", &r.actions));
        assert!(md.contains("📝 **drift 2 · exists 1**"));
        assert!(md.contains("| ~ | appstore | `price` | `a.monthly` | USD 1.99 → USD 2.99 |"));
        // セルの | はエスケープする
        assert!(md.contains("`a\\|b [en-US]`"));
        // 一致は表に出さない（畳んだ全件にだけ）
        let table = md.split("<details>").next().unwrap();
        assert!(!table.contains("`group`"));
    }

    #[test]
    fn skipped_providers_are_not_called_clean() {
        let mut r = Report::new("appstore");
        r.add(
            "provider",
            "",
            Status::Skip,
            "ASC_KEY_ID / ASC_ISSUER_ID not set",
        );
        let md = render(&rep("iap-plan", &r.actions));
        assert!(md.contains("⚠️ **確認していない provider: appstore**"));
        assert!(!md.contains("差分なし"));
        assert!(md.contains("| - | appstore | `provider` | — |"));
        // 片方だけ確認できたなら、差分なしと警告の両方
        r.add("group", "Pro", Status::Exists, "");
        let md = render(&rep("iap-plan", &r.actions));
        assert!(md.contains("確認していない provider: appstore"));
        assert!(md.contains("差分なし"));
    }

    #[test]
    fn errors_are_flagged() {
        let mut r = Report::new("appstore");
        r.add("provider", "", Status::Error, "HTTP 500");
        let md = render(&rep("iap-apply", &r.actions));
        assert!(md.contains("❌ **error 1**"));
        assert!(md.contains("<!-- releasectl:iap-apply -->"));
    }
}
