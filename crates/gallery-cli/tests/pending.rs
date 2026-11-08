//! `gallery pending` and `gallery install-desktop`.

use assert_cmd::Command;
use std::path::Path;

fn setup(tmp: &Path) -> std::path::PathBuf {
    std::fs::create_dir_all(tmp.join("shots")).unwrap();
    std::fs::write(tmp.join("shots/a.jpg"), b"x").unwrap();
    std::fs::write(tmp.join("shots/b.jpg"), b"y").unwrap();
    let cfg = tmp.join("g.toml");
    std::fs::write(
        &cfg,
        format!(
            "[[sources]]\nid = \"shots\"\npath = \"{r}/shots\"\n\
             [[targets.after_ai]]\nname = \"pics\"\n[[targets.after_ai.filters]]\n\
             sources = [\"shots\"]\nmatch = \"*.jpg\"\n[[targets.after_ai.outputs]]\ndir = \"{r}/out\"\n",
            r = tmp.display()
        ),
    )
    .unwrap();
    cfg
}

fn gallery(tmp: &Path) -> Command {
    let mut c = Command::cargo_bin("gallery").unwrap();
    c.env("LC_ALL", "C")
        .env("HOME", tmp)
        .env("XDG_DATA_HOME", tmp.join("data"))
        .env("XDG_CACHE_HOME", tmp.join("cache"))
        .env("XDG_CONFIG_HOME", tmp.join("config"));
    c
}

#[test]
fn pending_prints_counts_and_json() {
    let d = tempfile::tempdir().unwrap();
    let cfg = setup(d.path());
    let out = gallery(d.path())
        .args(["pending", "-c"])
        .arg(&cfg)
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(out.status.success(), "{text}");
    assert!(text.starts_with("pending: 2\n"), "{text}");
    assert!(text.contains("  pics: 2"), "{text}");

    let out = gallery(d.path())
        .args(["pending", "--json", "-c"])
        .arg(&cfg)
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["to_process"], 2);
    assert_eq!(v["need_ai"], 0);
}

#[test]
fn install_desktop_writes_under_the_data_home_only() {
    let d = tempfile::tempdir().unwrap();
    let exe = d.path().join("gallery-gui");
    std::fs::write(&exe, b"").unwrap();
    gallery(d.path())
        .args(["install-desktop", "--exe"])
        .arg(&exe)
        .assert()
        .success();
    let text = std::fs::read_to_string(d.path().join("data/applications/gallery.desktop")).unwrap();
    assert!(text.contains("Exec="), "{text}");
    assert!(
        d.path()
            .join("data/icons/hicolor/128x128/apps/gallery.png")
            .is_file()
    );
    assert!(!d.path().join("config/autostart").exists());
}
