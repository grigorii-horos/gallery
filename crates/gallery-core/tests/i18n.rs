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
    // The Romance languages have "many" for a round million.
    for l in ["es", "fr", "it", "pt-BR"] {
        assert_eq!(cat(l, 1_000_000), "many", "{l}");
        assert_eq!(cat(l, 2_000_000), "many", "{l}");
        assert_eq!(cat(l, 1_000_001), "other", "{l}");
    }
    assert_eq!(cat("de", 1_000_000), "other");
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

// ---- the catalogs ----

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

type Catalog = BTreeMap<String, String>;

fn load(code: &str) -> Catalog {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../locales/{code}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

const SUFFIXES: [&str; 6] = ["zero", "one", "two", "few", "many", "other"];

fn base(key: &str) -> &str {
    for s in SUFFIXES {
        if let Some(b) = key.strip_suffix(&format!("_{s}")) {
            return b;
        }
    }
    key
}

/// `%{name}` placeholders of a message.
fn placeholders(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = text;
    while let Some(i) = rest.find("%{") {
        let tail = &rest[i + 2..];
        let Some(end) = tail.find('}') else { break };
        out.insert(tail[..end].to_string());
        rest = &tail[end + 1..];
    }
    out
}

/// Keys of English that are plural (`x_one` + `x_other`).
fn plural_bases(en: &Catalog) -> BTreeSet<String> {
    en.keys()
        .filter(|k| k.ends_with("_one"))
        .map(|k| base(k).to_string())
        .collect()
}

#[test]
fn every_language_has_every_key_of_english_and_no_other() {
    let en = load("en");
    let bases = |c: &Catalog| {
        c.keys()
            .map(|k| base(k).to_string())
            .collect::<BTreeSet<_>>()
    };
    for l in LANGUAGES.iter().filter(|l| l.code != "en") {
        let c = load(l.code);
        let (want, have) = (bases(&en), bases(&c));
        let missing: Vec<_> = want.difference(&have).collect();
        let extra: Vec<_> = have.difference(&want).collect();
        assert!(missing.is_empty(), "{}: missing {missing:?}", l.code);
        assert!(extra.is_empty(), "{}: not in English {extra:?}", l.code);
    }
}

#[test]
fn plural_messages_carry_exactly_the_languages_forms() {
    let en = load("en");
    for l in LANGUAGES {
        let c = load(l.code);
        let need: BTreeSet<&str> = gallery_core::i18n::plural_categories(l.code)
            .iter()
            .copied()
            .collect();
        for b in plural_bases(&en) {
            let have: BTreeSet<&str> = SUFFIXES
                .iter()
                .copied()
                .filter(|s| c.contains_key(&format!("{b}_{s}")))
                .collect();
            assert_eq!(have, need, "{}: {b}", l.code);
        }
    }
}

#[test]
fn plain_keys_do_not_look_like_plural_forms() {
    let en = load("en");
    let plural = plural_bases(&en);
    for k in en.keys() {
        if base(k) == k.as_str() {
            assert!(!plural.contains(k), "{k}");
        }
    }
}

#[test]
fn translations_use_the_placeholders_of_english() {
    let en = load("en");
    for l in LANGUAGES {
        for (key, text) in load(l.code) {
            let b = base(&key);
            let reference = en
                .get(&key)
                .or_else(|| en.get(&format!("{b}_other")))
                .or_else(|| en.get(b))
                .unwrap_or_else(|| panic!("{} {key}: not in English", l.code));
            assert_eq!(
                placeholders(&text),
                placeholders(reference),
                "{} {key}",
                l.code
            );
            assert!(!text.trim().is_empty(), "{} {key} is empty", l.code);
        }
    }
}

#[test]
fn leading_separators_of_fragments_survive_translation() {
    // Some messages are glued to others (", stabilization"): the separator is part of them.
    let en = load("en");
    for l in LANGUAGES {
        let c = load(l.code);
        for key in [
            "preset.stabilization",
            "preset.denoise",
            "preset.keep_names",
            "preset.lower_ext",
            "undo.output_remains",
            "explain.exclude_fired",
            "explain.exclude_not_fired",
            "plan.newer_version",
            "config.preset_renamed",
            "explain.tree.unsure",
        ] {
            let lead = |s: &str| s.starts_with([' ', ',', ';', '，', '；', '—', '–', '（']);
            assert_eq!(
                lead(&c[key]),
                lead(&en[key]),
                "{} {key}: {:?}",
                l.code,
                c[key]
            );
        }
    }
}

#[test]
fn plurals_read_right_in_russian_polish_and_chinese() {
    let f = |loc: &str, n: u64| with_locale(loc, || tn!("count.files", n));
    assert_eq!(f("en", 1), "1 file");
    assert_eq!(f("en", 3), "3 files");
    assert_eq!(f("ru", 1), "1 файл");
    assert_eq!(f("ru", 3), "3 файла");
    assert_eq!(f("ru", 11), "11 файлов");
    assert_eq!(f("ru", 21), "21 файл");
    assert_eq!(f("uk", 22), "22 файли");
    assert_eq!(f("pl", 1), "1 plik");
    assert_eq!(f("pl", 4), "4 pliki");
    assert_eq!(f("pl", 5), "5 plików");
    assert_eq!(f("pl", 22), "22 pliki");
    assert_eq!(f("zh-CN", 1), "1 个文件");
    assert_eq!(f("zh-CN", 5), "5 个文件");
    assert_eq!(f("fr", 0), "0 fichier");
    assert_eq!(f("fr", 2), "2 fichiers");
    assert_eq!(f("de", 1), "1 Datei");
}

#[test]
fn sizes_and_decimals_follow_the_language() {
    use gallery_core::triage::size_text;
    assert_eq!(with_locale("en", || size_text(1536)), "1.5 KB");
    assert_eq!(with_locale("ru", || size_text(1536)), "1,5 КБ");
    assert_eq!(with_locale("fr", || size_text(5 * 1024 * 1024)), "5,0 Mo");
    assert_eq!(with_locale("zh-CN", || size_text(512)), "512 B");
}

#[test]
fn a_core_error_speaks_the_language_in_force() {
    use gallery_core::mask::Mask;
    let err = |loc: &str| with_locale(loc, || Mask::parse("^(").unwrap_err().to_string());
    assert!(
        err("en").starts_with("config: bad mask '^('"),
        "{}",
        err("en")
    );
    assert!(
        err("ru").starts_with("конфигурация: неверная маска «^(»"),
        "{}",
        err("ru")
    );
    assert!(
        err("de").starts_with("Konfiguration: ungültige Maske „^(“"),
        "{}",
        err("de")
    );
}
