//! `--lang` and the environment pick the language of the CLI's output.

use assert_cmd::Command;
use std::path::Path;

fn gallery(tmp: &Path) -> Command {
    let mut c = Command::cargo_bin("gallery").unwrap();
    c.env("HOME", tmp)
        .env("XDG_DATA_HOME", tmp.join("data"))
        .env("GALLERY_DATA_DIR", tmp.join("data/gallery"))
        .env("XDG_CACHE_HOME", tmp.join("cache"))
        .env("XDG_CONFIG_HOME", tmp.join("config"))
        // No locale from the test machine's environment.
        .env_remove("LC_MESSAGES")
        .env_remove("LANG")
        .env("LC_ALL", "C");
    c
}

fn run(c: &mut Command, args: &[&str]) -> (String, String, bool) {
    let out = c.args(args).output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

#[test]
fn presets_listing_is_in_the_chosen_language() {
    let d = tempfile::tempdir().unwrap();
    let (ru, _, ok) = run(&mut gallery(d.path()), &["--lang", "ru", "presets"]);
    assert!(ok);
    assert!(ru.starts_with("Пресеты имён\n"), "{ru}");
    assert!(ru.contains("Пресеты содержимого\n"), "{ru}");
    assert!(ru.contains("Маски (@имя)\n"), "{ru}");
    // Names, masks and parameters stay as they are.
    assert!(
        ru.contains("avif-high: AVIF: качество 72, скорость 6 [маска: *.{jpg,jpeg,heic,heif}]"),
        "{ru}"
    );
    let (en, _, _) = run(&mut gallery(d.path()), &["presets"]);
    assert!(en.starts_with("Name presets\n"), "{en}");
    assert!(
        en.contains("avif-high: AVIF: quality 72, speed 6 [mask: *.{jpg,jpeg,heic,heif}]"),
        "{en}"
    );
}

#[test]
fn lang_equals_form_and_aliases_work() {
    let d = tempfile::tempdir().unwrap();
    let (de, _, _) = run(&mut gallery(d.path()), &["presets", "--lang=de"]);
    assert!(de.starts_with("Namens-Voreinstellungen\n"), "{de}");
    // pt_PT is pt-BR; an unknown language is English.
    let (pt, _, _) = run(&mut gallery(d.path()), &["--lang", "pt_PT", "presets"]);
    assert!(pt.starts_with("Predefinições de nome\n"), "{pt}");
    let (xx, _, _) = run(&mut gallery(d.path()), &["--lang", "xx", "presets"]);
    assert!(xx.starts_with("Name presets\n"), "{xx}");
}

#[test]
fn environment_picks_the_language_when_no_flag() {
    let d = tempfile::tempdir().unwrap();
    let mut c = gallery(d.path());
    c.env("LC_ALL", "uk_UA.UTF-8");
    let (uk, _, _) = run(&mut c, &["presets"]);
    assert!(uk.starts_with("Пресети імен\n"), "{uk}");
    // LC_ALL beats LANG; the flag beats both.
    let mut c = gallery(d.path());
    c.env("LC_ALL", "fr_FR.UTF-8").env("LANG", "de_DE.UTF-8");
    let (fr, _, _) = run(&mut c, &["presets"]);
    assert!(fr.starts_with("Préréglages de nom\n"), "{fr}");
    let mut c = gallery(d.path());
    c.env("LC_ALL", "fr_FR.UTF-8").env("LANG", "de_DE.UTF-8");
    let (zh, _, _) = run(&mut c, &["--lang", "zh-CN", "presets"]);
    assert!(zh.starts_with("名称预设\n"), "{zh}");
    // LANG alone is read when the others are unset.
    let mut c = gallery(d.path());
    c.env_remove("LC_ALL").env("LANG", "pl_PL.UTF-8");
    let (pl, _, _) = run(&mut c, &["presets"]);
    assert!(pl.starts_with("Ustawienia wstępne nazw\n"), "{pl}");
}

#[test]
fn help_and_errors_are_translated() {
    let d = tempfile::tempdir().unwrap();
    let (help, _, ok) = run(&mut gallery(d.path()), &["--lang", "ru", "-h"]);
    assert!(ok);
    assert!(help.contains("Использование:"), "{help}");
    assert!(help.contains("Команды:"), "{help}");
    assert!(help.contains("Показать встроенные пресеты"), "{help}");
    assert!(help.contains("--lang"), "{help}");
    // A runtime error: the prefix and the core's message.
    let missing = d.path().join("none.json");
    let (_, err, ok) = run(
        &mut gallery(d.path()),
        &["--lang", "ru", "undo", missing.to_str().unwrap()],
    );
    assert!(!ok);
    assert!(err.starts_with("ошибка: "), "{err}");
}
