mod support;

use gallery_core::apply::{ApplyOptions, ItemResult, ItemStatus, Report, apply, report_path};
use gallery_core::config::{ContentPreset, MetadataPreset, PhotoPreset, VideoPreset};
use gallery_core::plan::{OutputOp, PLAN_VERSION, Plan, PlanItem, PlannedOutput, Side};
use gallery_core::route::{AiMeta, Via};
use gallery_core::tools::Tools;
use gallery_core::trash::{TrashMode, TrashRef};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

fn require_all() -> bool {
    ["ffmpeg", "ffprobe", "exiftool", "avifenc", "magick"]
        .iter()
        .all(|t| support::tool_available(t))
}

macro_rules! require_tools {
    () => {
        if !require_all() {
            eprintln!("skip: tools not found");
            return;
        }
    };
}

fn hash(p: &Path) -> blake3::Hash {
    blake3::hash(&std::fs::read(p).unwrap())
}

fn mtime_ns(p: &Path) -> i128 {
    std::fs::metadata(p)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i128
}

/// Plan item for an existing `src` with outputs `(dest, steps)`.
fn item(src: &Path, root: &Path, outs: Vec<(PathBuf, Vec<&str>)>) -> PlanItem {
    PlanItem {
        ai_meta: None,
        source: src.to_path_buf(),
        source_root: root.to_path_buf(),
        size: std::fs::metadata(src).unwrap().len(),
        mtime_ns: mtime_ns(src),
        target: "t".into(),
        via: Via::Mask,
        outputs: outs
            .into_iter()
            .map(|(dest, steps)| PlannedOutput {
                dest,
                op: OutputOp::Write,
                steps: steps.into_iter().map(String::from).collect(),
                trace: vec![],
            })
            .collect(),
        kind: Default::default(),
    }
}

fn presets() -> BTreeMap<String, ContentPreset> {
    let mut m = BTreeMap::new();
    m.insert(
        "Metadata".into(),
        ContentPreset::Metadata(MetadataPreset::full()),
    );
    m.insert(
        "AVIF".into(),
        ContentPreset::Photo(PhotoPreset::avif_fast()),
    );
    m.insert(
        "Video".into(),
        ContentPreset::Video(VideoPreset {
            preset: 12,
            ..VideoPreset::high()
        }),
    );
    m
}

/// Save a plan with `items` next to the other test files; return its path.
fn save_plan(dir: &Path, items: Vec<PlanItem>) -> PathBuf {
    let plan = Plan {
        version: PLAN_VERSION,
        created: chrono::Local::now().to_rfc3339(),
        side: Side::Local,
        tz: None,
        content_presets: presets(),
        items,
        review: vec![],
        unmatched: vec![],
        warnings: vec![],
        unchanged: vec![],
        retry_of: None,
        kept: Vec::new(),
    };
    let path = dir.join("plan.json");
    plan.save(&path).unwrap();
    path
}

fn opts() -> ApplyOptions {
    ApplyOptions {
        dry_run: false,
        jobs: 2,
        trash: TrashMode::Folder,
        ..Default::default()
    }
}

fn run(plan: &Path, o: &ApplyOptions) -> Report {
    apply(plan, &Tools::discover().unwrap(), o).unwrap()
}

/// Names in `dir` (sorted), or nothing when the directory is absent.
fn names(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut v: Vec<String> = rd
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn trashed_path(r: &ItemResult) -> PathBuf {
    match &r.status {
        ItemStatus::Done {
            trashed: Some(TrashRef::Folder { path }),
        } => path.clone(),
        other => panic!("expected Done in folder trash, got {other:?} / {r:?}"),
    }
}

#[test]
fn apply_happy_path() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    let src = support::fixture_jpeg(
        &root.join("sub"),
        "a.jpg",
        Some("2026:04:12 10:33:14"),
        None,
    );
    let orig = hash(&src);
    let (o1, o2) = (d.path().join("out1"), d.path().join("out2/deep"));
    let plan = save_plan(
        d.path(),
        vec![item(
            &src,
            &root,
            vec![
                (o1.join("a.jpg"), vec![]),
                (o2.join("b.jpg"), vec!["Metadata"]),
            ],
        )],
    );
    let rep = run(&plan, &opts());

    assert_eq!(rep.run_id.len(), "20261104-120000".len(), "{}", rep.run_id);
    assert_eq!(rep.plan, plan);
    assert!(!rep.started.is_empty() && !rep.finished.is_empty());
    let r = &rep.items[0];
    assert_eq!(r.source, src);
    let t = trashed_path(r);
    assert_eq!(
        t,
        root.join(".gallery-trash")
            .join(&rep.run_id)
            .join("sub/a.jpg")
    );
    assert_eq!(hash(&t), orig);
    assert!(!src.exists());
    assert_eq!(r.outputs.len(), 2);
    assert_eq!(
        r.outputs[0].actual.as_deref(),
        Some(o1.join("a.jpg").as_path())
    );
    assert_eq!(
        r.outputs[1].actual.as_deref(),
        Some(o2.join("b.jpg").as_path())
    );
    assert!(r.outputs.iter().all(|o| o.error.is_none() && !o.fell_back));
    // The item's wall time is recorded for the estimate.
    assert!(r.elapsed_ms.is_some());
    assert_eq!(hash(&o1.join("a.jpg")), orig);
    assert!(o2.join("b.jpg").exists());
    assert_eq!(names(&o1), ["a.jpg"]);
    assert_eq!(names(&o2), ["b.jpg"]);

    // The report on disk is the returned one.
    let text = std::fs::read_to_string(report_path(&plan)).unwrap();
    let disk: Report = serde_json::from_str(&text).unwrap();
    assert_eq!(disk.items, rep.items);
    assert_eq!(report_path(&plan), d.path().join("plan.report.json"));
}

#[cfg(unix)]
#[test]
fn apply_failure_keeps_source() {
    use std::os::unix::fs::PermissionsExt;
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    let src = support::fixture_jpeg(&root, "a.jpg", None, None);
    let orig = hash(&src);
    let (o1, ro) = (d.path().join("out1"), d.path().join("ro"));
    std::fs::create_dir_all(&ro).unwrap();
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
    let plan = save_plan(
        d.path(),
        vec![item(
            &src,
            &root,
            vec![(o1.join("a.jpg"), vec![]), (ro.join("a.jpg"), vec![])],
        )],
    );
    let rep = run(&plan, &opts());
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();

    let r = &rep.items[0];
    assert!(
        matches!(r.status, ItemStatus::Failed { trashed: None }),
        "{r:?}"
    );
    assert_eq!(hash(&src), orig);
    assert!(!root.join(".gallery-trash").exists());
    // Every output is produced before any is placed: the failed one says
    // why and the good one was not placed.
    assert!(r.outputs[0].actual.is_none());
    assert!(names(&o1).is_empty());
    assert!(r.outputs[1].actual.is_none());
    assert!(r.outputs[1].error.is_some());
    assert!(names(&ro).is_empty());
}

#[test]
fn apply_skips_stale_source() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    let src = support::fixture_jpeg(&root, "a.jpg", None, None);
    let out = d.path().join("out");
    let plan = save_plan(
        d.path(),
        vec![item(&src, &root, vec![(out.join("a.jpg"), vec![])])],
    );
    let f = std::fs::File::options().write(true).open(&src).unwrap();
    f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
        .unwrap();
    drop(f);
    let rep = run(&plan, &opts());
    assert!(
        matches!(rep.items[0].status, ItemStatus::Stale),
        "{:?}",
        rep.items[0]
    );
    assert!(src.exists());
    assert!(!out.exists());
}

#[test]
fn apply_missing_source() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    let src = support::fixture_jpeg(&root, "a.jpg", None, None);
    let out = d.path().join("out");
    let plan = save_plan(
        d.path(),
        vec![item(&src, &root, vec![(out.join("a.jpg"), vec![])])],
    );
    std::fs::remove_file(&src).unwrap();
    let rep = run(&plan, &opts());
    assert!(
        matches!(rep.items[0].status, ItemStatus::Missing),
        "{:?}",
        rep.items[0]
    );
    assert!(!out.exists());
}

#[test]
fn apply_never_overwrites() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    let a = support::fixture_jpeg(&root, "a.jpg", None, None);
    let b = support::fixture_jpeg(&root, "b.jpg", Some("2026:01:01 00:00:00"), None);
    let out = d.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("a.jpg"), b"old").unwrap();
    // A different case still collides: the listing is case-insensitive.
    std::fs::write(out.join("B.JPG"), b"old B").unwrap();
    let plan = save_plan(
        d.path(),
        vec![
            item(&a, &root, vec![(out.join("a.jpg"), vec![])]),
            item(&b, &root, vec![(out.join("b.jpg"), vec![])]),
        ],
    );
    let rep = run(&plan, &opts());
    assert_eq!(std::fs::read(out.join("a.jpg")).unwrap(), b"old");
    assert_eq!(std::fs::read(out.join("B.JPG")).unwrap(), b"old B");
    assert_eq!(
        rep.items[0].outputs[0].actual.as_deref(),
        Some(out.join("a_1.jpg").as_path())
    );
    assert_eq!(
        rep.items[1].outputs[0].actual.as_deref(),
        Some(out.join("b_1.jpg").as_path())
    );
    assert_eq!(names(&out), ["B.JPG", "a.jpg", "a_1.jpg", "b_1.jpg"]);
}

#[test]
fn apply_leaves_no_partial_outputs() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    // Not a decodable image: the AVIF step fails mid-pipe after metadata.
    let src = root.join("broken.jpg");
    std::fs::write(&src, b"definitely not a jpeg").unwrap();
    let out = d.path().join("out");
    std::fs::create_dir_all(out.join(".gallery-tmp-19990101-000000-dir")).unwrap();
    std::fs::write(out.join(".gallery-tmp-19990101-000000-dir/x"), b"x").unwrap();
    std::fs::write(out.join(".gallery-tmp-19990101-000000-old.jpg"), b"x").unwrap();
    std::fs::write(out.join(".keep"), b"x").unwrap();
    let plan = save_plan(
        d.path(),
        vec![item(
            &src,
            &root,
            vec![(out.join("broken.avif"), vec!["Metadata", "AVIF"])],
        )],
    );
    let rep = run(&plan, &opts());
    let r = &rep.items[0];
    assert!(
        matches!(r.status, ItemStatus::Failed { trashed: None }),
        "{r:?}"
    );
    assert!(r.outputs[0].error.is_some());
    assert!(src.exists());
    // Leftovers of the earlier run are gone, unrelated files stay, and the
    // failed output left nothing behind.
    assert_eq!(names(&out), [".keep"]);
}

#[test]
fn apply_dry_run_writes_nothing() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    let src = support::fixture_jpeg(&root, "a.jpg", None, None);
    let orig = hash(&src);
    let out = d.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join(".gallery-tmp-19990101-000000-old"), b"x").unwrap();
    let plan = save_plan(
        d.path(),
        vec![item(
            &src,
            &root,
            vec![(out.join("a.jpg"), vec!["Metadata"])],
        )],
    );
    let o = ApplyOptions {
        dry_run: true,
        ..opts()
    };
    let rep = run(&plan, &o);
    assert!(
        matches!(rep.items[0].status, ItemStatus::DryRun),
        "{:?}",
        rep.items[0]
    );
    assert_eq!(hash(&src), orig);
    assert_eq!(names(&out), [".gallery-tmp-19990101-000000-old"]);
    assert!(!root.join(".gallery-trash").exists());
    assert!(!report_path(&plan).exists());
    assert_eq!(rep.items[0].elapsed_ms, None);
}

#[test]
fn apply_in_place_output_replaces_source_after_trash() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    // The date in the name makes the metadata step write it into the file.
    let src = support::fixture_jpeg(&root, "IMG_20260412_103314.jpg", None, None);
    let orig = hash(&src);
    let out = d.path().join("out");
    let plan = save_plan(
        d.path(),
        vec![item(
            &src,
            &root,
            // The in-place output comes first: it must still wait for the trash.
            vec![(src.clone(), vec!["Metadata"]), (out.join("b.jpg"), vec![])],
        )],
    );
    let rep = run(&plan, &opts());
    let r = &rep.items[0];
    let t = trashed_path(r);
    assert_eq!(hash(&t), orig);
    assert_eq!(r.outputs[0].actual.as_deref(), Some(src.as_path()));
    assert_ne!(
        hash(&src),
        orig,
        "metadata step rewrote the in-place output"
    );
    assert_eq!(hash(&out.join("b.jpg")), orig);
    assert_eq!(names(&root), [".gallery-trash", "IMG_20260412_103314.jpg"]);
}

#[test]
fn apply_item_without_outputs_keeps_source() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    let src = support::fixture_jpeg(&root, "a.jpg", None, None);
    let plan = save_plan(d.path(), vec![item(&src, &root, vec![])]);
    let rep = run(&plan, &opts());
    let r = &rep.items[0];
    assert!(
        matches!(r.status, ItemStatus::Failed { trashed: None }),
        "{r:?}"
    );
    assert_eq!(r.error.as_deref(), Some("target without outputs"));
    assert!(src.exists());
    assert!(!root.join(".gallery-trash").exists());
}

#[test]
fn apply_same_names_from_two_sources_do_not_collide() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    let mut items = Vec::new();
    let mut hashes = Vec::new();
    // Eight sources named alike, all planned onto the same dest (as a server
    // plan does without seeing the disk): reservation must stay race-free.
    for i in 0..8 {
        let root = d.path().join(format!("s{i}"));
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("a.jpg");
        std::fs::write(&src, format!("content {i}")).unwrap();
        hashes.push(hash(&src));
        items.push(item(&src, &root, vec![(out.join("a.jpg"), vec![])]));
    }
    let plan = save_plan(d.path(), items);
    let o = ApplyOptions { jobs: 4, ..opts() };
    let rep = run(&plan, &o);
    let mut actual: Vec<PathBuf> = Vec::new();
    for (r, h) in rep.items.iter().zip(&hashes) {
        trashed_path(r);
        let a = r.outputs[0].actual.clone().unwrap();
        assert_eq!(&hash(&a), h, "{a:?}");
        actual.push(a);
    }
    actual.sort();
    actual.dedup();
    assert_eq!(actual.len(), 8);
    assert_eq!(names(&out).len(), 8);
}

#[test]
fn apply_fell_back_uses_result_ext() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    // No video stream: the video preset hands the input over unchanged.
    let src = support::fixture_wav(&root, "voice.wav");
    let out = d.path().join("out");
    let plan = save_plan(
        d.path(),
        vec![item(
            &src,
            &root,
            vec![(out.join("voice.av1.mp4"), vec!["Video"])],
        )],
    );
    let rep = run(&plan, &opts());
    let r = &rep.items[0];
    trashed_path(r);
    assert!(r.outputs[0].fell_back);
    assert_eq!(r.outputs[0].planned, out.join("voice.av1.mp4"));
    assert_eq!(
        r.outputs[0].actual.as_deref(),
        Some(out.join("voice.wav").as_path())
    );
    assert_eq!(names(&out), ["voice.wav"]);
}

#[test]
fn apply_twice_keeps_previous_report() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    let src = support::fixture_jpeg(&root, "a.jpg", None, None);
    let out = d.path().join("out");
    let plan = save_plan(
        d.path(),
        vec![item(&src, &root, vec![(out.join("a.jpg"), vec![])])],
    );
    let first = run(&plan, &opts());
    let first_text = std::fs::read_to_string(report_path(&plan)).unwrap();
    // Re-applying the same plan must not destroy the first run's undo record.
    let second = run(&plan, &opts());
    assert!(matches!(second.items[0].status, ItemStatus::Missing));
    let kept = d.path().join(format!("plan.report.{}.json", first.run_id));
    assert_eq!(std::fs::read_to_string(&kept).unwrap(), first_text);
    let now: Report =
        serde_json::from_str(&std::fs::read_to_string(report_path(&plan)).unwrap()).unwrap();
    assert_eq!(now.items, second.items);
}

#[test]
fn apply_fell_back_onto_the_source_path_is_in_place() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    // Already AV1: the video preset hands it over unchanged, under its own
    // extension, so the result wants the source's own path.
    let src = support::fixture_clip(&root, "X.mp4", (160, 120), "libsvtav1", false);
    let orig = hash(&src);
    let plan_for = |src: &Path| {
        save_plan(
            d.path(),
            vec![item(
                src,
                &root,
                vec![(root.join("X.av1.mp4"), vec!["Video"])],
            )],
        )
    };
    let rep = run(&plan_for(&src), &opts());
    let r = &rep.items[0];
    let t = trashed_path(r);
    assert_eq!(hash(&t), orig);
    assert!(r.outputs[0].fell_back);
    assert_eq!(r.outputs[0].actual.as_deref(), Some(src.as_path()));
    assert_eq!(hash(&src), orig);
    assert_eq!(names(&root), [".gallery-trash", "X.mp4"]);

    // A rerun over the result does not grow the name either (a new second:
    // the run id names the trash folder).
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let rep = run(&plan_for(&src), &opts());
    trashed_path(&rep.items[0]);
    assert_eq!(
        rep.items[0].outputs[0].actual.as_deref(),
        Some(src.as_path())
    );
    assert_eq!(names(&root), [".gallery-trash", "X.mp4"]);
}

/// Raw QuickTime:CreateDate (stored as UTC) and the mtime of `p`, in UTC.
fn utc_dates(p: &Path) -> (String, chrono::DateTime<chrono::Utc>) {
    let out = std::process::Command::new("exiftool")
        .args(["-s3", "-QuickTime:CreateDate"])
        .arg(p)
        .env("TZ", "UTC")
        .output()
        .unwrap();
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let mtime = std::fs::metadata(p).unwrap().modified().unwrap().into();
    (raw, mtime)
}

#[test]
fn apply_uses_the_planning_machines_zone() {
    use chrono::TimeZone;
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    let out = d.path().join("out");
    // The applying machine is in UTC: its own conversions and its children.
    let tools = Tools {
        tz: Some("UTC".into()),
        ..Tools::discover().unwrap()
    };
    let run_with_tz = |name: &str, tz: &str| {
        // January: Moscow is UTC+3, unlike most of Europe.
        let src = support::fixture_mp4(&root, name, 1, None);
        let path = save_plan(
            d.path(),
            vec![item(&src, &root, vec![(out.join(name), vec!["Metadata"])])],
        );
        let mut plan = Plan::load(&path).unwrap();
        plan.tz = Some(tz.into());
        plan.save(&path).unwrap();
        let rep = apply(&path, &tools, &opts()).unwrap();
        trashed_path(&rep.items[0]);
        (rep, utc_dates(&out.join(name)))
    };

    // Planned in Moscow: 10:33:14 there is 07:33:14 UTC.
    let (rep, (raw, mtime)) = run_with_tz("VID_20260112_103314.mp4", "Europe/Moscow");
    assert_eq!(raw, "2026:01:12 07:33:14");
    assert_eq!(
        mtime,
        chrono::Utc
            .with_ymd_and_hms(2026, 1, 12, 7, 33, 14)
            .unwrap()
    );
    assert!(
        rep.tool_warnings
            .iter()
            .all(|w| !w.contains("Europe/Moscow")),
        "{:?}",
        rep.tool_warnings
    );

    // An unknown zone falls back to this machine's, with a warning.
    let (rep, (raw, mtime)) = run_with_tz("VID_20260113_103314.mp4", "Mars/Olympus");
    assert_eq!(raw, "2026:01:13 10:33:14");
    assert_eq!(
        mtime,
        chrono::Utc
            .with_ymd_and_hms(2026, 1, 13, 10, 33, 14)
            .unwrap()
    );
    assert!(
        rep.tool_warnings.iter().any(|w| w.contains("Mars/Olympus")),
        "{:?}",
        rep.tool_warnings
    );
}

/// Mode bits (permissions only) of `p`.
#[cfg(unix)]
fn mode(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
}

/// Encoded and metadata outputs get the source's permission bits, not the
/// 0600 of the temp file they were staged in.
#[cfg(unix)]
#[test]
fn apply_outputs_take_the_source_permissions() {
    use std::os::unix::fs::PermissionsExt;
    require_tools!();
    // Media from FAT/NTFS mounts shows 0777: outputs get rw only.
    for (src_mode, want) in [(0o644, 0o644), (0o640, 0o640), (0o777, 0o666)] {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("src");
        std::fs::create_dir_all(&root).unwrap();
        let src = support::fixture_jpeg(&root, "a.jpg", Some("2026:04:12 10:33:14"), None);
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(src_mode)).unwrap();
        let out = d.path().join("out");
        let plan = save_plan(
            d.path(),
            vec![item(
                &src,
                &root,
                vec![
                    (out.join("copy.jpg"), vec![]),
                    (out.join("meta.jpg"), vec!["Metadata"]),
                    (out.join("a.avif"), vec!["AVIF"]),
                ],
            )],
        );
        let rep = run(&plan, &opts());
        let r = &rep.items[0];
        assert!(
            r.outputs.iter().all(|o| o.error.is_none()),
            "{:?}",
            r.outputs
        );
        for o in &r.outputs {
            let p = o.actual.as_ref().unwrap();
            assert_eq!(mode(p), want, "{} (source {src_mode:o})", p.display());
        }
    }
}

/// exiftool's `-G1` record of `p` for the given tags.
fn tags_of(p: &Path, tags: &[&str]) -> serde_json::Value {
    let out = std::process::Command::new("exiftool")
        .args(["-j", "-G1", "-charset", "filename=utf8"])
        .args(tags.iter().map(|t| format!("-{t}")))
        .arg(p)
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    v[0].clone()
}

#[test]
fn apply_writes_ai_tags_into_processed_outputs_only() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    let photo = support::fixture_jpeg(
        &root,
        "a.jpg",
        Some("2026:04:12 10:33:14"),
        Some((52.52, 13.405)),
    );
    // A keyword the source already has stays, and is not repeated.
    support::exiftool_set(&photo, &["-XMP-dc:Subject=семья", "-XMP-dc:Subject=кот"]);
    let clip = support::fixture_mp4(&root, "v.mp4", 1, Some("2026:04:12 08:33:14"));
    let orig = hash(&photo);
    let meta = AiMeta {
        tags: vec!["кот".into(), "диван".into()],
        description: "Рыжий кот спит на диване.".into(),
    };
    let ai = |mut it: PlanItem| {
        it.via = Via::Ai {
            reason: "кот".into(),
            low: false,
        };
        it.ai_meta = Some(meta.clone());
        it
    };
    let out = d.path().join("out");
    let plan = save_plan(
        d.path(),
        vec![
            ai(item(
                &photo,
                &root,
                vec![
                    (out.join("copy.jpg"), vec![]),
                    (out.join("meta.jpg"), vec!["Metadata"]),
                    (out.join("small.avif"), vec!["AVIF"]),
                ],
            )),
            ai(item(
                &clip,
                &root,
                vec![(out.join("v.mp4"), vec!["Metadata"])],
            )),
        ],
    );
    let rep = run(&plan, &opts());
    for r in &rep.items {
        assert!(matches!(r.status, ItemStatus::Done { .. }), "{r:?}");
        for o in &r.outputs {
            assert!(o.error.is_none() && o.anomalies.is_empty(), "{o:?}");
            assert!(o.warnings.is_empty(), "{o:?}");
        }
    }

    // The byte copy is the source exactly.
    assert_eq!(hash(&out.join("copy.jpg")), orig);
    assert_eq!(
        tags_of(&out.join("copy.jpg"), &["XMP-dc:Subject"])["XMP-dc:Subject"],
        serde_json::json!(["семья", "кот"])
    );

    let jpeg = tags_of(
        &out.join("meta.jpg"),
        &["XMP-dc:Subject", "XMP-dc:Description", "IPTC:Keywords"],
    );
    assert_eq!(
        jpeg["XMP-dc:Subject"],
        serde_json::json!(["семья", "кот", "диван"]),
        "{jpeg}"
    );
    assert_eq!(jpeg["XMP-dc:Description"], "Рыжий кот спит на диване.");
    assert_eq!(jpeg["IPTC:Keywords"], serde_json::json!(["кот", "диван"]));

    // The AVIF encoder carries the source's XMP over; the clip had none.
    for (p, subject) in [
        (
            out.join("small.avif"),
            serde_json::json!(["семья", "кот", "диван"]),
        ),
        (out.join("v.mp4"), serde_json::json!(["кот", "диван"])),
    ] {
        let t = tags_of(&p, &["XMP-dc:Subject", "XMP-dc:Description"]);
        assert_eq!(t["XMP-dc:Subject"], subject, "{p:?}: {t}");
        assert_eq!(t["XMP-dc:Description"], "Рыжий кот спит на диване.");
    }
}

#[test]
fn apply_warns_when_the_format_takes_no_ai_tags() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    let clip = support::fixture_mkv(&root, "v.mkv");
    let mut it = item(
        &clip,
        &root,
        vec![(d.path().join("out/v.mkv"), vec!["Metadata"])],
    );
    it.via = Via::Ai {
        reason: "cat".into(),
        low: false,
    };
    it.ai_meta = Some(AiMeta {
        tags: vec!["cat".into()],
        description: "A cat.".into(),
    });
    let rep = run(&save_plan(d.path(), vec![it]), &opts());
    let r = &rep.items[0];
    // Keywords are a nicety: the file is placed all the same.
    assert!(matches!(r.status, ItemStatus::Done { .. }), "{r:?}");
    let w = &r.outputs[0].warnings;
    assert!(w.len() == 1 && w[0].contains("mkv"), "{w:?}");
    assert!(d.path().join("out/v.mkv").exists());
}

#[test]
fn apply_keeps_an_existing_description_and_merges_tags() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    let photo = support::fixture_jpeg(&root, "a.jpg", Some("2026:04:12 10:33:14"), None);
    support::exiftool_set(
        &photo,
        &[
            "-XMP-dc:Description=Бабушкин кот, 2026.",
            "-XMP-dc:Subject=семья",
            "-IPTC:CodedCharacterSet=UTF8",
            "-IPTC:Keywords=семья",
        ],
    );
    let mut it = item(
        &photo,
        &root,
        vec![(d.path().join("out/a.jpg"), vec!["Metadata"])],
    );
    it.via = Via::Ai {
        reason: "кот".into(),
        low: false,
    };
    it.ai_meta = Some(AiMeta {
        tags: vec!["кот".into(), "семья".into()],
        description: "Рыжий кот спит на диване.".into(),
    });
    let rep = run(&save_plan(d.path(), vec![it]), &opts());
    let r = &rep.items[0];
    assert!(matches!(r.status, ItemStatus::Done { .. }), "{r:?}");
    assert!(r.outputs[0].warnings.is_empty() && r.outputs[0].anomalies.is_empty());
    let t = tags_of(
        &d.path().join("out/a.jpg"),
        &["XMP-dc:Subject", "XMP-dc:Description", "IPTC:Keywords"],
    );
    // The user's description is never overwritten; keywords merge, once each.
    assert_eq!(t["XMP-dc:Description"], "Бабушкин кот, 2026.", "{t}");
    let sorted = |v: &serde_json::Value| {
        let mut v: Vec<String> = serde_json::from_value(v.clone()).unwrap();
        v.sort();
        v
    };
    assert_eq!(sorted(&t["XMP-dc:Subject"]), ["кот", "семья"], "{t}");
    assert_eq!(sorted(&t["IPTC:Keywords"]), ["кот", "семья"], "{t}");
}
