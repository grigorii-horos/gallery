//! `gallery examples list|clear` over a corrections file in a temp data dir.

use assert_cmd::Command;
use gallery_core::learn::{Correction, Store};
use gallery_core::overrides::Action;
use std::path::Path;

fn gallery(tmp: &Path) -> Command {
    let mut c = Command::cargo_bin("gallery").unwrap();
    c.env("LC_ALL", "C")
        .env("HOME", tmp)
        .env("XDG_DATA_HOME", tmp.join("data"))
        .env("XDG_CACHE_HOME", tmp.join("cache"))
        .env("XDG_CONFIG_HOME", tmp.join("config"));
    c
}

fn run(tmp: &Path, args: &[&str]) -> String {
    let out = gallery(tmp).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn list_and_clear_by_source() {
    let d = tempfile::tempdir().unwrap();
    assert_eq!(run(d.path(), &["examples", "list"]), "No examples\n");
    let store = Store::new(d.path().join("data/gallery/ai-examples.jsonl"));
    let c = |src: &str, file: &str, at: &str| {
        Correction::new(
            src,
            file,
            Some("Photos".into()),
            "Lidl receipt",
            Action::Target {
                name: "Receipts".into(),
            },
            at,
        )
    };
    store
        .append(&[
            c("cam", "a.jpg", "1"),
            c("phone", "b.jpg", "2"),
            c("cam", "c.jpg", "3"),
        ])
        .unwrap();

    let all = run(d.path(), &["examples", "list"]);
    assert!(
        all.contains("cam: 2\n") && all.contains("phone: 1\n"),
        "{all}"
    );
    assert!(
        all.contains("c.jpg: Lidl receipt — AI chose Photos, correct is Receipts"),
        "{all}"
    );
    assert!(
        all.find("c.jpg").unwrap() < all.find("a.jpg").unwrap(),
        "newest first: {all}"
    );

    let only = run(d.path(), &["examples", "list", "--source", "phone"]);
    assert!(only.contains("phone: 1") && !only.contains("cam"), "{only}");

    assert_eq!(
        run(d.path(), &["examples", "clear", "--source", "cam"]),
        "Examples deleted: 2\n"
    );
    assert_eq!(store.load().len(), 1);
    assert_eq!(
        run(d.path(), &["examples", "clear"]),
        "Examples deleted: 1\n"
    );
    assert!(store.load().is_empty());
}
