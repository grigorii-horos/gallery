//! Locale detection, plural forms and the AI reason language.

use gallery_core::ai::system_prompt;
use gallery_core::i18n::{
    LANGUAGES, from_setting, language_name, locale, pick, plural_category, resolve, with_locale,
};
use gallery_core::route::SourcePrompt;
use gallery_core::{t, tn};

#[test]
fn exact_language_match_comes_first() {
    assert_eq!(resolve("uk_UA"), Some("uk"));
    assert_eq!(resolve("ru_UA.UTF-8"), Some("ru"));
    assert_eq!(resolve("ru-RU"), Some("ru"));
    assert_eq!(resolve("de_AT.UTF-8@euro"), Some("de"));
    assert_eq!(resolve("pt-BR"), Some("pt-BR"));
    assert_eq!(resolve("pt_br"), Some("pt-BR"));
    assert_eq!(resolve("zh_CN.UTF-8"), Some("zh-CN"));
}

#[test]
fn regional_variants_fall_back_to_the_one_we_have() {
    assert_eq!(resolve("pt_PT"), Some("pt-BR"));
    assert_eq!(resolve("pt"), Some("pt-BR"));
    assert_eq!(resolve("zh_TW"), Some("zh-CN"));
    assert_eq!(resolve("zh-Hant-HK"), Some("zh-CN"));
    assert_eq!(resolve("es_MX"), Some("es"));
    assert_eq!(resolve("fr_CA.UTF-8"), Some("fr"));
}

#[test]
fn unknown_and_posix_locales_are_not_ours() {
    for tag in ["C", "POSIX", "", "xx_YY", "ja_JP.UTF-8", "tlh"] {
        assert_eq!(resolve(tag), None, "{tag}");
    }
    // `pick` ends in English; the first resolvable tag wins.
    assert_eq!(pick(["ja_JP", "C"]), "en");
    assert_eq!(pick(["ja-JP", "uk-UA", "ru"]), "uk");
    assert_eq!(pick(Vec::<String>::new()), "en");
}

#[test]
fn settings_value_is_a_code_or_system() {
    assert_eq!(from_setting("de"), "de");
    assert_eq!(from_setting("zh_TW"), "zh-CN");
    assert_eq!(from_setting("klingon"), "en");
    // "system" and blank follow the OS, whatever it says: always a supported one.
    for v in ["system", "", "  "] {
        assert!(LANGUAGES.iter().any(|l| l.code == from_setting(v)));
    }
}

#[test]
fn ten_languages_with_native_and_english_names() {
    let codes: Vec<_> = LANGUAGES.iter().map(|l| l.code).collect();
    assert_eq!(
        codes,
        [
            "en", "ru", "uk", "de", "fr", "es", "pt-BR", "it", "pl", "zh-CN"
        ]
    );
    assert!(
        LANGUAGES
            .iter()
            .all(|l| !l.native.is_empty() && !l.english.is_empty())
    );
}

#[test]
fn plural_categories_follow_cldr() {
    let cat = |l: &str, n| plural_category(l, n);
    // Russian and Ukrainian: one / few / many.
    for l in ["ru", "uk"] {
        for (n, want) in [
            (0, "many"),
            (1, "one"),
            (2, "few"),
            (4, "few"),
            (5, "many"),
            (11, "many"),
            (12, "many"),
            (14, "many"),
            (21, "one"),
            (22, "few"),
            (25, "many"),
            (101, "one"),
            (111, "many"),
            (112, "many"),
        ] {
            assert_eq!(cat(l, n), want, "{l} {n}");
        }
    }
    // Polish: only exactly 1 is "one".
    for (n, want) in [
        (1, "one"),
        (2, "few"),
        (5, "many"),
        (12, "many"),
        (21, "many"),
        (22, "few"),
        (0, "many"),
    ] {
        assert_eq!(cat("pl", n), want, "pl {n}");
    }
    // Chinese has none.
    for n in [0, 1, 2, 100] {
        assert_eq!(cat("zh-CN", n), "other");
    }
    // Western languages: one / other; French and Portuguese count 0 as one.
    assert_eq!(cat("en", 1), "one");
    assert_eq!(cat("de", 2), "other");
    assert_eq!(cat("fr", 0), "one");
    assert_eq!(cat("pt-BR", 0), "one");
    assert_eq!(cat("es", 0), "other");
}

#[test]
fn english_plural_messages_fill_in_the_count() {
    with_locale("en", || {
        assert_eq!(tn!("count.files", 1), "1 file");
        assert_eq!(tn!("count.files", 5), "5 files");
        assert_eq!(
            tn!("explain.other_targets", 1, source = "shots"),
            "1 more target not for source shots"
        );
    });
}

#[test]
fn a_missing_key_stands_for_itself() {
    with_locale("ru", || assert_eq!(t!("no.such.key"), "no.such.key"));
}

#[test]
fn the_thread_override_does_not_leak() {
    let before = locale();
    with_locale("ru", || {
        assert_eq!(locale(), "ru");
        with_locale("de", || assert_eq!(locale(), "de"));
        assert_eq!(locale(), "ru");
    });
    assert_eq!(locale(), before);
}

#[test]
fn ai_reason_language_follows_the_locale_when_unset() {
    let cats = vec![("Boards".to_string(), "dev boards".to_string())];
    let blank = SourcePrompt::default();
    let ru = with_locale("ru", || {
        assert_eq!(language_name(), "Russian");
        system_prompt(&cats, &blank)
    });
    assert!(ru.contains("written in Russian"), "{ru}");
    let zh = with_locale("zh-CN", || system_prompt(&cats, &blank));
    assert!(zh.contains("written in Simplified Chinese"), "{zh}");
    let en = with_locale("en", || system_prompt(&cats, &blank));
    assert!(en.contains("written in English"), "{en}");
}

#[test]
fn an_explicit_ai_language_wins_over_the_locale() {
    let cats = vec![("Boards".to_string(), "dev boards".to_string())];
    let explicit = SourcePrompt {
        language: "Ukrainian".into(),
        ..Default::default()
    };
    let p = with_locale("de", || system_prompt(&cats, &explicit));
    assert!(p.contains("written in Ukrainian"), "{p}");
    assert!(!p.contains("German"), "{p}");
}
