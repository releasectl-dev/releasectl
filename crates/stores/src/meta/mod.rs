//! ストアの掲載情報（`releasectl meta`、Go 版 `internal/storemeta`）。
//! docs: 02_cli-architecture/06-meta.md。

mod appstore;
mod appstore_app;
mod appstore_creatives;
mod appstore_export;
mod appstore_shots;
mod googleplay;
mod googleplay_export;
mod googleplay_images;

use crate::Reconciler;
use releasectl_core::meta::MetaConfig;
use serde_json::{Map, Value};
use std::path::Path;

pub type MetaBuilder = fn(&MetaConfig) -> Result<Box<dyn Reconciler>, String>;

/// 実行順に並べた provider。
pub const META_PROVIDERS: &[(&str, MetaBuilder)] = &[
    ("appstore", appstore::build),
    ("googleplay", googleplay::build),
];

// --- テキストの比較 ---

/// 空でなければ入れる。末尾の空白・改行は落とす（YAML の | で付く分。Play は保存時に落とす）。
pub(crate) fn put_if(m: &mut Map<String, Value>, key: &str, val: &str) {
    let v = val.trim_end();
    if !v.is_empty() {
        m.insert(key.to_string(), Value::String(v.to_string()));
    }
}

/// 今の値と希望の値が同じか。文字列は末尾の空白を無視し、null と "" を同じとみなす。
/// bool は null を false とみなす。
pub(crate) fn same_value(cur: Option<&Value>, want: &Value) -> bool {
    match want {
        Value::String(w) => cur.and_then(Value::as_str).unwrap_or("").trim_end() == w.trim_end(),
        Value::Bool(b) => cur.and_then(Value::as_bool).unwrap_or(false) == *b,
        other => cur == Some(other),
    }
}

/// 希望の値のうち、今と違うものだけ（Go 版 `changedAttrs`）。
pub(crate) fn changed_attrs(current: &Value, desired: &Map<String, Value>) -> Map<String, Value> {
    desired
        .iter()
        .filter(|(k, v)| !same_value(current.get(k.as_str()), v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// 違う項目の名前（detail 用）。
pub(crate) fn keys(m: &Map<String, Value>) -> String {
    let mut k: Vec<&str> = m.keys().map(String::as_str).collect();
    k.sort();
    k.join(", ")
}

/// App Store が「編集できない」と返した属性の名前（`Attribute 'whatsNew' cannot be edited`）。
pub(crate) fn non_editable_attrs(body: &[u8]) -> Vec<String> {
    let s = String::from_utf8_lossy(body);
    let mut out = Vec::new();
    let mut rest = s.as_ref();
    while let Some(i) = rest.find("Attribute '") {
        rest = &rest[i + "Attribute '".len()..];
        let Some(j) = rest.find('\'') else { break };
        let name = &rest[..j];
        if rest[j..].starts_with("' cannot be edited") && !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
        rest = &rest[j..];
    }
    out
}

// --- 画像のファイル ---

/// 手元の画像 1 枚（名前・中身・両ストアのハッシュ）。App Store は MD5（sourceFileChecksum）、
/// Google Play は SHA-256 で、上げ済みのものと突き合わせる。
pub(crate) struct ShotFile {
    pub name: String,
    pub data: Vec<u8>,
    pub md5: String,
    pub sha256: String,
}

pub(crate) fn md5_hex(data: &[u8]) -> String {
    use md5::{Digest, Md5};
    hex::encode(Md5::digest(data))
}

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(data))
}

/// dir の画像（png / jpg）をファイル名の順に読む（NN- の接頭辞がストアでの並びになる）。
/// ディレクトリが無ければ空（呼び出し側が skip と報告する）。
pub(crate) fn list_shots(dir: &Path) -> Result<Vec<ShotFile>, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read {}: {e}", dir.display())),
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|t| !t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            matches!(
                releasectl_core::meta::ext_lower(n).as_str(),
                "png" | "jpg" | "jpeg"
            )
        })
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let data = std::fs::read(dir.join(&name))
                .map_err(|e| format!("read {}: {e}", dir.join(&name).display()))?;
            Ok(ShotFile {
                md5: md5_hex(&data),
                sha256: sha256_hex(&data),
                name,
                data,
            })
        })
        .collect()
}

/// set の言語のディレクトリ。ロケールそのままの名前（en-GB）があればそれ、無ければ地域を落とした名前（en）。
pub(crate) fn shots_dir(
    shots: &releasectl_core::meta::Screenshots,
    set: &releasectl_core::meta::ScreenshotSet,
    locale: &str,
) -> std::path::PathBuf {
    let full = shots.full_locale_dir(set, locale);
    if full.is_dir() {
        full
    } else {
        shots.locale_dir(set, locale)
    }
}

/// 中身から決める拡張子（png / jpg）。どちらでもなければ None。
pub(crate) fn image_ext(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpg")
    } else {
        None
    }
}

/// `meta init` の画像 1 枚を `dir/NN.<ext>` に書く（NN は 1 始まりの並び順）。書いたファイル名を返す。
pub(crate) fn write_shot(dir: &Path, index: usize, data: &[u8]) -> Result<String, String> {
    let ext = image_ext(data).ok_or("downloaded file is not a png / jpg")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let name = format!("{:02}.{ext}", index + 1);
    let path = dir.join(&name);
    std::fs::write(&path, data).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(name)
}

pub(crate) fn content_type_for(name: &str) -> &'static str {
    match releasectl_core::meta::ext_lower(name).as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        _ => "image/png",
    }
}

/// 同じハッシュの集合か（順不同・大文字小文字を無視）。
pub(crate) fn same_digest_set(local: &[String], existing: &[String]) -> bool {
    if local.len() != existing.len() {
        return false;
    }
    let mut a: Vec<String> = local.iter().map(|s| s.to_lowercase()).collect();
    let mut b: Vec<String> = existing.iter().map(|s| s.to_lowercase()).collect();
    a.sort();
    b.sort();
    a == b
}

/// 同じ順に同じハッシュか（App Store は set の中の順に出す）。
pub(crate) fn same_digest_order(local: &[String], existing: &[String]) -> bool {
    local.len() == existing.len()
        && local
            .iter()
            .zip(existing)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_compare_ignores_trailing_newline_and_null() {
        assert!(same_value(Some(&json!("a b")), &json!("a b\n")));
        assert!(same_value(None, &json!("")));
        assert!(same_value(Some(&Value::Null), &json!("")));
        assert!(!same_value(Some(&json!("a")), &json!("b")));
        assert!(same_value(Some(&Value::Null), &json!(false)));
        assert!(!same_value(Some(&json!(true)), &json!(false)));
    }

    #[test]
    fn changed_attrs_and_put_if() {
        let mut want = Map::new();
        put_if(&mut want, "description", "Hello.\n");
        put_if(&mut want, "keywords", "");
        assert_eq!(want.get("description"), Some(&json!("Hello.")));
        assert!(!want.contains_key("keywords"));
        let cur = json!({"description": "Hello.", "whatsNew": "x"});
        assert!(changed_attrs(&cur, &want).is_empty());
        put_if(&mut want, "whatsNew", "y");
        assert_eq!(keys(&changed_attrs(&cur, &want)), "whatsNew");
    }

    #[test]
    fn parses_non_editable_errors() {
        let body = br#"{"errors":[{"detail":"Attribute 'whatsNew' cannot be edited at this time"},{"detail":"Attribute 'whatsNew' cannot be edited"},{"detail":"Attribute 'x' is invalid"}]}"#;
        assert_eq!(non_editable_attrs(body), vec!["whatsNew".to_string()]);
    }

    #[test]
    fn digest_sets() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(same_digest_set(&s(&["a", "B"]), &s(&["b", "A"])));
        assert!(!same_digest_set(&s(&["a"]), &s(&["a", "a"])));
        assert!(same_digest_set(&s(&[]), &s(&[])));
        assert!(same_digest_order(&s(&["a", "B"]), &s(&["A", "b"])));
        assert!(!same_digest_order(&s(&["a", "b"]), &s(&["b", "a"])));
        assert_eq!(content_type_for("x.JPG"), "image/jpeg");
        assert_eq!(content_type_for("x.png"), "image/png");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(image_ext(b"\x89PNG\r\n\x1a\n...."), Some("png"));
        assert_eq!(image_ext(&[0xff, 0xd8, 0xff, 0xe0]), Some("jpg"));
        assert_eq!(image_ext(b"<html>"), None);
    }
}
