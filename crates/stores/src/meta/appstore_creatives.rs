//! App Store のクリエイティブ（製品ページのヘッダー・検索結果。iOS 27〜。Go 版 `appstore_creatives.go`）。
//! 配置タイプ × ロケールごとに:
//!
//! 1. ローカライズのその配置と、置かれているアセットの referenceName を読む
//! 2. referenceName に手元のファイルの MD5 を入れてあるので、一致すれば同じファイル（触らない）
//! 3. 違えばアプリのアセットライブラリに上げ（予約 → part を送る → uploaded:true → 処理を待つ）、
//!    古い配置を外して新しいアセットを置く
//!
//! 置き換えられたアセットはライブラリに残る（配置されないだけ）。

use super::appstore::MetaAppStore;
use super::appstore_shots::upload_ops;
use super::md5_hex;
use crate::appstore::{attr, get_all};
use releasectl_core::meta::{Creative, base_lang, creative_media};
use releasectl_core::{Report, Status};
use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, Instant};

/// ヘッダー・検索結果の配置が受け付ける唯一の配置グループ（2026-10）。
const CREATIVE_GROUP: &str = "DEFAULT_PROFILE";

/// referenceName: 「言語 ファイル名 MD5 の先頭 12 桁」。
fn reference_name(lang: &str, file: &str, data: &[u8]) -> String {
    format!("{lang} {file} {}", &md5_hex(data)[..12])
}

impl MetaAppStore {
    pub(super) fn reconcile_creatives(
        &self,
        apply: bool,
        app_id: &str,
        version_id: &str,
        r: &mut Report,
    ) {
        let Some(shots) = &self.cfg.screenshots else {
            return;
        };
        if shots.creatives.is_empty() {
            return;
        }
        let locs = match self.version_localizations(version_id) {
            Ok(l) => l,
            Err(e) => {
                r.add("creative", version_id, Status::Error, e);
                return;
            }
        };
        for cr in &shots.creatives {
            for loc in &self.cfg.locales {
                let target = format!("{}/{}", cr.placement_type, loc.locale);
                let Some((loc_id, _)) = locs.get(&loc.locale) else {
                    r.add(
                        "creative",
                        &target,
                        Status::Skip,
                        format!("no version localization for {}", loc.locale),
                    );
                    continue;
                };
                let lang = base_lang(&loc.locale);
                let path = Path::new(&shots.base_dir)
                    .join(&cr.dir)
                    .join(lang)
                    .join(&cr.file);
                let data = match std::fs::read(&path) {
                    Ok(d) => d,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        r.add(
                            "creative",
                            &target,
                            Status::Skip,
                            format!("no file {}", path.display()),
                        );
                        continue;
                    }
                    Err(e) => {
                        r.add(
                            "creative",
                            &target,
                            Status::Error,
                            format!("read {}: {e}", path.display()),
                        );
                        continue;
                    }
                };
                let rf = reference_name(lang, &cr.file, &data);
                self.reconcile_creative(apply, app_id, loc_id, cr, &rf, &data, &target, r);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn reconcile_creative(
        &self,
        apply: bool,
        app_id: &str,
        loc_id: &str,
        cr: &Creative,
        rf: &str,
        data: &[u8],
        target: &str,
        r: &mut Report,
    ) {
        let (placement, placed_ref) = match self.find_placement(loc_id, &cr.placement_type) {
            Ok(v) => v,
            Err(e) => {
                r.add("creative", target, Status::Error, e);
                return;
            }
        };
        if placement.is_some() && placed_ref == rf {
            r.add("creative", target, Status::Exists, &cr.file);
            return;
        }
        let (pending, done, verb) = if placement.is_some() {
            (Status::Drift, Status::Updated, "replace")
        } else {
            (Status::Create, Status::Created, "place")
        };
        if !apply {
            r.add("creative", target, pending, format!("{verb} {}", cr.file));
            return;
        }
        let ty = creative_media(&cr.file).expect("validated in config");
        let asset = match self.upload_library_asset(app_id, ty, &cr.file, rf, data) {
            Ok(id) => id,
            Err(e) => {
                r.add("creative", target, Status::Error, e);
                return;
            }
        };
        if let Some(pid) = &placement {
            match self.cli.r#do(
                "DELETE",
                &format!("/v1/appAssetLibraryPlacements/{pid}"),
                None,
            ) {
                Err(e) => {
                    r.add("creative", target, Status::Error, e);
                    return;
                }
                Ok(resp) if !resp.ok() && resp.status != 404 => {
                    r.add(
                        "creative",
                        target,
                        Status::Error,
                        format!("remove old placement: {}", resp.error()),
                    );
                    return;
                }
                Ok(_) => {}
            }
        }
        let rel = if ty == "appAssetLibraryVideos" {
            "video"
        } else {
            "image"
        };
        let body = json!({"data": {
            "type": "appAssetLibraryPlacements",
            "attributes": {"placementType": cr.placement_type, "placementGroup": CREATIVE_GROUP},
            "relationships": {
                rel: {"data": {"type": ty, "id": asset}},
                "appStoreVersionLocalization": {"data": {"type": "appStoreVersionLocalizations", "id": loc_id}},
            },
        }});
        match self
            .cli
            .r#do("POST", "/v1/appAssetLibraryPlacements", Some(&body))
        {
            Err(e) => r.add("creative", target, Status::Error, e),
            Ok(resp) if !resp.ok() => r.add(
                "creative",
                target,
                Status::Error,
                format!("place: {}", resp.error()),
            ),
            Ok(_) => r.add("creative", target, done, &cr.file),
        }
    }

    /// ローカライズのその配置（無ければ None）と、置かれている画像 / 動画の referenceName。
    fn find_placement(
        &self,
        loc_id: &str,
        placement_type: &str,
    ) -> Result<(Option<String>, String), String> {
        let path = format!(
            "/v1/appStoreVersionLocalizations/{loc_id}/placements?filter[placementType]={placement_type}\
             &include=image,video&fields[appAssetLibraryImages]=referenceName&fields[appAssetLibraryVideos]=referenceName&limit=50"
        );
        let resp = self.cli.r#do("GET", &path, None)?;
        if !resp.ok() {
            return Err(format!("list placements: {}", resp.error()));
        }
        let v: Value = resp.json()?;
        let Some(p) = v
            .get("data")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
        else {
            return Ok((None, String::new()));
        };
        let pid = p
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let asset = [
            "/relationships/image/data/id",
            "/relationships/video/data/id",
        ]
        .iter()
        .find_map(|ptr| p.pointer(ptr).and_then(Value::as_str))
        .unwrap_or("");
        let rf = v
            .get("included")
            .and_then(Value::as_array)
            .and_then(|inc| {
                inc.iter()
                    .find(|r| r.get("id").and_then(Value::as_str) == Some(asset))
            })
            .and_then(|r| {
                r.pointer("/attributes/referenceName")
                    .and_then(Value::as_str)
            })
            .unwrap_or("")
            .to_string();
        Ok((Some(pid), rf))
    }

    /// アセットライブラリ（id はアプリの id）に画像 / 動画を予約して送り、確定して、処理が終わるのを待つ。
    /// 同じ referenceName（同じファイル）が既にあればそれを使う（前の run が処理待ちでタイムアウトすると、
    /// 上がったまま配置されないアセットが残り、予約し直すと 409 DUPLICATE になるため）。
    fn upload_library_asset(
        &self,
        app_id: &str,
        ty: &str,
        file: &str,
        rf: &str,
        data: &[u8],
    ) -> Result<String, String> {
        if let Some(id) = self.find_library_asset(app_id, ty, rf)? {
            self.wait_processed(ty, &id)?;
            return Ok(id);
        }
        let body = json!({"data": {
            "type": ty,
            "attributes": {"category": "CREATIVE_ASSETS", "fileName": file, "fileSize": data.len(), "referenceName": rf},
            "relationships": {"assetLibrary": {"data": {"type": "appAssetLibraries", "id": app_id}}},
        }});
        let resp = self.cli.r#do("POST", &format!("/v1/{ty}"), Some(&body))?;
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
        self.put_parts(&upload_ops(&rsv), data)?;
        let patch = json!({"data": {"type": ty, "id": id, "attributes": {"uploaded": true}}});
        let resp = self
            .cli
            .r#do("PATCH", &format!("/v1/{ty}/{id}"), Some(&patch))?;
        if !resp.ok() {
            return Err(format!("commit: {}", resp.error()));
        }
        self.wait_processed(ty, &id)?;
        Ok(id)
    }

    fn find_library_asset(
        &self,
        app_id: &str,
        ty: &str,
        rf: &str,
    ) -> Result<Option<String>, String> {
        let rel = if ty == "appAssetLibraryVideos" {
            "videos"
        } else {
            "images"
        };
        Ok(get_all(
            &self.cli,
            &format!("/v1/appAssetLibraries/{app_id}/{rel}?fields[{ty}]=referenceName&limit=200"),
            "list assets",
        )?
        .into_iter()
        .find(|a| attr(a, "referenceName") == rf)
        .map(|a| a.id))
    }

    /// 上げたアセットがアップロードの状態を抜けるまで待つ（動画は変換に時間がかかる）。FAILED は理由を返す。
    fn wait_processed(&self, ty: &str, id: &str) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(10 * 60);
        loop {
            let resp = self.cli.r#do(
                "GET",
                &format!("/v1/{ty}/{id}?fields[{ty}]=state,stateDetails"),
                None,
            )?;
            if !resp.ok() {
                return Err(format!("asset state: {}", resp.error()));
            }
            let v: Value = resp.json()?;
            match v
                .pointer("/data/attributes/state")
                .and_then(Value::as_str)
                .unwrap_or("")
            {
                "AWAITING_UPLOAD" | "UPLOAD_COMPLETE" => {}
                "FAILED" => {
                    let d = v
                        .pointer("/data/attributes/stateDetails")
                        .cloned()
                        .unwrap_or(Value::Null);
                    return Err(format!("asset processing failed: {d}"));
                }
                _ => return Ok(()),
            }
            if Instant::now() > deadline {
                return Err(format!("asset {id} still processing after 10 min"));
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_name_has_lang_file_and_md5() {
        assert_eq!(
            reference_name("ja", "header.mp4", b"abc"),
            "ja header.mp4 900150983cd2"
        );
    }
}
