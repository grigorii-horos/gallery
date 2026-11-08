mod support;

use gallery_core::Error;
use gallery_core::apply::{ApplyOptions, ItemResult, ItemStatus, Report, apply, report_path};
use gallery_core::config::{ContentPreset, MetadataPreset, VideoPreset};
use gallery_core::plan::{OutputOp, PLAN_VERSION, Plan, PlanItem, PlannedOutput, Side};
use gallery_core::route::Via;
use gallery_core::tools::Tools;
use gallery_core::trash::{TrashMode, TrashRef};
use gallery_core::undo::undo;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

macro_rules! require_tools {
    () => {
        if !["ffmpeg", "ffprobe", "exiftool", "avifenc", "magick"]
            .iter()
            .all(|t| support::tool_available(t))
        {
            eprintln!("skip: tools not found");
            return;
        }
    };
}

fn hash(p: &Path) -> blake3::Hash {
    blake3::hash(&std::fs::read(p).unwrap())
}

/// Plan item for an existing `src` with outputs `(dest, steps)`.
fn item(src: &Path, root: &Path, outs: Vec<(PathBuf, Vec<&str>)>) -> PlanItem {
    let meta = std::fs::metadata(src).unwrap();
    PlanItem {
        source: src.to_path_buf(),
        source_root: root.to_path_buf(),
        size: meta.len(),
        mtime_ns: meta
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i128,
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
    }
}

fn save_plan(dir: &Path, items: Vec<PlanItem>) -> PathBuf {
    let mut presets = BTreeMap::new();
    presets.insert(
        "Metadata".into(),
        ContentPreset::Metadata(MetadataPreset::full()),
    );
    presets.insert(
        "Video".into(),
        ContentPreset::Video(VideoPreset {
            preset: 12,
            ..VideoPreset::high()
        }),
    );
    let plan = Plan {
        version: PLAN_VERSION,
        created: chrono::Local::now().to_rfc3339(),
        side: Side::Local,
        tz: None,
        content_presets: presets,
        items,
        review: vec![],
        unmatched: vec![],
        warnings: vec![],
        unchanged: vec![],
        retry_of: None,
    };
    let path = dir.join("plan.json");
    plan.save(&path).unwrap();
    path
}

fn run(plan: &Path) -> Report {
    let o = ApplyOptions {
        dry_run: false,
        jobs: 2,
        trash: TrashMode::Folder,
        ..Default::default()
    };
    apply(plan, &Tools::discover().unwrap(), &o).unwrap()
}

fn trashed_path(r: &ItemResult) -> PathBuf {
    match &r.status {
        ItemStatus::Done {
            trashed: Some(TrashRef::Folder { path }),
        } => path.clone(),
        other => panic!("expected Done in folder trash, got {other:?} / {r:?}"),
    }
}

fn actual(r: &ItemResult, k: usize) -> PathBuf {
    r.outputs[k].actual.clone().expect("placed output")
}

#[test]
fn undo_restores_and_removes() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    let a = support::fixture_jpeg(&root, "a.jpg", None, None);
    let b = support::fixture_jpeg(&root.join("sub"), "b.jpg", None, None);
    let (ha, hb) = (hash(&a), hash(&b));
    let out = d.path().join("out");
    let plan = save_plan(
        d.path(),
        vec![
            item(&a, &root, vec![(out.join("a.jpg"), vec!["Metadata"])]),
            item(
                &b,
                &root,
                vec![
                    (out.join("b.jpg"), vec![]),
                    (out.join("copy/b.jpg"), vec!["Metadata"]),
                ],
            ),
        ],
    );
    let rep = run(&plan);
    let outs = [
        actual(&rep.items[0], 0),
        actual(&rep.items[1], 0),
        actual(&rep.items[1], 1),
    ];
    assert!(!a.exists() && !b.exists());

    let s = undo(&report_path(&plan)).unwrap();
    assert_eq!(s.problems, Vec::<String>::new());
    assert_eq!((s.restored, s.removed_outputs), (2, 3));
    assert_eq!(hash(&a), ha);
    assert_eq!(hash(&b), hb);
    for o in &outs {
        assert!(!o.exists(), "{o:?} not removed");
    }
    assert!(!trashed_path(&rep.items[0]).exists());
    let after: Report =
        serde_json::from_str(&std::fs::read_to_string(report_path(&plan)).unwrap()).unwrap();
    assert!(after.undone);
    assert_eq!(after.items, rep.items);
}

#[test]
fn undo_uses_actual_paths() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    // No video stream: the output falls back to the input's own extension.
    let wav = support::fixture_wav(&root, "voice.wav");
    let jpg = support::fixture_jpeg(&root, "a.jpg", None, None);
    let out = d.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    // Taken: the jpeg output lands under another name.
    std::fs::write(out.join("a.jpg"), b"user file").unwrap();
    let plan = save_plan(
        d.path(),
        vec![
            item(
                &wav,
                &root,
                vec![(out.join("voice.av1.mp4"), vec!["Video"])],
            ),
            item(&jpg, &root, vec![(out.join("a.jpg"), vec![])]),
        ],
    );
    let rep = run(&plan);
    assert!(rep.items[0].outputs[0].fell_back);
    let (wav_out, jpg_out) = (actual(&rep.items[0], 0), actual(&rep.items[1], 0));
    assert_eq!(wav_out, out.join("voice.wav"));
    assert_ne!(jpg_out, out.join("a.jpg"));

    let s = undo(&report_path(&plan)).unwrap();
    assert_eq!(s.problems, Vec::<String>::new());
    assert_eq!((s.restored, s.removed_outputs), (2, 2));
    assert!(wav.exists() && jpg.exists());
    assert!(!wav_out.exists() && !jpg_out.exists());
    assert_eq!(std::fs::read(out.join("a.jpg")).unwrap(), b"user file");
}

#[test]
fn undo_twice_errors() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    let a = support::fixture_jpeg(&root, "a.jpg", None, None);
    let plan = save_plan(
        d.path(),
        vec![item(&a, &root, vec![(d.path().join("out/a.jpg"), vec![])])],
    );
    run(&plan);
    undo(&report_path(&plan)).unwrap();
    let before = std::fs::read_to_string(report_path(&plan)).unwrap();
    match undo(&report_path(&plan)) {
        Err(Error::Plan(m)) => assert_eq!(m, "already undone"),
        other => panic!("{other:?}"),
    }
    assert!(a.exists());
    assert_eq!(std::fs::read_to_string(report_path(&plan)).unwrap(), before);
}

#[test]
fn undo_does_not_overwrite_restored_place() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    let a = support::fixture_jpeg(&root, "a.jpg", None, None);
    let plan = save_plan(
        d.path(),
        vec![item(&a, &root, vec![(d.path().join("out/a.jpg"), vec![])])],
    );
    let rep = run(&plan);
    let trashed = trashed_path(&rep.items[0]);
    let th = hash(&trashed);
    let out = actual(&rep.items[0], 0);
    std::fs::write(&a, b"new file").unwrap();

    let s = undo(&report_path(&plan)).unwrap();
    assert_eq!((s.restored, s.removed_outputs), (0, 0));
    assert_eq!(s.problems.len(), 1, "{:?}", s.problems);
    assert_eq!(std::fs::read(&a).unwrap(), b"new file");
    assert_eq!(hash(&trashed), th);
    // The source was not restored: its output may be the only copy.
    assert!(out.exists());
}

#[test]
fn undo_in_place_output() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    let a = support::fixture_jpeg(&root, "a.jpg", Some("2026:01:02 03:04:05"), None);
    let ha = hash(&a);
    let plan = save_plan(
        d.path(),
        vec![item(&a, &root, vec![(a.clone(), vec!["Metadata"])])],
    );
    let rep = run(&plan);
    assert_eq!(actual(&rep.items[0], 0), a);
    assert_ne!(
        hash(&a),
        ha,
        "the in-place output must differ for this test"
    );

    let s = undo(&report_path(&plan)).unwrap();
    assert_eq!(s.problems, Vec::<String>::new());
    assert_eq!((s.restored, s.removed_outputs), (1, 1));
    assert_eq!(hash(&a), ha);
    let left: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != ".gallery-trash")
        .collect();
    assert_eq!(left, ["a.jpg"]);
}

#[test]
fn undo_keeps_outputs_when_source_is_gone_from_trash() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    let a = support::fixture_jpeg(&root, "a.jpg", None, None);
    let plan = save_plan(
        d.path(),
        vec![item(&a, &root, vec![(d.path().join("out/a.jpg"), vec![])])],
    );
    let rep = run(&plan);
    std::fs::remove_file(trashed_path(&rep.items[0])).unwrap();
    let out = actual(&rep.items[0], 0);

    let s = undo(&report_path(&plan)).unwrap();
    assert_eq!((s.restored, s.removed_outputs), (0, 0));
    assert_eq!(s.problems.len(), 1, "{:?}", s.problems);
    assert!(out.exists());
    assert!(!a.exists());
}

#[test]
fn undo_keeps_changed_output() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    let a = support::fixture_jpeg(&root, "a.jpg", None, None);
    let plan = save_plan(
        d.path(),
        vec![item(&a, &root, vec![(d.path().join("out/a.jpg"), vec![])])],
    );
    let rep = run(&plan);
    let out = actual(&rep.items[0], 0);
    std::fs::write(&out, b"edited after apply").unwrap();

    let s = undo(&report_path(&plan)).unwrap();
    assert_eq!((s.restored, s.removed_outputs), (1, 0));
    assert_eq!(s.problems.len(), 1, "{:?}", s.problems);
    assert!(a.exists());
    assert_eq!(std::fs::read(&out).unwrap(), b"edited after apply");
}

#[test]
fn undo_keeps_output_edited_with_same_size() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    let a = support::fixture_jpeg(&root, "a.jpg", None, None);
    let plan = save_plan(
        d.path(),
        vec![item(&a, &root, vec![(d.path().join("out/a.jpg"), vec![])])],
    );
    let rep = run(&plan);
    let out = actual(&rep.items[0], 0);
    assert!(rep.items[0].outputs[0].hash.is_some());
    let mut bytes = std::fs::read(&out).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&out, &bytes).unwrap();

    let s = undo(&report_path(&plan)).unwrap();
    assert_eq!((s.restored, s.removed_outputs), (1, 0));
    assert_eq!(s.problems.len(), 1, "{:?}", s.problems);
    assert_eq!(std::fs::read(&out).unwrap(), bytes);
}
