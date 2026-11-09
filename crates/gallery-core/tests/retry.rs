mod support;

use gallery_core::apply::{ApplyOptions, ItemStatus, Report, apply, report_path};
use gallery_core::plan::{OutputOp, PLAN_VERSION, Plan, PlanItem, PlannedOutput, Side};
use gallery_core::route::Via;
use gallery_core::tools::Tools;
use gallery_core::trash::TrashMode;
use gallery_core::undo::{prepare_retry, undo, undo_items};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

fn hash(p: &Path) -> blake3::Hash {
    blake3::hash(&std::fs::read(p).unwrap())
}

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

fn save(path: &Path, items: Vec<PlanItem>) {
    Plan {
        version: PLAN_VERSION,
        created: chrono::Local::now().to_rfc3339(),
        side: Side::Local,
        tz: None,
        content_presets: BTreeMap::new(),
        items,
        review: vec![],
        unmatched: vec![],
        warnings: vec![],
        unchanged: vec![],
        retry_of: None,
        kept: Vec::new(),
    }
    .save(path)
    .unwrap();
}

/// Byte copies of `src` to every `dests`.
fn item(src: &Path, root: &Path, dests: &[PathBuf]) -> PlanItem {
    let meta = std::fs::metadata(src).unwrap();
    PlanItem {
        ai_meta: None,
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
        outputs: dests
            .iter()
            .map(|d| PlannedOutput {
                dest: d.clone(),
                op: OutputOp::Write,
                steps: vec![],
                trace: vec![],
            })
            .collect(),
        kind: Default::default(),
        captured: None,
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

fn load_report(p: &Path) -> Report {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// The state of a failure after the first placement: every `(source, first,
/// second)` has its first copy placed, the second output failed and the source
/// is untouched. (A failure while producing now stops an item before anything
/// is placed, so this state is built by running the first output alone and
/// recording the second as failed.) Leaves the full plan at `plan`.
fn partial_failures(plan: &Path, tools: &Tools, root: &Path, items: &[(&Path, &Path, &Path)]) {
    save(
        plan,
        items
            .iter()
            .map(|(s, f, _)| item(s, root, &[f.to_path_buf()]))
            .collect(),
    );
    let rep = apply(plan, tools, &opts()).unwrap();
    let rp = report_path(plan);
    let mut json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&rp).unwrap()).unwrap();
    for (i, (src, _, second)) in items.iter().enumerate() {
        let trashed = match &rep.items[i].status {
            ItemStatus::Done {
                trashed: Some(gallery_core::trash::TrashRef::Folder { path }),
            } => path.clone(),
            other => panic!("{other:?}"),
        };
        std::fs::rename(trashed, src).unwrap();
        let it = &mut json["items"][i];
        it["status"] = serde_json::json!({"state": "failed", "trashed": null});
        it["outputs"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "planned": second, "actual": null, "error": "test: second output failed",
                "warnings": [], "fell_back": false
            }));
    }
    std::fs::write(&rp, serde_json::to_string_pretty(&json).unwrap()).unwrap();
    save(
        plan,
        items
            .iter()
            .map(|(s, f, sec)| item(s, root, &[f.to_path_buf(), sec.to_path_buf()]))
            .collect(),
    );
}

#[test]
fn retry_removes_placed_outputs_first_so_nothing_is_duplicated() {
    if !support::tool_available("exiftool") {
        eprintln!("skip: exiftool not found");
        return;
    }
    let tools = Tools::discover().unwrap();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    let src = support::fixture_jpeg(&root, "a.jpg", None, None);
    let orig = hash(&src);
    let (o1, ro) = (d.path().join("out1"), d.path().join("ro"));
    std::fs::create_dir_all(&ro).unwrap();
    let plan = d.path().join("plan.json");

    // First run: the first copy is placed, the second fails.
    partial_failures(
        &plan,
        &tools,
        &root,
        &[(&src, &o1.join("a.jpg"), &ro.join("a.jpg"))],
    );
    assert_eq!(names(&o1), ["a.jpg"]);

    // Retry: the placed output is undone in the old report first.
    let rp = report_path(&plan);
    let retry = prepare_retry(&plan, &rp).unwrap();
    assert_eq!(retry.items.len(), 1);
    assert_eq!(retry.items[0].source, src);
    assert!(names(&o1).is_empty(), "placed output removed");
    assert_eq!(hash(&src), orig, "source untouched");
    let old = load_report(&rp);
    assert_eq!(old.undone_sources, vec![src.clone()]);
    // Its only item is undone, so the old report is too.
    assert!(old.undone);

    // The retry run puts every output at its planned name.
    let retry_path = d.path().join("retry.json");
    retry.save(&retry_path).unwrap();
    let rep2 = apply(&retry_path, &tools, &opts()).unwrap();
    assert!(
        matches!(rep2.items[0].status, ItemStatus::Done { .. }),
        "{rep2:?}"
    );
    assert_eq!(names(&o1), ["a.jpg"], "no duplicate");
    assert_eq!(names(&ro), ["a.jpg"]);
    assert_eq!(hash(&o1.join("a.jpg")), orig);

    // The old report cannot be undone again: the retry's outputs stay.
    assert!(undo(&rp).is_err());
    assert_eq!(names(&o1), ["a.jpg"]);
    assert_eq!(names(&ro), ["a.jpg"]);
    // And the retry itself can be undone: the source comes back.
    undo(&report_path(&retry_path)).unwrap();
    assert_eq!(hash(&src), orig);
    assert!(names(&o1).is_empty() && names(&ro).is_empty());
}

#[test]
fn retry_of_an_unfinished_run_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let plan = d.path().join("plan.json");
    save(&plan, vec![]);
    let rp = report_path(&plan);
    std::fs::write(
        &rp,
        r#"{"run_id":"20261101-000000","plan":"plan.json","started":"2026-11-01T00:00:00+00:00","finished":"","items":[],"tool_warnings":[]}"#,
    )
    .unwrap();
    assert!(prepare_retry(&plan, &rp).is_err());
}

#[test]
fn undo_items_touches_only_the_given_items() {
    if !support::tool_available("exiftool") {
        eprintln!("skip: exiftool not found");
        return;
    }
    let tools = Tools::discover().unwrap();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("src");
    std::fs::create_dir_all(&root).unwrap();
    let a = support::fixture_jpeg(&root, "a.jpg", None, None);
    let b = support::fixture_jpeg(&root, "b.jpg", None, None);
    let (out, ro) = (d.path().join("out"), d.path().join("ro"));
    std::fs::create_dir_all(&ro).unwrap();
    let plan = d.path().join("plan.json");
    partial_failures(
        &plan,
        &tools,
        &root,
        &[
            (&a, &out.join("a.jpg"), &ro.join("a.jpg")),
            (&b, &out.join("b.jpg"), &ro.join("b.jpg")),
        ],
    );
    assert_eq!(names(&out), ["a.jpg", "b.jpg"]);

    let rp = report_path(&plan);
    let s = undo_items(&rp, std::slice::from_ref(&a)).unwrap();
    assert!(s.problems.is_empty(), "{s:?}");
    assert_eq!(names(&out), ["b.jpg"]);
    let r = load_report(&rp);
    assert_eq!(r.undone_sources, vec![a.clone()]);
    assert!(!r.undone, "other items are not undone yet");
    assert!(a.is_file() && b.is_file());

    // A full undo finishes the rest and closes the report.
    undo(&rp).unwrap();
    assert!(names(&out).is_empty());
    assert!(load_report(&rp).undone);
}
