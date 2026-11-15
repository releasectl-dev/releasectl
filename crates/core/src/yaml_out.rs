//! `init` が書き出す YAML の小さな書き手（docs: 02_cli-architecture/08-init.md）。
//! 人が読んで commit するファイルなので、キーの順番と書式をこちらで決める:
//! 日本語は `\u` にせずそのまま、複数行は `|`、それ以外で安全でない文字列は二重引用符。

/// YAML を 1 行ずつ積む。
#[derive(Default)]
pub struct Yaml {
    out: String,
}

impl Yaml {
    pub fn finish(self) -> String {
        self.out
    }

    pub fn blank(&mut self) {
        self.out.push('\n');
    }

    pub fn comment(&mut self, indent: usize, text: &str) {
        self.line(indent, &format!("# {text}"));
    }

    /// そのままの 1 行（`key:` や `- key: value` を呼び出し側が組み立てる）。
    pub fn line(&mut self, indent: usize, text: &str) {
        self.out.push_str(&" ".repeat(indent));
        self.out.push_str(text);
        self.out.push('\n');
    }

    /// `key: value`。空（末尾の空白を落として空）なら書かない。複数行は `|`。
    pub fn str(&mut self, indent: usize, key: &str, value: &str) {
        self.str_with(indent, key, value);
    }

    /// リストの最初の項目（`- key: value`）。
    pub fn item_str(&mut self, indent: usize, key: &str, value: &str) {
        self.str_with(indent, &format!("- {key}"), value);
    }

    fn str_with(&mut self, indent: usize, key: &str, value: &str) {
        let v = value.trim_end();
        if v.is_empty() {
            return;
        }
        if v.contains('\n') && block_safe(v) {
            // `- key: |` の中身は、キーの位置（"- " の分だけ右）から 2 つ下げる
            let body = indent + if key.starts_with("- ") { 4 } else { 2 };
            self.line(indent, &format!("{key}: |"));
            for l in v.split('\n') {
                if l.is_empty() {
                    self.out.push('\n');
                } else {
                    self.line(body, l);
                }
            }
        } else {
            self.line(indent, &format!("{key}: {}", scalar(v)));
        }
    }

    pub fn bool(&mut self, indent: usize, key: &str, value: bool) {
        self.line(indent, &format!("{key}: {value}"));
    }

    /// 小数（価格）。`4.99` / `5`。
    pub fn num(&mut self, indent: usize, key: &str, value: &str) {
        self.line(indent, &format!("{key}: {value}"));
    }
}

/// `|` のブロックでそのまま書けるか。書けないもの（CR・制御文字・先頭が空白や空行・行末の空白）は引用符にする。
fn block_safe(v: &str) -> bool {
    let first = v.split('\n').next().unwrap_or("");
    !first.is_empty()
        && !first.starts_with([' ', '\t'])
        && v.split('\n').all(|l| !l.ends_with([' ', '\t']))
        && v.chars().all(|c| c == '\n' || !needs_escape(c))
        && !v.contains('\t')
}

/// 二重引用符の中でエスケープが要る文字（表示できないもの）。
fn needs_escape(c: char) -> bool {
    c.is_control() || matches!(c, '\u{85}' | '\u{2028}' | '\u{2029}' | '\u{feff}')
}

/// 1 行の値。識別子・URL・メールのような安全なものは引用符なし、それ以外は二重引用符。
pub fn scalar(v: &str) -> String {
    if plain_safe(v) {
        return v.to_string();
    }
    let mut s = String::with_capacity(v.len() + 2);
    s.push('"');
    for c in v.chars() {
        match c {
            '\\' => s.push_str("\\\\"),
            '"' => s.push_str("\\\""),
            '\n' => s.push_str("\\n"),
            '\t' => s.push_str("\\t"),
            '\r' => s.push_str("\\r"),
            c if needs_escape(c) => s.push_str(&format!("\\u{:04X}", c as u32)),
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

/// 引用符なしで書いて、文字列として読み戻せるか（英字で始まり、記号は URL・メール・ID に出るものだけ。
/// true / null などの YAML が別の型に読む語は除く）。
fn plain_safe(v: &str) -> bool {
    let mut chars = v.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    if !v
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_./:@+-".contains(c))
        || v.ends_with(':')
    {
        return false;
    }
    !matches!(
        v.to_ascii_lowercase().as_str(),
        "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "y" | "n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn round_trip(v: &str) -> String {
        let mut y = Yaml::default();
        y.line(0, "top:");
        y.str(2, "k", v);
        y.line(2, "list:");
        y.item_str(4, "k", v);
        y.str(6, "other", "x");
        let text = y.finish();
        #[derive(serde::Deserialize)]
        struct Top {
            top: Inner,
        }
        #[derive(serde::Deserialize)]
        struct Inner {
            k: String,
            list: Vec<BTreeMap<String, String>>,
        }
        let t: Top = serde_yaml_ng::from_str(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        assert_eq!(t.top.list[0]["k"], t.top.k, "{text}");
        assert_eq!(t.top.list[0]["other"], "x", "{text}");
        t.top.k
    }

    #[test]
    fn strings_round_trip() {
        for v in [
            "plain",
            "https://example.com/a?b=c",
            "a@example.com",
            "true",
            "No",
            "1.0",
            "+81 3 1234 5678",
            "日本語の説明。",
            "colon: and # hash",
            "quote \" and \\ backslash",
            "- starts with dash",
            "tab\there",
            "nbsp\u{a0}and bom\u{feff}and ls\u{2028}",
            "line one\nline two\n\nafter blank",
            "箇条書き:\n- a\n- b: c\n# not a comment",
            " leading space\nsecond",
            "trailing space \nsecond",
            "\nleading newline",
            "cr\r\nlf",
            "emoji 🎉\n2 行目",
        ] {
            // `|` は末尾に改行を 1 つ付ける（plan は末尾の空白を無視して比べる）
            assert_eq!(round_trip(v).trim_end(), v.trim_end(), "{v:?}");
        }
    }

    #[test]
    fn style() {
        let mut y = Yaml::default();
        y.str(0, "name", "E2E 月額");
        y.str(0, "url", "https://example.com");
        y.str(0, "empty", " \n");
        y.str(0, "description", "一行目\n\n三行目\n");
        y.item_str(2, "notes", "a\nb");
        assert_eq!(
            y.finish(),
            "name: \"E2E 月額\"\nurl: https://example.com\ndescription: |\n  一行目\n\n  三行目\n  - notes: |\n      a\n      b\n"
        );
    }
}
