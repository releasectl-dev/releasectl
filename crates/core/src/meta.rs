//! `metadata.yaml`（ストアの掲載情報の desired-state）の読み込みと検証
//! （Go 版 `internal/storemeta/config.go`）。docs: 02_cli-architecture/06-meta.md。

use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::config::ConfigError;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MetaConfig {
    #[serde(default)]
    pub app_name: String,
    /// App Store の bundle id
    #[serde(default)]
    pub bundle_id: String,
    /// Google Play の package name
    #[serde(default)]
    pub package_name: String,
    /// 編集する App Store の版（"1.0.0"）。空なら編集できる状態の版
    #[serde(default)]
    pub app_store_version: String,
    /// App Store の審査情報（連絡先・デモ・メモ）
    #[serde(default)]
    pub review: Option<Review>,
    /// App Store の年齢区分のプリセット
    #[serde(default)]
    pub age_rating: Option<AgeRating>,
    /// App Store のカテゴリ・コンテンツの権利・著作権表示・価格
    #[serde(default, rename = "appstore")]
    pub app_store_app: Option<AppStoreApp>,
    /// Google Play の掲載の連絡先
    #[serde(default)]
    pub play_contact: Option<PlayContact>,
    /// Google Play の既定の言語（"ja-JP"）
    #[serde(default)]
    pub default_language: String,
    #[serde(default)]
    pub locales: Vec<Locale>,
    /// 掲載画像（任意）
    #[serde(default)]
    pub screenshots: Option<Screenshots>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Review {
    #[serde(default)]
    pub contact_first_name: String,
    #[serde(default)]
    pub contact_last_name: String,
    #[serde(default)]
    pub contact_phone: String,
    #[serde(default)]
    pub contact_email: String,
    #[serde(default)]
    pub demo_account_required: bool,
    #[serde(default)]
    pub demo_account_name: String,
    #[serde(default)]
    pub demo_account_password: String,
    #[serde(default)]
    pub notes: String,
}

/// 年齢区分。今は "none"（すべての申告を NONE / false）だけ。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgeRating {
    #[serde(default)]
    pub preset: String,
}

/// 空の項目は触らない。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AppStoreApp {
    /// appCategories の id（MUSIC / UTILITIES など）
    #[serde(default)]
    pub primary_category: String,
    #[serde(default)]
    pub secondary_category: String,
    /// none | thirdParty
    #[serde(default)]
    pub content_rights: String,
    /// 版の著作権表示（"2026 Example Inc."）
    #[serde(default)]
    pub copyright: String,
    /// "free" だけ
    #[serde(default)]
    pub price: String,
}

impl AppStoreApp {
    /// コンテンツの権利の yaml の値 → ASC の enum。
    pub fn content_rights_value(&self) -> Option<&'static str> {
        content_rights_value(&self.content_rights)
    }
}

pub fn content_rights_value(v: &str) -> Option<&'static str> {
    match v {
        "none" => Some("DOES_NOT_USE_THIRD_PARTY_CONTENT"),
        "thirdParty" => Some("USES_THIRD_PARTY_CONTENT"),
        _ => None,
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayContact {
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub phone: String,
    #[serde(default)]
    pub website: String,
}

/// 言語ごとの掲載。各ストアは自分の項目だけを読む: App Store は subtitle / keywords /
/// promotionalText / whatsNew / privacyPolicyUrl、Play は shortDescription。name / description は共通。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Locale {
    /// App Store のロケール（"ja"）。Play は ja-JP に読み替える
    #[serde(default)]
    pub locale: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub short_description: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub keywords: String,
    #[serde(default)]
    pub promotional_text: String,
    #[serde(default)]
    pub whats_new: String,
    #[serde(default, rename = "supportUrl")]
    pub support_url: String,
    #[serde(default, rename = "marketingUrl")]
    pub marketing_url: String,
    #[serde(default, rename = "privacyPolicyUrl")]
    pub privacy_policy_url: String,
}

/// 掲載画像。画像は base_dir（設定ファイルからの相対）の下に置く。各セットは言語ごとの
/// ディレクトリ（ja / en …）を、App Store の表示タイプか Play の画像タイプ 1 つに対応させる。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Screenshots {
    #[serde(default)]
    pub base_dir: String,
    #[serde(default)]
    pub appstore: Vec<ScreenshotSet>,
    #[serde(default)]
    pub play: Vec<ScreenshotSet>,
    /// App Store のクリエイティブ（製品ページのヘッダー・検索結果。iOS 27〜）
    #[serde(default)]
    pub creatives: Vec<Creative>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ScreenshotSet {
    #[serde(default)]
    pub dir: String,
    /// App Store の screenshotDisplayType（APP_IPHONE_67 など）
    #[serde(default)]
    pub display_type: String,
    /// Google Play の画像タイプ（phoneScreenshots など）
    #[serde(default)]
    pub image_type: String,
}

/// 言語ごとにアセットライブラリへ置く 1 つ: dir/<lang>/file を placement_type に置く。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Creative {
    #[serde(default)]
    pub dir: String,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub placement_type: String,
}

pub const CREATIVE_PLACEMENT_TYPES: [&str; 2] = [
    "PRODUCT_PAGE_HEADER_ASSET",
    "APP_STORE_SEARCH_RESULTS_ASSET",
];

/// クリエイティブのファイル → アセットライブラリのリソースの種類。
pub fn creative_media(name: &str) -> Option<&'static str> {
    match ext_lower(name).as_str() {
        "png" | "jpg" | "jpeg" => Some("appAssetLibraryImages"),
        "mp4" | "m4v" | "mov" => Some("appAssetLibraryVideos"),
        _ => None,
    }
}

pub fn ext_lower(name: &str) -> String {
    Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

impl Screenshots {
    /// あるセットのあるロケールの画像のディレクトリ（葉は地域を落とした言語: en-US → en）。
    pub fn locale_dir(&self, set: &ScreenshotSet, locale: &str) -> PathBuf {
        Path::new(&self.base_dir)
            .join(&set.dir)
            .join(base_lang(locale))
    }
}

/// 地域を落とす（en-US → en、ja → ja）。
pub fn base_lang(locale: &str) -> &str {
    locale.split('-').next().unwrap_or(locale)
}

/// App Store のロケール → Google Play の言語タグ（地域が要る）。
pub fn play_lang(asc_locale: &str) -> String {
    if asc_locale.contains('-') {
        return asc_locale.to_string();
    }
    match asc_locale {
        "ja" => "ja-JP",
        "en" => "en-US",
        "de" => "de-DE",
        "fr" => "fr-FR",
        other => other,
    }
    .to_string()
}

impl MetaConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<MetaConfig, ConfigError> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)?;
        let mut c = Self::parse(&raw, &path.display().to_string())?;
        // 相対の baseDir は設定ファイルのディレクトリから解決する。
        if let Some(s) = c.screenshots.as_mut()
            && !s.base_dir.is_empty()
            && Path::new(&s.base_dir).is_relative()
        {
            let dir = path.parent().unwrap_or(Path::new(""));
            s.base_dir = dir.join(&s.base_dir).display().to_string();
        }
        Ok(c)
    }

    pub fn parse(raw: &str, path: &str) -> Result<MetaConfig, ConfigError> {
        let c: MetaConfig = serde_yaml_ng::from_str(raw).map_err(|source| ConfigError::Parse {
            path: path.to_string(),
            source,
        })?;
        c.validate()?;
        Ok(c)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |m: String| Err(ConfigError::Invalid(m));
        if self.app_name.is_empty() {
            return bad("appName is required".into());
        }
        if self.bundle_id.is_empty() && self.package_name.is_empty() {
            return bad("at least one of bundleId / packageName is required".into());
        }
        if self.locales.is_empty() {
            return bad("at least one locale is required".into());
        }
        let mut seen = std::collections::HashSet::new();
        for (i, l) in self.locales.iter().enumerate() {
            if l.locale.is_empty() {
                return bad(format!("locales[{i}].locale is required"));
            }
            if !seen.insert(l.locale.as_str()) {
                return bad(format!("duplicate locale {:?}", l.locale));
            }
            if l.name.is_empty() && l.description.is_empty() {
                return bad(format!(
                    "locales[{i}] ({}): at least name or description is required",
                    l.locale
                ));
            }
            for (field, value, max) in [
                ("name", &l.name, 30),
                ("subtitle", &l.subtitle, 30),
                ("shortDescription", &l.short_description, 80),
                ("keywords", &l.keywords, 100),
                ("promotionalText", &l.promotional_text, 170),
                ("description", &l.description, 4000),
                ("whatsNew", &l.whats_new, 4000),
            ] {
                // 末尾の改行は送らないので数えない（YAML の | で付く分）
                let n = value.trim_end().chars().count();
                if n > max {
                    return bad(format!(
                        "locales[{i}] ({}): {field} is {n} chars, max {max}",
                        l.locale
                    ));
                }
            }
        }
        if let Some(a) = &self.age_rating
            && !a.preset.is_empty()
            && a.preset != "none"
        {
            return bad(format!(
                "ageRating.preset {:?} unsupported (only \"none\")",
                a.preset
            ));
        }
        if let Some(s) = &self.app_store_app {
            if !s.content_rights.is_empty() && s.content_rights_value().is_none() {
                return bad(format!(
                    "appstore.contentRights {:?} unsupported (none | thirdParty)",
                    s.content_rights
                ));
            }
            if !s.price.is_empty() && s.price != "free" {
                return bad(format!(
                    "appstore.price {:?} unsupported (only \"free\")",
                    s.price
                ));
            }
        }
        if let Some(s) = &self.screenshots {
            let any = !s.appstore.is_empty() || !s.play.is_empty() || !s.creatives.is_empty();
            if any && s.base_dir.is_empty() {
                return bad("screenshots.baseDir is required when sets are configured".into());
            }
            for (i, set) in s.appstore.iter().enumerate() {
                if set.dir.is_empty() || set.display_type.is_empty() {
                    return bad(format!(
                        "screenshots.appstore[{i}]: dir and displayType are required"
                    ));
                }
            }
            for (i, set) in s.play.iter().enumerate() {
                if set.dir.is_empty() || set.image_type.is_empty() {
                    return bad(format!(
                        "screenshots.play[{i}]: dir and imageType are required"
                    ));
                }
            }
            for (i, cr) in s.creatives.iter().enumerate() {
                if cr.dir.is_empty() || cr.file.is_empty() {
                    return bad(format!(
                        "screenshots.creatives[{i}]: dir and file are required"
                    ));
                }
                if !CREATIVE_PLACEMENT_TYPES.contains(&cr.placement_type.as_str()) {
                    return bad(format!(
                        "screenshots.creatives[{i}]: placementType {:?} unsupported",
                        cr.placement_type
                    ));
                }
                if creative_media(&cr.file).is_none() {
                    return bad(format!(
                        "screenshots.creatives[{i}]: {} is not an image (.png/.jpg) or video (.mp4/.m4v/.mov)",
                        cr.file
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"
appName: Demo
bundleId: com.example.demo
packageName: com.example.demo
locales:
  - locale: en-US
    name: "Demo"
    description: |
      Line one.
"#;

    fn err(raw: &str) -> String {
        MetaConfig::parse(raw, "t").unwrap_err().to_string()
    }

    #[test]
    fn ok_and_trailing_newline_not_counted() {
        let c = MetaConfig::parse(BASE, "t").unwrap();
        assert_eq!(c.locales[0].description, "Line one.\n");
    }

    #[test]
    fn char_limit() {
        let raw = BASE.replace("name: \"Demo\"", &format!("name: \"{}\"", "x".repeat(31)));
        assert_eq!(err(&raw), "locales[0] (en-US): name is 31 chars, max 30");
        // 全角も 1 文字
        let raw = BASE.replace("name: \"Demo\"", &format!("name: \"{}\"", "あ".repeat(30)));
        assert!(MetaConfig::parse(&raw, "t").is_ok());
    }

    #[test]
    fn needs_bundle_or_package_and_unique_locales() {
        let raw = BASE
            .replace("bundleId: com.example.demo\n", "")
            .replace("packageName: com.example.demo\n", "");
        assert_eq!(
            err(&raw),
            "at least one of bundleId / packageName is required"
        );
        let raw = format!("{BASE}  - locale: en-US\n    name: x\n");
        assert_eq!(err(&raw), "duplicate locale \"en-US\"");
    }

    #[test]
    fn screenshots_validation() {
        let with = |s: &str| format!("{BASE}screenshots:\n{s}");
        assert_eq!(
            err(&with("  baseDir: out\n  appstore:\n    - dir: iphone\n")),
            "screenshots.appstore[0]: dir and displayType are required"
        );
        assert_eq!(
            err(&with("  baseDir: out\n  play:\n    - dir: phone\n")),
            "screenshots.play[0]: dir and imageType are required"
        );
        assert_eq!(
            err(&with(
                "  play:\n    - dir: phone\n      imageType: phoneScreenshots\n"
            )),
            "screenshots.baseDir is required when sets are configured"
        );
        assert!(err(&with(
            "  baseDir: out\n  creatives:\n    - dir: c\n      file: a.gif\n      placementType: PRODUCT_PAGE_HEADER_ASSET\n"
        ))
        .contains("is not an image"));
        assert!(
            MetaConfig::parse(
                &with("  baseDir: out\n  appstore:\n    - dir: iphone\n      displayType: APP_IPHONE_67\n"),
                "t"
            )
            .is_ok()
        );
    }

    #[test]
    fn appstore_block_validation() {
        let raw = format!("{BASE}appstore:\n  contentRights: maybe\n");
        assert!(err(&raw).contains("contentRights"));
        let raw = format!("{BASE}appstore:\n  price: paid\n");
        assert!(err(&raw).contains("price"));
        let raw = format!("{BASE}ageRating:\n  preset: teen\n");
        assert!(err(&raw).contains("ageRating"));
    }

    #[test]
    fn locale_helpers() {
        assert_eq!(play_lang("ja"), "ja-JP");
        assert_eq!(play_lang("en-GB"), "en-GB");
        assert_eq!(play_lang("es"), "es");
        assert_eq!(base_lang("en-US"), "en");
        assert_eq!(base_lang("ja"), "ja");
        let s = Screenshots {
            base_dir: "/root/out".into(),
            ..Default::default()
        };
        let set = ScreenshotSet {
            dir: "appstore/iphone69".into(),
            ..Default::default()
        };
        assert_eq!(
            s.locale_dir(&set, "en-US"),
            PathBuf::from("/root/out/appstore/iphone69/en")
        );
        assert_eq!(creative_media("a.MOV"), Some("appAssetLibraryVideos"));
        assert_eq!(creative_media("a.jpg"), Some("appAssetLibraryImages"));
    }
}
