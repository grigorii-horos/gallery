// `$GALLERY_DATA_DIR` is honoured by debug builds only. A release test run
// would ignore it and take the real `~/.local/share/gallery/apply.lock`.
#[cfg(not(debug_assertions))]
compile_error!(
    "the CLI e2e suite needs debug assertions (`cargo test`, not `--release`): \
     only then is GALLERY_DATA_DIR honoured, keeping tests off the real data dir"
);

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::Command as Std;

fn have(tool: &str) -> bool {
    gallery_core::tools::find(tool).is_some()
}

fn make(cmd: &mut Std) {
    let out = cmd.output().expect("spawn fixture tool");
    assert!(
        out.status.success(),
        "{cmd:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    walk(dir, &mut Vec::new())
}

fn walk(dir: &Path, acc: &mut Vec<PathBuf>) -> Vec<PathBuf> {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, acc);
            } else {
                acc.push(p);
            }
        }
    }
    acc.clone()
}

fn hash(p: &Path) -> blake3::Hash {
    blake3::hash(&std::fs::read(p).unwrap())
}

/// A `gallery` command plus the temp root that holds its data dir; the root
/// is removed when this drops (after the command ran), never leaked.
struct Gallery {
    cmd: Command,
    _root: tempfile::TempDir,
}

impl std::ops::Deref for Gallery {
    type Target = Command;
    fn deref(&self) -> &Command {
        &self.cmd
    }
}

impl std::ops::DerefMut for Gallery {
    fn deref_mut(&mut self) -> &mut Command {
        &mut self.cmd
    }
}

fn gallery() -> Gallery {
    let mut cmd = Command::cargo_bin("gallery").unwrap();
    // Each call gets its own data dir: the global apply lock lives there,
    // and tests run in parallel (and must never touch the user's real one).
    let root = tempfile::Builder::new()
        .prefix("gallery-e2e-")
        .tempdir()
        .unwrap();
    let data = root.path().join("data");
    std::fs::create_dir(&data).unwrap();
    cmd.env("LC_ALL", "C")
        .env("GALLERY_TRASH", "folder")
        .env("GALLERY_DATA_DIR", data);
    Gallery { cmd, _root: root }
}

#[test]
fn e2e_scan_apply_undo() {
    for t in ["ffmpeg", "ffprobe", "exiftool", "avifenc", "magick"] {
        if !have(t) {
            eprintln!("skip: {t} not found");
            return;
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let src = root.join("src");
    std::fs::create_dir(&src).unwrap();
    let shot = src.join("Screenshot_20260101-120000.png");
    let photo = src.join("IMG_20260412_103314.jpg");
    let clip = src.join("clip.mp4");
    make(
        Std::new("magick")
            .args(["-size", "64x64", "gradient:red-blue"])
            .arg(&shot),
    );
    make(
        Std::new("magick")
            .args(["-size", "64x64", "gradient:red-blue"])
            .arg(&photo),
    );
    make(
        Std::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
            .args(["-i", "testsrc2=size=160x120:rate=10", "-t", "1"])
            .args([
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&clip),
    );

    let out = root.join("out");
    let cfg = format!(
        r#"
[[sources]]
id = "cam"
path = "{src}"

[name_presets."Date"]
template = "{{date}}_{{stem}}.{{ext}}"
date = "capture"
normalize_ext = true

[[targets.before_ai]]
name = "Screenshots"
[[targets.before_ai.filters]]
sources = ["cam"]
match = "*screenshot*"
[[targets.before_ai.outputs]]
dir = "{out}/shots"

[[targets.after_ai]]
name = "Timeline"
[[targets.after_ai.filters]]
sources = ["cam"]
match = "*"
[[targets.after_ai.outputs]]
dir = "{out}/timeline/{{YYYY}}"
content = [{{ mask = "*", preset = "meta-full" }}]
rename = [{{ mask = "*", preset = "Date" }}]
[[targets.after_ai.outputs]]
dir = "{out}/backup"
"#,
        src = src.to_string_lossy().replace('\\', "/"),
        out = out.to_string_lossy().replace('\\', "/")
    );
    let cfg_path = root.join("gallery.toml");
    std::fs::write(&cfg_path, cfg).unwrap();
    let plan_path = root.join("plan.json");

    gallery()
        .args(["scan", "--no-ai", "-c"])
        .arg(&cfg_path)
        .arg("-o")
        .arg(&plan_path)
        .assert()
        .success();
    let plan: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&plan_path).unwrap()).unwrap();
    assert_eq!(plan["items"].as_array().unwrap().len(), 3);
    assert!(shot.exists(), "scan must not touch sources");

    let originals: Vec<(PathBuf, blake3::Hash)> = [&shot, &photo, &clip]
        .into_iter()
        .map(|p| (p.clone(), hash(p)))
        .collect();

    let applied = gallery().arg("apply").arg(&plan_path).assert().success();
    // Tool versions go to the report only, never to the output as warnings.
    let out_text = String::from_utf8_lossy(&applied.get_output().stdout).into_owned()
        + &String::from_utf8_lossy(&applied.get_output().stderr);
    assert!(
        !out_text.contains("version") && !out_text.contains("warning"),
        "{out_text}"
    );
    assert!(!shot.exists() && !photo.exists() && !clip.exists());
    assert_eq!(files_under(&src.join(".gallery-trash")).len(), 3);

    // Exact output set: the screenshot is copied as is, the photo is renamed by
    // its capture date, the clip has no date and keeps its name; backups are copies.
    let mut got = files_under(&out);
    got.sort();
    let mut want: Vec<PathBuf> = [
        "backup/IMG_20260412_103314.jpg",
        "backup/clip.mp4",
        "shots/Screenshot_20260101-120000.png",
        "timeline/2026/20260412_103314_IMG_20260412_103314.jpg",
        "timeline/unknown/clip.mp4",
    ]
    .iter()
    .map(|r| out.join(r))
    .collect();
    want.sort();
    assert_eq!(got, want);
    assert_eq!(
        hash(&out.join("backup/IMG_20260412_103314.jpg")),
        originals[1].1
    );
    assert_eq!(hash(&out.join("backup/clip.mp4")), originals[2].1);
    assert_eq!(
        hash(&out.join("shots/Screenshot_20260101-120000.png")),
        originals[0].1
    );
    let report = root.join("plan.report.json");
    let rep: serde_json::Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    assert!(
        rep["tool_versions"]["ffmpeg"]
            .as_str()
            .is_some_and(|v| v.contains("version")),
        "{rep}"
    );
    assert!(rep["tool_warnings"].as_array().unwrap().is_empty(), "{rep}");

    gallery().arg("undo").arg(&report).assert().success();
    for (p, h) in &originals {
        assert_eq!(hash(p), *h, "{} restored with original bytes", p.display());
    }
    assert!(files_under(&out).is_empty());
    assert!(files_under(&src.join(".gallery-trash")).is_empty());
}

#[test]
fn short_help_lists_only_usage_and_options() {
    let out = gallery()
        .arg("-h")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("Usage"));
    assert!(text.lines().count() < 22, "{text}");
}

#[test]
fn e2e_screenshots_renamed_in_place() {
    for t in ["exiftool", "magick"] {
        if !have(t) {
            eprintln!("skip: {t} not found");
            return;
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let shots = root.join("Screenshots");
    std::fs::create_dir(&shots).unwrap();
    let raw = shots.join("Screenshot_20260101-120000.png");
    let named = shots.join("20251231_235959.png");
    for p in [&raw, &named] {
        make(
            Std::new("magick")
                .args(["-size", "16x16", "gradient:red-blue"])
                .arg(p),
        );
    }
    let raw_hash = hash(&raw);
    let named_hash = hash(&named);

    // The source folder is the target folder: files are renamed in place.
    let cfg = format!(
        r#"
[[sources]]
id = "screenshots"
path = "{shots}"

[[targets.before_ai]]
name = "Screenshots"
[[targets.before_ai.filters]]
sources = ["screenshots"]
match = "*"
[[targets.before_ai.outputs]]
dir = "{shots}"
rename = [{{ mask = "*", preset = "capture-date" }}]
"#,
        shots = shots.to_string_lossy().replace('\\', "/")
    );
    let cfg_path = root.join("gallery.toml");
    std::fs::write(&cfg_path, cfg).unwrap();
    let plan_path = root.join("plan.json");
    let scan = || {
        let out = gallery()
            .args(["scan", "--no-ai", "-c"])
            .arg(&cfg_path)
            .arg("-o")
            .arg(&plan_path)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let plan: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&plan_path).unwrap()).unwrap();
        (String::from_utf8(out).unwrap(), plan)
    };

    let (text, plan) = scan();
    assert_eq!(plan["items"].as_array().unwrap().len(), 1, "{plan}");
    assert_eq!(plan["items"][0]["outputs"][0]["op"], "rename");
    assert_eq!(plan["unchanged"].as_array().unwrap().len(), 1, "{plan}");
    assert!(text.contains("unchanged: 1"), "{text}");

    let out = gallery()
        .arg("apply")
        .arg(&plan_path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("renamed: 1"), "{text}");
    let renamed = shots.join("20260101_120000.png");
    let mut got = files_under(&shots);
    got.sort();
    assert_eq!(got, vec![named.clone(), renamed.clone()]);
    assert_eq!(hash(&renamed), raw_hash);
    assert_eq!(hash(&named), named_hash);
    assert!(!shots.join(".gallery-trash").exists());

    // Every file already has its name: nothing to do.
    let (text, plan) = scan();
    assert!(plan["items"].as_array().unwrap().is_empty(), "{plan}");
    assert_eq!(plan["unchanged"].as_array().unwrap().len(), 2, "{plan}");
    assert!(text.contains("unchanged: 2"), "{text}");
}

#[test]
fn e2e_in_place_rename_with_backup_is_idempotent() {
    for t in ["exiftool", "magick"] {
        if !have(t) {
            eprintln!("skip: {t} not found");
            return;
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let shots = root.join("Screenshots");
    let backup = root.join("backup");
    std::fs::create_dir(&shots).unwrap();
    for (name, fill) in [
        ("Screenshot_20260101-120000.png", "gradient:red-blue"),
        ("20251231_235959.png", "gradient:green-yellow"),
    ] {
        make(
            Std::new("magick")
                .args(["-size", "16x16", fill])
                .arg(shots.join(name)),
        );
    }
    // Source folder = target folder, plus a byte-copy backup; no exclude mask.
    let cfg = format!(
        r#"
[[sources]]
id = "screenshots"
path = "{shots}"

[[targets.before_ai]]
name = "Screenshots"
[[targets.before_ai.filters]]
sources = ["screenshots"]
match = "*"
[[targets.before_ai.outputs]]
dir = "{shots}"
rename = [{{ mask = "*", preset = "capture-date" }}]
[[targets.before_ai.outputs]]
dir = "{backup}"
rename = [{{ mask = "*", preset = "capture-date" }}]
"#,
        shots = shots.to_string_lossy().replace('\\', "/"),
        backup = backup.to_string_lossy().replace('\\', "/")
    );
    let cfg_path = root.join("gallery.toml");
    std::fs::write(&cfg_path, cfg).unwrap();
    let plan_path = root.join("plan.json");
    let scan = || {
        gallery()
            .args(["scan", "--no-ai", "-c"])
            .arg(&cfg_path)
            .arg("-o")
            .arg(&plan_path)
            .assert()
            .success();
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&plan_path).unwrap()).unwrap()
    };

    let plan = scan();
    assert_eq!(plan["items"].as_array().unwrap().len(), 2, "{plan}");
    gallery().arg("apply").arg(&plan_path).assert().success();
    let mut backups = files_under(&backup);
    backups.sort();
    assert_eq!(
        backups,
        vec![
            backup.join("20251231_235959.png"),
            backup.join("20260101_120000.png")
        ]
    );

    // The backups are there and the names are right: nothing left to do.
    let plan = scan();
    assert!(plan["items"].as_array().unwrap().is_empty(), "{plan}");
    assert_eq!(plan["unchanged"].as_array().unwrap().len(), 2, "{plan}");
    gallery().arg("apply").arg(&plan_path).assert().success();
    let mut again = files_under(&backup);
    again.sort();
    assert_eq!(again, backups);
}

#[test]
fn e2e_screenshots_sorted_into_dated_subfolders() {
    for t in ["exiftool", "magick"] {
        if !have(t) {
            eprintln!("skip: {t} not found");
            return;
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let shots = root.join("Screenshots");
    std::fs::create_dir(&shots).unwrap();
    let raw = shots.join("Screenshot_2026-07-05-00-20-22.png");
    let named = shots.join("20251231_235959.png");
    for (p, fill) in [
        (&raw, "gradient:red-blue"),
        (&named, "gradient:green-yellow"),
    ] {
        make(Std::new("magick").args(["-size", "16x16", fill]).arg(p));
    }
    let raw_hash = hash(&raw);
    let named_hash = hash(&named);

    // The source folder is the target folder; the name preset makes folders.
    // Recursive, so the moved files are scanned again on the next run.
    let cfg = format!(
        r#"
[[sources]]
id = "screenshots"
path = "{shots}"
recursive = true

[[targets.before_ai]]
name = "Screenshots"
[[targets.before_ai.filters]]
sources = ["screenshots"]
match = "*"
[[targets.before_ai.outputs]]
dir = "{shots}"
rename = [{{ mask = "*", preset = "capture-date-dirs" }}]
"#,
        shots = shots.to_string_lossy().replace('\\', "/")
    );
    let cfg_path = root.join("gallery.toml");
    std::fs::write(&cfg_path, cfg).unwrap();
    let plan_path = root.join("plan.json");
    let scan = || {
        gallery()
            .args(["scan", "--no-ai", "-c"])
            .arg(&cfg_path)
            .arg("-o")
            .arg(&plan_path)
            .assert()
            .success();
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&plan_path).unwrap()).unwrap()
    };

    let plan = scan();
    let items = plan["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{plan}");
    assert!(
        items.iter().all(|i| i["outputs"][0]["op"] == "rename"),
        "{plan}"
    );

    let out = gallery()
        .arg("apply")
        .arg(&plan_path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("renamed: 2"), "{text}");
    let moved = shots.join("2026/07/20260705_002022.png");
    let moved_named = shots.join("2025/12/20251231_235959.png");
    let mut got = files_under(&shots);
    got.sort();
    let mut want = vec![moved.clone(), moved_named.clone()];
    want.sort();
    assert_eq!(got, want);
    assert_eq!(hash(&moved), raw_hash);
    assert_eq!(hash(&moved_named), named_hash);
    assert!(!shots.join(".gallery-trash").exists());

    // The files are where their names put them: nothing to do.
    let plan = scan();
    assert!(plan["items"].as_array().unwrap().is_empty(), "{plan}");
    assert_eq!(plan["unchanged"].as_array().unwrap().len(), 2, "{plan}");
}

#[test]
fn presets_command_lists_builtins() {
    let out = gallery().arg("presets").output().unwrap();
    assert!(out.status.success());
    let t = String::from_utf8(out.stdout).unwrap();
    assert!(t.contains("Name presets"));
    assert!(t.contains("Content presets"));
    assert!(t.contains("avif-high: AVIF: quality 72, speed 6"));
    assert!(t.contains("capture-date: {date:YYYYMMDD_HHMMSS}.{ext}"));
}

#[test]
fn e2e_pilot_runs_a_slice_then_the_rest() {
    for t in ["ffmpeg", "ffprobe", "exiftool", "avifenc", "magick"] {
        if !have(t) {
            eprintln!("skip: {t} not found");
            return;
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let src = root.join("src");
    std::fs::create_dir(&src).unwrap();
    let names = [
        "IMG_20260412_103314.jpg",
        "IMG_20260413_103314.jpg",
        "IMG_20260414_103314.jpg",
    ];
    for n in names {
        make(
            Std::new("magick")
                .args(["-size", "64x64", "gradient:red-blue"])
                .arg(src.join(n)),
        );
    }
    let out = root.join("out");
    let cfg = format!(
        r#"
[[sources]]
id = "cam"
path = "{src}"

[[targets.after_ai]]
name = "Timeline"
[[targets.after_ai.filters]]
sources = ["cam"]
match = "*"
[[targets.after_ai.outputs]]
dir = "{out}/timeline"
content = [{{ mask = "*", preset = "meta-full" }}]
"#,
        src = src.to_string_lossy().replace('\\', "/"),
        out = out.to_string_lossy().replace('\\', "/")
    );
    let cfg_path = root.join("gallery.toml");
    std::fs::write(&cfg_path, cfg).unwrap();
    let plan_path = root.join("plan.json");
    gallery()
        .args(["scan", "--no-ai", "-c"])
        .arg(&cfg_path)
        .arg("-o")
        .arg(&plan_path)
        .assert()
        .success();

    let applied = gallery()
        .arg("apply")
        .arg(&plan_path)
        .args(["--pilot", "1"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&applied.get_output().stdout).into_owned();
    assert!(
        stdout.contains("pilot:") && stdout.contains("discrepancies with the sources: 0"),
        "{stdout}"
    );
    let (pilot, rest) = (
        root.join("plan-pilot.plan.json"),
        root.join("plan-rest.plan.json"),
    );
    let count = |p: &Path| {
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
        v["items"].as_array().unwrap().len()
    };
    assert_eq!((count(&pilot), count(&rest)), (1, 2));
    assert_eq!(files_under(&out).len(), 1);
    assert_eq!(names_left(&src), 2);
    assert!(root.join("plan-pilot.plan.report.json").exists());

    gallery().arg("apply").arg(&rest).assert().success();
    assert_eq!(files_under(&out).len(), 3);
    assert_eq!(names_left(&src), 0);
}

/// Regular files directly in `dir`.
fn names_left(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .filter(|e| e.as_ref().unwrap().path().is_file())
        .count()
}

#[test]
fn explain_prints_a_tree_with_a_verdict() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/clip.mp4"), b"x").unwrap();
    std::fs::write(root.join("src/a.jpg"), b"x").unwrap();
    let cfg = root.join("gallery.toml");
    std::fs::write(
        &cfg,
        format!(
            "[[sources]]\nid = \"s\"\npath = \"{r}/src\"\n\n\
             [[targets.before_ai]]\nname = \"pics\"\n\
             [[targets.before_ai.filters]]\nsources = [\"s\"]\nmatch = \"@photo-original\"\n\
             [[targets.before_ai.outputs]]\ndir = \"{r}/out\"\n",
            r = root.to_string_lossy().replace('\\', "/")
        ),
    )
    .unwrap();
    let ov = root.join("overrides.json");
    std::fs::write(&ov, r#"{"version":1,"entries":[]}"#).ok();
    let out = gallery()
        .args(["explain", "-c"])
        .arg(&cfg)
        .arg("--overrides")
        .arg(&ov)
        .arg(root.join("src/clip.mp4"))
        .arg(root.join("src/a.jpg"))
        .arg(root.join("elsewhere.jpg"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("├─ Source: s"), "{text}");
    assert!(
        text.contains("Extension .mp4 is not in the match [@photo-original] of target pics"),
        "{text}"
    );
    assert!(
        text.contains("Will be processed: target pics (mask)"),
        "{text}"
    );
    assert!(text.contains("File not found"), "{text}");
}

#[test]
fn scan_estimate_counts_ai_files_without_any_call_or_plan() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/cat.jpg"), b"x").unwrap();
    std::fs::write(root.join("src/note.txt"), b"x").unwrap();
    let cfg = root.join("gallery.toml");
    std::fs::write(
        &cfg,
        format!(
            "[[sources]]\nid = \"s\"\npath = \"{r}/src\"\n\n\
             [[targets.ai]]\nname = \"cats\"\nsources = [\"s\"]\ndescription = \"cats\"\n\
             prefilter = {{ match = \"*.jpg\" }}\n\
             [[targets.ai.outputs]]\ndir = \"{r}/out\"\n",
            r = root.to_string_lossy().replace('\\', "/")
        ),
    )
    .unwrap();
    let ov = root.join("overrides.json");
    std::fs::write(&ov, r#"{"version":1,"entries":[]}"#).unwrap();
    let out = gallery()
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("CLAUDE_API_KEY")
        .args(["scan", "--estimate", "-c"])
        .arg(&cfg)
        .arg("--overrides")
        .arg(&ov)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("AI: ≈ $0.0") && text.contains("(1 files, 0 cached)"),
        "{text}"
    );
    assert!(text.contains("rough estimate"), "{text}");
    // Nothing written: no plan, no cache.
    assert!(!root.join("cache").exists());
    assert_eq!(names_left(&root.join("src")), 2);
}

#[test]
fn apply_dry_run_prints_the_estimate() {
    for t in ["ffmpeg", "ffprobe", "exiftool", "avifenc", "magick"] {
        if !have(t) {
            eprintln!("skip: {t} not found");
            return;
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.txt"), vec![b'x'; 5000]).unwrap();
    let cfg = root.join("gallery.toml");
    std::fs::write(
        &cfg,
        format!(
            "[[sources]]\nid = \"s\"\npath = \"{r}/src\"\n\n\
             [[targets.after_ai]]\nname = \"docs\"\n\
             [[targets.after_ai.filters]]\nsources = [\"s\"]\nmatch = \"*\"\n\
             [[targets.after_ai.outputs]]\ndir = \"{r}/out\"\n",
            r = root.to_string_lossy().replace('\\', "/")
        ),
    )
    .unwrap();
    let plan = root.join("plan.json");
    let ov = root.join("overrides.json");
    std::fs::write(&ov, r#"{"version":1,"entries":[]}"#).unwrap();
    gallery()
        .args(["scan", "--no-ai", "-c"])
        .arg(&cfg)
        .arg("-o")
        .arg(&plan)
        .arg("--overrides")
        .arg(&ov)
        .assert()
        .success();
    let out = gallery()
        .env("XDG_DATA_HOME", root.join("data"))
        .args(["apply", "--dry-run"])
        .arg(&plan)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("Estimate (rough estimate):"), "{text}");
    assert!(text.contains("docs: 1 file,"), "{text}");
    assert!(text.contains("to trash: 1"), "{text}");
    // A dry run writes nothing.
    assert!(!root.join("out").exists());
}

#[test]
fn apply_pilot_dry_run_writes_no_pilot_or_rest_plans() {
    for t in ["ffmpeg", "ffprobe", "exiftool", "avifenc", "magick"] {
        if !have(t) {
            eprintln!("skip: {t} not found");
            return;
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir(root.join("src")).unwrap();
    for n in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(root.join("src").join(n), vec![b'x'; 100]).unwrap();
    }
    let cfg = root.join("gallery.toml");
    std::fs::write(
        &cfg,
        format!(
            "[[sources]]\nid = \"s\"\npath = \"{r}/src\"\n\n\
             [[targets.after_ai]]\nname = \"docs\"\n\
             [[targets.after_ai.filters]]\nsources = [\"s\"]\nmatch = \"*\"\n\
             [[targets.after_ai.outputs]]\ndir = \"{r}/out\"\n",
            r = root.to_string_lossy().replace('\\', "/")
        ),
    )
    .unwrap();
    let ov = root.join("overrides.json");
    std::fs::write(&ov, r#"{"version":1,"entries":[]}"#).unwrap();
    let plan = root.join("plan.json");
    gallery()
        .args(["scan", "--no-ai", "-c"])
        .arg(&cfg)
        .arg("-o")
        .arg(&plan)
        .arg("--overrides")
        .arg(&ov)
        .assert()
        .success();
    gallery()
        .arg("apply")
        .arg(&plan)
        .args(["--pilot", "1", "--dry-run"])
        .assert()
        .success();
    let mut left: Vec<_> = std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, ["gallery.toml", "overrides.json", "plan.json", "src"]);
    assert!(!root.join("out").exists());
}

#[test]
fn apply_and_undo_are_refused_while_another_run_holds_the_global_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let lock = data.join("apply.lock");
    // This test process stands for the other run: it holds the flock.
    let held = gallery_core::lock::FileLock::acquire(&lock, |_, _| String::new()).unwrap();
    for args in [["apply", "nope.plan.json"], ["undo", "nope.report.json"]] {
        let out = gallery()
            .env("GALLERY_DATA_DIR", &data)
            .current_dir(tmp.path())
            .args(args)
            .assert()
            .failure();
        let err = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
        assert!(err.contains("Another Gallery run"), "{err}");
        // Windows locks are mandatory: the holder's pid is unreadable there.
        if cfg!(unix) {
            assert!(err.contains(&std::process::id().to_string()), "{err}");
        }
        assert!(lock.exists(), "a live holder's lock stays");
    }
    drop(held);
}

#[test]
fn a_leftover_global_lock_file_does_not_block() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let lock = data.join("apply.lock");
    std::fs::write(&lock, "4194999").unwrap();
    let out = gallery()
        .env("GALLERY_DATA_DIR", &data)
        .current_dir(tmp.path())
        .args(["undo", "nope.report.json"])
        .assert()
        .failure();
    let err = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(!err.contains("Another Gallery run"), "{err}");
    assert!(
        gallery_core::lock::FileLock::acquire(&lock, |_, _| String::new()).is_ok(),
        "released on every exit path"
    );
}
