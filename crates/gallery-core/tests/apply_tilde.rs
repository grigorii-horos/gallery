//! Own test binary: it changes HOME for the whole process.

mod support;

use gallery_core::apply::{ApplyOptions, ItemStatus, apply};
use gallery_core::plan::{OutputOp, PLAN_VERSION, Plan, PlanItem, PlannedOutput, Side};
use gallery_core::route::Via;
use gallery_core::tools::Tools;
use gallery_core::trash::{TrashMode, TrashRef};
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

/// A server plan keeps `~` literal; apply expands it with its own HOME.
#[test]
fn apply_expands_tilde_with_local_home() {
    require_tool!("exiftool");
    require_tool!("ffmpeg");
    let home = tempfile::tempdir().unwrap();
    // SAFETY: the only test of this binary; no other thread reads the env yet.
    unsafe { std::env::set_var("HOME", home.path()) };
    let tools = Tools::discover().unwrap();

    let root = home.path().join("phone");
    std::fs::create_dir_all(&root).unwrap();
    let src = root.join("a.jpg");
    std::fs::write(&src, b"jpeg bytes").unwrap();
    let meta = std::fs::metadata(&src).unwrap();
    let plan = Plan {
        version: PLAN_VERSION,
        created: chrono::Local::now().to_rfc3339(),
        side: Side::Server,
        tz: None,
        content_presets: Default::default(),
        items: vec![PlanItem {
            ai_meta: None,
            source: PathBuf::from("~/phone/a.jpg"),
            source_root: PathBuf::from("~/phone"),
            size: meta.len(),
            mtime_ns: meta
                .modified()
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos() as i128,
            target: "t".into(),
            via: Via::Mask,
            outputs: vec![PlannedOutput {
                dest: PathBuf::from("~/gallery/a.jpg"),
                op: OutputOp::Write,
                steps: vec![],
                trace: vec![],
            }],
            kind: Default::default(),
        }],
        review: vec![],
        unmatched: vec![],
        warnings: vec![],
        unchanged: vec![],
        retry_of: None,
        kept: Vec::new(),
    };
    let plan_path = home.path().join("plan.json");
    plan.save(&plan_path).unwrap();
    let opts = ApplyOptions {
        dry_run: false,
        jobs: 1,
        trash: TrashMode::Folder,
        ..Default::default()
    };
    let rep = apply(&plan_path, &tools, &opts).unwrap();
    let r = &rep.items[0];
    assert_eq!(r.source, src);
    let out = home.path().join("gallery/a.jpg");
    assert_eq!(r.outputs[0].actual.as_deref(), Some(out.as_path()));
    assert_eq!(std::fs::read(&out).unwrap(), b"jpeg bytes");
    match &r.status {
        ItemStatus::Done {
            trashed: Some(TrashRef::Folder { path }),
        } => assert!(path.starts_with(root.join(".gallery-trash")), "{path:?}"),
        other => panic!("{other:?}"),
    }
    assert!(!home.path().join("~").exists());
}
