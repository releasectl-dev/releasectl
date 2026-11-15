//! `metadata.yaml`（ストアの掲載情報の desired-state）の読み込みと検証
//! （Go 版 `internal/storemeta/config.go`）。docs: 02_cli-architecture/06-meta.md。

use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::config::ConfigError;

#[derive(Debug, Clone, Default, Deserialize)]
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
    /// 編集する App Store の版（"1.0.0"）。空なら編集できる状態の版。CLI の `--app-store-version` で上書きする
    #[serde(default)]
    pub app_store_version: String,
    /// `app_store_version` の版が無ければ作る。YAML には書かず、CLI の `--create-app-store-version` だけで決める
    /// （打ち間違いで版ができないように、毎回明示させる）
    #[serde(skip)]
    pub create_app_store_version: bool,
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

/// ASC の enum → コンテンツの権利の yaml の値（`meta export` 用。content_rights_value の逆）。
pub fn content_rights_yaml(api: &str) -> Option<&'static str> {
    ["none", "thirdParty"]
        .into_iter()
        .find(|y| content_rights_value(y) == Some(api))
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
    /// Google Play だけ別の文面にするときの上書き（無ければ共通の name / description）
    #[serde(default)]
    pub play: Option<PlayOverride>,
}

/// Google Play の title / fullDescription を App Store と別にする（空の項目は共通の値を使う）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayOverride {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
}

impl Locale {
    /// Google Play の title。
    pub fn play_name(&self) -> &str {
        match &self.play {
            Some(p) if !p.name.trim_end().is_empty() => &p.name,
            _ => &self.name,
        }
    }

    /// Google Play の fullDescription。
    pub fn play_description(&self) -> &str {
        match &self.play {
            Some(p) if !p.description.trim_end().is_empty() => &p.description,
            _ => &self.description,
        }
    }
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
    /// 言語のディレクトリ（地域を落とした名前。ja / en …）。
    pub fn locale_dir(&self, set: &ScreenshotSet, locale: &str) -> PathBuf {
        Path::new(&self.base_dir)
            .join(&set.dir)
            .join(base_lang(locale))
    }

    /// ロケールそのままの名前のディレクトリ（en-GB / zh-Hant …）。あればこちらを先に使う
    /// （en-US と en-GB のように、地域を落とすと同じ名前になる言語を分けるため）。
    pub fn full_locale_dir(&self, set: &ScreenshotSet, locale: &str) -> PathBuf {
        Path::new(&self.base_dir).join(&set.dir).join(locale)
    }
}

/// 地域を落とす（en-US → en、ja → ja）。
pub fn base_lang(locale: &str) -> &str {
    locale.split('-').next().unwrap_or(locale)
}

/// App Store と Google Play でコードが違う言語（App Store, Google Play）。ここに無い言語は同じコード。
const LOCALE_PAIRS: [(&str, &str); 20] = [
    ("ar-SA", "ar"),
    ("cs", "cs-CZ"),
    ("da", "da-DK"),
    ("el", "el-GR"),
    ("es-MX", "es-419"),
    ("fi", "fi-FI"),
    ("he", "iw-IL"),
    ("hi", "hi-IN"),
    ("hu", "hu-HU"),
    ("it", "it-IT"),
    ("ja", "ja-JP"),
    ("ko", "ko-KR"),
    ("ms", "ms-MY"),
    ("no", "no-NO"),
    ("pl", "pl-PL"),
    ("ru", "ru-RU"),
    ("sv", "sv-SE"),
    ("tr", "tr-TR"),
    ("zh-Hans", "zh-CN"),
    ("zh-Hant", "zh-TW"),
];

/// App Store のロケール → Google Play の言語タグ。
pub fn play_lang(asc_locale: &str) -> String {
    // 地域を省いた書き方（App Store のコードではないが、前から受け付けている）
    match asc_locale {
        "en" => return "en-US".to_string(),
        "de" => return "de-DE".to_string(),
        "fr" => return "fr-FR".to_string(),
        _ => {}
    }
    LOCALE_PAIRS
        .iter()
        .find(|(a, _)| *a == asc_locale)
        .map_or(asc_locale, |(_, p)| p)
        .to_string()
}

/// Google Play の言語タグ → App Store のロケール（`play_lang` の逆。`meta init` が 2 つのストアの言語を 1 つにする）。
pub fn asc_locale(play: &str) -> String {
    LOCALE_PAIRS
        .iter()
        .find(|(_, p)| *p == play)
        .map_or(play, |(a, _)| a)
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
            let play = l.play.clone().unwrap_or_default();
            for (field, value, max) in [
                ("name", &l.name, 30),
                ("subtitle", &l.subtitle, 30),
                ("shortDescription", &l.short_description, 80),
                ("keywords", &l.keywords, 100),
                ("promotionalText", &l.promotional_text, 170),
                ("description", &l.description, 4000),
                ("whatsNew", &l.whats_new, 4000),
                ("play.name", &play.name, 30),
                ("play.description", &play.description, 4000),
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

    #[test]
    fn content_rights_round_trip() {
        for y in ["none", "thirdParty"] {
            assert_eq!(
                content_rights_yaml(content_rights_value(y).unwrap()),
                Some(y)
            );
        }
        assert_eq!(content_rights_yaml("SOMETHING_NEW"), None);
    }
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
        assert_eq!(play_lang("de"), "de-DE");
        assert_eq!(play_lang("zh-Hans"), "zh-CN");
        assert_eq!(play_lang("pt-BR"), "pt-BR");
        for (a, p) in LOCALE_PAIRS {
            assert_eq!(asc_locale(&play_lang(a)), a);
            assert_eq!(play_lang(&asc_locale(p)), p);
        }
        assert_eq!(asc_locale("de-DE"), "de-DE");
        assert_eq!(asc_locale("en-US"), "en-US");
        let l = Locale {
            name: "A".into(),
            description: "d".into(),
            play: Some(PlayOverride {
                name: "P".into(),
                description: " \n".into(),
            }),
            ..Default::default()
        };
        assert_eq!((l.play_name(), l.play_description()), ("P", "d"));
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
