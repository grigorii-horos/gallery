//! Pilot run: a plan is split into a small trial slice and the rest; the
//! pilot runs like any plan (same guards, confirmation and busy slot), then
//! `result` shows each pilot file before and after so the user can go on with
//! the rest or undo the pilot. File logic stays in the core
//! (`plan::pilot`, `apply`, `verify`, `undo`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use gallery_core::apply::{ItemResult, ItemStatus, Report};
use gallery_core::plan::{Plan, PlanItem};
use gallery_core::tools::Tools;
use gallery_core::trash::TrashRef;
use gallery_core::{exif, verify};
use serde::Serialize;
use serde_json::Value;

use crate::apply::{ApplyPreview, ensure_local, preview_of};

/// Files per target of the pilot offered by default.
pub const DEFAULT_PER_TARGET: usize = 5;

/// `pilot_preview` payload: how the current plan splits for a pilot. Nothing
/// is written: the run makes the pilot plan itself, after its final check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PilotSplit {
    /// Identity of the plan this split is of, and of the chosen slice: the
    /// run names them, and a plan rebuilt since is refused.
    pub plan_id: String,
    pub pilot_id: String,
    pub per_target: usize,
    pub pilot_files: usize,
    /// What stays for after the pilot (the plan is rebuilt by then).
    pub rest_files: usize,
    /// What the pilot run would do, for its confirmation.
    pub preview: ApplyPreview,
}

/// Split `plan` for a pilot of `per_target` files per target: the pilot
/// plan and the rest. Refuses a plan with no files and one too small to split.
pub fn choose(plan: &Plan, per_target: usize) -> Result<(Plan, Plan), String> {
    let (pilot, rest) = gallery_core::plan::pilot(plan, per_target.max(1));
    if pilot.items.is_empty() {
        return Err(gallery_core::t!("pilot.no_files"));
    }
    if rest.items.is_empty() {
        return Err(gallery_core::t!("pilot.too_few"));
    }
    Ok((pilot, rest))
}

/// How the plan at `path` (the current plan) splits; refuses a server plan.
pub fn preview(path: &Path, per_target: usize) -> Result<PilotSplit, String> {
    // One read: the split and the identity it names are of the same bytes.
    let (plan, plan_id) = crate::apply::read_plan(path)?;
    ensure_local(&plan)?;
    let per_target = per_target.max(1);
    let (pilot, rest) = choose(&plan, per_target)?;
    let preview = ApplyPreview {
        plan_id: plan_id.clone(),
        ..preview_of(&pilot, path)
    };
    Ok(PilotSplit {
        plan_id,
        pilot_id: crate::current::slice_id(&pilot),
        per_target,
        pilot_files: pilot.items.len(),
        rest_files: rest.items.len(),
        preview,
    })
}

/// What a file looks like: size, mtime, capture date and whether it has GPS.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Facts {
    pub size: Option<u64>,
    /// Local time, `YYYY-MM-DD HH:MM:SS`.
    pub mtime: Option<String>,
    pub capture: Option<String>,
    /// `None`: not readable.
    pub gps: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PilotOutput {
    pub planned: String,
    pub actual: Option<String>,
    pub error: Option<String>,
    pub anomalies: Vec<String>,
    pub after: Facts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PilotFile {
    pub source: String,
    pub target: String,
    /// `done`, `failed`, `stale`, `missing`.
    pub status: &'static str,
    pub error: Option<String>,
    pub before: Facts,
    pub outputs: Vec<PilotOutput>,
}

/// `pilot_result` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PilotResult {
    pub report_path: String,
    pub files: Vec<PilotFile>,
    /// Files with an output that differed from its source.
    pub anomalies: usize,
    pub stopped_by_anomalies: bool,
}

fn fmt_time(t: DateTime<Local>) -> String {
    t.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn fmt_ns(ns: i128) -> Option<String> {
    let secs = i64::try_from(ns.div_euclid(1_000_000_000)).ok()?;
    Local.timestamp_opt(secs, 0).single().map(fmt_time)
}

fn fmt_capture(d: NaiveDateTime) -> String {
    d.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Facts of a file on disk (size, mtime) plus what its exiftool record says.
fn facts_of(path: &Path, rec: Option<&Value>) -> Facts {
    let meta = std::fs::metadata(path).ok();
    Facts {
        size: meta.as_ref().map(|m| m.len()),
        mtime: meta
            .and_then(|m| m.modified().ok())
            .map(|t| fmt_time(DateTime::<Local>::from(t))),
        capture: rec.and_then(verify::capture_of).map(fmt_capture),
        gps: rec.map(verify::has_gps),
    }
}

/// Where the original is now: its trash folder, or its own path when it was
/// not moved. The system trash is not searched.
fn original_now(it: &ItemResult) -> Option<PathBuf> {
    match &it.status {
        ItemStatus::Done { trashed } | ItemStatus::Failed { trashed } => match trashed {
            Some(TrashRef::Folder { path }) => Some(path.clone()),
            Some(TrashRef::System { .. }) => None,
            None => it
                .outputs
                .iter()
                .find_map(|o| o.renamed_from.as_ref().and(o.actual.clone()))
                .or_else(|| Some(it.source.clone())),
        },
        _ => Some(it.source.clone()),
    }
}

fn status_name(s: &ItemStatus) -> &'static str {
    match s {
        ItemStatus::Done { .. } | ItemStatus::DryRun => "done",
        ItemStatus::Failed { .. } => "failed",
        ItemStatus::Stale => "stale",
        ItemStatus::Missing => "missing",
    }
}

/// Each pilot file of the report at `path` before and after: size, mtime,
/// capture date, GPS; the report's own anomalies. One exiftool call reads
/// every file involved.
pub fn result(path: &Path, tools: &Tools) -> Result<PilotResult, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let report: Report =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let plan = Plan::load(&report.plan).map_err(|e| e.to_string())?;
    let mut tools = tools.clone();
    tools.tz = plan.tz.clone();
    let items: HashMap<&Path, &PlanItem> =
        plan.items.iter().map(|i| (i.source.as_path(), i)).collect();

    let mut to_read: Vec<PathBuf> = Vec::new();
    for it in &report.items {
        to_read.extend(original_now(it));
        to_read.extend(it.outputs.iter().filter_map(|o| o.actual.clone()));
    }
    to_read.sort();
    to_read.dedup();
    // A pilot is small; an unreadable file just has no capture date or GPS.
    let recs = exif::read(&tools, &to_read).unwrap_or_default();

    let mut files = Vec::new();
    for it in &report.items {
        let plan_item = items.get(it.source.as_path());
        let mut before = original_now(it)
            .map(|p| facts_of(&p, recs.get(&p)))
            .unwrap_or_default();
        // The plan holds the source's state at planning time: what the file
        // was before, wherever it lies now.
        if let Some(pi) = plan_item {
            before.size = Some(pi.size);
            before.mtime = fmt_ns(pi.mtime_ns);
        }
        files.push(PilotFile {
            source: it.source.display().to_string(),
            target: plan_item.map(|i| i.target.clone()).unwrap_or_default(),
            status: status_name(&it.status),
            error: it.error.clone(),
            before,
            outputs: it
                .outputs
                .iter()
                .map(|o| PilotOutput {
                    planned: o.planned.display().to_string(),
                    actual: o.actual.as_ref().map(|p| p.display().to_string()),
                    error: o.error.clone(),
                    anomalies: o.anomalies.clone(),
                    after: o
                        .actual
                        .as_ref()
                        .map(|p| facts_of(p, recs.get(p)))
                        .unwrap_or_default(),
                })
                .collect(),
        });
    }
    Ok(PilotResult {
        report_path: path.display().to_string(),
        anomalies: report
            .items
            .iter()
            .filter(|it| it.outputs.iter().any(|o| !o.anomalies.is_empty()))
            .count(),
        stopped_by_anomalies: report.stopped_by_anomalies,
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gallery_core::apply::{ApplyOptions, apply};
    use gallery_core::config::{ContentPreset, MetadataPreset};
    use gallery_core::plan::{OutputOp, PLAN_VERSION, PlannedOutput, Side};
    use gallery_core::route::Via;
    use gallery_core::trash::TrashMode;
    use std::collections::BTreeMap;
    use std::process::Command;

    fn item(dir: &Path, name: &str, target: &str, steps: &[&str]) -> PlanItem {
        let src = dir.join("src").join(name);
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        let st = Command::new("magick")
            .args(["-size", "64x48", "gradient:red-blue"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(st.success());
        let st = Command::new("exiftool")
            .args([
                "-overwrite_original",
                "-q",
                "-DateTimeOriginal=2026:04:12 10:33:14",
                "-GPSLatitude=10",
                "-GPSLatitudeRef=N",
                "-GPSLongitude=20",
                "-GPSLongitudeRef=E",
            ])
            .arg(&src)
            .status()
            .unwrap();
        assert!(st.success());
        let meta = std::fs::metadata(&src).unwrap();
        PlanItem {
            ai_meta: None,
            source: src,
            source_root: dir.join("src"),
            size: meta.len(),
            mtime_ns: gallery_core::scan::mtime_ns(&meta).unwrap(),
            target: target.into(),
            via: Via::Mask,
            outputs: vec![PlannedOutput {
                dest: dir.join("out").join(target).join(name),
                op: OutputOp::Write,
                steps: steps.iter().map(|s| s.to_string()).collect(),
                trace: vec![],
            }],
            kind: Default::default(),
        }
    }

    fn plan_of(items: Vec<PlanItem>, side: Side) -> Plan {
        let mut presets = BTreeMap::new();
        presets.insert(
            "M".to_string(),
            ContentPreset::Metadata(MetadataPreset::full()),
        );
        Plan {
            version: PLAN_VERSION,
            created: String::new(),
            side,
            tz: None,
            content_presets: presets,
            items,
            review: vec![],
            unmatched: vec![],
            warnings: vec![],
            unchanged: vec![],
            retry_of: None,
            kept: Vec::new(),
        }
    }

    fn have_tools() -> bool {
        ["ffmpeg", "ffprobe", "exiftool", "magick"]
            .iter()
            .all(|t| gallery_core::tools::find(t).is_some())
    }

    #[test]
    fn preview_splits_without_writing_and_refuses_what_cannot_be_a_pilot() {
        if !have_tools() {
            eprintln!("skip: tools not found");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let items: Vec<PlanItem> = (0..5)
            .map(|i| {
                item(
                    d.path(),
                    &format!("{i}.jpg"),
                    if i < 3 { "a" } else { "b" },
                    &["M"],
                )
            })
            .collect();
        let path = d.path().join("cur.plan.json");
        plan_of(items, Side::Local).save(&path).unwrap();
        let before = std::fs::read_dir(d.path()).unwrap().count();

        let s = preview(&path, 2).unwrap();
        assert_eq!(s.plan_id, s.preview.plan_id);
        assert_eq!(
            s.plan_id,
            blake3::hash(&std::fs::read(&path).unwrap())
                .to_hex()
                .to_string()
        );
        assert_eq!((s.pilot_files, s.rest_files), (4, 1));
        assert_eq!(s.preview.files, 4);
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), before);

        // A pilot of everything is no pilot.
        let e = preview(&path, 10).unwrap_err();
        assert!(e.contains("pilot would cover everything"), "{e}");

        let server = d.path().join("srv.plan.json");
        plan_of(vec![item(d.path(), "s.jpg", "a", &["M"])], Side::Server)
            .save(&server)
            .unwrap();
        assert!(preview(&server, 1).is_err());
    }

    #[test]
    fn result_shows_each_file_before_and_after() {
        if !have_tools() {
            eprintln!("skip: tools not found");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let items = vec![
            item(d.path(), "a.jpg", "a", &["M"]),
            item(d.path(), "b.jpg", "a", &["M"]),
        ];
        let path = d.path().join("20261107-100000.plan.json");
        plan_of(items, Side::Local).save(&path).unwrap();
        let tools = Tools::discover().unwrap();
        let opts = ApplyOptions {
            jobs: 1,
            trash: TrashMode::Folder,
            ..Default::default()
        };
        let rep = apply(&path, &tools, &opts).unwrap();
        let r = result(&gallery_core::apply::report_path(&path), &tools).unwrap();
        assert_eq!(
            r.report_path,
            gallery_core::apply::report_path(&path)
                .display()
                .to_string()
        );
        assert_eq!((r.files.len(), r.anomalies), (2, 0));
        assert!(!r.stopped_by_anomalies);
        let f = &r.files[0];
        assert_eq!(f.status, "done");
        assert_eq!(f.target, "a");
        assert_eq!(f.before.capture.as_deref(), Some("2026-04-12 10:33:14"));
        assert_eq!(f.before.gps, Some(true));
        let o = &f.outputs[0];
        assert_eq!(o.after.capture, f.before.capture);
        assert_eq!(o.after.gps, Some(true));
        assert!(o.after.size.is_some_and(|s| s > 0));
        // The metadata step stamps the capture date as the file's mtime.
        assert_eq!(o.after.mtime.as_deref(), Some("2026-04-12 10:33:14"));
        assert_eq!(rep.items.len(), 2);
    }
}
