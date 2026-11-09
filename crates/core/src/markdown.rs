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
    format!(
        "| {} | {} | `{}` | `{}` | {} |",
        a.status.symbol(),
        a.provider,
        cell(&a.op),
        cell(&a.target),
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

    if notable.is_empty() {
        out.push_str("✅ **差分なし** — ストアは desired-state と一致しています。\n\n");
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
            app: "demo",
            command,
            summary: Summary::of(actions),
            actions,
        }
    }

    #[test]
    fn no_changes() {
        let mut r = Report::new("googleplay");
        r.add("subscription", "m", Status::Exists, "");
        let md = render(&rep("plan", &r.actions));
        assert!(md.starts_with("<!-- releasectl:plan -->\n### releasectl plan — demo"));
        assert!(md.contains("差分なし"));
        assert!(md.contains("<details><summary>全 1 件（exists 1）</summary>"));
    }

    #[test]
    fn drift_table() {
        let mut r = Report::new("appstore");
        r.add_diff("price", "a.monthly", Status::Drift, "USD 1.99", "USD 2.99");
        r.add("localization", "a|b [en-US]", Status::Drift, "description");
        r.add("group", "Pro", Status::Exists, "");
        let md = render(&rep("plan", &r.actions));
        assert!(md.contains("📝 **drift 2 · exists 1**"));
        assert!(md.contains("| ~ | appstore | `price` | `a.monthly` | USD 1.99 → USD 2.99 |"));
        // セルの | はエスケープする
        assert!(md.contains("`a\\|b [en-US]`"));
        // 一致は表に出さない（畳んだ全件にだけ）
        let table = md.split("<details>").next().unwrap();
        assert!(!table.contains("`group`"));
    }

    #[test]
    fn errors_are_flagged() {
        let mut r = Report::new("appstore");
        r.add("provider", "", Status::Error, "HTTP 500");
        let md = render(&rep("apply", &r.actions));
        assert!(md.contains("❌ **error 1**"));
        assert!(md.contains("<!-- releasectl:apply -->"));
    }
}
