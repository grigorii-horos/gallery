//! `trash` and `keep` targets in apply and undo (plan version 2), and the
//! version gate.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use super::*;
use crate::plan::{KeptItem, PLAN_VERSION, Side};
use crate::route::Via;
use crate::trash::TrashRef;

fn no_tools() -> Tools {
    let p = PathBuf::from("/nonexistent/tool");
    Tools {
        ffmpeg: p.clone(),
        ffprobe: p.clone(),
        exiftool: p.clone(),
        avifenc: p.clone(),
        magick: p,
        optipng: None,
        tz: None,
    }
}

fn caps() -> Capabilities {
    Capabilities {
        svtav1: true,
        opus: true,
        vidstab: true,
        versions: BTreeMap::new(),
    }
}

fn opts() -> ApplyOptions {
    ApplyOptions {
        dry_run: false,
        jobs: 1,
        trash: TrashMode::Folder,
        ..Default::default()
    }
}

/// Write `body` at `p` with an mtime well in the past.
fn put(p: &Path, body: &[u8]) -> SystemTime {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
    let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    std::fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(t)
        .unwrap();
    std::fs::metadata(p).unwrap().modified().unwrap()
}

fn item(root: &Path, src: &Path, target: &str, kind: TargetKind) -> PlanItem {
    let meta = std::fs::metadata(src).unwrap();
    PlanItem {
        ai_meta: None,
        source: src.to_path_buf(),
        source_root: root.to_path_buf(),
        size: meta.len(),
        mtime_ns: mtime_ns(&meta).unwrap(),
        target: target.into(),
        via: Via::Mask,
        kind,
        outputs: Vec::new(),
    }
}

fn plan(items: Vec<PlanItem>, kept: Vec<KeptItem>) -> Plan {
    Plan {
        version: PLAN_VERSION,
        created: String::new(),
        side: Side::Local,
        tz: None,
        content_presets: BTreeMap::new(),
        items,
        review: vec![],
        unmatched: vec![],
        warnings: vec![],
        unchanged: vec![],
        kept,
        retry_of: None,
    }
}

/// `src/junk.jpg` for a trash target listed FIRST, then `src/a.jpg` that a
/// folders target moves to `src/sorted/a.jpg` (a rename: no tool needed).
fn fixture(d: &Path) -> (PathBuf, PathBuf, SystemTime) {
    let root = d.join("src");
    let junk = root.join("junk.jpg");
    let mtime = put(&junk, b"junk bytes");
    let a = root.join("a.jpg");
    put(&a, b"keeper");
    let mut moved = item(&root, &a, "Album", TargetKind::Folders);
    moved.outputs = vec![PlannedOutput {
        dest: root.join("sorted/a.jpg"),
        op: OutputOp::Rename,
        steps: vec![],
        trace: vec![],
    }];
    let p = plan(
        vec![item(&root, &junk, "Junk", TargetKind::Trash), moved],
        vec![],
    );
    let path = d.join("plan.json");
    p.save(&path).unwrap();
    (path, junk, mtime)
}

#[test]
fn trash_item_goes_to_the_trash_after_every_placement_and_undo_restores_it() {
    let d = tempfile::tempdir().unwrap();
    let (plan_path, junk, mtime) = fixture(d.path());
    let order = Mutex::new(Vec::new());
    let report = apply_impl(
        &plan_path,
        &no_tools(),
        &caps(),
        &opts(),
        &NoProgress,
        &Cancel::new(),
        &|r: &ItemResult| order.lock().unwrap().push(r.source.clone()),
    )
    .unwrap();
    // Listed first, done last: placements come before the trash.
    let order = order.into_inner().unwrap();
    assert_eq!(order.last(), Some(&junk), "{order:?}");
    // The report lists it, in plan order, with where it went.
    let r = &report.items[0];
    assert_eq!(r.source, junk);
    assert!(r.outputs.is_empty() && r.error.is_none(), "{r:?}");
    let ItemStatus::Done {
        trashed: Some(TrashRef::Folder { path }),
    } = &r.status
    else {
        panic!("{r:?}");
    };
    assert!(!junk.exists() && path.exists());
    assert!(d.path().join("src/sorted/a.jpg").exists());
    let on_disk: Report =
        serde_json::from_str(&std::fs::read_to_string(report_path(&plan_path)).unwrap()).unwrap();
    assert_eq!(on_disk.items, report.items);

    let s = crate::undo::undo(&report_path(&plan_path)).unwrap();
    assert_eq!(s.problems, Vec::<String>::new());
    assert_eq!(std::fs::read(&junk).unwrap(), b"junk bytes");
    assert_eq!(std::fs::metadata(&junk).unwrap().modified().unwrap(), mtime);
    assert!(d.path().join("src/a.jpg").exists());
}

#[test]
fn trash_item_is_journaled_before_the_run_can_die() {
    let d = tempfile::tempdir().unwrap();
    let (plan_path, junk, mtime) = fixture(d.path());
    CRASH_AFTER_TRASH.lock().unwrap().push(junk.clone());
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        apply_with_caps(&plan_path, &no_tools(), &caps(), &opts())
    }));
    assert!(crashed.is_err());
    assert!(!junk.exists());
    let rp = report_path(&plan_path);
    let run_id = serde_json::from_str::<Report>(&std::fs::read_to_string(&rp).unwrap())
        .unwrap()
        .run_id;
    let journaled = read_journal(&journal_path(&rp), &run_id).unwrap();
    let j = journaled
        .iter()
        .find(|r| r.source == junk)
        .expect("journaled");
    assert!(matches!(
        j.status,
        ItemStatus::Done {
            trashed: Some(TrashRef::Folder { .. })
        }
    ));

    let s = crate::undo::undo(&rp).unwrap();
    assert_eq!(s.problems, Vec::<String>::new());
    assert_eq!(std::fs::read(&junk).unwrap(), b"junk bytes");
    assert_eq!(std::fs::metadata(&junk).unwrap().modified().unwrap(), mtime);
}

#[test]
fn trash_item_dry_run_and_stale_touch_nothing() {
    let d = tempfile::tempdir().unwrap();
    let (plan_path, junk, _) = fixture(d.path());
    let dry = ApplyOptions {
        dry_run: true,
        ..opts()
    };
    let r = apply_with_caps(&plan_path, &no_tools(), &caps(), &dry).unwrap();
    assert_eq!(r.items[0].status, ItemStatus::DryRun);
    assert!(junk.exists());

    std::fs::write(&junk, b"changed since the plan").unwrap();
    let r = apply_with_caps(&plan_path, &no_tools(), &caps(), &opts()).unwrap();
    assert_eq!(r.items[0].status, ItemStatus::Stale);
    assert_eq!(std::fs::read(&junk).unwrap(), b"changed since the plan");
}

#[test]
fn keep_items_are_untouched_and_counted() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    let k = root.join("k.jpg");
    let mtime = put(&k, b"kept");
    let p = plan(
        vec![],
        vec![KeptItem {
            source: k.clone(),
            target: "Keep".into(),
        }],
    );
    let path = d.path().join("plan.json");
    p.save(&path).unwrap();
    let r = apply_with_caps(&path, &no_tools(), &caps(), &opts()).unwrap();
    assert!(r.items.is_empty());
    assert_eq!(r.kept, 1);
    assert_eq!(std::fs::read(&k).unwrap(), b"kept");
    assert_eq!(std::fs::metadata(&k).unwrap().modified().unwrap(), mtime);
    assert_eq!(
        std::fs::read_dir(&root).unwrap().count(),
        1,
        "no trash folder"
    );
    let on_disk: Report =
        serde_json::from_str(&std::fs::read_to_string(report_path(&path)).unwrap()).unwrap();
    assert_eq!(on_disk.kept, 1);
}

#[test]
fn version_1_plan_still_applies() {
    let d = tempfile::tempdir().unwrap();
    let (plan_path, junk, _) = fixture(d.path());
    // A version-1 plan: no `kind`, no `kept`; its only item is a folders one.
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&plan_path).unwrap()).unwrap();
    v["version"] = 1.into();
    let items = v["items"].as_array_mut().unwrap();
    items.remove(0);
    for it in items.iter_mut() {
        it.as_object_mut().unwrap().remove("kind");
    }
    std::fs::write(&plan_path, v.to_string()).unwrap();
    let r = apply_with_caps(&plan_path, &no_tools(), &caps(), &opts()).unwrap();
    assert_eq!(r.items.len(), 1);
    assert!(
        matches!(r.items[0].status, ItemStatus::Done { .. }),
        "{r:?}"
    );
    assert!(d.path().join("src/sorted/a.jpg").exists() && junk.exists());
}

#[test]
fn newer_plan_is_refused_before_anything_is_touched() {
    let d = tempfile::tempdir().unwrap();
    let (plan_path, junk, _) = fixture(d.path());
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&plan_path).unwrap()).unwrap();
    v["version"] = 3.into();
    std::fs::write(&plan_path, v.to_string()).unwrap();
    match apply_with_caps(&plan_path, &no_tools(), &caps(), &opts()) {
        Err(Error::Plan(m)) => assert!(m.contains(&crate::t!("plan.newer_version")), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(junk.exists() && d.path().join("src/a.jpg").exists());
    assert!(!d.path().join("src/sorted").exists());
    assert!(!report_path(&plan_path).exists());
}
