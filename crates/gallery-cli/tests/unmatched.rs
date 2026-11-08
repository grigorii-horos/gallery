//! `gallery unmatched`: the grouped "unmatched" table with reasons.

use assert_cmd::Command;
use std::path::Path;

fn write(root: &Path, rel: &str, n: usize) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, vec![b'x'; n]).unwrap();
}

fn setup(tmp: &Path) -> std::path::PathBuf {
    write(tmp, "shots/Screenshots/a.webm", 10);
    write(tmp, "shots/Screenshots/b.webm", 20);
    write(tmp, "shots/Screenshots/c.avif", 5);
    write(tmp, "shots/ok.jpg", 1);
    let cfg = tmp.join("g.toml");
    std::fs::write(
        &cfg,
        format!(
            "[[sources]]\nid = \"shots\"\npath = \"{r}/shots\"\nrecursive = true\n\
             [[targets.after_ai]]\nname = \"pics\"\n[[targets.after_ai.filters]]\n\
             sources = [\"shots\"]\nmatch = \"*.jpg\"\n[[targets.after_ai.outputs]]\ndir = \"{r}/out\"\n",
            r = tmp.display()
        ),
    )
    .unwrap();
    cfg
}

#[test]
fn prints_groups_with_reasons_without_a_plan() {
    let d = tempfile::tempdir().unwrap();
    let cfg = setup(d.path());
    let out = Command::cargo_bin("gallery")
        .unwrap()
        .env("LC_ALL", "C")
        .env("XDG_DATA_HOME", d.path().join("data"))
        .env("HOME", d.path())
        .args(["unmatched", "-c"])
        .arg(&cfg)
        .assert()
        .success();
    let text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(text.contains("3 files in 2 groups"), "{text}");
    let first = text
        .lines()
        .find(|l| l.starts_with("shots/Screenshots"))
        .unwrap();
    assert!(first.contains(".webm\t2\t30 B\t"), "{first}");
    assert!(first.contains("pics"), "{first}");
    assert!(text.find(".webm").unwrap() < text.find(".avif").unwrap());
}

#[test]
fn reads_a_saved_plan() {
    let d = tempfile::tempdir().unwrap();
    let cfg = setup(d.path());
    let plan = d.path().join("p.json");
    // A plan with one unmatched file.
    let mut p: serde_json::Value = serde_json::json!({
        "version": 1, "created": "t", "side": "local", "content_presets": {},
        "items": [], "review": [], "unmatched": [], "warnings": []
    });
    p["unmatched"] = serde_json::json!([d.path().join("shots/Screenshots/c.avif")]);
    std::fs::write(&plan, p.to_string()).unwrap();
    let out = Command::cargo_bin("gallery")
        .unwrap()
        .env("LC_ALL", "C")
        .env("HOME", d.path())
        .args(["unmatched", "-c"])
        .arg(&cfg)
        .arg("--plan")
        .arg(&plan)
        .assert()
        .success();
    let text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(text.contains(".avif\t1\t5 B"), "{text}");
    assert!(!text.contains(".webm"), "{text}");
}
