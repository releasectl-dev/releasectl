//! Google Play の掲載情報の今の値（`releasectl meta export`）。edit を開いて読み、捨てる（何も公開しない）。

use super::googleplay::MetaPlay;
use super::{sha256_hex, write_shot};
use crate::ExportOpts;
use releasectl_core::meta::asc_locale;
use releasectl_core::{Report, Status};
use serde_json::{Map, Value, json};

/// 出す画像の種類（Play の imageType）。
const IMAGE_TYPES: [&str; 8] = [
    "icon",
    "featureGraphic",
    "phoneScreenshots",
    "sevenInchScreenshots",
    "tenInchScreenshots",
    "tvBanner",
    "tvScreenshots",
    "wearScreenshots",
];

const LISTING_FIELDS: [&str; 3] = ["title", "shortDescription", "fullDescription"];

/// Play の画像の URL から元の大きさのものを取る。
fn download_original(url: &str) -> Result<Vec<u8>, String> {
    if url.is_empty() {
        return Err("no image url".into());
    }
    // Play の画像の URL は、そのままだと長辺 512px に縮めたものを返す。`=s0` で上げたときのファイルになる。
    if url
        .rsplit('/')
        .next()
        .is_some_and(|last| last.contains('='))
    {
        crate::http::download(url)
    } else {
        crate::http::download(&format!("{url}=s0"))
    }
}

fn pick(v: &Value, names: &[&str]) -> Map<String, Value> {
    names
        .iter()
        .filter_map(|n| Some((n.to_string(), json!(v.get(*n)?.as_str()?))))
        .collect()
}

impl MetaPlay {
    pub(super) fn export_store(&self, opts: &ExportOpts, r: &mut Report) -> Result<Value, String> {
        r.set_key_singleton(&["app", "listing", "details"]);
        let pkg = self.cfg.package_name.as_str();
        if pkg.is_empty() {
            r.add(
                "app",
                &self.cfg.app_name,
                Status::Skip,
                "packageName not set",
            );
            return Ok(Value::Null);
        }
        r.store_id("packageName", pkg);
        let edit = self.insert_edit(pkg)?;
        let edit_path = format!("/applications/{pkg}/edits/{edit}");
        let out = self.export_in_edit(pkg, &edit_path, opts, r);
        // 読むだけなので必ず捨てる。
        let _ = self.cli.r#do("DELETE", &edit_path, None);
        Ok(out)
    }

    fn export_in_edit(
        &self,
        pkg: &str,
        edit_path: &str,
        opts: &ExportOpts,
        r: &mut Report,
    ) -> Value {
        let mut out = Map::new();
        out.insert("packageName".into(), json!(pkg));

        let mut listings = Map::new();
        let mut images = Map::new();
        let mut image_files = Map::new();
        match self.cli.r#do("GET", &format!("{edit_path}/listings"), None) {
            Err(e) => r.add("listing", pkg, Status::Error, e),
            Ok(resp) if !resp.ok() => r.add(
                "listing",
                pkg,
                Status::Error,
                format!("list listings: {}", resp.error()),
            ),
            Ok(resp) => {
                let v: Value = resp.json().unwrap_or(Value::Null);
                let mut list: Vec<&Value> = v
                    .get("listings")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().collect())
                    .unwrap_or_default();
                list.sort_by_key(|l| l.get("language").and_then(Value::as_str).unwrap_or(""));
                for l in list {
                    let Some(lang) = l.get("language").and_then(Value::as_str) else {
                        continue;
                    };
                    listings.insert(lang.into(), Value::Object(pick(l, &LISTING_FIELDS)));
                    let mut by_type = Map::new();
                    let mut files_by_type = Map::new();
                    for ty in IMAGE_TYPES {
                        let target = format!("{ty}/{lang}");
                        let list =
                            match self.list_images(&format!("{edit_path}/listings/{lang}/{ty}")) {
                                Ok(l) if !l.is_empty() => l,
                                Ok(_) => continue,
                                Err(e) => {
                                    r.add("image", &target, Status::Error, e);
                                    continue;
                                }
                            };
                        // `<images>/play/<種類>/<metadata.yaml のロケール>/NN.png`
                        if let Some(base) = &opts.images {
                            let rel = format!("play/{ty}");
                            let dir = base.join(&rel).join(asc_locale(lang));
                            let mut names = Vec::new();
                            let mut same = true;
                            for (i, (sha, url)) in list.iter().enumerate() {
                                match download_original(url).and_then(|d| {
                                    same &= sha256_hex(&d).eq_ignore_ascii_case(sha);
                                    write_shot(&dir, i, &d)
                                }) {
                                    Ok(name) => names.push(name),
                                    Err(e) => r.add("image", &target, Status::Error, e),
                                }
                            }
                            files_by_type.insert(
                                ty.into(),
                                json!({"dir": rel, "files": names, "sameAsStore": same}),
                            );
                        }
                        let shas: Vec<&String> = list.iter().map(|(sha, _)| sha).collect();
                        by_type.insert(ty.into(), json!(shas));
                    }
                    if !by_type.is_empty() {
                        images.insert(lang.into(), Value::Object(by_type));
                    }
                    if !files_by_type.is_empty() {
                        image_files.insert(lang.into(), Value::Object(files_by_type));
                    }
                }
            }
        }
        out.insert("listings".into(), Value::Object(listings));
        out.insert("images".into(), Value::Object(images));
        if opts.images.is_some() {
            out.insert("imageFiles".into(), Value::Object(image_files));
        }

        match self.cli.r#do("GET", &format!("{edit_path}/details"), None) {
            Err(e) => r.add("details", pkg, Status::Error, e),
            Ok(resp) if !resp.ok() => r.add(
                "details",
                pkg,
                Status::Error,
                format!("get details: {}", resp.error()),
            ),
            Ok(resp) => {
                // metadata.yaml の playContact と defaultLanguage の名前で出す
                let v: Value = resp.json().unwrap_or(Value::Null);
                let mut contact = Map::new();
                for (api, yaml) in [
                    ("contactEmail", "email"),
                    ("contactPhone", "phone"),
                    ("contactWebsite", "website"),
                ] {
                    if let Some(s) = v.get(api).and_then(Value::as_str) {
                        contact.insert(yaml.into(), json!(s));
                    }
                }
                out.insert("playContact".into(), Value::Object(contact));
                if let Some(s) = v.get("defaultLanguage").and_then(Value::as_str) {
                    out.insert("defaultLanguage".into(), json!(s));
                }
            }
        }
        Value::Object(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_string_fields() {
        let v = json!({"language": "ja-JP", "title": "t", "video": "", "fullDescription": null});
        assert_eq!(
            Value::Object(pick(&v, &LISTING_FIELDS)),
            json!({"title": "t"})
        );
    }
}
