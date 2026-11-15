//! Google Play の掲載画像（Go 版 `googleplay_images.go`）。画像タイプ × 言語ごとに、手元のファイルの
//! SHA-256 と listing の今の画像を比べ、違えばその画像タイプを全部消してファイル名の順に上げ直す
//! （Play は画像タイプごとに丸ごと置き換わるので、冪等で並びも保てる）。

use super::googleplay::MetaPlay;
use super::{ShotFile, content_type_for, list_shots, same_digest_set};
use releasectl_core::meta::play_lang;
use releasectl_core::{Report, Status};
use serde_json::Value;

/// メディアアップロードのホスト（edit の JSON API とは別）。
const PLAY_UPLOAD_BASE: &str = "https://androidpublisher.googleapis.com/upload/androidpublisher/v3";

impl MetaPlay {
    /// 画像を揃える。変わった（plan なら変わる）なら true。
    pub(super) fn reconcile_images(&self, apply: bool, edit_path: &str, r: &mut Report) -> bool {
        let Some(shots) = &self.cfg.screenshots else {
            return false;
        };
        let mut changed = false;
        for set in &shots.play {
            for loc in &self.cfg.locales {
                let lang = play_lang(&loc.locale);
                let target = format!("{}/{lang}", set.image_type);
                let dir = super::shots_dir(shots, set, &loc.locale);
                let files = match list_shots(&dir) {
                    Ok(f) => f,
                    Err(e) => {
                        r.add("image", &target, Status::Error, e);
                        continue;
                    }
                };
                if files.is_empty() {
                    r.add(
                        "image",
                        &target,
                        Status::Skip,
                        format!("no images in {}", dir.display()),
                    );
                    continue;
                }
                changed |= self.reconcile_image_type(
                    apply,
                    edit_path,
                    &lang,
                    &set.image_type,
                    &target,
                    &files,
                    r,
                );
            }
        }
        changed
    }

    #[allow(clippy::too_many_arguments)]
    fn reconcile_image_type(
        &self,
        apply: bool,
        edit_path: &str,
        lang: &str,
        image_type: &str,
        target: &str,
        files: &[ShotFile],
        r: &mut Report,
    ) -> bool {
        let base = format!("{edit_path}/listings/{lang}/{image_type}");
        let existing = match self.list_image_shas(&base) {
            Ok(e) => e,
            Err(e) => {
                r.add("image", target, Status::Error, e);
                return false;
            }
        };
        let local: Vec<String> = files.iter().map(|f| f.sha256.clone()).collect();
        let n = files.len();
        if same_digest_set(&local, &existing) {
            r.add("image", target, Status::Exists, n.to_string());
            return false;
        }
        let (pending, done, verb) = if existing.is_empty() {
            (Status::Create, Status::Created, "upload")
        } else {
            (Status::Drift, Status::Updated, "replace")
        };
        if !apply {
            r.add(
                "image",
                target,
                pending,
                format!("{verb} {n} (now {})", existing.len()),
            );
            return true;
        }
        // 全部消して（コレクションへの DELETE）、ファイル名の順に上げ直す。
        if !existing.is_empty() {
            match self.cli.r#do("DELETE", &base, None) {
                Err(e) => {
                    r.add("image", target, Status::Error, e);
                    return false;
                }
                Ok(resp) if !resp.ok() => {
                    r.add("image", target, Status::Error, resp.error());
                    return false;
                }
                Ok(_) => {}
            }
        }
        let url = format!("{PLAY_UPLOAD_BASE}{base}?uploadType=media");
        for f in files {
            match self
                .cli
                .do_raw("POST", &url, &f.data, content_type_for(&f.name), true, &[])
            {
                Err(e) => {
                    r.add("image", &format!("{target}:{}", f.name), Status::Error, e);
                    return true;
                }
                Ok(resp) if !resp.ok() => {
                    r.add(
                        "image",
                        &format!("{target}:{}", f.name),
                        Status::Error,
                        resp.error(),
                    );
                    return true;
                }
                Ok(_) => {}
            }
        }
        r.add("image", target, done, n.to_string());
        true
    }

    /// listing の画像タイプの今の画像の SHA-256。404（listing も画像もまだ無い）は空。
    pub(super) fn list_image_shas(&self, base: &str) -> Result<Vec<String>, String> {
        Ok(self
            .list_images(base)?
            .into_iter()
            .map(|(sha, _)| sha)
            .collect())
    }

    /// listing の画像タイプの今の画像の（SHA-256, URL）。404（listing も画像もまだ無い）は空。
    pub(super) fn list_images(&self, base: &str) -> Result<Vec<(String, String)>, String> {
        let resp = self.cli.r#do("GET", base, None)?;
        if resp.status == 404 {
            return Ok(Vec::new());
        }
        if !resp.ok() {
            return Err(format!("list images: {}", resp.error()));
        }
        let v: Value = resp.json()?;
        Ok(v.get("images")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|im| {
                        let url = im.get("url").and_then(Value::as_str).unwrap_or("");
                        Some((im.get("sha256")?.as_str()?.to_string(), url.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
}
