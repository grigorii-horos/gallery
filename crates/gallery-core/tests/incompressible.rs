//! A file the content pipe cannot make smaller: in place it is left as it is
//! (never copied over itself and trashed), and later scans do not encode it
//! again until the file or the pipe changes.

mod support;

use gallery_core::apply::{ApplyOptions, ItemResult, ItemStatus, Report, apply, report_path};
use gallery_core::config::{Config, ContentPreset, PhotoPreset};
use gallery_core::plan::{OutputOp, PLAN_VERSION, Plan, PlanItem, PlannedOutput, Side, build_plan};
use gallery_core::route::{Decision, Via};
use gallery_core::scan::FileEntry;
use gallery_core::tools::Tools;
use gallery_core::trash::{TrashMode, TrashRef};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

fn require_all() -> bool {
    ["exiftool", "avifenc", "magick"]
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

/// A tiny flat PNG: no AVIF of it comes out smaller.
fn tiny_png(dir: &Path, name: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    let out = Command::new("magick")
        .args(["-size", "8x8", "xc:white"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    path
}

fn hash(p: &Path) -> blake3::Hash {
    blake3::hash(&std::fs::read(p).unwrap())
}

fn mtime_ns(p: &Path) -> i128 {
    gallery_core::scan::mtime_ns(&std::fs::metadata(p).unwrap()).unwrap()
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

/// The PNG-to-AVIF preset of the tests, at AVIF `quality`.
fn shot_preset(quality: u8) -> ContentPreset {
    ContentPreset::Photo(PhotoPreset {
        mask: PhotoPreset::png_avif().mask,
        quality,
        ..PhotoPreset::avif_fast()
    })
}

/// Source `s` at `<d>/src`, target `t` writing `content = ["shot"]` into
/// `dir` (the source folder unless given), plus `extra` outputs.
fn config(d: &Path, dir: Option<&Path>, extra: &str, quality: u8) -> Config {
    let r = d.to_string_lossy().replace('\\', "/");
    let dir = dir.map_or(format!("{r}/src"), |p| {
        p.to_string_lossy().replace('\\', "/")
    });
    let toml = format!(
        "[[sources]]\nid = \"s\"\npath = \"{r}/src\"\n\
         [[targets.after_ai]]\nname = \"t\"\n[[targets.after_ai.filters]]\n\
         sources = [\"s\"]\nmatch = \"*.png\"\n\
         [[targets.after_ai.outputs]]\ndir = \"{dir}\"\ncontent = [\"shot\"]\n{extra}"
    );
    let mut c = Config::from_str_at(&toml, d, Path::new("t.toml")).unwrap();
    c.content_presets
        .insert("shot".into(), shot_preset(quality));
    c
}

fn entry(d: &Path, src: &Path) -> FileEntry {
    let meta = std::fs::metadata(src).unwrap();
    FileEntry {
        source_id: "s".into(),
        path: src.to_path_buf(),
        rel: src.strip_prefix(d.join("src")).unwrap().to_path_buf(),
        size: meta.len(),
        mtime_ns: gallery_core::scan::mtime_ns(&meta).unwrap(),
    }
}

fn scan_plan(c: &Config, d: &Path, src: &Path) -> Plan {
    let routed = vec![(
        entry(d, src),
        Decision::Target {
            name: "t".into(),
            via: Via::Mask,
            ai_meta: None,
        },
    )];
    build_plan(c, routed, &HashMap::new(), Side::Local).unwrap()
}

fn opts() -> ApplyOptions {
    ApplyOptions {
        dry_run: false,
        jobs: 1,
        trash: TrashMode::Folder,
        ..Default::default()
    }
}

fn run_plan(d: &Path, plan: &Plan) -> (PathBuf, Report) {
    let path = d.join("plan.json");
    plan.save(&path).unwrap();
    let rep = apply(&path, &Tools::discover().unwrap(), &opts()).unwrap();
    (path, rep)
}

fn smallest() -> String {
    gallery_core::t!("apply.already_smallest")
}

fn untouched(r: &ItemResult) {
    assert_eq!(r.status, ItemStatus::Done { trashed: None }, "{r:?}");
}

#[test]
fn in_place_result_not_smaller_leaves_the_source_and_is_not_planned_again() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let d = d.path();
    let root = d.join("src");
    let src = tiny_png(&root, "shot.png");
    let (orig, orig_mtime) = (hash(&src), mtime_ns(&src));
    let c = config(d, None, "", 72);

    let plan = scan_plan(&c, d, &src);
    assert_eq!(plan.items.len(), 1, "{plan:?}");
    assert_eq!(plan.items[0].outputs[0].dest, root.join("shot.avif"));
    let (plan_path, rep) = run_plan(d, &plan);
    let r = &rep.items[0];
    untouched(r);
    let o = &r.outputs[0];
    assert!(o.fell_back && o.error.is_none(), "{o:?}");
    assert!(o.warnings.contains(&smallest()), "{o:?}");
    assert_eq!(o.actual.as_deref(), Some(src.as_path()));
    assert!(o.anomalies.is_empty());
    // Not written over itself, not in the trash: the very same file.
    assert_eq!(hash(&src), orig);
    assert_eq!(mtime_ns(&src), orig_mtime);
    assert_eq!(names(&root), ["shot.png"]);

    // Undo has nothing to do and leaves the file.
    let u = gallery_core::undo::undo(&report_path(&plan_path)).unwrap();
    assert_eq!((u.restored, u.removed_outputs), (0, 0), "{u:?}");
    assert!(u.problems.is_empty(), "{u:?}");
    assert_eq!(hash(&src), orig);

    // The next scan knows it: nothing to do, no encode.
    let again = scan_plan(&c, d, &src);
    assert!(again.items.is_empty(), "{again:?}");
    assert_eq!(again.unchanged, std::slice::from_ref(&src));

    // Another pipe (other settings) tries again.
    let other = scan_plan(&config(d, None, "", 60), d, &src);
    assert_eq!(other.items.len(), 1, "{other:?}");
    assert_eq!(other.items[0].outputs[0].op, OutputOp::Write);

    // So does a changed file (a new mtime).
    let f = std::fs::File::options().write(true).open(&src).unwrap();
    f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
        .unwrap();
    drop(f);
    let touched = scan_plan(&c, d, &src);
    assert_eq!(touched.items.len(), 1, "{touched:?}");
    assert_eq!(touched.items[0].outputs[0].op, OutputOp::Write);
}

#[test]
fn in_place_not_smaller_with_a_backup_writes_the_backup_and_keeps_the_source() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let d = d.path();
    let root = d.join("src");
    let src = tiny_png(&root, "shot.png");
    let orig = hash(&src);
    let backup = d.join("backup");
    let extra = format!(
        "[[targets.after_ai.outputs]]\ndir = \"{}\"\n",
        backup.to_string_lossy().replace('\\', "/")
    );
    let c = config(d, None, &extra, 72);

    let plan = scan_plan(&c, d, &src);
    let ops: Vec<OutputOp> = plan.items[0].outputs.iter().map(|o| o.op).collect();
    assert_eq!(ops, [OutputOp::Write, OutputOp::Write]);
    let (_, rep) = run_plan(d, &plan);
    let r = &rep.items[0];
    // The in-place output is the source itself: it stays, nothing is trashed.
    untouched(r);
    assert!(r.outputs[0].warnings.contains(&smallest()));
    assert_eq!(
        r.outputs[1].actual.as_deref(),
        Some(backup.join("shot.png").as_path())
    );
    assert_eq!(hash(&backup.join("shot.png")), orig);
    assert_eq!(hash(&src), orig);
    assert_eq!(names(&root), ["shot.png"]);

    // Next scan: kept in place, and the backup already holds these bytes.
    let again = scan_plan(&c, d, &src);
    assert!(again.items.is_empty(), "{again:?}");
    assert_eq!(again.unchanged, std::slice::from_ref(&src));
    assert_eq!(names(&backup), ["shot.png"]);
}

/// A hand-made plan of one item with outputs `(dest, steps)`.
fn plan_of(src: &Path, root: &Path, outs: Vec<(PathBuf, Vec<&str>)>) -> Plan {
    let meta = std::fs::metadata(src).unwrap();
    let mut presets = BTreeMap::new();
    presets.insert("shot".to_string(), shot_preset(72));
    Plan {
        version: PLAN_VERSION,
        created: chrono::Local::now().to_rfc3339(),
        side: Side::Local,
        tz: None,
        content_presets: presets,
        items: vec![PlanItem {
            ai_meta: None,
            source: src.to_path_buf(),
            source_root: root.to_path_buf(),
            size: meta.len(),
            mtime_ns: gallery_core::scan::mtime_ns(&meta).unwrap(),
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
            captured: None,
        }],
        review: vec![],
        unmatched: vec![],
        warnings: vec![],
        unchanged: vec![],
        kept: vec![],
        retry_of: None,
    }
}

#[test]
fn not_smaller_elsewhere_is_still_copied_and_the_source_trashed() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let d = d.path();
    let root = d.join("src");
    let src = tiny_png(&root, "shot.png");
    let orig = hash(&src);
    let out = d.join("out");
    let (_, rep) = run_plan(
        d,
        &plan_of(&src, &root, vec![(out.join("shot.avif"), vec!["shot"])]),
    );
    let r = &rep.items[0];
    let ItemStatus::Done {
        trashed: Some(TrashRef::Folder { path }),
    } = &r.status
    else {
        panic!("expected the source in the trash: {r:?}");
    };
    assert_eq!(hash(path), orig);
    assert!(r.outputs[0].fell_back);
    assert!(!r.outputs[0].warnings.contains(&smallest()));
    assert_eq!(
        r.outputs[0].actual.as_deref(),
        Some(out.join("shot.png").as_path())
    );
    assert_eq!(hash(&out.join("shot.png")), orig);
    assert!(!src.exists());
}

#[test]
fn not_smaller_under_a_new_name_in_its_folder_renames_the_source() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let d = d.path();
    let root = d.join("src");
    let src = tiny_png(&root, "shot.png");
    let (orig, orig_mtime) = (hash(&src), mtime_ns(&src));
    let renamed = root.join("20260412_103314.png");
    let (plan_path, rep) = run_plan(
        d,
        &plan_of(
            &src,
            &root,
            vec![(root.join("20260412_103314.avif"), vec!["shot"])],
        ),
    );
    let r = &rep.items[0];
    // Moved, not copied and trashed.
    untouched(r);
    let o = &r.outputs[0];
    assert!(o.warnings.contains(&smallest()), "{o:?}");
    assert_eq!(o.renamed_from.as_deref(), Some(src.as_path()));
    assert_eq!(o.actual.as_deref(), Some(renamed.as_path()));
    assert_eq!(names(&root), ["20260412_103314.png"]);
    assert_eq!((hash(&renamed), mtime_ns(&renamed)), (orig, orig_mtime));

    // Undo gives the old name back.
    let u = gallery_core::undo::undo(&report_path(&plan_path)).unwrap();
    assert!(u.problems.is_empty(), "{u:?}");
    assert_eq!(names(&root), ["shot.png"]);
    assert_eq!(hash(&src), orig);
}

/// Every file of a run is recorded (the store is written once, at the end,
/// with all of them), whatever worker finished it.
#[test]
fn every_file_of_a_parallel_run_is_remembered() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let d = d.path();
    let root = d.join("src");
    let srcs: Vec<PathBuf> = (0..6)
        .map(|i| tiny_png(&root, &format!("shot{i}.png")))
        .collect();
    let c = config(d, None, "", 72);
    let routed = |srcs: &[PathBuf]| -> Vec<(FileEntry, Decision)> {
        srcs.iter()
            .map(|s| {
                (
                    entry(d, s),
                    Decision::Target {
                        name: "t".into(),
                        via: Via::Mask,
                        ai_meta: None,
                    },
                )
            })
            .collect()
    };
    let plan = build_plan(&c, routed(&srcs), &HashMap::new(), Side::Local).unwrap();
    assert_eq!(plan.items.len(), 6);
    let path = d.join("plan.json");
    plan.save(&path).unwrap();
    let o = ApplyOptions { jobs: 4, ..opts() };
    let rep = apply(&path, &Tools::discover().unwrap(), &o).unwrap();
    for r in &rep.items {
        untouched(r);
    }
    let again = build_plan(&c, routed(&srcs), &HashMap::new(), Side::Local).unwrap();
    assert!(again.items.is_empty(), "{again:?}");
    assert_eq!(again.unchanged.len(), 6);
}

/// A metadata step after the encoder changes the fallback's bytes: the
/// first run replaces the source (as before), and the result is
/// remembered, so the next scan keeps it instead of looping.
#[test]
fn a_metadata_step_after_the_fallback_stops_after_one_run() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let d = d.path();
    let root = d.join("src");
    let src = tiny_png(&root, "shot.png");
    let orig = hash(&src);
    let mut c = config(d, None, "", 72);
    c.targets[0].outputs[0]
        .content
        .push(gallery_core::config::PipeStep::named("meta-full"));

    let plan = scan_plan(&c, d, &src);
    assert_eq!(plan.items[0].outputs[0].steps, ["shot", "meta-full"]);
    let (_, rep) = run_plan(d, &plan);
    let r = &rep.items[0];
    assert!(r.outputs[0].fell_back, "{r:?}");
    assert_eq!(r.outputs[0].actual.as_deref(), Some(src.as_path()));
    if hash(&src) == orig {
        // The metadata step had nothing to write: the plain kept case.
        untouched(r);
    } else {
        assert!(
            matches!(r.status, ItemStatus::Done { trashed: Some(_) }),
            "{r:?}"
        );
    }
    // Either way the next scan leaves the file alone.
    let again = scan_plan(&c, d, &src);
    assert!(again.items.is_empty(), "{again:?}");
    assert_eq!(again.unchanged, std::slice::from_ref(&src));
}

/// `<stem>.avif` is taken, so the plan reserved `<stem>_1.avif`; the
/// fallback would be `<stem>_1.png`. That number is no name worth a
/// rename: the source stays as it is.
#[test]
fn a_collision_numbered_fallback_name_keeps_the_source() {
    require_tools!();
    let d = tempfile::tempdir().unwrap();
    let d = d.path();
    let root = d.join("src");
    let src = tiny_png(&root, "shot.png");
    std::fs::write(root.join("shot.avif"), b"another file").unwrap();
    let orig = hash(&src);
    let c = config(d, None, "", 72);
    let plan = scan_plan(&c, d, &src);
    assert_eq!(plan.items[0].outputs[0].dest, root.join("shot_1.avif"));
    let (_, rep) = run_plan(d, &plan);
    let r = &rep.items[0];
    untouched(r);
    assert!(r.outputs[0].renamed_from.is_none(), "{r:?}");
    assert_eq!(r.outputs[0].actual.as_deref(), Some(src.as_path()));
    assert_eq!(names(&root), ["shot.avif", "shot.png"]);
    assert_eq!(hash(&src), orig);
    let again = scan_plan(&c, d, &src);
    assert!(again.items.is_empty(), "{again:?}");
}
