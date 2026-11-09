//! `gallery check -c`, `explain` and `unmatched` with the ordered targets.

use assert_cmd::Command;
use std::path::Path;

fn gallery(tmp: &Path) -> Command {
    let mut c = Command::cargo_bin("gallery").unwrap();
    c.env("LC_ALL", "C")
        .env("HOME", tmp)
        .env("XDG_DATA_HOME", tmp.join("data"))
        .env("GALLERY_DATA_DIR", tmp.join("data/gallery"))
        .env("XDG_CACHE_HOME", tmp.join("cache"))
        .env("XDG_CONFIG_HOME", tmp.join("config"));
    c
}

fn write(p: &Path, content: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

fn stdout(a: assert_cmd::assert::Assert) -> String {
    String::from_utf8(a.get_output().stdout.clone()).unwrap()
}

#[test]
fn check_prints_the_migration_notice_for_the_old_shape_and_exits_0() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("old.toml");
    write(
        &cfg,
        &format!(
            "[[sources]]\nid = \"s\"\npath = \"{r}/s\"\n\
             [[targets.after_ai]]\nname = \"all\"\n[[targets.after_ai.filters]]\nsources = [\"s\"]\n\
             [[targets.after_ai.outputs]]\ndir = \"{r}/out\"\n",
            r = d.path().to_string_lossy().replace('\\', "/")
        ),
    );
    let out = stdout(
        gallery(d.path())
            .args(["check", "-c"])
            .arg(&cfg)
            .assert()
            .success(),
    );
    assert!(
        out.contains("old three-stage targets") && out.contains("old.toml"),
        "{out}"
    );
    assert!(out.contains("ok (1 sources, 1 targets)"), "{out}");
}

#[test]
fn check_is_clean_for_the_new_shape_and_the_shipped_example() {
    let d = tempfile::tempdir().unwrap();
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/gallery.toml");
    let out = stdout(
        gallery(d.path())
            .args(["check", "-c"])
            .arg(&example)
            .assert()
            .success(),
    );
    assert!(!out.contains("old three-stage"), "{out}");
    assert!(out.contains("ok (3 sources, 10 targets)"), "{out}");
}

#[test]
fn check_rejects_an_invalid_config() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("bad.toml");
    write(
        &cfg,
        "[[sources]]\nid = \"s\"\npath = \"/x\"\n[[targets]]\nname = \"t\"\n[[targets.filters]]\nsources = [\"nope\"]\n",
    );
    gallery(d.path())
        .args(["check", "-c"])
        .arg(&cfg)
        .assert()
        .failure();
}

#[test]
fn explain_prints_the_first_miss_per_target() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path().to_string_lossy().replace('\\', "/");
    write(&d.path().join("cam/20260101_120000.jpg"), "1");
    write(&d.path().join("partner/20260101_200000.jpg"), "2");
    let cfg = d.path().join("g.toml");
    write(
        &cfg,
        &format!(
            "[[sources]]\nid = \"cam\"\npath = \"{r}/cam\"\n[[sources]]\nid = \"partner\"\npath = \"{r}/partner\"\n\
             [[targets]]\nname = \"Together\"\n[[targets.filters]]\nsources = [\"cam\"]\n\
             near = {{ sources = [\"partner\"], time = \"2h\", km = 1 }}\n\
             [[targets.outputs]]\ndir = \"{r}/out\"\n"
        ),
    );
    let out = stdout(
        gallery(d.path())
            .args(["explain", "-c"])
            .arg(&cfg)
            .arg(d.path().join("cam/20260101_120000.jpg"))
            .assert()
            .success(),
    );
    assert!(
        out.contains("near: 0 files of partner within 2h / 1.0 km"),
        "{out}"
    );
}

#[test]
fn unmatched_without_a_plan_sees_groups() {
    // The twin copy is trashed by a duplicate rule, so only the third file
    // is left unmatched: conditions over groups work without `--plan`.
    let d = tempfile::tempdir().unwrap();
    let r = d.path().to_string_lossy().replace('\\', "/");
    write(&d.path().join("s/a.jpg"), "same");
    write(&d.path().join("s/b.jpg"), "same");
    write(&d.path().join("s/c.jpg"), "other");
    let cfg = d.path().join("g.toml");
    write(
        &cfg,
        &format!(
            "[[sources]]\nid = \"s\"\npath = \"{r}/s\"\n\
             [[targets]]\nname = \"Dups\"\nkind = \"trash\"\n[[targets.filters]]\nsources = [\"s\"]\n\
             match = \"*.jpg\"\nduplicate = {{ of = [\"sources\"] }}\n"
        ),
    );
    let out = stdout(
        gallery(d.path())
            .args(["unmatched", "-c"])
            .arg(&cfg)
            .assert()
            .success(),
    );
    // a.jpg or b.jpg (the original) and c.jpg stay: two unmatched files.
    assert!(out.contains("2 files"), "{out}");
}
