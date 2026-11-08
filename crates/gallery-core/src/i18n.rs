//! Message catalogs and the active locale.
//!
//! Every text a person reads (errors, verdicts, labels, the tray, notifications,
//! CLI output) is a key in `locales/<lang>.json` at the workspace root, with
//! `%{name}` placeholders. Plural messages carry one key per CLDR category:
//! `files_one`, `files_few`, `files_many`, `files_other`. English is the source
//! and the fallback. Preset and mask names, config keys and paths are never
//! translated.
//!
//! The locale is process-wide ([`set_locale`]); [`with_locale`] overrides it for
//! the calling thread only, which is what tests and the `explain` renderers use.

use std::cell::Cell;
use std::fmt::Display;
use std::sync::RwLock;

rust_i18n::i18n!("../../locales", fallback = "en");

/// One supported language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Language {
    /// BCP 47 code, also the catalog file name.
    pub code: &'static str,
    /// Name in the language itself, for the language picker.
    pub native: &'static str,
    /// Name in English; what the AI is asked to write its reasons in.
    pub english: &'static str,
}

/// The ten languages, English first. English is the source and the fallback.
pub const LANGUAGES: [Language; 10] = [
    Language {
        code: "en",
        native: "English",
        english: "English",
    },
    Language {
        code: "ru",
        native: "Русский",
        english: "Russian",
    },
    Language {
        code: "uk",
        native: "Українська",
        english: "Ukrainian",
    },
    Language {
        code: "de",
        native: "Deutsch",
        english: "German",
    },
    Language {
        code: "fr",
        native: "Français",
        english: "French",
    },
    Language {
        code: "es",
        native: "Español",
        english: "Spanish",
    },
    Language {
        code: "pt-BR",
        native: "Português (Brasil)",
        english: "Brazilian Portuguese",
    },
    Language {
        code: "it",
        native: "Italiano",
        english: "Italian",
    },
    Language {
        code: "pl",
        native: "Polski",
        english: "Polish",
    },
    Language {
        code: "zh-CN",
        native: "简体中文",
        english: "Simplified Chinese",
    },
];

/// The fallback language.
pub const DEFAULT: &str = "en";

static CURRENT: RwLock<&'static str> = RwLock::new(DEFAULT);

thread_local! {
    static OVERRIDE: Cell<Option<&'static str>> = const { Cell::new(None) };
}

/// The supported language a locale tag such as `ru_UA.UTF-8`, `pt-PT` or
/// `zh_TW` belongs to: an exact language match first (`uk_UA` is `uk`, `ru_UA`
/// is `ru`), then the language's only regional variant (`pt_PT` is `pt-BR`,
/// `zh_TW` is `zh-CN`). `None` for an unsupported language or `C`/`POSIX`.
pub fn resolve(tag: &str) -> Option<&'static str> {
    // `ru_UA.UTF-8@euro` -> `ru_UA`
    let tag = tag.split(['.', '@']).next().unwrap_or("").trim();
    let lang = tag
        .split(['_', '-'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if lang.is_empty() {
        return None;
    }
    // An exact code first (`pt-BR`, `zh-CN`), in either separator.
    let norm = tag.replace('_', "-");
    if let Some(l) = LANGUAGES
        .iter()
        .find(|l| l.code.eq_ignore_ascii_case(&norm))
    {
        return Some(l.code);
    }
    LANGUAGES
        .iter()
        .find(|l| l.code.split('-').next().is_some_and(|c| c == lang))
        .map(|l| l.code)
}

/// The first of `tags` that [`resolve`]s, else English.
pub fn pick<I, S>(tags: I) -> &'static str
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    tags.into_iter()
        .find_map(|t| resolve(t.as_ref()))
        .unwrap_or(DEFAULT)
}

/// The language of the operating system's UI, else English.
pub fn detect_system() -> &'static str {
    pick(sys_locale::get_locales())
}

/// The language the environment asks for (`LC_ALL`, `LC_MESSAGES`, `LANG`,
/// first one set and not `C`/`POSIX` wins), else English. The CLI uses this.
pub fn detect_env() -> &'static str {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let tag = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| get(k));
    tag.and_then(|t| resolve(&t)).unwrap_or(DEFAULT)
}

/// Settings value to locale: `"system"` or empty follows the OS, a language
/// code (or anything [`resolve`] knows) is that language, junk is English.
pub fn from_setting(value: &str) -> &'static str {
    match value.trim() {
        "" | "system" => detect_system(),
        v => resolve(v).unwrap_or(DEFAULT),
    }
}

/// Makes `code` the process-wide language (unknown codes become English) and
/// returns the one now in force.
pub fn set_locale(code: &str) -> &'static str {
    let code = resolve(code).unwrap_or(DEFAULT);
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = code;
    code
}

/// The language in force: the calling thread's override, else the global one.
pub fn locale() -> &'static str {
    OVERRIDE
        .with(Cell::get)
        .unwrap_or_else(|| *CURRENT.read().unwrap_or_else(|e| e.into_inner()))
}

/// Runs `f` with `code` as the language of this thread only.
pub fn with_locale<R>(code: &str, f: impl FnOnce() -> R) -> R {
    let code = resolve(code).unwrap_or(DEFAULT);
    let prev = OVERRIDE.with(|o| o.replace(Some(code)));
    struct Restore(Option<&'static str>);
    impl Drop for Restore {
        fn drop(&mut self) {
            OVERRIDE.with(|o| o.set(self.0));
        }
    }
    let _restore = Restore(prev);
    f()
}

/// The English name of the language in force ("Russian"): what the AI writes
/// its reasons in when `[ai] language` is unset.
pub fn language_name() -> &'static str {
    LANGUAGES
        .iter()
        .find(|l| l.code == locale())
        .map_or("English", |l| l.english)
}

/// The CLDR plural category of an integer count in `code`.
pub fn plural_category(code: &str, n: u64) -> &'static str {
    let lang = code.split('-').next().unwrap_or("");
    let few = |n: u64| (2..=4).contains(&(n % 10)) && !(12..=14).contains(&(n % 100));
    match lang {
        "ru" | "uk" => {
            if n % 10 == 1 && n % 100 != 11 {
                "one"
            } else if few(n) {
                "few"
            } else {
                "many"
            }
        }
        "pl" => {
            if n == 1 {
                "one"
            } else if few(n) {
                "few"
            } else {
                "many"
            }
        }
        "zh" => "other",
        // French and Portuguese count 0 as singular too.
        "fr" | "pt" => {
            if n <= 1 {
                "one"
            } else {
                "other"
            }
        }
        _ => {
            if n == 1 {
                "one"
            } else {
                "other"
            }
        }
    }
}

/// The plural suffixes a catalog of `code` must carry for every plural message.
pub fn plural_categories(code: &str) -> &'static [&'static str] {
    match code.split('-').next().unwrap_or("") {
        "ru" | "uk" | "pl" => &["one", "few", "many", "other"],
        "zh" => &["other"],
        _ => &["one", "other"],
    }
}

/// The text of `key` in `locale`, else in English; `None` when neither has it.
fn find(locale: &str, key: &str) -> Option<String> {
    _rust_i18n_try_translate(locale, key)
        .or_else(|| _rust_i18n_try_translate(DEFAULT, key))
        .map(|t| t.into_owned())
}

/// Like [`find`], but a key found nowhere stands for itself.
fn lookup(locale: &str, key: &str) -> String {
    find(locale, key).unwrap_or_else(|| key.to_string())
}

fn fill(mut text: String, args: &[(&str, String)]) -> String {
    if !text.contains("%{") {
        return text;
    }
    for (name, value) in args {
        text = text.replace(&format!("%{{{name}}}"), value);
    }
    text
}

/// The message `key` in the language in force with `%{name}` filled from `args`.
/// A key missing everywhere is returned as is.
pub fn translate(key: &str, args: &[(&str, String)]) -> String {
    fill(lookup(locale(), key), args)
}

/// The plural message `key` for `count` (`key_one`, `key_few`, ...), with
/// `%{count}` and `args` filled. A category the language's catalog lacks falls
/// back to `key_other`.
pub fn translate_plural(key: &str, count: u64, args: &[(&str, String)]) -> String {
    let loc = locale();
    let cat = plural_category(loc, count);
    let text = find(loc, &format!("{key}_{cat}"))
        .or_else(|| find(loc, &format!("{key}_other")))
        .unwrap_or_else(|| key.to_string());
    let mut all = vec![("count", count.to_string())];
    all.extend(args.iter().map(|(k, v)| (*k, v.clone())));
    fill(text, &all)
}

/// Helper for the macros: anything printable as a placeholder value.
pub fn arg(v: &dyn Display) -> String {
    v.to_string()
}

/// `t!("key")` or `t!("key", name = value, ...)`: the message in the language in force.
#[macro_export]
macro_rules! t {
    ($key:expr $(,)?) => {
        $crate::i18n::translate($key, &[])
    };
    ($key:expr, $($name:ident = $val:expr),+ $(,)?) => {
        $crate::i18n::translate($key, &[$((stringify!($name), $crate::i18n::arg(&$val))),+])
    };
}

/// `tn!("key", count, name = value, ...)`: the plural message for `count`;
/// `%{count}` is filled in.
#[macro_export]
macro_rules! tn {
    ($key:expr, $count:expr $(,)?) => {
        $crate::i18n::translate_plural($key, ($count) as u64, &[])
    };
    ($key:expr, $count:expr, $($name:ident = $val:expr),+ $(,)?) => {
        $crate::i18n::translate_plural(
            $key,
            ($count) as u64,
            &[$((stringify!($name), $crate::i18n::arg(&$val))),+],
        )
    };
}

/// `v` with `places` decimals and the language's decimal separator.
pub fn decimal(v: f64, places: usize) -> String {
    let s = format!("{v:.places$}");
    let sep = translate("format.decimal_sep", &[]);
    if sep == "." || sep == "format.decimal_sep" {
        s
    } else {
        s.replace('.', &sep)
    }
}

/// The message `key` if some catalog has it, else `None` (unlike
/// [`translate`], which returns the key itself).
pub fn try_translate(key: &str) -> Option<String> {
    find(locale(), key)
}
