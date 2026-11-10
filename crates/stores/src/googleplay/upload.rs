//! .aab を Google Play のテストトラックへ上げる（Go 版 `cmd/playupload`）。
//!
//! Android Publisher v3 の edit のトランザクション: edit を作る → .aab を上げる →
//! トラックをその versionCode に向ける → commit。plan（`apply = false`）は上げて検証
//! （validate）までして edit を捨てるので、公開されるものは何も変わらない。

use crate::http::Client;
use serde_json::{Value, json};

const UPLOAD_BASE: &str = "https://androidpublisher.googleapis.com/upload/androidpublisher/v3";

pub struct UploadArgs<'a> {
    pub package: &'a str,
    pub aab: &'a [u8],
    pub aab_label: &'a str,
    /// internal / alpha / beta
    pub track: &'a str,
    /// completed / draft（公開前のアプリは draft しか受け付けない）
    pub status: &'a str,
}

/// 進み具合は `log` に 1 行ずつ渡す（CLI が表示する）。
pub fn upload(args: &UploadArgs, apply: bool, log: &mut dyn FnMut(String)) -> Result<(), String> {
    // .aab は do_raw（アップロード用の長いタイムアウト）で送る。
    let cli = super::new_client().map_err(|e| format!("google play auth: {e}"))?;
    let pkg = args.package;
    let edit_id = insert_edit(&cli, pkg)?;
    let edit_path = format!("/applications/{pkg}/edits/{edit_id}");

    let res = (|| {
        log(format!(
            "uploading {} ({:.1} MB) to {pkg} …",
            args.aab_label,
            args.aab.len() as f64 / 1e6
        ));
        let resp = cli.do_raw(
            "POST",
            &format!("{UPLOAD_BASE}{edit_path}/bundles?uploadType=media"),
            args.aab,
            "application/octet-stream",
            true,
            &[],
        )?;
        if !resp.ok() {
            return Err(format!("upload bundle: {}", resp.error()));
        }
        let v: Value = resp.json()?;
        let version_code = v.get("versionCode").and_then(Value::as_i64).unwrap_or(0);
        log(format!("  versionCode {version_code}"));

        let track = args.track;
        let body = json!({
            "track": track,
            "releases": [{"versionCodes": [version_code.to_string()], "status": args.status}],
        });
        let resp = cli.r#do("PUT", &format!("{edit_path}/tracks/{track}"), Some(&body))?;
        if !resp.ok() {
            return Err(format!("update track {track}: {}", resp.error()));
        }

        if !apply {
            let resp = cli.r#do("POST", &format!("{edit_path}:validate"), None)?;
            if !resp.ok() {
                return Err(format!("validate: {}", resp.error()));
            }
            log(format!(
                "PLAN ok — {track} track would get versionCode {version_code} ({}). edit discarded.",
                args.status
            ));
            return Ok(false);
        }
        let resp = cli.r#do("POST", &format!("{edit_path}:commit"), None)?;
        if !resp.ok() {
            return Err(format!("commit: {}", resp.error()));
        }
        log(format!(
            "APPLIED — {track} track now serves versionCode {version_code} ({}).",
            args.status
        ));
        Ok(true)
    })();

    // commit しなかった edit は何も公開しない。いつでも片付ける。
    if !matches!(res, Ok(true)) {
        let _ = cli.r#do("DELETE", &edit_path, None);
    }
    res.map(|_| ())
}

fn insert_edit(cli: &Client, pkg: &str) -> Result<String, String> {
    let resp = cli.r#do(
        "POST",
        &format!("/applications/{pkg}/edits"),
        Some(&json!({})),
    )?;
    if !resp.ok() {
        return Err(format!("insert edit: {}", resp.error()));
    }
    let v: Value = resp.json()?;
    match v.get("id").and_then(Value::as_str) {
        Some(id) if !id.is_empty() => Ok(id.to_string()),
        _ => Err("insert edit: empty edit id".into()),
    }
}
