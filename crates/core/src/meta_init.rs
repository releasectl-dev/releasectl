//! `releasectl meta init`: ストアの今の値（`meta export` の JSON）を `metadata.yaml` の形に畳む
//! （docs: 02_cli-architecture/08-init.md）。I/O は持たない。
//!
//! 契約: 書き出した YAML をそのまま `meta plan` に通すと差分が出ない。YAML の形に入らないものは
//! `notes` に入れて人に伝える。

use crate::meta::asc_locale;
use crate::yaml_out::Yaml;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub struct MetaInitInput<'a> {
    pub app_name: &'a str,
    /// YAML の先頭に入れるコメント（いつ・どの版から作ったか）
    pub header: &'a [String],
    /// export の `appstore`（流さなかったら None）
    pub appstore: Option<&'a Value>,
    /// export の `googleplay`
    pub googleplay: Option<&'a Value>,
    /// `screenshots.baseDir`（YAML からの相対）
    pub base_dir: &'a str,
}

pub struct MetaInit {
    pub yaml: String,
    /// YAML に書けなかったもの・人が確かめること
    pub notes: Vec<String>,
}

fn text<'a>(v: Option<&'a Value>, path: &[&str]) -> &'a str {
    let mut cur = v;
    for p in path {
        cur = cur.and_then(|c| c.get(*p));
    }
    cur.and_then(Value::as_str).unwrap_or("")
}

fn object_keys(v: Option<&Value>, key: &str) -> Vec<String> {
    v.and_then(|v| v.get(key))
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// 末尾の空白を無視して同じか（plan の比べ方と同じ）。
fn same(a: &str, b: &str) -> bool {
    a.trim_end() == b.trim_end()
}

pub fn meta_yaml(input: &MetaInitInput) -> MetaInit {
    let (asc, play) = (input.appstore, input.googleplay);
    let mut notes: Vec<String> = Vec::new();
    let mut y = Yaml::default();
    for h in input.header {
        y.comment(0, h);
    }
    y.str(0, "appName", input.app_name);
    y.str(0, "bundleId", text(asc, &["bundleId"]));
    y.str(0, "packageName", text(play, &["packageName"]));

    // 審査情報（パスワードは書かない。空のパスワードは plan / apply が触らない）
    if let Some(rv) = asc.and_then(|a| a.get("review")) {
        y.blank();
        y.line(0, "review:");
        for k in [
            "contactFirstName",
            "contactLastName",
            "contactPhone",
            "contactEmail",
        ] {
            y.str(2, k, text(Some(rv), &[k]));
        }
        y.bool(
            2,
            "demoAccountRequired",
            rv.get("demoAccountRequired")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        );
        y.str(2, "demoAccountName", text(Some(rv), &["demoAccountName"]));
        if rv.get("hasDemoAccountPassword").and_then(Value::as_bool) == Some(true) {
            y.comment(
                2,
                "demoAccountPassword is not written here. While it is empty, releasectl leaves the store's value alone.",
            );
            notes.push(
                "review.demoAccountPassword is set in App Store Connect but was not written to the YAML (it would be committed). It stays unmanaged while empty."
                    .into(),
            );
        }
        y.str(2, "notes", text(Some(rv), &["notes"]));
    }

    // App Store のアプリの設定
    let settings = asc.and_then(|a| a.get("appstore"));
    let setting_keys = [
        "primaryCategory",
        "secondaryCategory",
        "contentRights",
        "copyright",
    ];
    if setting_keys
        .iter()
        .any(|k| !text(settings, &[k]).is_empty())
    {
        y.blank();
        y.line(0, "appstore:");
        for k in setting_keys {
            y.str(2, k, text(settings, &[k]));
        }
    }

    // Google Play の連絡先と既定の言語
    let contact = play.and_then(|p| p.get("playContact"));
    let contact_keys = ["email", "phone", "website"];
    if contact_keys.iter().any(|k| !text(contact, &[k]).is_empty()) {
        y.blank();
        y.line(0, "playContact:");
        for k in contact_keys {
            y.str(2, k, text(contact, &[k]));
        }
    }
    let default_language = text(play, &["defaultLanguage"]);
    if !default_language.is_empty() {
        y.blank();
        y.str(0, "defaultLanguage", default_language);
    }

    // 言語: App Store のロケールを軸に、Play の言語タグを読み替えて 1 つにする
    let asc_locales: BTreeSet<String> = object_keys(asc, "appInfo")
        .into_iter()
        .chain(object_keys(asc, "versionLocalizations"))
        .collect();
    let play_langs: BTreeMap<String, String> = object_keys(play, "listings")
        .into_iter()
        .map(|lang| (asc_locale(&lang), lang))
        .collect();
    let all: BTreeSet<&String> = asc_locales.iter().chain(play_langs.keys()).collect();

    y.blank();
    y.line(0, "locales:");
    let mut first = true;
    for locale in all {
        let in_asc = asc_locales.contains(locale);
        let lang = play_langs.get(locale);
        let info = |k: &str| text(asc, &["appInfo", locale, k]);
        let ver = |k: &str| text(asc, &["versionLocalizations", locale, k]);
        let listing = |k: &str| match lang {
            Some(lang) => text(play, &["listings", lang, k]),
            None => "",
        };
        let (title, full) = (listing("title"), listing("fullDescription"));
        // 共通の値は App Store のもの（無ければ Play のもの）。Play が違うときだけ上書きを書く
        let name = if info("name").is_empty() {
            title
        } else {
            info("name")
        };
        let description = if ver("description").is_empty() {
            full
        } else {
            ver("description")
        };
        if name.trim_end().is_empty() && description.trim_end().is_empty() {
            notes.push(format!(
                "locale {locale}: no name or description in the stores; not written."
            ));
            continue;
        }
        if !first {
            y.blank();
        }
        first = false;
        y.line(2, &format!("- locale: {}", crate::yaml_out::scalar(locale)));
        y.str(4, "name", name);
        y.str(4, "subtitle", info("subtitle"));
        y.str(4, "shortDescription", listing("shortDescription"));
        y.str(4, "description", description);
        for k in [
            "keywords",
            "promotionalText",
            "whatsNew",
            "supportUrl",
            "marketingUrl",
        ] {
            y.str(4, k, ver(k));
        }
        y.str(4, "privacyPolicyUrl", info("privacyPolicyUrl"));
        let play_name = if same(title, name) { "" } else { title };
        let play_description = if same(full, description) { "" } else { full };
        if !play_name.trim_end().is_empty() || !play_description.trim_end().is_empty() {
            y.line(4, "play:");
            y.str(6, "name", play_name);
            y.str(6, "description", play_description);
        }

        // 片方のストアにしか無い言語は、apply がもう片方にも作ろうとする
        match (in_asc, lang) {
            (true, None) if play.is_some() => notes.push(format!(
                "locale {locale}: only on the App Store. `meta apply` would create the Google Play listing ({}).",
                crate::meta::play_lang(locale)
            )),
            (false, Some(lang)) if asc.is_some() => notes.push(format!(
                "locale {locale}: only on Google Play ({lang}). `meta apply` would create the App Store localization."
            )),
            _ => {}
        }
    }

    // 画像（ダウンロードしたもの）。`screenshots` に書くのは、置いたファイルがストアのハッシュと同じ種類だけ
    // （App Store は配信用に作り直した画像しか取れないので、書くと plan が毎回「違う」と言う）。
    let mut asc_sets: BTreeMap<String, String> = BTreeMap::new();
    let mut play_sets: BTreeMap<String, String> = BTreeMap::new();
    let mut differ: BTreeMap<String, String> = BTreeMap::new();
    for (store, key, sets) in [
        (asc, "screenshotFiles", &mut asc_sets),
        (play, "imageFiles", &mut play_sets),
    ] {
        let Some(by_locale) = store.and_then(|s| s.get(key)).and_then(Value::as_object) else {
            continue;
        };
        for by_type in by_locale.values() {
            for (ty, f) in by_type.as_object().into_iter().flatten() {
                let dir = text(Some(f), &["dir"]);
                if dir.is_empty() {
                    continue;
                }
                sets.insert(ty.clone(), dir.to_string());
                if f.get("sameAsStore").and_then(Value::as_bool) == Some(false) {
                    differ.insert(ty.clone(), dir.to_string());
                }
            }
        }
        sets.retain(|ty, _| !differ.contains_key(ty));
    }
    if !asc_sets.is_empty() || !play_sets.is_empty() {
        y.blank();
        y.line(0, "screenshots:");
        y.str(2, "baseDir", input.base_dir);
        for (name, key, sets) in [
            ("appstore", "displayType", &asc_sets),
            ("play", "imageType", &play_sets),
        ] {
            if sets.is_empty() {
                continue;
            }
            y.line(2, &format!("{name}:"));
            for (ty, dir) in sets {
                y.line(4, &format!("- {key}: {ty}"));
                y.str(6, "dir", dir);
            }
        }
    }
    for (ty, dir) in &differ {
        notes.push(format!(
            "images {ty}: saved under {}/{dir} for reference, but not added to `screenshots` (the store only serves re-encoded copies, so they would always show as changed). To manage them, put your original files there and add the set to `screenshots`.",
            input.base_dir
        ));
    }

    MetaInit {
        yaml: y.finish(),
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::MetaConfig;
    use serde_json::json;

    fn run(asc: Option<&Value>, play: Option<&Value>) -> (MetaConfig, MetaInit) {
        let out = meta_yaml(&MetaInitInput {
            app_name: "demo",
            header: &["Generated by releasectl meta init.".to_string()],
            appstore: asc,
            googleplay: play,
            base_dir: "shots",
        });
        let cfg = MetaConfig::parse(&out.yaml, "t").unwrap_or_else(|e| panic!("{e}\n{}", out.yaml));
        (cfg, out)
    }

    fn asc() -> Value {
        json!({
            "bundleId": "com.example.demo",
            "appId": "1",
            "appInfo": {
                "ja": {"name": "デモ", "subtitle": "サブ", "privacyPolicyUrl": "https://example.com/p"},
                "en-US": {"name": "Demo"},
                "en-GB": {"name": "Demo UK"}
            },
            "versionLocalizations": {
                "ja": {"description": "一行目\n二行目", "keywords": "a,b", "whatsNew": "修正"},
                "en-US": {"description": "Hello: world"},
                "en-GB": {"description": "Hello"}
            },
            "review": {"contactFirstName": "A", "contactPhone": "+81 3 1234 5678", "demoAccountRequired": true,
                       "demoAccountName": "demo@example.com", "hasDemoAccountPassword": true, "notes": "n1\nn2"},
            "appstore": {"primaryCategory": "MUSIC", "contentRights": "none", "copyright": "2026 Example"},
            "screenshotFiles": {
                "ja": {"APP_IPHONE_67": {"dir": "appstore/APP_IPHONE_67", "files": ["01.png"], "sameAsStore": true}},
                "en-US": {"APP_IPHONE_67": {"dir": "appstore/APP_IPHONE_67", "files": ["01.png"], "sameAsStore": false}}
            }
        })
    }

    fn play() -> Value {
        json!({
            "packageName": "com.example.demo",
            "listings": {
                "ja-JP": {"title": "デモ", "shortDescription": "短い", "fullDescription": "Play の説明"},
                "en-US": {"title": "Demo for Android", "fullDescription": "Hello: world\n"},
                "ko-KR": {"title": "데모", "fullDescription": "설명"}
            },
            "playContact": {"email": "a@example.com"},
            "defaultLanguage": "en-US",
            "imageFiles": {"ja-JP": {"icon": {"dir": "play/icon", "files": ["01.png"], "sameAsStore": true}}}
        })
    }

    #[test]
    fn folds_two_stores_into_locales() {
        let (c, out) = run(Some(&asc()), Some(&play()));
        assert_eq!(c.bundle_id, "com.example.demo");
        assert_eq!(c.package_name, "com.example.demo");
        assert_eq!(c.default_language, "en-US");
        let locales: Vec<&str> = c.locales.iter().map(|l| l.locale.as_str()).collect();
        assert_eq!(locales, ["en-GB", "en-US", "ja", "ko"]);

        // ja: 名前は同じ、説明は Play だけ違う → 上書きは説明だけ
        let ja = &c.locales[2];
        assert_eq!(ja.name, "デモ");
        assert_eq!(ja.description.trim_end(), "一行目\n二行目");
        assert_eq!(ja.short_description, "短い");
        assert_eq!(ja.whats_new, "修正");
        assert_eq!(ja.play_name(), "デモ");
        assert_eq!(ja.play_description().trim_end(), "Play の説明");
        assert!(ja.play.as_ref().unwrap().name.is_empty());

        // en-US: 説明は末尾の改行だけの違い → 上書きなし。名前は違う
        let en = &c.locales[1];
        assert_eq!(en.play_name(), "Demo for Android");
        assert!(en.play.as_ref().unwrap().description.is_empty());

        // ko: Play にしか無い → 共通の値に入れる（上書きなし）
        let ko = &c.locales[3];
        assert_eq!(
            (ko.name.as_str(), ko.description.as_str()),
            ("데모", "설명")
        );
        assert!(ko.play.is_none());

        let notes = out.notes.join("\n");
        assert!(
            notes.contains("locale en-GB: only on the App Store"),
            "{notes}"
        );
        assert!(
            notes.contains("locale ko: only on Google Play (ko-KR)"),
            "{notes}"
        );
        assert!(notes.contains("demoAccountPassword"), "{notes}");
        assert!(
            notes.contains("images APP_IPHONE_67: saved under shots/appstore/APP_IPHONE_67"),
            "{notes}"
        );
    }

    #[test]
    fn writes_settings_review_and_image_sets() {
        let (c, out) = run(Some(&asc()), Some(&play()));
        let rv = c.review.unwrap();
        assert_eq!(rv.contact_phone, "+81 3 1234 5678");
        assert!(rv.demo_account_required);
        assert_eq!(rv.demo_account_name, "demo@example.com");
        assert!(rv.demo_account_password.is_empty());
        assert_eq!(rv.notes.trim_end(), "n1\nn2");
        let s = c.app_store_app.unwrap();
        assert_eq!(s.primary_category, "MUSIC");
        assert_eq!(s.content_rights, "none");
        assert!(s.price.is_empty());
        assert!(c.age_rating.is_none());
        assert_eq!(c.play_contact.unwrap().email, "a@example.com");
        let shots = c.screenshots.unwrap();
        assert_eq!(shots.base_dir, "shots");
        // en-US の APP_IPHONE_67 がストアと違うので、その種類は書かない
        assert!(shots.appstore.is_empty());
        assert_eq!(shots.play[0].image_type, "icon");
        assert_eq!(shots.play[0].dir, "play/icon");
        assert!(
            out.yaml
                .starts_with("# Generated by releasectl meta init.\nappName: demo\n")
        );
    }

    #[test]
    fn one_store_only() {
        let (c, out) = run(None, Some(&play()));
        assert!(c.bundle_id.is_empty());
        assert!(c.review.is_none() && c.app_store_app.is_none());
        assert_eq!(c.locales.len(), 3);
        assert_eq!(c.locales[1].locale, "ja");
        assert!(c.locales.iter().all(|l| l.play.is_none()));
        // もう片方のストアを流していないので「片方にしか無い」とは言わない
        assert!(out.notes.is_empty(), "{:?}", out.notes);

        let (c, out) = run(Some(&asc()), None);
        assert!(c.package_name.is_empty());
        assert_eq!(c.locales.len(), 3);
        assert!(out.notes.iter().all(|n| !n.contains("only on")));
    }
}
