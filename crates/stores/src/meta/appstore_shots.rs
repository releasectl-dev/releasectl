//! App Store のスクリーンショット（Go 版 `appstore_shots.go`）。表示タイプ × ロケールごとに:
//!
//! 1. そのローカライズ + 表示タイプの appScreenshotSet を探す（無ければ作る）
//! 2. 手元のファイルの MD5 と set の中の sourceFileChecksum を比べる
//! 3. 違えば set の中を全部消して上げ直す（予約 → 予約された URL へ part ごとに PUT → uploaded:true + checksum）
//! 4. 並びをファイル名の順にする（同じ画像で並びだけ違うなら並べ替えだけ）

use super::appstore::MetaAppStore;
use super::{ShotFile, list_shots, same_digest_order, same_digest_set};
use crate::appstore::{attr, created_id, get_all};
use releasectl_core::{Report, Status};
use serde_json::{Value, json};

/// 予約が返すアップロードの part（スクリーンショットとアセットライブラリで同じ形）。
pub(super) struct UploadOp {
    method: String,
    url: String,
    offset: usize,
    length: usize,
    headers: Vec<(String, String)>,
}

pub(super) fn upload_ops(reservation: &Value) -> Vec<UploadOp> {
    reservation
        .pointer("/data/attributes/uploadOperations")
        .and_then(Value::as_array)
        .map(|ops| {
            ops.iter()
                .map(|op| UploadOp {
                    method: op
                        .get("method")
                        .and_then(Value::as_str)
                        .unwrap_or("PUT")
                        .to_string(),
                    url: op
                        .get("url")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    offset: op.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize,
                    length: op.get("length").and_then(Value::as_u64).unwrap_or(0) as usize,
                    headers: op
                        .get("requestHeaders")
                        .and_then(Value::as_array)
                        .map(|hs| {
                            hs.iter()
                                .filter_map(|h| {
                                    Some((
                                        h.get("name")?.as_str()?.to_string(),
                                        h.get("value")?.as_str()?.to_string(),
                                    ))
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// set の中のスクリーンショット 1 枚。
struct Shot {
    id: String,
    /// sourceFileChecksum（MD5）。処理中は空
    checksum: String,
    /// assetDeliveryState.state（AWAITING_UPLOAD / UPLOAD_COMPLETE / COMPLETE / FAILED）
    state: String,
}

impl Shot {
    /// まだ処理中で、チェックサムを比べられない。
    fn processing(&self) -> bool {
        self.checksum.is_empty()
            && matches!(self.state.as_str(), "AWAITING_UPLOAD" | "UPLOAD_COMPLETE")
    }
}

fn md5s(files: &[ShotFile]) -> Vec<String> {
    files.iter().map(|f| f.md5.clone()).collect()
}

impl MetaAppStore {
    pub(super) fn reconcile_screenshots(&self, apply: bool, version_id: &str, r: &mut Report) {
        let Some(shots) = &self.cfg.screenshots else {
            return;
        };
        if shots.appstore.is_empty() {
            return;
        }
        let locs = match self.version_localizations(version_id) {
            Ok(l) => l,
            Err(e) => {
                r.add("screenshots", version_id, Status::Error, e);
                return;
            }
        };
        for set in &shots.appstore {
            for loc in &self.cfg.locales {
                let target = format!("{}/{}", set.display_type, loc.locale);
                let Some((loc_id, _)) = locs.get(&loc.locale) else {
                    r.add(
                        "screenshot",
                        &target,
                        Status::Skip,
                        format!("no version localization for {}", loc.locale),
                    );
                    continue;
                };
                let dir = shots.locale_dir(set, &loc.locale);
                let files = match list_shots(&dir) {
                    Ok(f) => f,
                    Err(e) => {
                        r.add("screenshot", &target, Status::Error, e);
                        continue;
                    }
                };
                if files.is_empty() {
                    r.add(
                        "screenshot",
                        &target,
                        Status::Skip,
                        format!("no images in {}", dir.display()),
                    );
                    continue;
                }
                self.reconcile_screenshot_set(apply, loc_id, &set.display_type, &target, &files, r);
            }
        }
    }

    fn reconcile_screenshot_set(
        &self,
        apply: bool,
        loc_id: &str,
        display_type: &str,
        target: &str,
        files: &[ShotFile],
        r: &mut Report,
    ) {
        let (set_id, shots) = match self.find_screenshot_set(loc_id, display_type) {
            Ok(v) => v,
            Err(e) => {
                r.add("screenshot", target, Status::Error, e);
                return;
            }
        };
        // 上げた直後は App Store が処理中で、チェックサムが空で返る。比べると「違う」になって
        // 上げ直しを繰り返すので、処理中のものがあれば比べずに後回しにする（失敗したものは上げ直す）。
        let processing = shots.iter().filter(|s| s.processing()).count();
        if processing > 0 {
            r.add(
                "screenshot",
                target,
                Status::Skip,
                format!("{processing} still processing in App Store Connect; re-run to compare"),
            );
            return;
        }
        let existing: Vec<String> = shots.into_iter().map(|s| s.checksum).collect();
        let local = md5s(files);
        let n = files.len();
        if let Some(set_id) = &set_id
            && same_digest_set(&local, &existing)
        {
            if same_digest_order(&local, &existing) {
                r.add("screenshot", target, Status::Exists, n.to_string());
                return;
            }
            // 同じ画像で並びだけ違う: 上げ直さずに並べ替える
            if !apply {
                r.add("screenshot", target, Status::Drift, format!("reorder {n}"));
                return;
            }
            match self.reorder_screenshots(set_id, &local) {
                Ok(()) => r.add(
                    "screenshot",
                    target,
                    Status::Updated,
                    format!("reordered {n}"),
                ),
                Err(e) => r.add("screenshot", target, Status::Error, format!("reorder: {e}")),
            }
            return;
        }
        if !apply {
            match set_id {
                None => r.add("screenshot", target, Status::Create, format!("upload {n}")),
                Some(_) => r.add("screenshot", target, Status::Drift, format!("replace {n}")),
            }
            return;
        }
        let (set_id, done) = match set_id {
            None => match self.create_screenshot_set(loc_id, display_type) {
                Ok(id) => (id, Status::Created),
                Err(e) => {
                    r.add("screenshot", target, Status::Error, e);
                    return;
                }
            },
            Some(id) => {
                if let Err(e) = self.clear_screenshot_set(&id) {
                    r.add("screenshot", target, Status::Error, e);
                    return;
                }
                (id, Status::Updated)
            }
        };
        let mut ids = Vec::with_capacity(n);
        for f in files {
            match self.upload_screenshot(&set_id, f) {
                Ok(id) => ids.push(id),
                Err(e) => {
                    r.add(
                        "screenshot",
                        &format!("{target}:{}", f.name),
                        Status::Error,
                        e,
                    );
                    return;
                }
            }
        }
        match self.order_screenshots(&set_id, &ids) {
            Ok(()) => r.add("screenshot", target, done, n.to_string()),
            // 並びは見た目だけ。画像は上がっているので成功として補足を付ける
            Err(e) => r.add(
                "screenshot",
                target,
                done,
                format!("{n} (order skipped: {e})"),
            ),
        }
    }

    /// 表示タイプの set の id（無ければ None）と、その中のスクリーンショット（並び順）。
    fn find_screenshot_set(
        &self,
        loc_id: &str,
        display_type: &str,
    ) -> Result<(Option<String>, Vec<Shot>), String> {
        let sets = get_all(
            &self.cli,
            &format!("/v1/appStoreVersionLocalizations/{loc_id}/appScreenshotSets?limit=50"),
            "list screenshot sets",
        )?;
        let Some(set) = sets
            .iter()
            .find(|s| attr(s, "screenshotDisplayType") == display_type)
        else {
            return Ok((None, Vec::new()));
        };
        Ok((Some(set.id.clone()), self.list_screenshots(&set.id)?))
    }

    /// set の中のスクリーンショット。
    fn list_screenshots(&self, set_id: &str) -> Result<Vec<Shot>, String> {
        Ok(get_all(
            &self.cli,
            &format!("/v1/appScreenshotSets/{set_id}/appScreenshots?limit=50"),
            "list screenshots",
        )?
        .into_iter()
        .map(|s| Shot {
            checksum: attr(&s, "sourceFileChecksum").to_string(),
            state: s
                .attributes
                .pointer("/assetDeliveryState/state")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            id: s.id,
        })
        .collect())
    }

    fn create_screenshot_set(&self, loc_id: &str, display_type: &str) -> Result<String, String> {
        let body = json!({"data": {
            "type": "appScreenshotSets",
            "attributes": {"screenshotDisplayType": display_type},
            "relationships": {"appStoreVersionLocalization": {"data": {"type": "appStoreVersionLocalizations", "id": loc_id}}},
        }});
        let resp = self
            .cli
            .r#do("POST", "/v1/appScreenshotSets", Some(&body))?;
        if !resp.ok() {
            return Err(format!("create screenshot set: {}", resp.error()));
        }
        created_id(&resp).ok_or_else(|| "create screenshot set: empty id".to_string())
    }

    /// set の中を全部消す（並びを決め直し、古い画像を残さないため）。
    fn clear_screenshot_set(&self, set_id: &str) -> Result<(), String> {
        for Shot { id, .. } in self.list_screenshots(set_id)? {
            let resp = self
                .cli
                .r#do("DELETE", &format!("/v1/appScreenshots/{id}"), None)?;
            if !resp.ok() && resp.status != 404 {
                return Err(format!("delete screenshot {id}: {}", resp.error()));
            }
        }
        Ok(())
    }

    /// 予約 → 予約された URL へ part ごとに送る → uploaded:true + MD5 で確定。新しい id を返す。
    fn upload_screenshot(&self, set_id: &str, f: &ShotFile) -> Result<String, String> {
        let body = json!({"data": {
            "type": "appScreenshots",
            "attributes": {"fileSize": f.data.len(), "fileName": f.name},
            "relationships": {"appScreenshotSet": {"data": {"type": "appScreenshotSets", "id": set_id}}},
        }});
        let resp = self.cli.r#do("POST", "/v1/appScreenshots", Some(&body))?;
        if !resp.ok() {
            return Err(format!("reserve: {}", resp.error()));
        }
        let rsv: Value = resp.json()?;
        let id = rsv
            .pointer("/data/id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or("reserve: empty id")?
            .to_string();
        self.put_parts(&upload_ops(&rsv), &f.data)?;
        let patch = json!({"data": {
            "type": "appScreenshots",
            "id": id,
            "attributes": {"uploaded": true, "sourceFileChecksum": f.md5},
        }});
        let resp = self
            .cli
            .r#do("PATCH", &format!("/v1/appScreenshots/{id}"), Some(&patch))?;
        if !resp.ok() {
            return Err(format!("commit: {}", resp.error()));
        }
        Ok(id)
    }

    /// 予約された part を送る。予約 URL は自分のヘッダー（Content-Type を含む）を持ち、
    /// ASC の Authorization を付けてはいけない。
    pub(super) fn put_parts(&self, ops: &[UploadOp], data: &[u8]) -> Result<(), String> {
        for op in ops {
            let end = op.offset + op.length;
            if end > data.len() {
                return Err(format!(
                    "upload op out of range ({}:{end} of {})",
                    op.offset,
                    data.len()
                ));
            }
            let resp = self.cli.do_raw(
                &op.method,
                &op.url,
                &data[op.offset..end],
                "",
                false,
                &op.headers,
            )?;
            if !resp.ok() {
                return Err(format!("upload part: {}", resp.error()));
            }
        }
        Ok(())
    }

    /// set の並びを ids の順にする。
    fn order_screenshots(&self, set_id: &str, ids: &[String]) -> Result<(), String> {
        let data: Vec<Value> = ids
            .iter()
            .map(|id| json!({"type": "appScreenshots", "id": id}))
            .collect();
        let resp = self.cli.r#do(
            "PATCH",
            &format!("/v1/appScreenshotSets/{set_id}/relationships/appScreenshots"),
            Some(&json!({"data": data})),
        )?;
        if !resp.ok() {
            return Err(resp.error());
        }
        Ok(())
    }

    /// set の中の画像を手元の MD5 の順に並べ替える（set は同じ画像の組を持っていること）。
    fn reorder_screenshots(&self, set_id: &str, local: &[String]) -> Result<(), String> {
        let mut by_md5: Vec<(String, String)> = self
            .list_screenshots(set_id)?
            .into_iter()
            .map(|s| (s.checksum.to_lowercase(), s.id))
            .collect();
        let mut ids = Vec::with_capacity(local.len());
        for m in local {
            let k = m.to_lowercase();
            let i = by_md5
                .iter()
                .position(|(sum, _)| *sum == k)
                .ok_or_else(|| format!("screenshot {m} not in set"))?;
            ids.push(by_md5.remove(i).1);
        }
        self.order_screenshots(set_id, &ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processing_shots_are_not_compared() {
        let shot = |checksum: &str, state: &str| Shot {
            id: "1".into(),
            checksum: checksum.into(),
            state: state.into(),
        };
        assert!(shot("", "UPLOAD_COMPLETE").processing());
        assert!(shot("", "AWAITING_UPLOAD").processing());
        assert!(!shot("abc", "COMPLETE").processing());
        // 失敗したものは比べて（違うので）上げ直す
        assert!(!shot("", "FAILED").processing());
    }

    #[test]
    fn parses_upload_operations() {
        let rsv = json!({"data": {"id": "s1", "attributes": {"uploadOperations": [
            {"method": "PUT", "url": "https://u/1", "offset": 0, "length": 10,
             "requestHeaders": [{"name": "Content-Type", "value": "image/png"}]},
            {"url": "https://u/2", "offset": 10, "length": 5}
        ]}}});
        let ops = upload_ops(&rsv);
        assert_eq!(ops.len(), 2);
        assert_eq!(
            ops[0].headers,
            vec![("Content-Type".to_string(), "image/png".to_string())]
        );
        assert_eq!(ops[1].method, "PUT");
        assert_eq!((ops[1].offset, ops[1].length), (10, 5));
    }
}
