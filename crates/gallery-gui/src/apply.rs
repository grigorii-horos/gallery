//! Running a plan from the GUI: what a run will do (for the confirmation),
//! the core apply in a background thread with `apply-progress` events, and
//! its result as `apply-done` or `apply-error`.
//!
//! Only local plans run here: a server plan holds the server's paths and is
//! run on the server with `gallery apply`; the GUI never touches the server.

use std::path::Path;
use std::sync::Mutex;

use gallery_core::apply::{
    ApplyOptions, ItemResult, ItemStatus, Report, apply_observed, report_path,
};
use gallery_core::plan::{OutputOp, Plan, PlanItem, Side};
use gallery_core::progress::{Cancel, ProgressEvent};
use gallery_core::tools::Tools;
use gallery_core::trash::TrashMode;
use serde::{Deserialize, Serialize};

pub const PROGRESS_EVENT: &str = "apply-progress";
pub const DONE_EVENT: &str = "apply-done";
pub const ERROR_EVENT: &str = "apply-error";

/// Why an undone plan does not run again.
pub fn undone_message() -> String {
    gallery_core::t!("apply.undone")
}

/// Failures listed in `apply-done`; the report holds all of them.
const MAX_FAILURES: usize = 200;

/// What "Dry-run" / "Run" asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyRequest {
    pub plan_path: String,
    pub dry_run: bool,
    /// Files in parallel; `None` or 0: half the CPUs.
    #[serde(default)]
    pub jobs: Option<usize>,
    /// Place outputs that differ from their source (date, mtime, GPS, size,
    /// duration) instead of failing them: the confirmation's checkbox.
    #[serde(default)]
    pub allow_anomalies: bool,
    /// Pause the local Syncthing folders the plan touches (real runs only);
    /// they stay paused until the user resumes them.
    #[serde(default)]
    pub pause_sync: bool,
}

impl ApplyRequest {
    pub fn options(&self, trash: TrashMode) -> ApplyOptions {
        let mut o = ApplyOptions {
            dry_run: self.dry_run,
            trash,
            allow_anomalies: self.allow_anomalies,
            ..ApplyOptions::default()
        };
        if let Some(j) = self.jobs.filter(|&j| j > 0) {
            o.jobs = j;
        }
        o
    }
}

/// `apply-progress` payload: finished of all files, the last finished one,
/// and the problems so far.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ApplyProgress {
    pub done: usize,
    pub total: usize,
    pub current: Option<String>,
    /// Items that failed.
    pub failed: usize,
    /// Items skipped: changed after planning or gone.
    pub skipped: usize,
}

/// `apply_status` payload: the running apply, for a reloaded webview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplyStatus {
    pub request: ApplyRequest,
    pub progress: ApplyProgress,
}

/// `apply_status` payload: the running apply (if any) and the last finished
/// one, for a reloaded webview that missed `apply-done`.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyState {
    pub running: Option<ApplyStatus>,
    pub last_done: Option<ApplyDone>,
}

/// What a run of the plan would do, counted from the plan alone (nothing on
/// disk is looked at or changed): the confirmation dialog shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ApplyPreview {
    pub plan_path: String,
    /// Identity of the plan file these counts are of (a hash of the very
    /// bytes read): a run confirmed from this dialog names it.
    pub plan_id: String,
    pub created: String,
    pub side: Option<Side>,
    /// Files the run processes.
    pub files: usize,
    /// Of them, sources that go to the trash once their outputs are in place.
    pub trash: usize,
    /// Sources renamed in their folder (or moved inside their source root)
    /// instead of copied: they never go to the trash.
    pub renamed: usize,
    /// Sources an output keeps where they are (no rename): untouched.
    pub kept: usize,
    /// New files written.
    pub writes: usize,
    /// Files outside the run, left as they are.
    pub review: usize,
    pub unmatched: usize,
    pub unchanged: usize,
    /// A run of this plan already finished (its report is next to it and
    /// not cancelled): running it again would only find the sources gone.
    /// `None`: no such run.
    pub applied: Option<Applied>,
}

/// How the finished run of a plan ended, from its report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Applied {
    /// Nothing failed.
    Clean,
    /// Some files failed: retry them from the history, not by a blind rerun.
    WithFailures,
    /// The run was undone in full: a fresh scan makes the next plan.
    Undone,
}

/// Whether the source of `item` goes to the trash, is renamed, or stays.
fn fate(item: &PlanItem) -> (bool, bool, bool) {
    let renamed = item.outputs.iter().any(|o| o.op == OutputOp::Rename);
    let kept = !renamed && item.outputs.iter().any(|o| o.op == OutputOp::Keep);
    (!renamed && !kept, renamed, kept)
}

pub fn preview_of(plan: &Plan, plan_path: &Path) -> ApplyPreview {
    let mut p = ApplyPreview {
        plan_path: plan_path.display().to_string(),
        created: plan.created.clone(),
        side: Some(plan.side),
        files: plan.items.len(),
        review: plan.review.len(),
        unmatched: plan.unmatched.len(),
        unchanged: plan.unchanged.len(),
        ..ApplyPreview::default()
    };
    for it in &plan.items {
        let (trash, renamed, kept) = fate(it);
        p.trash += usize::from(trash);
        p.renamed += usize::from(renamed);
        p.kept += usize::from(kept);
        p.writes += it
            .outputs
            .iter()
            .filter(|o| o.op == OutputOp::Write)
            .count();
    }
    p
}

/// [`preview_of`] the plan file at `path`, plus whether it already ran.
pub fn preview(path: &Path) -> Result<ApplyPreview, String> {
    let (plan, plan_id) = read_plan(path)?;
    Ok(ApplyPreview {
        applied: already_applied(path),
        plan_id,
        ..preview_of(&plan, path)
    })
}

/// The plan at `path` and its identity, from one read of the file.
pub fn read_plan(path: &Path) -> Result<(Plan, String), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = std::str::from_utf8(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let plan = Plan::parse(text, path).map_err(|e| e.to_string())?;
    Ok((plan, blake3::hash(&bytes).to_hex().to_string()))
}

/// A report sits next to the plan and is not of a cancelled run (a
/// cancelled run may be continued). An unreadable report counts as a run.
pub fn already_applied(plan_path: &Path) -> Option<Applied> {
    let text = std::fs::read_to_string(report_path(plan_path)).ok()?;
    match serde_json::from_str::<Report>(&text) {
        Ok(r) if r.undone => Some(Applied::Undone),
        Ok(r) if r.cancelled => None,
        Ok(r)
            if r.items
                .iter()
                .any(|i| matches!(i.status, ItemStatus::Failed { .. })) =>
        {
            Some(Applied::WithFailures)
        }
        Ok(_) => Some(Applied::Clean),
        Err(_) => Some(Applied::Clean),
    }
}

/// Whether the manual decisions changed after the plan file was written:
/// the plan then does not hold them.
fn overrides_newer(plan_path: &Path, overrides_file: Option<&Path>) -> bool {
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (overrides_file.and_then(mtime), mtime(plan_path)) {
        (Some(o), Some(p)) => o > p,
        _ => false,
    }
}

/// Why a real run of the plan must not start now (a dry run always may):
/// it already ran, or manual decisions came after the plan was built.
pub fn guard_real_run(req: &ApplyRequest, overrides_file: Option<&Path>) -> Result<(), String> {
    if req.dry_run {
        return Ok(());
    }
    let path = Path::new(&req.plan_path);
    match already_applied(path) {
        Some(Applied::WithFailures) => {
            return Err(gallery_core::t!("apply.already_run_failures"));
        }
        Some(Applied::Clean) => {
            return Err(gallery_core::t!("apply.already_run"));
        }
        Some(Applied::Undone) => {
            return Err(undone_message());
        }
        None => {}
    }
    if overrides_newer(path, overrides_file) {
        return Err(gallery_core::t!("apply.decisions_changed"));
    }
    Ok(())
}

/// Refuse a server plan: its paths are the server's.
pub fn ensure_local(plan: &Plan) -> Result<(), String> {
    match plan.side {
        Side::Local => Ok(()),
        Side::Server => Err(gallery_core::t!("apply.server_plan")),
    }
}

/// Counts of a finished run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ApplySummary {
    /// Files in the plan.
    pub total: usize,
    /// Every output in place (for a dry run: would be processed).
    pub done: usize,
    /// Of `done`, sources renamed instead of trashed.
    pub renamed: usize,
    pub failed: usize,
    /// Changed after planning: not touched.
    pub stale: usize,
    /// Gone: nothing done.
    pub missing: usize,
    /// Not started because of "Cancel": not touched.
    pub not_started: usize,
    pub cancelled: bool,
    /// The run stopped itself: too many outputs differed from their source.
    pub stopped_by_anomalies: bool,
    /// Items with an output that differed from its source.
    pub anomalies: usize,
}

pub fn summarize(report: &Report, total: usize) -> ApplySummary {
    let mut s = ApplySummary {
        total,
        cancelled: report.cancelled,
        stopped_by_anomalies: report.stopped_by_anomalies,
        anomalies: report
            .items
            .iter()
            .filter(|it| it.outputs.iter().any(|o| !o.anomalies.is_empty()))
            .count(),
        not_started: total.saturating_sub(report.items.len()),
        ..ApplySummary::default()
    };
    for it in &report.items {
        match it.status {
            ItemStatus::Done { .. } => {
                s.done += 1;
                s.renamed += usize::from(it.outputs.iter().any(|o| o.renamed_from.is_some()));
            }
            ItemStatus::DryRun => s.done += 1,
            ItemStatus::Failed { .. } => s.failed += 1,
            ItemStatus::Stale => s.stale += 1,
            ItemStatus::Missing => s.missing += 1,
        }
    }
    s
}

/// A file the run did not process, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Failure {
    pub source: String,
    pub reason: String,
}

fn failure(it: &ItemResult) -> Option<Failure> {
    let reason = match &it.status {
        ItemStatus::Failed { .. } => it
            .error
            .clone()
            .or_else(|| {
                it.outputs.iter().find_map(|o| {
                    let e = o.error.as_ref()?;
                    Some(format!("{}: {e}", o.planned.display()))
                })
            })
            .unwrap_or_else(|| gallery_core::t!("cli.unknown_error")),
        ItemStatus::Stale => gallery_core::t!("apply.stale"),
        ItemStatus::Missing => gallery_core::t!("undo.file_missing"),
        ItemStatus::Done { .. } | ItemStatus::DryRun => return None,
    };
    Some(Failure {
        source: it.source.display().to_string(),
        reason,
    })
}

/// `apply-done` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplyDone {
    pub plan_path: String,
    pub dry_run: bool,
    /// `None` for a dry run (it writes no report).
    pub report_path: Option<String>,
    pub summary: ApplySummary,
    /// The first failed or skipped files.
    pub failures: Vec<Failure>,
    /// Problems of the run as a whole (time zone, journal).
    pub warnings: Vec<String>,
}

/// `apply-error` payload: the run did not happen (or its report could not
/// be written — then the journal next to the plan keeps undo possible).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplyError {
    pub message: String,
}

impl From<String> for ApplyError {
    fn from(message: String) -> Self {
        ApplyError { message }
    }
}

/// Run the plan of `req`: refuse a server plan, then the core apply with
/// progress (`on_progress` gets a snapshot per finished file) and `cancel`.
pub fn run(
    req: &ApplyRequest,
    tools: &Tools,
    trash: TrashMode,
    on_progress: &(dyn Fn(ApplyProgress) + Sync),
    cancel: &Cancel,
) -> Result<ApplyDone, ApplyError> {
    let path = Path::new(&req.plan_path);
    let plan = Plan::load(path).map_err(|e| e.to_string())?;
    ensure_local(&plan)?;
    let total = plan.items.len();
    let state = Mutex::new(ApplyProgress {
        total,
        ..ApplyProgress::default()
    });
    let lock = || state.lock().unwrap_or_else(|e| e.into_inner());
    let on_item = |r: &ItemResult| {
        let mut s = lock();
        match r.status {
            ItemStatus::Failed { .. } => s.failed += 1,
            ItemStatus::Stale | ItemStatus::Missing => s.skipped += 1,
            _ => {}
        }
    };
    let progress = |ev: ProgressEvent| {
        let snapshot = {
            let mut s = lock();
            // Events come from worker threads: never step back.
            if ev.done >= s.done {
                s.done = ev.done;
                s.current = ev.current.map(|p| p.display().to_string());
            }
            s.clone()
        };
        on_progress(snapshot);
    };
    let mut opts = req.options(trash);
    let mut sync_warning = None;
    if req.pause_sync && !req.dry_run {
        // Never a reason not to run: a warning instead.
        match crate::sync::discover() {
            Ok(s) => opts.pause_sync = Some(s),
            Err(e) => sync_warning = Some(gallery_core::t!("apply.sync_was_not_paused", error = e)),
        }
    }
    let report = apply_observed(path, tools, &opts, &progress, cancel, &on_item)
        .map_err(|e| e.to_string())?;
    Ok(ApplyDone {
        plan_path: req.plan_path.clone(),
        dry_run: req.dry_run,
        report_path: (!req.dry_run).then(|| report_path(path).display().to_string()),
        summary: summarize(&report, total),
        failures: report
            .items
            .iter()
            .filter_map(failure)
            .take(MAX_FAILURES)
            .collect(),
        warnings: report
            .tool_warnings
            .iter()
            .cloned()
            .chain(sync_warning)
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::tests::ScanEnv;
    use gallery_core::plan::PlannedOutput;
    use gallery_core::route::Via;
    use serde_json::json;
    use std::path::PathBuf;

    fn item(src: &str, ops: &[OutputOp]) -> PlanItem {
        PlanItem {
            source: src.into(),
            source_root: "/s".into(),
            size: 1,
            mtime_ns: 1,
            target: "t".into(),
            via: Via::Mask,
            outputs: ops
                .iter()
                .enumerate()
                .map(|(i, &op)| PlannedOutput {
                    dest: format!("/o/{i}{src}").into(),
                    op,
                    steps: vec![],
                    trace: vec![],
                })
                .collect(),
        }
    }

    fn plan(side: Side, items: Vec<PlanItem>) -> Plan {
        let mut p: Plan = serde_json::from_value(json!({
            "version": 1, "created": "2026-11-05T10:00:00+02:00", "side": "local",
            "content_presets": {}, "items": [], "review": [],
            "unmatched": ["/s/u.bin"], "warnings": [], "unchanged": ["/s/k.jpg", "/s/k2.jpg"]
        }))
        .unwrap();
        p.side = side;
        p.items = items;
        p
    }

    #[test]
    fn preview_counts_what_goes_to_trash_and_what_is_renamed() {
        use OutputOp::*;
        let p = plan(
            Side::Local,
            vec![
                item("/s/a.jpg", &[Write]),
                item("/s/b.jpg", &[Write, Write]),
                item("/s/c.jpg", &[Rename]),
                item("/s/d.jpg", &[Write, Rename]),
                item("/s/e.jpg", &[Keep, Write]),
            ],
        );
        let v = preview_of(&p, Path::new("/p/x.plan.json"));
        assert_eq!(
            serde_json::to_value(&v).unwrap(),
            json!({"plan_path": "/p/x.plan.json", "plan_id": "", "created": "2026-11-05T10:00:00+02:00",
                   "side": "local", "files": 5, "trash": 2, "renamed": 2, "kept": 1,
                   "writes": 5, "review": 0, "unmatched": 1, "unchanged": 2,
                   "applied": null})
        );
        // Every file has one fate.
        assert_eq!(v.trash + v.renamed + v.kept, v.files);
    }

    #[test]
    fn preview_reads_the_plan_file_and_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("a.plan.json");
        plan(Side::Local, vec![item("/s/a.jpg", &[OutputOp::Write])])
            .save(&path)
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        let v = preview(&path).unwrap();
        assert_eq!((v.files, v.trash), (1, 1));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        // Its identity is that of the bytes read, the one a run names.
        assert_eq!(v.plan_id, blake3::hash(&before).to_hex().to_string());
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
        assert!(preview(&d.path().join("none.plan.json")).is_err());
        assert_eq!(v.applied, None);
    }

    #[test]
    fn a_plan_with_a_finished_report_is_already_applied() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("a.plan.json");
        plan(Side::Local, vec![]).save(&path).unwrap();
        assert_eq!(already_applied(&path), None);
        let mut r = Report {
            run_id: "x".into(),
            plan: path.clone(),
            started: String::new(),
            finished: "f".into(),
            items: vec![],
            tool_versions: Default::default(),
            tool_warnings: vec![],
            undone: false,
            undone_sources: vec![],
            cancelled: true,
            stopped_by_anomalies: false,
        };
        let write = |r: &Report| {
            std::fs::write(report_path(&path), serde_json::to_string(r).unwrap()).unwrap()
        };
        // A cancelled run may be continued.
        write(&r);
        assert_eq!(already_applied(&path), None);
        r.cancelled = false;
        write(&r);
        assert_eq!(already_applied(&path), Some(Applied::Clean));
        assert_eq!(preview(&path).unwrap().applied, Some(Applied::Clean));
        r.items.push(ItemResult {
            source: "/s/a.jpg".into(),
            status: ItemStatus::Failed { trashed: None },
            outputs: vec![],
            error: Some("x".into()),
            elapsed_ms: None,
        });
        write(&r);
        assert_eq!(already_applied(&path), Some(Applied::WithFailures));
        assert_eq!(
            serde_json::to_value(preview(&path).unwrap().applied).unwrap(),
            json!("with_failures")
        );
        // Undone in full (even a cancelled run): rescan instead.
        r.undone = true;
        r.cancelled = true;
        write(&r);
        assert_eq!(already_applied(&path), Some(Applied::Undone));
        assert_eq!(
            serde_json::to_value(preview(&path).unwrap().applied).unwrap(),
            json!("undone")
        );
        let req = ApplyRequest {
            plan_path: path.display().to_string(),
            dry_run: false,
            jobs: None,
            allow_anomalies: false,
            pause_sync: false,
        };
        let e = guard_real_run(&req, None).unwrap_err();
        assert!(e.contains("undone") && e.contains("Sort"), "{e}");
        std::fs::write(report_path(&path), "{").unwrap();
        assert_eq!(already_applied(&path), Some(Applied::Clean));
    }

    #[test]
    fn a_real_run_is_refused_when_applied_or_overrides_are_newer() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("a.plan.json");
        plan(Side::Local, vec![]).save(&path).unwrap();
        let req = |dry_run| ApplyRequest {
            plan_path: path.display().to_string(),
            dry_run,
            jobs: None,
            allow_anomalies: false,
            pause_sync: false,
        };
        assert!(guard_real_run(&req(false), None).is_ok());
        // Manual decisions written after the plan.
        let ov = d.path().join("overrides.json");
        std::fs::write(&ov, "{}").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&ov)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let e = guard_real_run(&req(false), Some(&ov)).unwrap_err();
        assert!(e.contains("refresh"), "{e}");
        assert!(guard_real_run(&req(true), Some(&ov)).is_ok());
        // Already applied.
        std::fs::write(
            report_path(&path),
            serde_json::to_string(&Report {
                run_id: "x".into(),
                plan: path.clone(),
                started: String::new(),
                finished: "f".into(),
                items: vec![],
                tool_versions: Default::default(),
                tool_warnings: vec![],
                undone: false,
                undone_sources: vec![],
                cancelled: false,
                stopped_by_anomalies: false,
            })
            .unwrap(),
        )
        .unwrap();
        let e = guard_real_run(&req(false), None).unwrap_err();
        assert!(e.contains("already run"), "{e}");
        assert!(guard_real_run(&req(true), None).is_ok());
    }

    #[test]
    fn a_server_plan_is_refused_and_nothing_is_touched() {
        assert!(ensure_local(&plan(Side::Local, vec![])).is_ok());
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("a.jpg");
        std::fs::write(&src, "a").unwrap();
        let mut it = item(&src.display().to_string(), &[OutputOp::Write]);
        it.source_root = d.path().into();
        it.outputs[0].dest = d.path().join("out/a.jpg");
        let path = d.path().join("srv.plan.json");
        plan(Side::Server, vec![it]).save(&path).unwrap();
        let req = ApplyRequest {
            plan_path: path.display().to_string(),
            dry_run: false,
            jobs: Some(1),
            allow_anomalies: false,
            pause_sync: false,
        };
        let tools = Tools::discover().unwrap();
        let e = run(
            &req,
            &tools,
            TrashMode::Folder,
            &|_: ApplyProgress| {},
            &Cancel::new(),
        )
        .unwrap_err();
        assert!(e.message.contains("server plan"), "{}", e.message);
        assert!(e.message.contains("gallery apply"), "{}", e.message);
        assert_eq!(std::fs::read_to_string(&src).unwrap(), "a");
        assert!(!d.path().join("out").exists());
        assert!(!report_path(&path).exists());
    }

    #[test]
    fn summary_counts_statuses_and_not_started() {
        let r = |src: &str, status: ItemStatus, renamed: bool| ItemResult {
            source: src.into(),
            status,
            outputs: vec![gallery_core::apply::OutputResult {
                planned: "/o/x".into(),
                actual: None,
                error: None,
                warnings: vec![],
                fell_back: false,
                size: None,
                hash: None,
                renamed_from: renamed.then(|| PathBuf::from(src)),
                anomalies: Vec::new(),
            }],
            error: None,
            elapsed_ms: None,
        };
        let report = Report {
            run_id: "x".into(),
            plan: "/p".into(),
            started: String::new(),
            finished: String::new(),
            items: vec![
                r("/a", ItemStatus::Done { trashed: None }, true),
                r("/b", ItemStatus::Done { trashed: None }, false),
                r("/c", ItemStatus::Failed { trashed: None }, false),
                r("/d", ItemStatus::Stale, false),
                r("/e", ItemStatus::Missing, false),
            ],
            tool_versions: Default::default(),
            tool_warnings: vec![],
            undone: false,
            undone_sources: vec![],
            cancelled: true,
            stopped_by_anomalies: false,
        };
        assert_eq!(
            serde_json::to_value(summarize(&report, 8)).unwrap(),
            json!({"total": 8, "done": 2, "renamed": 1, "failed": 1, "stale": 1,
                   "missing": 1, "not_started": 3, "cancelled": true,
                   "stopped_by_anomalies": false, "anomalies": 0})
        );
        let f: Vec<_> = report.items.iter().filter_map(failure).collect();
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].reason, "unknown error");
    }

    #[test]
    fn options_from_request() {
        let mut r = ApplyRequest {
            plan_path: "/p".into(),
            dry_run: true,
            jobs: Some(3),
            allow_anomalies: true,
            pause_sync: false,
        };
        let o = r.options(TrashMode::Folder);
        assert!(o.allow_anomalies);
        assert!(o.dry_run);
        assert_eq!((o.jobs, o.trash), (3, TrashMode::Folder));
        r.jobs = Some(0);
        assert_eq!(
            r.options(TrashMode::Folder).jobs,
            ApplyOptions::default().jobs
        );
        let r: ApplyRequest =
            serde_json::from_value(json!({"plan_path": "/p", "dry_run": false})).unwrap();
        assert_eq!(r.jobs, None);
        assert!(!r.allow_anomalies);
        assert!(!r.options(TrashMode::Folder).allow_anomalies);
    }

    /// A real scan of a temp fixture, then dry run and a real run with the
    /// folder trash: never the user's files.
    #[test]
    fn dry_run_then_real_run_on_a_temp_fixture() {
        let d = tempfile::tempdir().unwrap();
        let config = crate::scan::tests::fixture(d.path());
        let plans = d.path().join("plans");
        let tools = Tools::discover().unwrap();
        let env = ScanEnv {
            config: &config,
            plans_dir: &plans,
            overrides: None,
            tools: &tools,
        };
        let done =
            crate::scan::tests::run(&env, None, &|_: ProgressEvent| {}, &Cancel::new()).unwrap();
        let v = preview(Path::new(&done.plan_path)).unwrap();
        assert_eq!((v.files, v.trash), (3, 3));

        let mut req = ApplyRequest {
            plan_path: done.plan_path.clone(),
            dry_run: true,
            jobs: Some(2),
            allow_anomalies: false,
            pause_sync: false,
        };
        let seen = Mutex::new(Vec::new());
        let sink = |p: ApplyProgress| seen.lock().unwrap().push(p);
        let dry = run(&req, &tools, TrashMode::Folder, &sink, &Cancel::new()).unwrap();
        assert_eq!(dry.report_path, None);
        assert_eq!((dry.summary.done, dry.summary.failed), (3, 0));
        assert!(d.path().join("src/cat.jpg").exists());
        assert!(!d.path().join("out").exists());
        assert_eq!(seen.lock().unwrap().len(), 3);
        assert!(seen.lock().unwrap().iter().all(|p| p.total == 3));

        // One source changed since the plan: skipped, counted, untouched.
        std::fs::write(d.path().join("src/note.txt"), "changed!").unwrap();
        req.dry_run = false;
        seen.lock().unwrap().clear();
        let real = run(&req, &tools, TrashMode::Folder, &sink, &Cancel::new()).unwrap();
        let s = &real.summary;
        assert_eq!((s.done, s.stale, s.failed, s.cancelled), (2, 1, 0, false));
        assert_eq!(real.failures.len(), 1);
        assert!(real.failures[0].source.ends_with("note.txt"));
        let rp = real.report_path.clone().unwrap();
        assert!(Path::new(&rp).exists());
        assert_eq!(seen.lock().unwrap().last().unwrap().skipped, 1);
        assert_eq!(
            std::fs::read_to_string(d.path().join("src/note.txt")).unwrap(),
            "changed!"
        );
        assert!(!d.path().join("src/cat.jpg").exists());
        assert!(d.path().join("src/.gallery-trash").exists());
        assert!(d.path().join("out/rest").read_dir().unwrap().count() >= 2);
    }

    #[test]
    fn cancel_before_start_runs_nothing() {
        let d = tempfile::tempdir().unwrap();
        let config = crate::scan::tests::fixture(d.path());
        let plans = d.path().join("plans");
        let tools = Tools::discover().unwrap();
        let env = ScanEnv {
            config: &config,
            plans_dir: &plans,
            overrides: None,
            tools: &tools,
        };
        let done =
            crate::scan::tests::run(&env, None, &|_: ProgressEvent| {}, &Cancel::new()).unwrap();
        let req = ApplyRequest {
            plan_path: done.plan_path,
            dry_run: false,
            jobs: Some(1),
            allow_anomalies: false,
            pause_sync: false,
        };
        let c = Cancel::new();
        c.cancel();
        let out = run(&req, &tools, TrashMode::Folder, &|_: ApplyProgress| {}, &c).unwrap();
        assert!(out.summary.cancelled);
        assert_eq!((out.summary.done, out.summary.not_started), (0, 3));
        assert!(d.path().join("src/cat.jpg").exists());
        assert!(!d.path().join("out").exists());
    }
}
