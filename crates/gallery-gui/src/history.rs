//! "History": the reports of past runs — the GUI's own (next to its plans,
//! current and archived) and the server's (synced back into the export
//! folder by Syncthing) — with undo and retry of the failed files for the
//! local ones.
//!
//! Undo and retry run only for a report of a local run of a GUI plan: the
//! report sits in the GUI plans folder, next to its local plan, and was
//! written for that very plan path. Anything else (a server report, a
//! report copied from elsewhere) is refused before a file is touched: its
//! paths may be another machine's. A server report gets the `gallery undo`
//! command to run there instead.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gallery_core::apply::{ItemResult, ItemStatus, Report};
use gallery_core::plan::Side;
use gallery_core::progress::{Cancel, ProgressEvent};
use gallery_core::trash::TrashRef;
use serde::{Deserialize, Serialize};

use crate::apply::{ApplyPreview, ApplySummary, preview_of, summarize};

pub const PROGRESS_EVENT: &str = "undo-progress";
pub const DONE_EVENT: &str = "undo-done";
pub const ERROR_EVENT: &str = "undo-error";

const REPORT_MARK: &str = ".report.";

/// Why a report is not undone or retried here.
fn server_refusal() -> String {
    gallery_core::t!("history.server_refusal")
}

/// One report in the list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportEntry {
    pub path: String,
    pub file_name: String,
    /// The plan next to the report (the local copy for a server report).
    pub plan_path: String,
    pub plan_exists: bool,
    pub side: Side,
    pub run_id: String,
    pub started: String,
    /// Empty: the run never finished (it died, or runs elsewhere now).
    pub finished: String,
    /// An earlier run of the plan, kept as `<plan>.report.<run_id>.json`.
    pub archived: bool,
    /// Counts of the run; `total` is the plan's file count when the plan
    /// is readable.
    pub summary: ApplySummary,
    pub undone: bool,
    /// Some items undone (a cancelled or troubled undo, or a retry's
    /// partial undo of the failed files), not all.
    pub partially_undone: bool,
    pub undone_items: usize,
    /// The report could not be read; the rest is empty then.
    pub error: Option<String>,
}

/// An output of an item as the detail shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutputView {
    pub planned: String,
    pub actual: Option<String>,
    pub error: Option<String>,
    pub warnings: Vec<String>,
    /// This output is the source itself, renamed in its folder from here.
    pub renamed_from: Option<String>,
}

/// One file of a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ItemView {
    pub source: String,
    /// `done`, `failed`, `stale`, `missing` or `dry_run`.
    pub status: &'static str,
    /// Where the source went to (folder trash path, or the original path
    /// for the system trash).
    pub trashed: Option<String>,
    pub renamed: bool,
    /// Already undone (restored, outputs removed).
    pub undone: bool,
    /// The plan's target that routed the file; `None` when the plan is gone.
    pub target: Option<String>,
    /// What undoing just this file would do (all zero: nothing to undo).
    pub undo: UndoPreview,
    pub error: Option<String>,
    pub outputs: Vec<OutputView>,
}

/// What an undo of the report would do with the items not undone yet, as
/// recorded (the undo checks every file again and keeps what changed).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct UndoPreview {
    /// Items still to undo.
    pub items: usize,
    /// Sources to bring back from the trash.
    pub restore: usize,
    /// Sources renamed in place to get their old names back.
    pub rename_back: usize,
    /// Output files to remove.
    pub remove_outputs: usize,
}

/// `load_report` payload.
#[derive(Debug, Clone, Serialize)]
pub struct ReportDetail {
    pub entry: ReportEntry,
    pub items: Vec<ItemView>,
    /// Problems of the run as a whole.
    pub warnings: Vec<String>,
    pub undo: UndoPreview,
    /// For a server report: the command that undoes it on the server.
    pub server_command: Option<String>,
}

/// Just what the list needs of a plan, without its items' contents.
#[derive(Deserialize)]
struct PlanHead {
    side: Side,
    items: Vec<serde::de::IgnoredAny>,
}

fn plan_head(path: &Path) -> Option<PlanHead> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// `<stem>.report.json` or `<stem>.report.<run_id>.json`, not hidden.
pub fn is_report_name(name: &str) -> bool {
    !name.starts_with('.') && name.ends_with(".json") && name.contains(REPORT_MARK)
}

/// The plan a report belongs to: `<stem>.report[.<run_id>].json` →
/// `<stem>.json` next to it.
pub fn plan_of_report(report: &Path) -> Option<PathBuf> {
    let name = report.file_name()?.to_str()?;
    let i = name.rfind(REPORT_MARK)?;
    Some(report.with_file_name(format!("{}.json", &name[..i])))
}

/// The report is an earlier run's, kept under its run id.
fn is_archived(name: &str) -> bool {
    !name.ends_with(".report.json")
}

fn read_report(path: &Path) -> Result<Report, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|e| gallery_core::t!("history.unreadable", path = path.display(), error = e))
}

fn entry_of(
    path: &Path,
    report: Result<&Report, String>,
    side: Side,
    total: Option<usize>,
) -> ReportEntry {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let plan = plan_of_report(path);
    let mut e = ReportEntry {
        path: path.display().to_string(),
        archived: is_archived(&file_name),
        file_name,
        plan_path: plan
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        plan_exists: plan.is_some_and(|p| p.is_file()),
        side,
        run_id: String::new(),
        started: String::new(),
        finished: String::new(),
        summary: ApplySummary::default(),
        undone: false,
        partially_undone: false,
        undone_items: 0,
        error: None,
    };
    match report {
        Ok(r) => {
            e.run_id = r.run_id.clone();
            e.started = r.started.clone();
            e.finished = r.finished.clone();
            e.summary = summarize(r, total.unwrap_or(r.items.len()).max(r.items.len()));
            e.undone = r.undone;
            e.undone_items = r.undone_sources.len();
            e.partially_undone = !r.undone && !r.undone_sources.is_empty();
        }
        Err(why) => e.error = Some(why),
    }
    e
}

/// The reports in `dir` (not recursive). Reports of the export folder are
/// the server's; the plans folder's are local unless their plan says
/// otherwise. Plans are read once per plan, without their items.
fn list_in(dir: &Path, from_export: bool) -> Vec<ReportEntry> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut heads: HashMap<PathBuf, Option<PlanHead>> = HashMap::new();
    let mut out = Vec::new();
    for path in rd.filter_map(|e| e.ok()).map(|e| e.path()) {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if !is_report_name(name) || !path.is_file() {
            continue;
        }
        let head = plan_of_report(&path).and_then(|p| {
            heads
                .entry(p.clone())
                .or_insert_with(|| plan_head(&p))
                .as_ref()
                .map(|h| (h.side, h.items.len()))
        });
        let side = match head {
            _ if from_export => Side::Server,
            Some((side, _)) => side,
            None => Side::Local,
        };
        let report = read_report(&path);
        out.push(entry_of(
            &path,
            report.as_ref().map_err(Clone::clone),
            side,
            head.map(|h| h.1),
        ));
    }
    out
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Time of the run for sorting (unix seconds); unparsable last.
fn started_at(e: &ReportEntry) -> i64 {
    chrono::DateTime::parse_from_rfc3339(&e.started).map_or(i64::MIN, |t| t.timestamp())
}

/// Every report of the GUI plans folder and the export folder, newest
/// first. Reads only.
pub fn list(plans_dir: Option<&Path>, export_dir: Option<&Path>) -> Vec<ReportEntry> {
    let mut out = plans_dir.map(|d| list_in(d, false)).unwrap_or_default();
    if let Some(x) = export_dir
        && plans_dir.is_none_or(|p| !same_dir(p, x))
    {
        out.extend(list_in(x, true));
    }
    out.sort_by(|a, b| {
        started_at(b)
            .cmp(&started_at(a))
            .then_with(|| b.run_id.cmp(&a.run_id))
            .then_with(|| b.path.cmp(&a.path))
    });
    out
}

fn status_name(s: &ItemStatus) -> &'static str {
    match s {
        ItemStatus::Done { .. } => "done",
        ItemStatus::Failed { .. } => "failed",
        ItemStatus::Stale => "stale",
        ItemStatus::Missing => "missing",
        ItemStatus::DryRun => "dry_run",
    }
}

fn trashed_of(it: &ItemResult) -> Option<&TrashRef> {
    match &it.status {
        ItemStatus::Done { trashed } | ItemStatus::Failed { trashed } => trashed.as_ref(),
        _ => None,
    }
}

fn shown(p: &Path) -> String {
    p.display().to_string()
}

fn item_view(it: &ItemResult, undone: bool, target: Option<String>) -> ItemView {
    let undo = if undone {
        UndoPreview::default()
    } else {
        item_preview(it)
    };
    ItemView {
        target,
        undo,
        source: shown(&it.source),
        status: status_name(&it.status),
        trashed: trashed_of(it).map(|t| match t {
            TrashRef::Folder { path } => shown(path),
            TrashRef::System { original } => shown(original),
        }),
        renamed: it
            .outputs
            .iter()
            .any(|o| o.renamed_from.is_some() && o.actual.is_some()),
        undone,
        error: it.error.clone(),
        outputs: it
            .outputs
            .iter()
            .map(|o| OutputView {
                planned: shown(&o.planned),
                actual: o.actual.as_deref().map(shown),
                error: o.error.clone(),
                warnings: o.warnings.clone(),
                renamed_from: o.renamed_from.as_deref().map(shown),
            })
            .collect(),
    }
}

/// What an undo would do with the items of `r` not undone yet.
pub fn undo_preview(r: &Report) -> UndoPreview {
    let mut u = UndoPreview::default();
    if r.undone {
        return u;
    }
    for it in r
        .items
        .iter()
        .filter(|it| !r.undone_sources.contains(&it.source))
    {
        u.add(&item_preview(it));
    }
    u
}

impl UndoPreview {
    fn add(&mut self, o: &UndoPreview) {
        self.items += o.items;
        self.restore += o.restore;
        self.rename_back += o.rename_back;
        self.remove_outputs += o.remove_outputs;
    }
}

/// What undoing the one item would do, as recorded; all zero when there is
/// nothing to undo (a stale or missing file, a `keep`-like item).
fn item_preview(it: &ItemResult) -> UndoPreview {
    let renamed = it
        .outputs
        .iter()
        .any(|o| o.renamed_from.is_some() && o.actual.is_some());
    let outputs = it
        .outputs
        .iter()
        .filter(|o| o.renamed_from.is_none())
        .filter(|o| o.actual.as_deref().is_some_and(|a| a != it.source))
        .count();
    let restore = !renamed && trashed_of(it).is_some();
    // An in-place output sits where the source comes back: removed too.
    let in_place = restore
        && it
            .outputs
            .iter()
            .any(|o| o.actual.as_deref() == Some(it.source.as_path()));
    if !(renamed || restore || outputs > 0) {
        return UndoPreview::default();
    }
    UndoPreview {
        items: 1,
        rename_back: usize::from(renamed),
        restore: usize::from(restore),
        remove_outputs: outputs + usize::from(in_place),
    }
}

/// `'…'` for a shell, unless the text needs no quoting.
pub fn shell_quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+:@%=,".contains(c));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// `gallery undo <the report's path on the server>`: the report records the
/// plan's path there, and the report sits next to it.
pub fn server_command(report: &Report, file_name: &str) -> String {
    // The server may run another OS than this GUI (a Linux server seen from
    // Windows): a `/`-rooted path is absolute there and is joined with `/`.
    let plan = report.plan.to_string_lossy();
    if plan.starts_with('/')
        && !cfg!(unix)
        && let Some((dir, _)) = plan.rsplit_once('/')
    {
        return format!(
            "gallery undo {}",
            shell_quote(&format!("{dir}/{file_name}"))
        );
    }
    match report.plan.parent().filter(|d| d.is_absolute()) {
        Some(dir) => format!("gallery undo {}", shell_quote(&shown(&dir.join(file_name)))),
        None => gallery_core::t!("history.undo_command", file = file_name),
    }
}

/// The target of each file of the plan next to the report (empty when the
/// plan cannot be read).
fn plan_targets(report: &Path) -> HashMap<PathBuf, String> {
    plan_of_report(report)
        .and_then(|p| gallery_core::plan::Plan::load(&p).ok())
        .map(|p| {
            p.items
                .into_iter()
                .map(|it| (it.source, it.target))
                .collect()
        })
        .unwrap_or_default()
}

/// The report at `path` with its files, for the detail view; `export_dir`
/// marks a report found there as the server's. Reads only.
pub fn load(path: &Path, export_dir: Option<&Path>) -> Result<ReportDetail, String> {
    let mut report = read_report(path)?;
    // An unfinished run: what the core undo would see.
    gallery_core::undo::merge_journal(path, &mut report).map_err(|e| e.to_string())?;
    let head = plan_of_report(path).and_then(|p| plan_head(&p));
    let in_export = export_dir.is_some_and(|x| path.parent().is_some_and(|d| same_dir(d, x)));
    let side = match &head {
        _ if in_export => Side::Server,
        Some(h) => h.side,
        None => Side::Local,
    };
    let entry = entry_of(
        path,
        Ok(&report),
        side,
        head.as_ref().map(|h| h.items.len()),
    );
    let targets = plan_targets(path);
    let items = report
        .items
        .iter()
        .map(|it| {
            item_view(
                it,
                report.undone || report.undone_sources.contains(&it.source),
                targets.get(&it.source).cloned(),
            )
        })
        .collect();
    Ok(ReportDetail {
        server_command: (side == Side::Server).then(|| server_command(&report, &entry.file_name)),
        undo: undo_preview(&report),
        warnings: report.tool_warnings.clone(),
        entry,
        items,
    })
}

/// The report at `path` if it records a local run of a GUI plan: it sits in
/// `plans_dir`, next to its plan, which is a local plan, and it was written
/// for that plan's path. Anything else is refused: its paths may be
/// another machine's.
pub fn ensure_local_report(path: &Path, plans_dir: Option<&Path>) -> Result<Report, String> {
    let outside = || {
        gallery_core::t!(
            "history.outside",
            refusal = server_refusal(),
            path = path.display()
        )
    };
    let dir = plans_dir.ok_or_else(|| gallery_core::t!("history.no_plans_dir"))?;
    let parent = path.parent().ok_or_else(outside)?;
    let (Ok(parent), Ok(dir)) = (parent.canonicalize(), dir.canonicalize()) else {
        return Err(outside());
    };
    if parent != dir {
        return Err(outside());
    }
    let report = read_report(path)?;
    let plan = plan_of_report(path).ok_or_else(|| gallery_core::t!("history.not_a_report_name"))?;
    let head = plan_head(&plan)
        .ok_or_else(|| gallery_core::t!("history.plan_missing", path = plan.display()))?;
    if head.side == Side::Server {
        return Err(server_refusal());
    }
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    if canon(&report.plan) != canon(&plan) {
        return Err(gallery_core::t!(
            "history.other_path",
            report = report.plan.display(),
            plan = plan.display()
        ));
    }
    Ok(report)
}

/// What "Undo" asks for (also the running undo, for a reloaded webview).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct UndoProgress {
    pub done: usize,
    pub total: usize,
    pub current: Option<String>,
}

/// `undo_status` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UndoStatus {
    pub report_path: String,
    pub progress: UndoProgress,
}

/// `undo-done` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UndoDone {
    pub report_path: String,
    pub restored: usize,
    pub removed_outputs: usize,
    /// Files left as they were, and why; a rerun of undo retries them.
    pub problems: Vec<String>,
    pub cancelled: bool,
    /// Every item of the report is undone now.
    pub undone: bool,
}

/// `undo-error` payload: the undo did not run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UndoError {
    pub message: String,
}

impl From<String> for UndoError {
    fn from(message: String) -> Self {
        UndoError { message }
    }
}

/// Check that undo of the report may start: local, not undone yet.
pub fn guard_undo(path: &Path, plans_dir: Option<&Path>) -> Result<Report, String> {
    let r = ensure_local_report(path, plans_dir)?;
    if r.undone {
        return Err(gallery_core::t!("history.already_undone"));
    }
    Ok(r)
}

/// Undo the run of the local report at `path` (checked again here) with a
/// progress snapshot per undone item and `cancel` between items.
pub fn undo(
    path: &Path,
    plans_dir: Option<&Path>,
    on_progress: &(dyn Fn(UndoProgress) + Sync),
    cancel: &Cancel,
) -> Result<UndoDone, UndoError> {
    guard_undo(path, plans_dir)?;
    let progress = |ev: ProgressEvent| {
        on_progress(UndoProgress {
            done: ev.done,
            total: ev.total,
            current: ev.current.map(|p| shown(&p)),
        })
    };
    let s = gallery_core::undo::undo_with(path, &progress, cancel).map_err(|e| e.to_string())?;
    let undone = read_report(path).map(|r| r.undone).unwrap_or(false);
    Ok(UndoDone {
        report_path: shown(path),
        restored: s.restored,
        removed_outputs: s.removed_outputs,
        problems: s.problems,
        cancelled: s.cancelled,
        undone,
    })
}

/// Which items of a run an undo takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// Every item not undone yet.
    All,
    /// These files only.
    Files(Vec<PathBuf>),
    /// Every file the plan routed to the target.
    Target(String),
}

/// Undo `selection` of the local report at `path` (checked again here, a
/// server report is refused as in [`undo`]). A partial undo records its
/// items in the report; a later full undo skips them.
pub fn undo_selected(
    path: &Path,
    plans_dir: Option<&Path>,
    selection: &Selection,
    on_progress: &(dyn Fn(UndoProgress) + Sync),
    cancel: &Cancel,
) -> Result<UndoDone, UndoError> {
    let sources = match selection {
        Selection::All => return undo(path, plans_dir, on_progress, cancel),
        Selection::Files(f) => f.clone(),
        Selection::Target(t) => selection_sources(path, plans_dir, t)?,
    };
    guard_undo(path, plans_dir)?;
    if sources.is_empty() {
        return Err(gallery_core::t!("history.no_files_selected").into());
    }
    gallery_core::undo::ensure_in_report(path, &sources).map_err(|e| e.to_string())?;
    let progress = |ev: ProgressEvent| {
        on_progress(UndoProgress {
            done: ev.done,
            total: ev.total,
            current: ev.current.map(|p| shown(&p)),
        })
    };
    let s = gallery_core::undo::undo_items_with(path, &sources, &progress, cancel)
        .map_err(|e| e.to_string())?;
    let undone = read_report(path).map(|r| r.undone).unwrap_or(false);
    Ok(UndoDone {
        report_path: shown(path),
        restored: s.restored,
        removed_outputs: s.removed_outputs,
        problems: s.problems,
        cancelled: s.cancelled,
        undone,
    })
}

/// The files of `target` in the local report (after the local checks).
fn selection_sources(
    path: &Path,
    plans_dir: Option<&Path>,
    target: &str,
) -> Result<Vec<PathBuf>, String> {
    guard_undo(path, plans_dir)?;
    let plan = plan_of_report(path);
    gallery_core::undo::target_sources(path, plan.as_deref(), target).map_err(|e| e.to_string())
}

/// `start_retry` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetryDone {
    pub report_path: String,
    /// The saved retry plan; `None` when no failed file could be retried.
    pub plan_path: Option<String>,
    /// Failed files of the run.
    pub failed: usize,
    /// Of them, in the retry plan.
    pub files: usize,
    /// Failed files left out: changed since, their source went to the
    /// trash, or their leftover outputs could not be removed.
    pub left_out: usize,
    /// What a run of the retry plan would do.
    pub preview: Option<ApplyPreview>,
    /// The retry plan was prepared before and has not run yet: given back
    /// as it is, nothing was touched now.
    pub reused: bool,
}

/// `<stem>-retry.plan.json` next to the plan (`-retry-2`, … if taken).
pub fn retry_path(plan: &Path) -> PathBuf {
    let name = plan
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = name
        .strip_suffix(".plan.json")
        .or_else(|| name.strip_suffix(".json"))
        .unwrap_or(&name)
        .to_string();
    (1..)
        .map(|n| match n {
            1 => plan.with_file_name(format!("{stem}-retry.plan.json")),
            n => plan.with_file_name(format!("{stem}-retry-{n}.plan.json")),
        })
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .expect("a free name")
}

/// A retry plan of `plan` that was prepared but has not run yet (no report
/// next to it): a second retry gives it back instead of a duplicate.
pub fn pending_retry(plan: &Path, run_id: &str) -> Option<PathBuf> {
    retry_plans(plan, run_id)
        .into_iter()
        .rfind(|p| std::fs::symlink_metadata(gallery_core::apply::report_path(p)).is_err())
}

/// Retry plans of `plan` made for the report with `run_id`, by name order.
fn retry_plans(plan: &Path, run_id: &str) -> Vec<PathBuf> {
    let none = Vec::new;
    let Some(name) = plan.file_name().and_then(|n| n.to_str()) else {
        return none();
    };
    let Some(stem) = name
        .strip_suffix(".plan.json")
        .or_else(|| name.strip_suffix(".json"))
    else {
        return none();
    };
    let prefix = format!("{stem}-retry");
    let Some(dir) = plan.parent() else {
        return none();
    };
    let Ok(rd) = std::fs::read_dir(dir) else {
        return none();
    };
    let mut found: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.strip_prefix(&prefix)
                    .and_then(|rest| rest.strip_suffix(".plan.json"))
                    .is_some_and(|mid| {
                        mid.is_empty()
                            || mid
                                .strip_prefix('-')
                                .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()))
                    })
            })
        })
        .filter(|p| {
            gallery_core::plan::Plan::load(p).is_ok_and(|q| q.retry_of.as_deref() == Some(run_id))
        })
        .collect();
    // By the number in the name (`-retry` = 1, `-retry-2`, … `-retry-10`),
    // not by text: `-retry-10` is later than `-retry-2`.
    let number = |p: &PathBuf| -> u64 {
        p.file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix(&prefix))
            .and_then(|rest| rest.strip_suffix(".plan.json"))
            .and_then(|mid| mid.strip_prefix('-'))
            .and_then(|n| n.parse().ok())
            .unwrap_or(1)
    };
    found.sort_by_key(|p| (number(p), p.clone()));
    found
}

/// Sources a retry plan of this report already ran successfully: they must
/// not be retried (or undone) again, whether or not they stayed in place.
fn retried_done(plan: &Path, run_id: &str) -> std::collections::HashSet<PathBuf> {
    let mut done = std::collections::HashSet::new();
    for p in retry_plans(plan, run_id) {
        let rp = gallery_core::apply::report_path(&p);
        let Ok(mut r) = read_report(&rp) else {
            continue;
        };
        // A retry that died has its Done items only in the journal.
        if gallery_core::undo::merge_journal(&rp, &mut r).is_err() {
            continue;
        }
        if r.undone {
            continue;
        }
        done.extend(
            r.items
                .iter()
                .filter(|it| matches!(it.status, ItemStatus::Done { .. }))
                .filter(|it| !r.undone_sources.contains(&it.source))
                .map(|it| it.source.clone()),
        );
    }
    done
}

/// "Retry failed": for the local report at `path`, the core removes
/// the outputs the failed files had already placed (a partial undo) and
/// builds the plan of the failed files that are unchanged; it is saved
/// next to the plan as `…-retry.plan.json`. Nothing runs yet: the Run
/// screen runs it after its own confirmation.
pub fn retry(path: &Path, plans_dir: Option<&Path>) -> Result<RetryDone, String> {
    let report = ensure_local_report(path, plans_dir)?;
    let failed = report
        .items
        .iter()
        .filter(|i| matches!(i.status, ItemStatus::Failed { .. }))
        .count();
    if failed == 0 {
        return Err(gallery_core::t!("history.no_failed"));
    }
    let plan = plan_of_report(path).ok_or_else(|| gallery_core::t!("history.not_a_report"))?;
    if let Some(p) = pending_retry(&plan, &report.run_id) {
        let sub = gallery_core::plan::Plan::load(&p).map_err(|e| e.to_string())?;
        return Ok(RetryDone {
            report_path: shown(path),
            plan_path: Some(shown(&p)),
            failed,
            files: sub.items.len(),
            left_out: failed.saturating_sub(sub.items.len()),
            preview: Some(preview_of(&sub, &p)),
            reused: true,
        });
    }
    let sub = gallery_core::undo::prepare_retry_excluding(
        &plan,
        path,
        &retried_done(&plan, &report.run_id),
    )
    .map_err(|e| e.to_string())?;
    let files = sub.items.len();
    let mut done = RetryDone {
        report_path: shown(path),
        plan_path: None,
        failed,
        files,
        left_out: failed.saturating_sub(files),
        preview: None,
        reused: false,
    };
    if files > 0 {
        let out = retry_path(&plan);
        sub.save(&out).map_err(|e| e.to_string())?;
        done.preview = Some(preview_of(&sub, &out));
        done.plan_path = Some(shown(&out));
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::{ApplyRequest, run as apply_run};
    use crate::scan::tests::ScanEnv;
    use gallery_core::plan::Plan;
    use gallery_core::tools::Tools;
    use gallery_core::trash::TrashMode;
    use serde_json::json;
    use std::sync::Mutex;

    /// A scanned fixture in `d` (plans in `d/plans`), applied for real with
    /// the folder trash; `block` makes the output folder a file first so
    /// every item fails untouched.
    fn applied(d: &Path, block: bool) -> (PathBuf, PathBuf, Tools) {
        let config = crate::scan::tests::fixture(d);
        let plans = d.join("plans");
        let tools = Tools::discover().unwrap();
        let env = ScanEnv {
            config: &config,
            plans_dir: &plans,
            overrides: None,
            tools: &tools,
        };
        let done =
            crate::scan::tests::run(&env, None, &|_: ProgressEvent| {}, &Cancel::new()).unwrap();
        if block {
            std::fs::create_dir_all(d.join("out")).unwrap();
            std::fs::write(d.join("out/rest"), "in the way").unwrap();
        }
        let req = ApplyRequest {
            plan_path: done.plan_path.clone(),
            dry_run: false,
            jobs: Some(1),
            allow_anomalies: false,
            pause_sync: false,
        };
        let out = apply_run(&req, &tools, TrashMode::Folder, &|_| {}, &Cancel::new()).unwrap();
        (PathBuf::from(out.report_path.unwrap()), plans, tools)
    }

    fn src_files(d: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(d.join("src"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.'))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn report_names_and_their_plans() {
        assert!(is_report_name("20261105-101010.plan.report.json"));
        assert!(is_report_name(
            "20261105-101010.plan.report.20261105-101011_1.json"
        ));
        assert!(!is_report_name("20261105-101010.plan.json"));
        assert!(!is_report_name("20261105-101010.plan.report.jsonl"));
        assert!(!is_report_name(".report-x.report.json"));
        let p = Path::new("/p/20261105-101010.plan.report.20261105-101011.json");
        assert_eq!(
            plan_of_report(p).unwrap(),
            Path::new("/p/20261105-101010.plan.json")
        );
        assert_eq!(
            plan_of_report(Path::new("/p/a.plan.report.json")).unwrap(),
            Path::new("/p/a.plan.json")
        );
        assert_eq!(plan_of_report(Path::new("/p/a.plan.json")), None);
        assert_eq!(
            retry_path(Path::new("/nonexistent/a.plan.json")),
            Path::new("/nonexistent/a-retry.plan.json")
        );
    }

    #[test]
    fn retry_names_never_collide() {
        let d = tempfile::tempdir().unwrap();
        let plan = d.path().join("20261105-101010.plan.json");
        let first = retry_path(&plan);
        assert!(first.ends_with("20261105-101010-retry.plan.json"));
        std::fs::write(&first, "{}").unwrap();
        assert!(retry_path(&plan).ends_with("20261105-101010-retry-2.plan.json"));
    }

    #[test]
    fn shell_quoting() {
        assert_eq!(
            shell_quote("/srv/plans/a.plan.report.json"),
            "/srv/plans/a.plan.report.json"
        );
        assert_eq!(shell_quote("/srv/my plans/a"), "'/srv/my plans/a'");
        assert_eq!(shell_quote("/srv/it's"), r"'/srv/it'\''s'");
    }

    /// Fixture apply → the list shows it → undo restores every source and
    /// removes the outputs → the list says undone and a second undo is
    /// refused; the plan's preview says `undone`.
    #[test]
    fn apply_then_list_then_undo() {
        let d = tempfile::tempdir().unwrap();
        let (rp, plans, _) = applied(d.path(), false);
        assert!(
            src_files(d.path()).is_empty(),
            "all sources went to the trash"
        );

        let list = list(Some(&plans), None);
        assert_eq!(list.len(), 1);
        let e = &list[0];
        assert_eq!(e.path, rp.display().to_string());
        assert_eq!(e.side, Side::Local);
        assert!(e.plan_exists && !e.archived && !e.undone && !e.partially_undone);
        assert_eq!(
            (e.summary.total, e.summary.done, e.summary.failed),
            (3, 3, 0)
        );
        assert!(!e.finished.is_empty());

        let detail = load(&rp, None).unwrap();
        assert_eq!(detail.items.len(), 3);
        assert!(
            detail
                .items
                .iter()
                .all(|i| i.status == "done" && i.trashed.is_some())
        );
        assert_eq!(
            detail.undo,
            UndoPreview {
                items: 3,
                restore: 3,
                rename_back: 0,
                remove_outputs: 3
            }
        );
        assert_eq!(detail.server_command, None);

        let seen = Mutex::new(Vec::new());
        let done = undo(
            &rp,
            Some(&plans),
            &|p| seen.lock().unwrap().push(p),
            &Cancel::new(),
        )
        .unwrap();
        assert_eq!((done.restored, done.removed_outputs), (3, 3));
        assert!(done.undone && !done.cancelled && done.problems.is_empty());
        assert_eq!(seen.lock().unwrap().len(), 3);
        assert_eq!(src_files(d.path()), ["cat.jpg", "kept.txt", "note.txt"]);
        assert_eq!(
            std::fs::read_to_string(d.path().join("src/cat.jpg")).unwrap(),
            "cat.jpg"
        );
        assert_eq!(d.path().join("out/rest").read_dir().unwrap().count(), 0);

        let e = &super::list(Some(&plans), None)[0];
        assert!(e.undone && !e.partially_undone);
        assert_eq!(load(&rp, None).unwrap().undo, UndoPreview::default());
        let again = undo(&rp, Some(&plans), &|_| {}, &Cancel::new()).unwrap_err();
        assert!(
            again.message.contains("already undone"),
            "{}",
            again.message
        );
        let plan = plan_of_report(&rp).unwrap();
        assert_eq!(
            crate::apply::preview(&plan).unwrap().applied,
            Some(crate::apply::Applied::Undone)
        );
    }

    /// A fixture with three targets: `pics` (jpg, outputs), `rest` (text,
    /// outputs) and `junk` (tmp, trash kind).
    fn three_targets(d: &Path) -> (PathBuf, PathBuf) {
        let src = d.join("src");
        std::fs::create_dir(&src).unwrap();
        for f in ["cat.jpg", "dog.jpg", "note.txt", "junk.tmp"] {
            std::fs::write(src.join(f), f).unwrap();
        }
        let q = |p: PathBuf| p.to_string_lossy().replace('\\', "/");
        let toml = format!(
            r#"
[[sources]]
id = "s"
path = "{src}"

[[targets]]
name = "junk"
kind = "trash"
filters = [{{ sources = ["s"], match = "*.tmp" }}]

[[targets]]
name = "pics"
filters = [{{ sources = ["s"], match = "*.jpg" }}]
outputs = [{{ dir = "{out}/pics" }}]

[[targets]]
name = "rest"
filters = [{{ sources = ["s"], match = "*" }}]
outputs = [{{ dir = "{out}/rest" }}]
"#,
            src = q(src),
            out = q(d.join("out"))
        );
        let cfg = d.join("gallery.toml");
        std::fs::write(&cfg, toml).unwrap();
        let plans = d.join("plans");
        let tools = Tools::discover().unwrap();
        let env = ScanEnv {
            config: &cfg,
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
        let out = apply_run(&req, &tools, TrashMode::Folder, &|_| {}, &Cancel::new()).unwrap();
        (PathBuf::from(out.report_path.unwrap()), plans)
    }

    fn out_names(d: &Path, sub: &str) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(d.join("out").join(sub))
            .map(|r| {
                r.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    /// Undo of one target restores only its files; another target's outputs
    /// stay; one file of another target undoes alone; the later full undo
    /// finishes the rest; a server-side report is refused.
    #[test]
    fn undo_one_target_then_one_file_then_the_rest() {
        let d = tempfile::tempdir().unwrap();
        let (rp, plans) = three_targets(d.path());
        assert!(src_files(d.path()).is_empty());
        assert_eq!(out_names(d.path(), "pics").len(), 2);
        assert_eq!(out_names(d.path(), "rest").len(), 1);

        let detail = load(&rp, None).unwrap();
        let of = |t: &str| {
            detail
                .items
                .iter()
                .filter(|i| i.target.as_deref() == Some(t))
                .count()
        };
        assert_eq!((of("pics"), of("rest"), of("junk")), (2, 1, 1));
        let junk = detail
            .items
            .iter()
            .find(|i| i.target.as_deref() == Some("junk"))
            .unwrap();
        assert_eq!(
            junk.undo,
            UndoPreview {
                items: 1,
                restore: 1,
                rename_back: 0,
                remove_outputs: 0
            }
        );

        // The trash target: its source comes back, nothing else moves.
        let r = undo_selected(
            &rp,
            Some(&plans),
            &Selection::Target("junk".into()),
            &|_| {},
            &Cancel::new(),
        )
        .unwrap();
        assert_eq!((r.restored, r.removed_outputs, r.undone), (1, 0, false));
        assert_eq!(src_files(d.path()), ["junk.tmp"]);
        assert_eq!(out_names(d.path(), "pics").len(), 2);

        // The pics target: its sources come back and its outputs go.
        let r = undo_selected(
            &rp,
            Some(&plans),
            &Selection::Target("pics".into()),
            &|_| {},
            &Cancel::new(),
        )
        .unwrap();
        assert_eq!((r.restored, r.removed_outputs), (2, 2));
        assert_eq!(src_files(d.path()), ["cat.jpg", "dog.jpg", "junk.tmp"]);
        assert!(out_names(d.path(), "pics").is_empty());
        assert_eq!(out_names(d.path(), "rest").len(), 1, "other target stays");
        assert!(!read_report(&rp).unwrap().undone);
        assert_eq!(load(&rp, None).unwrap().undo.items, 1);

        // Already undone files are no work for a second try.
        let again = undo_selected(
            &rp,
            Some(&plans),
            &Selection::Files(vec![d.path().join("src/cat.jpg")]),
            &|_| {},
            &Cancel::new(),
        )
        .unwrap();
        assert_eq!((again.restored, again.removed_outputs), (0, 0));

        // A path outside the run and an unknown target are errors.
        let bad = Selection::Files(vec![d.path().join("src/nope.jpg")]);
        assert!(undo_selected(&rp, Some(&plans), &bad, &|_| {}, &Cancel::new()).is_err());
        let bad = Selection::Target("nope".into());
        assert!(undo_selected(&rp, Some(&plans), &bad, &|_| {}, &Cancel::new()).is_err());

        // The full undo skips what is done and finishes the run.
        let done = undo(&rp, Some(&plans), &|_| {}, &Cancel::new()).unwrap();
        assert_eq!((done.restored, done.removed_outputs), (1, 1));
        assert!(done.undone);
        assert_eq!(src_files(d.path()).len(), 4);
        assert!(out_names(d.path(), "rest").is_empty());
    }

    #[test]
    fn partial_undo_of_a_server_report_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let (rp, _plans) = three_targets(d.path());
        let other = d.path().join("elsewhere");
        std::fs::create_dir(&other).unwrap();
        let e = undo_selected(
            &rp,
            Some(&other),
            &Selection::Target("pics".into()),
            &|_| {},
            &Cancel::new(),
        )
        .unwrap_err();
        assert!(!e.message.is_empty());
        assert_eq!(out_names(d.path(), "pics").len(), 2);
    }

    #[test]
    fn a_cancelled_undo_touches_nothing_and_leaves_the_report_open() {
        let d = tempfile::tempdir().unwrap();
        let (rp, plans, _) = applied(d.path(), false);
        let c = Cancel::new();
        c.cancel();
        let done = undo(&rp, Some(&plans), &|_| {}, &c).unwrap();
        assert!(done.cancelled && !done.undone);
        assert_eq!(done.restored, 0);
        assert!(src_files(d.path()).is_empty());
        assert!(!list(Some(&plans), None)[0].undone);
    }

    /// Every item fails untouched; with the obstacle gone, "Retry
    /// failed" saves `…-retry.plan.json` with all three, which runs clean.
    #[test]
    fn retry_failed_files() {
        let d = tempfile::tempdir().unwrap();
        let (rp, plans, tools) = applied(d.path(), true);
        let e = &list(Some(&plans), None)[0];
        assert_eq!(e.summary.failed, 3);
        assert_eq!(src_files(d.path()), ["cat.jpg", "kept.txt", "note.txt"]);
        let detail = load(&rp, None).unwrap();
        assert!(
            detail
                .items
                .iter()
                .all(|i| i.status == "failed" && i.trashed.is_none())
        );

        std::fs::remove_file(d.path().join("out/rest")).unwrap();
        let r = retry(&rp, Some(&plans)).unwrap();
        assert_eq!((r.failed, r.files, r.left_out), (3, 3, 0));
        let sub = PathBuf::from(r.plan_path.clone().unwrap());
        assert!(
            sub.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .ends_with("-retry.plan.json")
        );
        assert_eq!(sub.parent().unwrap(), plans);
        let pv = r.preview.unwrap();
        assert_eq!((pv.files, pv.trash), (3, 3));
        // Nothing ran yet.
        assert_eq!(src_files(d.path()).len(), 3);
        // While that plan has not run, a second retry gives the same plan
        // back instead of a duplicate (and touches nothing).
        let again = retry(&rp, Some(&plans)).unwrap();
        assert!(again.reused);
        assert_eq!(again.plan_path, r.plan_path);
        assert_eq!(again.files, 3);
        let retries = std::fs::read_dir(&plans)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains("-retry")
            })
            .count();
        assert_eq!(retries, 1);

        let req = ApplyRequest {
            plan_path: sub.display().to_string(),
            dry_run: false,
            jobs: Some(1),
            allow_anomalies: false,
            pause_sync: false,
        };
        crate::apply::guard_real_run(&req, None).unwrap();
        let out = apply_run(&req, &tools, TrashMode::Folder, &|_| {}, &Cancel::new()).unwrap();
        assert_eq!((out.summary.done, out.summary.failed), (3, 0));
        assert!(src_files(d.path()).is_empty());
        // Both runs are listed, the retry first.
        let l = list(Some(&plans), None);
        assert_eq!(l.len(), 2);
        assert!(l.iter().any(|e| e.plan_path == sub.display().to_string()));
        // A second retry of the old report finds the sources gone.
        let r2 = retry(&rp, Some(&plans)).unwrap();
        assert_eq!((r2.files, r2.plan_path), (0, None));
    }

    /// The retry plan ran and its sources are back in place (hard links kept
    /// before the run): a second retry of the old report builds nothing and
    /// does not touch the retry's outputs; the retry is recorded for that
    /// very report.
    #[test]
    fn no_second_retry_after_the_retry_ran() {
        let d = tempfile::tempdir().unwrap();
        let (rp, plans, tools) = applied(d.path(), true);
        std::fs::remove_file(d.path().join("out/rest")).unwrap();
        let r = retry(&rp, Some(&plans)).unwrap();
        let sub = PathBuf::from(r.plan_path.clone().unwrap());
        let run_id = read_report(&rp).unwrap().run_id;
        assert_eq!(
            gallery_core::plan::Plan::load(&sub).unwrap().retry_of,
            Some(run_id.clone())
        );
        assert_eq!(pending_retry(&sub_plan_of(&rp), &run_id), Some(sub.clone()));
        assert_eq!(pending_retry(&sub_plan_of(&rp), "other-run"), None);

        let keep = d.path().join("keep");
        std::fs::create_dir_all(&keep).unwrap();
        let names = src_files(d.path());
        for n in &names {
            std::fs::hard_link(d.path().join("src").join(n), keep.join(n)).unwrap();
        }
        let req = ApplyRequest {
            plan_path: sub.display().to_string(),
            dry_run: false,
            jobs: Some(1),
            allow_anomalies: false,
            pause_sync: false,
        };
        crate::apply::guard_real_run(&req, None).unwrap();
        let out = apply_run(&req, &tools, TrashMode::Folder, &|_| {}, &Cancel::new()).unwrap();
        assert_eq!((out.summary.done, out.summary.failed), (3, 0));
        for n in &names {
            std::fs::hard_link(keep.join(n), d.path().join("src").join(n)).unwrap();
        }
        assert_eq!(src_files(d.path()), names);

        let r2 = retry(&rp, Some(&plans)).unwrap();
        assert_eq!((r2.files, r2.plan_path, r2.left_out), (0, None, 3));
        let retries = std::fs::read_dir(&plans)
            .unwrap()
            .filter(|e| {
                let n = e.as_ref().unwrap().file_name();
                n.to_string_lossy().ends_with("-retry-2.plan.json")
            })
            .count();
        assert_eq!(retries, 0);
    }

    fn sub_plan_of(report: &Path) -> PathBuf {
        plan_of_report(report).unwrap()
    }

    /// A retry run that died leaves its Done items only in the journal:
    /// they still count as retried, or the next retry would duplicate them.
    #[test]
    fn retried_done_reads_the_journal_of_an_unfinished_retry() {
        let d = tempfile::tempdir().unwrap();
        let (rp, plans, _) = applied(d.path(), true);
        std::fs::remove_file(d.path().join("out/rest")).unwrap();
        let r = retry(&rp, Some(&plans)).unwrap();
        let sub = PathBuf::from(r.plan_path.unwrap());
        let plan = sub_plan_of(&rp);
        let run_id = read_report(&rp).unwrap().run_id;
        let item = |s: &str| json!({"source": s, "status": {"state": "done", "trashed": null}, "outputs": []});
        let sub_rp = gallery_core::apply::report_path(&sub);
        let report = json!({
            "run_id": "rr", "plan": sub, "started": "", "finished": "",
            "tool_warnings": [], "items": []
        });
        std::fs::write(&sub_rp, report.to_string()).unwrap();
        let line = json!({"run_id": "rr", "item": item("/s/journaled.jpg")}).to_string() + "\n";
        std::fs::write(gallery_core::apply::journal_path(&sub_rp), line).unwrap();
        let done = retried_done(&plan, &run_id);
        assert!(done.contains(Path::new("/s/journaled.jpg")), "{done:?}");
    }

    /// Retry plans order by their number, not by name: `-retry-10` is later
    /// than `-retry-2`, so it is the pending one that is picked.
    #[test]
    fn the_latest_pending_retry_is_the_highest_number() {
        let d = tempfile::tempdir().unwrap();
        let (rp, plans, _) = applied(d.path(), true);
        std::fs::remove_file(d.path().join("out/rest")).unwrap();
        let r = retry(&rp, Some(&plans)).unwrap();
        let first = PathBuf::from(r.plan_path.unwrap());
        let plan = sub_plan_of(&rp);
        let run_id = read_report(&rp).unwrap().run_id;
        let stem = plan.file_name().unwrap().to_str().unwrap();
        let stem = stem.strip_suffix(".plan.json").unwrap();
        let sub = Plan::load(&first).unwrap();
        for n in [2, 10] {
            sub.save(&plans.join(format!("{stem}-retry-{n}.plan.json")))
                .unwrap();
        }
        assert_eq!(
            pending_retry(&plan, &run_id),
            Some(plans.join(format!("{stem}-retry-10.plan.json")))
        );
    }

    /// An unfinished run: the preview counts the journal's items like the
    /// core undo does.
    #[test]
    fn undo_preview_merges_the_journal_of_an_unfinished_run() {
        let d = tempfile::tempdir().unwrap();
        let rp = d.path().join("a.plan.report.json");
        let item = |s: &str, trashed: bool| {
            json!({"source": s,
                "status": {"state": "done", "trashed": trashed.then(|| json!({"kind": "folder", "path": "/t/x"}))},
                "outputs": []})
        };
        let report = json!({
            "run_id": "r", "plan": d.path().join("a.plan.json"), "started": "", "finished": "",
            "tool_warnings": [], "items": [item("/s/a.jpg", true)]
        });
        std::fs::write(&rp, report.to_string()).unwrap();
        let line = |run: &str, it: serde_json::Value| {
            json!({"run_id": run, "item": it}).to_string() + "\n"
        };
        let j = line("r", item("/s/b.jpg", true)) + &line("other", item("/s/c.jpg", true));
        std::fs::write(gallery_core::apply::journal_path(&rp), j).unwrap();
        let detail = load(&rp, None).unwrap();
        assert_eq!(detail.undo.restore, 2);
        assert_eq!(detail.items.len(), 2);
    }

    #[test]
    fn a_run_without_failures_has_nothing_to_retry() {
        let d = tempfile::tempdir().unwrap();
        let (rp, plans, _) = applied(d.path(), false);
        let e = retry(&rp, Some(&plans)).unwrap_err();
        assert!(e.contains("no failed"), "{e}");
    }

    /// A server report (in the export folder, or next to a server plan, or
    /// written for another plan path) is listed but never undone or retried,
    /// even when its paths exist here.
    #[test]
    fn server_reports_are_refused() {
        let d = tempfile::tempdir().unwrap();
        let (rp, plans, _) = applied(d.path(), false);
        let report = read_report(&rp).unwrap();

        // 1. Synced back into the export folder.
        let export = d.path().join("export");
        std::fs::create_dir(&export).unwrap();
        let plan = plan_of_report(&rp).unwrap();
        let mut server_plan = Plan::load(&plan).unwrap();
        server_plan.side = Side::Server;
        server_plan
            .save(&export.join("20261105-090000.plan.json"))
            .unwrap();
        let mut srv = report.clone();
        srv.plan = "/srv/sync/plans/20261105-090000.plan.json".into();
        srv.started = "2020-01-01T00:00:00+00:00".into();
        let srv_path = export.join("20261105-090000.plan.report.json");
        std::fs::write(&srv_path, serde_json::to_string(&srv).unwrap()).unwrap();

        let l = list(Some(&plans), Some(&export));
        assert_eq!(l.len(), 2);
        assert_eq!(
            (l[0].side, l[1].side),
            (Side::Local, Side::Server),
            "newest first"
        );
        let detail = load(&srv_path, Some(&export)).unwrap();
        assert_eq!(
            detail.server_command.as_deref(),
            Some("gallery undo /srv/sync/plans/20261105-090000.plan.report.json")
        );
        let e = undo(&srv_path, Some(&plans), &|_| {}, &Cancel::new()).unwrap_err();
        assert!(e.message.contains("server"), "{}", e.message);
        assert!(
            retry(&srv_path, Some(&plans))
                .unwrap_err()
                .contains("server")
        );

        // 2. In the plans folder, next to a server plan.
        let p2 = plans.join("20261105-080000.plan.json");
        server_plan.save(&p2).unwrap();
        let mut r2 = report.clone();
        r2.plan = p2.clone();
        let r2_path = plans.join("20261105-080000.plan.report.json");
        std::fs::write(&r2_path, serde_json::to_string(&r2).unwrap()).unwrap();
        assert!(
            list(Some(&plans), None)
                .iter()
                .any(|e| e.side == Side::Server)
        );
        let e = undo(&r2_path, Some(&plans), &|_| {}, &Cancel::new()).unwrap_err();
        assert!(e.message.contains("server"), "{}", e.message);

        // 3. Next to a local plan, but written for another plan path.
        let p3 = plans.join("20261105-070000.plan.json");
        std::fs::copy(&plan, &p3).unwrap();
        let mut r3 = report.clone();
        r3.plan = "/elsewhere/20261105-070000.plan.json".into();
        let r3_path = plans.join("20261105-070000.plan.report.json");
        std::fs::write(&r3_path, serde_json::to_string(&r3).unwrap()).unwrap();
        let e = undo(&r3_path, Some(&plans), &|_| {}, &Cancel::new()).unwrap_err();
        assert!(e.message.contains("different path"), "{}", e.message);

        // None of it touched a file: the real run's sources are still in
        // the trash and its outputs in place.
        assert!(src_files(d.path()).is_empty());
        assert_eq!(d.path().join("out/rest").read_dir().unwrap().count(), 3);
        assert!(!read_report(&rp).unwrap().undone);
        // No GUI plans folder: nothing is local.
        assert!(ensure_local_report(&rp, None).is_err());
    }

    #[test]
    fn archived_and_unreadable_reports_are_listed() {
        let d = tempfile::tempdir().unwrap();
        let (rp, plans, _) = applied(d.path(), false);
        let arch = plan_of_report(&rp)
            .unwrap()
            .with_extension("report.20200101-000000.json");
        let mut old = read_report(&rp).unwrap();
        old.started = "2020-01-01T00:00:00+00:00".into();
        old.undone_sources = vec![old.items[0].source.clone()];
        std::fs::write(&arch, serde_json::to_string(&old).unwrap()).unwrap();
        std::fs::write(plans.join("x.plan.report.json"), "{").unwrap();
        let l = list(Some(&plans), None);
        assert_eq!(l.len(), 3);
        let a = l.iter().find(|e| e.archived).unwrap();
        assert!(a.partially_undone && a.undone_items == 1);
        assert_eq!(
            a.plan_path,
            plan_of_report(&rp).unwrap().display().to_string()
        );
        let bad = l.iter().find(|e| e.error.is_some()).unwrap();
        assert!(!bad.plan_exists);
        assert_eq!(l.last().unwrap().path, bad.path, "unparsable last");
        // The archived detail marks the undone item.
        let det = load(&arch, None).unwrap();
        assert_eq!(det.items.iter().filter(|i| i.undone).count(), 1);
        assert_eq!(det.undo.items, 2);
    }

    #[test]
    fn undo_preview_counts_renames_and_in_place_outputs() {
        let r: Report = serde_json::from_value(json!({
            "run_id": "x", "plan": "/p/a.plan.json", "started": "", "finished": "f",
            "tool_warnings": [], "undone_sources": ["/s/done.jpg"],
            "items": [
                {"source": "/s/a.jpg", "status": {"state": "done", "trashed": {"kind": "folder", "path": "/t/a.jpg"}},
                 "outputs": [{"planned": "/o/a.jpg", "actual": "/o/a.jpg", "error": null, "warnings": [], "fell_back": false},
                             {"planned": "/s/a.jpg", "actual": "/s/a.jpg", "error": null, "warnings": [], "fell_back": false}]},
                {"source": "/s/b.jpg", "status": {"state": "done", "trashed": null},
                 "outputs": [{"planned": "/s/B.jpg", "actual": "/s/B.jpg", "renamed_from": "/s/b.jpg", "error": null, "warnings": [], "fell_back": false},
                             {"planned": "/o/b.jpg", "actual": "/o/b.jpg", "error": null, "warnings": [], "fell_back": false}]},
                {"source": "/s/c.jpg", "status": {"state": "stale"}, "outputs": []},
                {"source": "/s/done.jpg", "status": {"state": "done", "trashed": {"kind": "system", "original": "/s/done.jpg"}},
                 "outputs": [{"planned": "/o/d.jpg", "actual": "/o/d.jpg", "error": null, "warnings": [], "fell_back": false}]}
            ]
        }))
        .unwrap();
        assert_eq!(
            undo_preview(&r),
            UndoPreview {
                items: 2,
                restore: 1,
                rename_back: 1,
                remove_outputs: 3
            }
        );
        assert_eq!(
            server_command(&r, "a.plan.report.json"),
            "gallery undo /p/a.plan.report.json"
        );
    }
}
