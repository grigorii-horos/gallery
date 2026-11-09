//! Undo of an applied plan, from its report: sources come back from the
//! trash to their places, then the outputs are removed.
//!
//! Safety rules:
//! - nothing is overwritten: an occupied original place keeps its occupant
//!   and the source stays in the trash;
//! - an item's outputs are removed only once its source is back (or was never
//!   trashed): until then they may be the only copy;
//! - an output whose size or blake3 differs from what apply placed is kept;
//! - a path that is any item's source is never removed as an output, unless
//!   that item's source is still in the trash (so it can still come back):
//!   this holds across a crashed and rerun undo too;
//! - a source renamed in place gets its old name back first (only when that
//!   name is free and the file is unchanged); its other outputs go only after
//!   that, and the renamed file itself is never removed.

use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::apply::{
    ItemResult, ItemStatus, OutputResult, Report, RunLock, file_hash, journal_path, move_noreplace,
    read_journal, rename_noreplace, write_report,
};
use crate::plan::{Plan, subplan};
use crate::progress::{Cancel, NoProgress, Progress, ProgressEvent, Stage};
use crate::trash::TrashRef;
use crate::{Error, Result};

/// Prefix of an in-place output moved aside so that its source can come back.
const ASIDE_PREFIX: &str = ".gallery-undo-";
/// Seconds of slack around the run when matching system trash deletion times
/// (stored with whole-second precision).
const WINDOW_SLACK: i64 = 2;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UndoSummary {
    pub restored: usize,
    pub removed_outputs: usize,
    pub problems: Vec<String>,
    /// The undo was cancelled before every item was undone; a rerun of
    /// undo finishes the rest.
    pub cancelled: bool,
}

/// Undo the run recorded in the report at `report_path`. Per-item trouble goes
/// to `problems`; items undone without trouble are recorded in the report and
/// skipped by a rerun. Only an undo without problems marks the report
/// `undone`; undoing an undone report is an error.
pub fn undo(report_path: &Path) -> Result<UndoSummary> {
    undo_with(report_path, &NoProgress, &Cancel::new())
}

/// [`undo`] with a [`Stage::Undo`] event per finished item and cancellation:
/// once `cancel` is set no new item starts (the one in flight finishes); the
/// items undone so far are recorded in the report, which stays open, so a
/// rerun undoes the rest.
pub fn undo_with(
    report_path: &Path,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<UndoSummary> {
    undo_selected(report_path, None, progress, cancel)
}

/// Undo only the items of `sources` in the report at `report_path`, with
/// every guard of [`undo`]; the other items are left as they are. Items
/// undone without trouble are recorded in `undone_sources` (a later full
/// undo skips them); the report is marked `undone` only once every item is.
pub fn undo_items(report_path: &Path, sources: &[PathBuf]) -> Result<UndoSummary> {
    undo_items_with(report_path, sources, &NoProgress, &Cancel::new())
}

/// [`undo_items`] with progress and cancellation, as [`undo_with`].
pub fn undo_items_with(
    report_path: &Path,
    sources: &[PathBuf],
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<UndoSummary> {
    let only: HashSet<PathBuf> = sources.iter().cloned().collect();
    undo_selected(report_path, Some(&only), progress, cancel)
}

/// An error naming the first of `sources` that is not an item of the report.
pub fn ensure_in_report(report_path: &Path, sources: &[PathBuf]) -> Result<()> {
    resolve_in_report(report_path, sources).map(|_| ())
}

/// The spelling of a path that stands for the file itself: its longest
/// existing ancestor canonical (`/var` and `/private/var` are one place on
/// macOS, a symlinked folder is its target) and the rest appended, so a file
/// that is gone (moved by the run) still has one.
fn file_identity(p: &Path) -> PathBuf {
    crate::plan::resolve(p).unwrap_or_else(|| p.to_path_buf())
}

/// `sources` as the report spells them, matched by the exact path and else by
/// file identity ([`file_identity`]); an error names the first that is not an
/// item of the report (a typo or another run's file: [`undo_items`] alone
/// ignores such a path). Hand the result to [`undo_items`], which compares
/// the report's own spelling.
pub fn resolve_in_report(report_path: &Path, sources: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut report = read_report(report_path)?;
    merge_journal(report_path, &mut report)?;
    let known: Vec<&PathBuf> = report.items.iter().map(|it| &it.source).collect();
    let ids: HashMap<PathBuf, &PathBuf> = known.iter().map(|k| (file_identity(k), *k)).collect();
    sources
        .iter()
        .map(|s| {
            known
                .iter()
                .find(|k| **k == s)
                .copied()
                .or_else(|| ids.get(&file_identity(s)).copied())
                .cloned()
                .ok_or_else(|| Error::Plan(crate::t!("undo.no_such_file", path = s.display())))
        })
        .collect()
}

/// The plan the report belongs to: `plan`, else the one the report names,
/// else the file next to the report.
fn plan_for(report_path: &Path, report: &Report, plan: Option<&Path>) -> Result<Plan> {
    let no_plan = || Error::Plan(crate::t!("undo.no_plan"));
    let path = match plan {
        Some(p) => p.to_path_buf(),
        None if report.plan.is_file() => report.plan.clone(),
        None => {
            let name = report_path.file_name().and_then(|n| n.to_str());
            name.and_then(|n| n.rfind(".report.").map(|i| &n[..i]))
                .map(|stem| report_path.with_file_name(format!("{stem}.json")))
                .ok_or_else(no_plan)?
        }
    };
    if !path.is_file() {
        return Err(no_plan());
    }
    Plan::load(&path)
}

/// The target of each item of the report. A report written since targets
/// were recorded says it itself; for an older one the plan item of the same
/// source counts only when its planned outputs are the report item's: a plan
/// rescanned since may route the file elsewhere, and such an item has no
/// known target (it is left out).
pub fn item_targets(report: &Report, plan: Option<&Plan>) -> HashMap<PathBuf, String> {
    let planned = |outs: &mut dyn Iterator<Item = &PathBuf>| {
        let mut v: Vec<PathBuf> = outs.cloned().collect();
        v.sort();
        v
    };
    let by_source: HashMap<&PathBuf, &crate::plan::PlanItem> = plan
        .map(|p| p.items.iter().map(|it| (&it.source, it)).collect())
        .unwrap_or_default();
    let mut map = HashMap::new();
    for it in &report.items {
        let target = match &it.target {
            Some(t) => Some(t.clone()),
            None => by_source.get(&it.source).and_then(|pi| {
                let same = planned(&mut pi.outputs.iter().map(|o| &o.dest))
                    == planned(&mut it.outputs.iter().map(|o| &o.planned));
                same.then(|| pi.target.clone())
            }),
        };
        if let Some(t) = target {
            map.insert(it.source.clone(), t);
        }
    }
    map
}

/// The target of each item of the report at `report_path` (see
/// [`item_targets`]); the plan is read only for items without a recorded
/// target, and its absence just leaves those out.
pub fn report_targets(report_path: &Path, report: &Report) -> HashMap<PathBuf, String> {
    let plan = if report.items.iter().any(|it| it.target.is_none()) {
        plan_for(report_path, report, None).ok()
    } else {
        None
    };
    item_targets(report, plan.as_ref())
}

/// The sources of the report's items that were routed to `target`
/// (`folders` and `trash` targets alike; a `keep` target's files are not in
/// a report), the unfinished run's journal merged in. An error when no item
/// of the report is known to belong to the target.
pub fn target_sources(
    report_path: &Path,
    plan: Option<&Path>,
    target: &str,
) -> Result<Vec<PathBuf>> {
    let mut report = read_report(report_path)?;
    merge_journal(report_path, &mut report)?;
    let plan = if plan.is_some() || report.items.iter().any(|it| it.target.is_none()) {
        Some(plan_for(report_path, &report, plan)?)
    } else {
        None
    };
    let targets = item_targets(&report, plan.as_ref());
    let sources: Vec<PathBuf> = report
        .items
        .iter()
        .filter(|it| targets.get(&it.source).is_some_and(|t| t == target))
        .map(|it| it.source.clone())
        .collect();
    if sources.is_empty() {
        return Err(Error::Plan(crate::t!(
            "undo.no_such_target",
            target = target
        )));
    }
    Ok(sources)
}

fn read_report(report_path: &Path) -> Result<Report> {
    let text = std::fs::read_to_string(report_path)?;
    serde_json::from_str(&text).map_err(|e| Error::Plan(crate::t!("undo.report", error = e)))
}

/// The plan to retry the failed items of a finished run ("Retry
/// failed"): [`subplan`] of the plan, after a partial undo
/// ([`undo_items`]) of those items in the old report removes the outputs
/// they had already placed, so the retry puts every output at its planned
/// name instead of beside a leftover copy. Those items never touched their
/// sources (see [`subplan`]). Items whose partial undo had problems are left
/// out of the retry.
pub fn prepare_retry(plan_path: &Path, report_path: &Path) -> Result<Plan> {
    prepare_retry_excluding(plan_path, report_path, &HashSet::new())
}

/// [`prepare_retry`] leaving out `exclude`: sources an earlier retry of this
/// report already finished, which must neither be undone nor run again. The
/// plan records the report's `run_id` in `retry_of`.
pub fn prepare_retry_excluding(
    plan_path: &Path,
    report_path: &Path,
    exclude: &HashSet<PathBuf>,
) -> Result<Plan> {
    let plan = Plan::load(plan_path)?;
    let report = read_report(report_path)?;
    if report.finished.is_empty() {
        return Err(Error::Plan(crate::t!("undo.not_finished")));
    }
    let mut sub = subplan(&plan, &report);
    sub.retry_of = Some(report.run_id.clone());
    sub.items.retain(|it| !exclude.contains(&it.source));
    if !report.undone {
        let to_undo: Vec<PathBuf> = sub
            .items
            .iter()
            .map(|it| it.source.clone())
            .filter(|s| !report.undone_sources.contains(s))
            .collect();
        if !to_undo.is_empty() {
            undo_items(report_path, &to_undo)?;
        }
    }
    let report = read_report(report_path)?;
    sub.items
        .retain(|it| report.undone || report.undone_sources.contains(&it.source));
    Ok(sub)
}

/// For a run that died (`finished` empty) the journal knows the items the
/// last report write missed, and is never older than it: merge them in.
pub fn merge_journal(report_path: &Path, report: &mut Report) -> Result<()> {
    if report.finished.is_empty() {
        for j in read_journal(&journal_path(report_path), &report.run_id)? {
            match report.items.iter_mut().find(|it| it.source == j.source) {
                Some(it) => *it = j,
                None => report.items.push(j),
            }
        }
    }
    Ok(())
}

fn undo_selected(
    report_path: &Path,
    only: Option<&HashSet<PathBuf>>,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<UndoSummary> {
    let _lock = RunLock::acquire_for_report(report_path)?;
    let mut report = read_report(report_path)?;
    if report.undone {
        return Err(Error::Plan(crate::t!("undo.already_undone")));
    }
    merge_journal(report_path, &mut report)?;
    // Items a previous, partly failed undo already finished stay as they are.
    let finished: HashSet<PathBuf> = report.undone_sources.iter().cloned().collect();
    let mut u = Undo {
        sum: UndoSummary::default(),
        // Their sources (if trashed or renamed) are back in place.
        restored: report
            .items
            .iter()
            .filter(|it| {
                finished.contains(&it.source)
                    && (trashed_of(it).is_some() || renamed_of(it).is_some())
            })
            .map(|it| it.source.clone())
            .collect(),
        renamed: report
            .items
            .iter()
            .filter_map(renamed_of)
            .filter_map(|o| o.actual.clone())
            .collect(),
        window: run_window(&report)?,
        trash: TrashIndex::new(Box::new(system::list)),
        sources: report
            .items
            .iter()
            .map(|it| (it.source.clone(), trashed_of(it).cloned()))
            .collect(),
    };
    // A place may be taken by another item's output (a name reused within the
    // run): that output goes away later in the pass, so retry the waiting
    // items while a pass still lets some of them through.
    let mut pending: Vec<&ItemResult> = report
        .items
        .iter()
        .filter(|it| !finished.contains(&it.source))
        .filter(|it| only.is_none_or(|o| o.contains(&it.source)))
        .collect();
    let mut waiting: Vec<(&ItemResult, String)>;
    let mut now_finished = Vec::new();
    let total = pending.len();
    let mut done = 0;
    'passes: loop {
        let before = pending.len();
        waiting = Vec::new();
        for it in pending {
            // Only between items: an item, once started, is undone (or left)
            // whole on its own path.
            if cancel.is_cancelled() {
                u.sum.cancelled = true;
                break 'passes;
            }
            let problems = u.sum.problems.len();
            match u.item(it) {
                Some(why) => {
                    waiting.push((it, why));
                    continue;
                }
                None if u.sum.problems.len() == problems => now_finished.push(it.source.clone()),
                None => {}
            }
            done += 1;
            progress.step(ProgressEvent {
                stage: Stage::Undo,
                done,
                total,
                current: Some(it.source.clone()),
            });
        }
        if waiting.is_empty() || waiting.len() == before {
            break;
        }
        pending = waiting.iter().map(|(it, _)| *it).collect();
    }
    // Waiting items of a cancelled undo were not given every pass: not a
    // problem yet, a rerun retries them.
    if !u.sum.cancelled {
        u.sum
            .problems
            .extend(waiting.into_iter().map(|(_, why)| why));
    }
    report.undone_sources.extend(now_finished);
    // With problems (or items left by a cancel) the report stays open: a
    // rerun retries the rest.
    report.undone = u.sum.problems.is_empty()
        && !u.sum.cancelled
        && (only.is_none() || {
            let done: HashSet<&PathBuf> = report.undone_sources.iter().collect();
            report.items.iter().all(|it| done.contains(&it.source))
        });
    write_report(report_path, &report)?;
    Ok(u.sum)
}

struct Undo {
    sum: UndoSummary,
    /// Paths sources were restored to in this undo.
    restored: HashSet<PathBuf>,
    window: Window,
    /// The system trash, listed once per undo run on first use.
    trash: TrashIndex<system::Item>,
    /// Every item's source and where it was trashed to.
    sources: Vec<(PathBuf, Option<TrashRef>)>,
    /// Where apply renamed sources to: never removed as outputs.
    renamed: HashSet<PathBuf>,
}

/// From the report's start to its end (now, for an unfinished run).
fn run_window(r: &Report) -> Result<Window> {
    let ts = |s: &str| {
        chrono::DateTime::parse_from_rfc3339(s)
            .map(|t| t.timestamp())
            .map_err(|e| Error::Plan(crate::t!("undo.report_time", value = s, error = e)))
    };
    let to = if r.finished.is_empty() {
        chrono::Local::now().timestamp()
    } else {
        ts(&r.finished)?
    };
    Ok(Window {
        from: ts(&r.started)? - WINDOW_SLACK,
        to: to + WINDOW_SLACK,
    })
}

/// The output that is the item's source renamed in place, if any.
fn renamed_of(it: &ItemResult) -> Option<&OutputResult> {
    it.outputs
        .iter()
        .find(|o| o.renamed_from.is_some() && o.actual.is_some())
}

fn trashed_of(it: &ItemResult) -> Option<&TrashRef> {
    match &it.status {
        ItemStatus::Done { trashed } | ItemStatus::Failed { trashed } => trashed.as_ref(),
        ItemStatus::Stale | ItemStatus::Missing | ItemStatus::DryRun => None,
    }
}

impl Undo {
    fn problem(&mut self, path: &Path, why: impl std::fmt::Display) {
        self.sum.problems.push(format!("{}: {why}", path.display()));
    }

    /// Undo one item. Returns why it has to wait when its original place is
    /// taken by something other than its own output; nothing changed then.
    fn item(&mut self, it: &ItemResult) -> Option<String> {
        let src = &it.source;
        if let Some(o) = renamed_of(it) {
            // Renamed in place, never trashed: the old name comes back before
            // any other output goes.
            match self.rename_back(src, o) {
                RenameBack::Done => {}
                RenameBack::Wait(why) => return Some(why),
                RenameBack::Failed => return None,
            }
        } else if let Some(t) = trashed_of(it) {
            let found = match self.locate(t) {
                Ok(f) => f,
                Err(why) => {
                    self.problem(src, crate::t!("undo.outputs_kept", why = why));
                    return None;
                }
            };
            // An in-place output sits at the source path: move it aside, and
            // back again if the source cannot be restored after all.
            let mut aside = None;
            if std::fs::symlink_metadata(src).is_ok() {
                let own = it
                    .outputs
                    .iter()
                    .find(|o| o.actual.as_deref() == Some(src.as_path()));
                if own.filter(|o| check_output(src, o).is_ok()).is_none() {
                    return Some(crate::t!("undo.dest_taken_trash", path = src.display()));
                }
                match move_aside(src) {
                    Ok(p) => aside = Some(p),
                    Err(e) => {
                        self.problem(src, crate::t!("undo.output_not_removed", error = e));
                        return None;
                    }
                }
            }
            if let Err(e) = self.restore(found, src) {
                let mut why = crate::t!("undo.not_restored", error = e);
                if let Some(a) = &aside
                    && let Err(e) = move_noreplace(a, src)
                {
                    why.push_str(&crate::t!(
                        "undo.output_remains",
                        path = a.display(),
                        error = e
                    ));
                }
                self.problem(src, why);
                return None;
            }
            self.sum.restored += 1;
            self.restored.insert(src.clone());
            if let Some(a) = aside {
                match std::fs::remove_file(&a) {
                    Ok(()) => self.sum.removed_outputs += 1,
                    Err(e) => self.problem(&a, crate::t!("undo.output_not_deleted", error = e)),
                }
            }
        }
        for o in &it.outputs {
            let Some(p) = o.actual.as_deref() else {
                continue;
            };
            // The source itself, under its new name (or back under the old).
            if o.renamed_from.is_some() {
                continue;
            }
            if self.renamed.contains(p) {
                self.problem(p, crate::t!("undo.renamed_source"));
                continue;
            }
            // The source path holds the source again (or still), and a restored
            // place holds a restored source: neither is an output any more.
            if p == src.as_path() || self.restored.contains(p) {
                continue;
            }
            if let Some(why) = self.someones_source(p) {
                self.problem(p, crate::t!("undo.output_kept", why = why));
                continue;
            }
            match check_output(p, o) {
                Ok(()) => match std::fs::remove_file(p) {
                    Ok(()) => self.sum.removed_outputs += 1,
                    Err(e) => self.problem(p, crate::t!("undo.output_not_deleted", error = e)),
                },
                Err(None) => {}
                Err(Some(why)) => self.problem(p, crate::t!("undo.output_kept", why = why)),
            }
        }
        None
    }
}

/// How giving a renamed source its old name back went.
enum RenameBack {
    Done,
    /// The old name is taken; nothing changed.
    Wait(String),
    /// Recorded as a problem; nothing else of the item may go.
    Failed,
}

impl Undo {
    /// Rename the file at `o.actual` (the source, renamed by apply) back to
    /// `src`; never overwrites and never touches a changed file.
    fn rename_back(&mut self, src: &Path, o: &OutputResult) -> RenameBack {
        let (Some(new), Some(old)) = (o.actual.as_deref(), o.renamed_from.as_deref()) else {
            return RenameBack::Done;
        };
        if old != src {
            self.problem(
                src,
                crate::t!("undo.renamed_from_other", path = old.display()),
            );
            return RenameBack::Failed;
        }
        let taken = || {
            RenameBack::Wait(crate::t!(
                "undo.dest_taken_renamed",
                path = src.display(),
                new = new.display()
            ))
        };
        if std::fs::symlink_metadata(src).is_ok() {
            // A rerun after an undo that died: the source already is back.
            if std::fs::symlink_metadata(new).is_err() && check_output(src, o).is_ok() {
                self.restored.insert(src.to_path_buf());
                return RenameBack::Done;
            }
            return taken();
        }
        match check_output(new, o) {
            Ok(()) => {}
            Err(why) => {
                let why = why.unwrap_or_else(|| crate::t!("undo.file_missing"));
                self.problem(new, crate::t!("undo.not_renamed_back_why", why = why));
                return RenameBack::Failed;
            }
        }
        match rename_noreplace(new, src) {
            Ok(()) => {
                self.sum.restored += 1;
                self.restored.insert(src.to_path_buf());
                RenameBack::Done
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => taken(),
            Err(e) => {
                self.problem(new, crate::t!("undo.not_renamed_back", error = e));
                RenameBack::Failed
            }
        }
    }

    /// Why `p` must stay because it is (or may again be) an item's source:
    /// it may go only while that item's source is still in the trash, since
    /// then `p` cannot hold the source itself.
    fn someones_source(&mut self, p: &Path) -> Option<String> {
        let trashed: Vec<Option<TrashRef>> = self
            .sources
            .iter()
            .filter(|(s, _)| s == p)
            .map(|(_, t)| t.clone())
            .collect();
        trashed.into_iter().find_map(|t| match t {
            Some(t) if self.locate(&t).is_ok() => None,
            _ => Some(crate::t!("undo.source_path_not_trashed")),
        })
    }

    fn locate(&mut self, t: &TrashRef) -> std::result::Result<Found, String> {
        match t {
            TrashRef::Folder { path } => match std::fs::symlink_metadata(path) {
                Ok(_) => Ok(Found::Folder(path.clone())),
                Err(e) => Err(crate::t!(
                    "undo.not_in_trash",
                    path = path.display(),
                    error = e
                )),
            },
            TrashRef::System { original } => {
                let (time, item) = self.trash.find(original, self.window)?;
                Ok(Found::System {
                    original: original.clone(),
                    time,
                    item,
                })
            }
        }
    }

    /// Put a found source back at `src`; never overwrites.
    fn restore(&mut self, found: Found, src: &Path) -> std::result::Result<(), String> {
        match found {
            Found::Folder(p) => {
                if let Some(dir) = src.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                }
                move_noreplace(&p, src).map_err(|e| e.to_string())
            }
            Found::System {
                original,
                time,
                item,
            } => {
                system::restore(item)?;
                self.trash.remove(&original, time);
                Ok(())
            }
        }
    }
}

/// `Ok` when `p` is a regular file of the recorded size and blake3 (each if
/// recorded); `Err(None)` when it is gone; otherwise why it must be kept.
fn check_output(p: &Path, o: &OutputResult) -> std::result::Result<(), Option<String>> {
    let changed = || Some(crate::t!("undo.changed_after_apply"));
    match std::fs::symlink_metadata(p) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Err(None),
        Err(e) => return Err(Some(e.to_string())),
        Ok(m) if !m.is_file() => return Err(Some(crate::t!("undo.not_a_file"))),
        Ok(m) if o.size.is_some_and(|s| s != m.len()) => return Err(changed()),
        Ok(_) => {}
    }
    match &o.hash {
        Some(h) => match file_hash(p) {
            Ok(got) if &got == h => Ok(()),
            Ok(_) => Err(changed()),
            Err(e) => Err(Some(e.to_string())),
        },
        None => Ok(()),
    }
}

/// Unix seconds in which this run could have trashed its sources.
#[derive(Debug, Clone, Copy)]
struct Window {
    from: i64,
    to: i64,
}

/// The latest candidate trashed from `original` within `w`.
fn pick<T>(
    cands: impl IntoIterator<Item = T>,
    original: &Path,
    w: Window,
    path: impl Fn(&T) -> PathBuf,
    time: impl Fn(&T) -> i64,
) -> Option<T> {
    cands
        .into_iter()
        .filter(|c| path(c) == original && (w.from..=w.to).contains(&time(c)))
        .max_by_key(|c| time(c))
}

/// A trashed source that is there to be restored.
enum Found {
    Folder(PathBuf),
    System {
        original: PathBuf,
        time: i64,
        item: system::Item,
    },
}

/// Trash entries as (original path, deletion time, entry).
type Listing<T> = Vec<(PathBuf, i64, T)>;
/// Trash entries by original path, as (deletion time, entry).
type ByPath<T> = HashMap<PathBuf, Vec<(i64, T)>>;

/// The system trash listing, read once on first use and indexed by original
/// path: listing it is slow, and an undo looks up every item.
struct TrashIndex<T> {
    list: Box<dyn FnMut() -> std::result::Result<Listing<T>, String>>,
    by_path: Option<std::result::Result<ByPath<T>, String>>,
}

impl<T: Clone> TrashIndex<T> {
    fn new(list: Box<dyn FnMut() -> std::result::Result<Listing<T>, String>>) -> Self {
        TrashIndex {
            list,
            by_path: None,
        }
    }

    /// The latest deletion from `original` within `w`, with its time.
    fn find(&mut self, original: &Path, w: Window) -> std::result::Result<(i64, T), String> {
        let list = &mut self.list;
        let index = self.by_path.get_or_insert_with(|| {
            let mut m: ByPath<T> = HashMap::new();
            for (path, time, item) in list()? {
                m.entry(path).or_default().push((time, item));
            }
            Ok(m)
        });
        let index = index
            .as_ref()
            .map_err(|e| crate::t!("undo.trash_unavailable", error = e))?;
        let cands = index.get(original).map(Vec::as_slice).unwrap_or_default();
        pick(cands, original, w, |_| original.to_path_buf(), |c| c.0)
            .cloned()
            .ok_or_else(|| crate::t!("undo.trash_deleted_during_run"))
    }

    /// Forget an entry that was restored.
    fn remove(&mut self, original: &Path, time: i64) {
        if let Some(Ok(index)) = &mut self.by_path
            && let Some(v) = index.get_mut(original)
            && let Some(i) = v.iter().position(|c| c.0 == time)
        {
            v.remove(i);
        }
    }
}

/// Move `p` to a free `.gallery-undo-<n>-<name>` sibling; return where.
fn move_aside(p: &Path) -> std::io::Result<PathBuf> {
    let dir = p.parent().unwrap_or(Path::new("."));
    let name = p
        .file_name()
        .ok_or_else(|| std::io::Error::other("no file name"))?
        .to_string_lossy();
    for n in 0.. {
        let to = dir.join(format!("{ASIDE_PREFIX}{n}-{name}"));
        match move_noreplace(p, &to) {
            Ok(()) => return Ok(to),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("an unbounded range")
}

/// The system trash, where the `trash` crate can list and restore it.
#[cfg(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
))]
mod system {
    use super::Listing;

    pub type Item = trash::TrashItem;

    /// Every entry of the system trash.
    pub fn list() -> Result<Listing<Item>, String> {
        let items = trash::os_limited::list().map_err(|e| e.to_string())?;
        Ok(items
            .into_iter()
            .map(|i| (i.original_path(), i.time_deleted, i))
            .collect())
    }

    /// Fails with a collision instead of overwriting.
    pub fn restore(item: Item) -> Result<(), String> {
        trash::os_limited::restore_all([item]).map_err(|e| e.to_string())
    }
}

/// No listing API here (macOS): the user restores by hand.
#[cfg(not(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
)))]
mod system {
    use super::Listing;

    #[derive(Clone)]
    pub enum Item {}

    pub fn list() -> Result<Listing<Item>, String> {
        Err(crate::t!("undo.no_trash_listing"))
    }

    pub fn restore(item: Item) -> Result<(), String> {
        match item {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::{ItemResult, ItemStatus, OutputResult, Report, write_report};
    use crate::trash::TrashRef;
    use std::path::PathBuf;

    fn output(actual: &Path) -> OutputResult {
        OutputResult {
            planned: actual.to_path_buf(),
            actual: Some(actual.to_path_buf()),
            error: None,
            warnings: vec![],
            fell_back: false,
            size: Some(std::fs::metadata(actual).unwrap().len()),
            hash: Some(crate::apply::file_hash(actual).unwrap()),
            renamed_from: None,
            anomalies: Vec::new(),
        }
    }

    fn save(dir: &Path, items: Vec<ItemResult>) -> PathBuf {
        let path = dir.join("plan.report.json");
        let r = Report {
            run_id: "r".into(),
            plan: dir.join("plan.json"),
            started: "2026-11-04T12:00:00+00:00".into(),
            finished: "2026-11-04T12:10:00+00:00".into(),
            items,
            tool_versions: Default::default(),
            tool_warnings: vec![],
            undone: false,
            undone_sources: vec![],
            cancelled: false,
            stopped_by_anomalies: false,
            kept: 0,
        };
        write_report(&path, &r).unwrap();
        path
    }

    fn put(p: &Path, data: &[u8]) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn requested_files_match_the_report_by_file_identity() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("real");
        put(&real.join("a.jpg"), b"a");
        // The report spells the folder through a symlink (as /var is on
        // macOS); the user names the same file by its real path.
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let gone = link.join("gone.jpg");
        let item = |source: PathBuf| ItemResult {
            source,
            status: ItemStatus::Missing,
            outputs: vec![],
            error: None,
            elapsed_ms: None,
            target: None,
        };
        let rp = save(d.path(), vec![item(link.join("a.jpg")), item(gone.clone())]);
        let by_real = real.join("a.jpg");
        assert_eq!(
            resolve_in_report(&rp, std::slice::from_ref(&by_real)).unwrap(),
            vec![link.join("a.jpg")]
        );
        // A file that is gone still matches by its exact spelling, and by its
        // real folder when the report spells it through the symlink.
        assert_eq!(
            resolve_in_report(&rp, std::slice::from_ref(&gone)).unwrap(),
            vec![gone.clone()]
        );
        assert_eq!(
            resolve_in_report(&rp, &[real.join("gone.jpg")]).unwrap(),
            vec![gone]
        );
        // Another file is no item of the run.
        assert!(resolve_in_report(&rp, &[real.join("b.jpg")]).is_err());
    }

    #[test]
    fn failed_in_place_restores_source_and_removes_kept_output() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src/a.jpg");
        let trashed = d.path().join("src/.gallery-trash/r/a.jpg");
        let kept = d.path().join("src/a_kept.jpg");
        put(&trashed, b"original");
        put(&kept, b"processed");
        let rp = save(
            d.path(),
            vec![ItemResult {
                source: src.clone(),
                status: ItemStatus::Failed {
                    trashed: Some(TrashRef::Folder {
                        path: trashed.clone(),
                    }),
                },
                outputs: vec![OutputResult {
                    error: Some("not placed".into()),
                    ..output(&kept)
                }],
                error: None,
                elapsed_ms: None,
                target: None,
            }],
        );
        let s = undo(&rp).unwrap();
        assert_eq!(s.problems, Vec::<String>::new());
        assert_eq!((s.restored, s.removed_outputs), (1, 1));
        assert_eq!(std::fs::read(&src).unwrap(), b"original");
        assert!(!kept.exists() && !trashed.exists());
    }

    #[test]
    fn undo_with_problems_can_be_retried() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("src/a.jpg"), d.path().join("src/b.jpg"));
        let (ta, tb) = (
            d.path().join("src/.gallery-trash/r/a.jpg"),
            d.path().join("src/.gallery-trash/r/b.jpg"),
        );
        let (oa, ob) = (d.path().join("out/a.jpg"), d.path().join("out/b.jpg"));
        put(&ta, b"a");
        put(&tb, b"b");
        put(&oa, b"a out");
        put(&ob, b"b out");
        // Someone else's file sits where a.jpg was.
        put(&a, b"intruder");
        let done = |source: &Path, trashed: &Path, out: &Path| ItemResult {
            source: source.to_path_buf(),
            status: ItemStatus::Done {
                trashed: Some(TrashRef::Folder {
                    path: trashed.to_path_buf(),
                }),
            },
            outputs: vec![output(out)],
            error: None,
            elapsed_ms: None,
            target: None,
        };
        let rp = save(d.path(), vec![done(&a, &ta, &oa), done(&b, &tb, &ob)]);
        let read =
            || -> Report { serde_json::from_str(&std::fs::read_to_string(&rp).unwrap()).unwrap() };

        let s = undo(&rp).unwrap();
        assert_eq!(s.problems.len(), 1, "{:?}", s.problems);
        assert_eq!((s.restored, s.removed_outputs), (1, 1));
        assert!(!read().undone, "a report with problems stays retryable");
        assert_eq!(std::fs::read(&a).unwrap(), b"intruder");
        assert!(ta.exists() && oa.exists());

        std::fs::remove_file(&a).unwrap();
        let s = undo(&rp).unwrap();
        // b.jpg, restored the first time, is neither a problem nor touched.
        assert_eq!(s.problems, Vec::<String>::new());
        assert_eq!((s.restored, s.removed_outputs), (1, 1));
        assert!(read().undone);
        assert_eq!(std::fs::read(&a).unwrap(), b"a");
        assert_eq!(std::fs::read(&b).unwrap(), b"b");
        assert!(!oa.exists() && !ob.exists());
        assert!(matches!(undo(&rp), Err(Error::Plan(_))));
    }

    #[test]
    fn unfinished_report_is_completed_from_the_journal() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("src/a.jpg"), d.path().join("src/b.jpg"));
        let (ta, tb) = (
            d.path().join("src/.gallery-trash/r/a.jpg"),
            d.path().join("src/.gallery-trash/r/b.jpg"),
        );
        let (oa, ob) = (d.path().join("out/a.jpg"), d.path().join("out/b.jpg"));
        put(&ta, b"a");
        put(&tb, b"b");
        put(&oa, b"a out");
        put(&ob, b"b out");
        let done = |source: &Path, trashed: &Path, out: &Path| ItemResult {
            source: source.to_path_buf(),
            status: ItemStatus::Done {
                trashed: Some(TrashRef::Folder {
                    path: trashed.to_path_buf(),
                }),
            },
            outputs: vec![output(out)],
            error: None,
            elapsed_ms: None,
            target: None,
        };
        // The last snapshot has a.jpg still in progress (no outputs yet) and
        // misses b.jpg; the run died before finishing.
        let rp = d.path().join("plan.report.json");
        let mut early = done(&a, &ta, &oa);
        early.outputs.clear();
        let report = Report {
            run_id: "r".into(),
            plan: d.path().join("plan.json"),
            started: chrono::Local::now().to_rfc3339(),
            finished: String::new(),
            items: vec![early],
            tool_versions: Default::default(),
            tool_warnings: vec![],
            undone: false,
            undone_sources: vec![],
            cancelled: false,
            stopped_by_anomalies: false,
            kept: 0,
        };
        write_report(&rp, &report).unwrap();
        let line = |run: &str, it: &ItemResult| {
            serde_json::json!({"run_id": run, "item": it}).to_string() + "\n"
        };
        let mut j = line("r", &done(&a, &ta, &oa));
        j += &line("r", &done(&b, &tb, &ob));
        // Another run's line and a torn last line are ignored.
        j += &line("other", &done(&d.path().join("src/x.jpg"), &ta, &oa));
        j += "{\"run_id\": \"r\", \"ite";
        std::fs::write(crate::apply::journal_path(&rp), j).unwrap();

        let s = undo(&rp).unwrap();
        assert_eq!(s.problems, Vec::<String>::new());
        assert_eq!((s.restored, s.removed_outputs), (2, 2));
        assert_eq!(std::fs::read(&a).unwrap(), b"a");
        assert_eq!(std::fs::read(&b).unwrap(), b"b");
        assert!(!oa.exists() && !ob.exists());
        let after: Report = serde_json::from_str(&std::fs::read_to_string(&rp).unwrap()).unwrap();
        assert_eq!(after.items.len(), 2);
        assert!(after.undone);
    }

    #[test]
    fn failed_without_trash_removes_placed_outputs_only() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src/a.jpg");
        let placed = d.path().join("out/a.jpg");
        put(&src, b"original");
        put(&placed, b"copy");
        let rp = save(
            d.path(),
            vec![ItemResult {
                source: src.clone(),
                status: ItemStatus::Failed { trashed: None },
                outputs: vec![
                    output(&placed),
                    OutputResult {
                        actual: None,
                        size: None,
                        error: Some("failed".into()),
                        ..output(&placed)
                    },
                ],
                error: None,
                elapsed_ms: None,
                target: None,
            }],
        );
        let s = undo(&rp).unwrap();
        assert_eq!(s.problems, Vec::<String>::new());
        assert_eq!((s.restored, s.removed_outputs), (0, 1));
        assert_eq!(std::fs::read(&src).unwrap(), b"original");
        assert!(!placed.exists());
    }

    #[test]
    fn place_taken_by_another_items_output_is_freed_first() {
        // Item 1's output took the name item 2's source had before the trash.
        let d = tempfile::tempdir().unwrap();
        let src1 = d.path().join("src/IMG_1.jpg");
        let src2 = d.path().join("src/2026.jpg");
        let t1 = d.path().join("src/.gallery-trash/r/IMG_1.jpg");
        let t2 = d.path().join("src/.gallery-trash/r/2026.jpg");
        put(&t1, b"one");
        put(&t2, b"two");
        put(&src2, b"one renamed");
        let out2 = d.path().join("src/2026-renamed.jpg");
        put(&out2, b"two renamed");
        let done = |source: &Path, trashed: &Path, out: &Path| ItemResult {
            source: source.to_path_buf(),
            status: ItemStatus::Done {
                trashed: Some(TrashRef::Folder {
                    path: trashed.to_path_buf(),
                }),
            },
            outputs: vec![output(out)],
            error: None,
            elapsed_ms: None,
            target: None,
        };
        // Item 2 comes first, so its place is still taken on the first pass.
        let rp = save(
            d.path(),
            vec![done(&src2, &t2, &out2), done(&src1, &t1, &src2)],
        );
        let s = undo(&rp).unwrap();
        assert_eq!(s.problems, Vec::<String>::new());
        assert_eq!((s.restored, s.removed_outputs), (2, 2));
        assert_eq!(std::fs::read(&src1).unwrap(), b"one");
        assert_eq!(std::fs::read(&src2).unwrap(), b"two");
        assert!(!out2.exists());
    }

    #[test]
    fn restored_source_is_never_removed_as_another_items_output() {
        // Item A's output at `x` was deleted by the user; item B's source,
        // trashed from `x`, comes back there first, with the same size.
        let d = tempfile::tempdir().unwrap();
        let x = d.path().join("src/x.jpg");
        let src_a = d.path().join("src/a.jpg");
        let ta = d.path().join("src/.gallery-trash/r/a.jpg");
        let tb = d.path().join("src/.gallery-trash/r/x.jpg");
        put(&ta, b"aaaa");
        put(&tb, b"bbbb");
        let item = |source: &Path, trashed: &Path, out: &Path| ItemResult {
            source: source.to_path_buf(),
            status: ItemStatus::Done {
                trashed: Some(TrashRef::Folder {
                    path: trashed.to_path_buf(),
                }),
            },
            outputs: vec![OutputResult {
                planned: out.to_path_buf(),
                actual: Some(out.to_path_buf()),
                error: None,
                warnings: vec![],
                fell_back: false,
                size: Some(4),
                hash: None,
                renamed_from: None,
                anomalies: Vec::new(),
            }],
            error: None,
            elapsed_ms: None,
            target: None,
        };
        let rp = save(
            d.path(),
            vec![
                item(&x, &tb, &d.path().join("out/b.jpg")),
                item(&src_a, &ta, &x),
            ],
        );
        let s = undo(&rp).unwrap();
        assert_eq!(s.restored, 2);
        assert_eq!(std::fs::read(&x).unwrap(), b"bbbb");
        assert_eq!(std::fs::read(&src_a).unwrap(), b"aaaa");
    }

    #[test]
    fn rerun_never_removes_a_restored_source() {
        // A first undo restored B to `x` and crashed before marking the report.
        // A's output was at `x` once; its record has the same size and, as in
        // an old report, no hash.
        let d = tempfile::tempdir().unwrap();
        let x = d.path().join("src/x.jpg");
        let src_a = d.path().join("src/a.jpg");
        let ta = d.path().join("src/.gallery-trash/r/a.jpg");
        let tb = d.path().join("src/.gallery-trash/r/x.jpg");
        put(&ta, b"aaaa");
        put(&x, b"bbbb");
        let item = |source: &Path, trashed: &Path, out: &Path, hash: Option<String>| ItemResult {
            source: source.to_path_buf(),
            status: ItemStatus::Done {
                trashed: Some(TrashRef::Folder {
                    path: trashed.to_path_buf(),
                }),
            },
            outputs: vec![OutputResult {
                planned: out.to_path_buf(),
                actual: Some(out.to_path_buf()),
                error: None,
                warnings: vec![],
                fell_back: false,
                size: Some(4),
                hash,
                renamed_from: None,
                anomalies: Vec::new(),
            }],
            error: None,
            elapsed_ms: None,
            target: None,
        };
        let rp = save(
            d.path(),
            vec![
                item(&x, &tb, &d.path().join("out/b.jpg"), None),
                item(&src_a, &ta, &x, None),
            ],
        );
        let s = undo(&rp).unwrap();
        assert_eq!(std::fs::read(&x).unwrap(), b"bbbb");
        assert_eq!(std::fs::read(&src_a).unwrap(), b"aaaa");
        assert_eq!(s.restored, 1);
        assert_eq!(s.removed_outputs, 0);
        // B is not in the trash any more; A's output at `x` is refused.
        assert_eq!(s.problems.len(), 2, "{:?}", s.problems);
    }

    #[test]
    fn output_with_other_content_of_same_size_is_kept() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src/a.jpg");
        let out = d.path().join("out/a.jpg");
        put(&src, b"original");
        put(&out, b"copy");
        let mut o = output(&out);
        std::fs::write(&out, b"edit").unwrap();
        o.size = Some(4);
        let rp = save(
            d.path(),
            vec![ItemResult {
                source: src.clone(),
                status: ItemStatus::Failed { trashed: None },
                outputs: vec![o],
                error: None,
                elapsed_ms: None,
                target: None,
            }],
        );
        let s = undo(&rp).unwrap();
        assert_eq!(s.removed_outputs, 0);
        assert_eq!(s.problems.len(), 1, "{:?}", s.problems);
        assert_eq!(std::fs::read(&out).unwrap(), b"edit");
    }

    /// A source renamed in place from `old` to `new` (which holds it now).
    fn renamed(old: &Path, new: &Path) -> OutputResult {
        OutputResult {
            renamed_from: Some(old.to_path_buf()),
            anomalies: Vec::new(),
            ..output(new)
        }
    }

    fn renamed_item(old: &Path, outputs: Vec<OutputResult>) -> ItemResult {
        ItemResult {
            source: old.to_path_buf(),
            status: ItemStatus::Done { trashed: None },
            outputs,
            error: None,
            elapsed_ms: None,
            target: None,
        }
    }

    #[test]
    fn renamed_source_gets_its_old_name_back() {
        let d = tempfile::tempdir().unwrap();
        let old = d.path().join("shots/Screenshot_1.png");
        let new = d.path().join("shots/20260101_120000.png");
        put(&new, b"original");
        let rp = save(
            d.path(),
            vec![renamed_item(&old, vec![renamed(&old, &new)])],
        );
        let s = undo(&rp).unwrap();
        assert_eq!(s.problems, Vec::<String>::new());
        assert_eq!((s.restored, s.removed_outputs), (1, 0));
        assert_eq!(std::fs::read(&old).unwrap(), b"original");
        assert!(!new.exists());
    }

    #[test]
    fn renamed_source_comes_back_before_its_backup_goes() {
        let d = tempfile::tempdir().unwrap();
        let old = d.path().join("shots/Screenshot_1.png");
        let new = d.path().join("shots/20260101_120000.png");
        let backup = d.path().join("backup/Screenshot_1.png");
        put(&new, b"original");
        put(&backup, b"original");
        // As the journal records a run that died right after the rename.
        let mut it = renamed_item(&old, vec![renamed(&old, &new), output(&backup)]);
        it.status = ItemStatus::Failed { trashed: None };
        let rp = save(d.path(), vec![it]);
        let s = undo(&rp).unwrap();
        assert_eq!(s.problems, Vec::<String>::new());
        assert_eq!((s.restored, s.removed_outputs), (1, 1));
        assert_eq!(std::fs::read(&old).unwrap(), b"original");
        assert!(!new.exists() && !backup.exists());
    }

    #[test]
    fn occupied_old_name_keeps_the_renamed_file_and_its_backup() {
        let d = tempfile::tempdir().unwrap();
        let old = d.path().join("shots/Screenshot_1.png");
        let new = d.path().join("shots/20260101_120000.png");
        let backup = d.path().join("backup/Screenshot_1.png");
        put(&new, b"original");
        put(&backup, b"original");
        put(&old, b"intruder");
        let rp = save(
            d.path(),
            vec![renamed_item(
                &old,
                vec![renamed(&old, &new), output(&backup)],
            )],
        );
        let s = undo(&rp).unwrap();
        assert_eq!(s.problems.len(), 1, "{:?}", s.problems);
        assert_eq!((s.restored, s.removed_outputs), (0, 0));
        assert_eq!(std::fs::read(&old).unwrap(), b"intruder");
        assert_eq!(std::fs::read(&new).unwrap(), b"original");
        assert_eq!(std::fs::read(&backup).unwrap(), b"original");
    }

    #[test]
    fn source_moved_into_a_subfolder_comes_back() {
        let d = tempfile::tempdir().unwrap();
        let old = d.path().join("shots/Screenshot_1.png");
        let new = d.path().join("shots/2026/01/20260101_120000.png");
        put(&new, b"original");
        let rp = save(
            d.path(),
            vec![renamed_item(&old, vec![renamed(&old, &new)])],
        );
        let s = undo(&rp).unwrap();
        assert_eq!(s.problems, Vec::<String>::new());
        assert_eq!((s.restored, s.removed_outputs), (1, 0));
        assert_eq!(std::fs::read(&old).unwrap(), b"original");
        assert!(!new.exists());
        // The made folders are left in place.
        assert!(d.path().join("shots/2026/01").is_dir());
    }

    #[test]
    fn subfolder_move_with_occupied_old_path_removes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let old = d.path().join("shots/Screenshot_1.png");
        let new = d.path().join("shots/2026/01/20260101_120000.png");
        let backup = d.path().join("backup/Screenshot_1.png");
        put(&new, b"original");
        put(&backup, b"original");
        put(&old, b"intruder");
        let rp = save(
            d.path(),
            vec![renamed_item(
                &old,
                vec![renamed(&old, &new), output(&backup)],
            )],
        );
        let s = undo(&rp).unwrap();
        assert_eq!(s.problems.len(), 1, "{:?}", s.problems);
        assert_eq!((s.restored, s.removed_outputs), (0, 0));
        assert_eq!(std::fs::read(&old).unwrap(), b"intruder");
        assert_eq!(std::fs::read(&new).unwrap(), b"original");
        assert_eq!(std::fs::read(&backup).unwrap(), b"original");
    }

    #[test]
    fn changed_renamed_file_is_not_renamed_back() {
        let d = tempfile::tempdir().unwrap();
        let old = d.path().join("shots/Screenshot_1.png");
        let new = d.path().join("shots/20260101_120000.png");
        let backup = d.path().join("backup/Screenshot_1.png");
        put(&new, b"original");
        put(&backup, b"original");
        let o = renamed(&old, &new);
        std::fs::write(&new, b"editedXX").unwrap();
        let rp = save(d.path(), vec![renamed_item(&old, vec![o, output(&backup)])]);
        let s = undo(&rp).unwrap();
        assert_eq!(s.problems.len(), 1, "{:?}", s.problems);
        assert_eq!((s.restored, s.removed_outputs), (0, 0));
        assert!(!old.exists());
        assert_eq!(std::fs::read(&new).unwrap(), b"editedXX");
        assert_eq!(std::fs::read(&backup).unwrap(), b"original");
    }

    #[test]
    fn system_trash_is_listed_once_per_run() {
        use std::cell::Cell;
        use std::rc::Rc;
        let calls = Rc::new(Cell::new(0));
        let c = calls.clone();
        let mut idx = TrashIndex::new(Box::new(move || {
            c.set(c.get() + 1);
            Ok(vec![
                (PathBuf::from("/p/a.jpg"), 1050, 'a'),
                (PathBuf::from("/p/a.jpg"), 5000, 'z'),
                (PathBuf::from("/p/b.jpg"), 1060, 'b'),
            ])
        }));
        let w = Window {
            from: 1000,
            to: 1100,
        };
        assert_eq!(idx.find(Path::new("/p/a.jpg"), w), Ok((1050, 'a')));
        assert_eq!(idx.find(Path::new("/p/b.jpg"), w), Ok((1060, 'b')));
        assert!(idx.find(Path::new("/p/c.jpg"), w).is_err());
        // Restored: no longer in the trash.
        idx.remove(Path::new("/p/a.jpg"), 1050);
        assert!(idx.find(Path::new("/p/a.jpg"), w).is_err());
        assert_eq!(calls.get(), 1);

        // A failed listing is not retried for every item either.
        let calls = Rc::new(Cell::new(0));
        let c = calls.clone();
        let mut idx: TrashIndex<char> = TrashIndex::new(Box::new(move || {
            c.set(c.get() + 1);
            Err("no trash".into())
        }));
        let e = idx.find(Path::new("/p/a.jpg"), w).unwrap_err();
        assert!(e.contains("no trash"), "{e}");
        assert!(idx.find(Path::new("/p/b.jpg"), w).is_err());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn system_trash_match_is_limited_to_the_run_window() {
        // 1000 s and 1100 s after the epoch, in another zone.
        let w = run_window(&Report {
            run_id: "r".into(),
            plan: "p".into(),
            started: "1970-01-01T03:16:40+03:00".into(),
            finished: "1970-01-01T00:18:20Z".into(),
            items: vec![],
            tool_versions: Default::default(),
            tool_warnings: vec![],
            undone: false,
            undone_sources: vec![],
            cancelled: false,
            stopped_by_anomalies: false,
            kept: 0,
        })
        .unwrap();
        let orig = Path::new("/p/a.jpg");
        let c = |p: &str, t: i64| (PathBuf::from(p), t);
        let pick_ = |cands: Vec<(PathBuf, i64)>| {
            pick(cands, orig, w, |c| c.0.clone(), |c| c.1).map(|c| c.1)
        };
        // A later deletion of another file from the same path is not ours.
        assert_eq!(
            pick_(vec![c("/p/a.jpg", 1050), c("/p/a.jpg", 5000)]),
            Some(1050)
        );
        // Within the tolerance on both ends.
        assert_eq!(pick_(vec![c("/p/a.jpg", 998)]), Some(998));
        assert_eq!(pick_(vec![c("/p/a.jpg", 1102)]), Some(1102));
        assert_eq!(pick_(vec![c("/p/a.jpg", 997), c("/p/a.jpg", 1103)]), None);
        assert_eq!(pick_(vec![c("/p/b.jpg", 1050)]), None);
        assert_eq!(pick_(vec![]), None);
    }
}

#[cfg(test)]
mod cancel_tests {
    use super::*;
    use crate::apply::write_report;
    use crate::progress::testing::Recorder;
    use crate::progress::{Cancel, NoProgress, ProgressEvent, Stage};

    fn put(p: &Path, data: &[u8]) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }

    /// `n` items: `src/f<i>.jpg` trashed to the folder trash, copied to
    /// `out/f<i>.jpg`. Returns the report path and the (source, output) pairs.
    fn trashed_run(d: &Path, n: usize) -> (PathBuf, Vec<(PathBuf, PathBuf)>) {
        let mut items = Vec::new();
        let mut pairs = Vec::new();
        for i in 0..n {
            let src = d.join(format!("src/f{i}.jpg"));
            let trashed = d.join(format!("src/.gallery-trash/r/f{i}.jpg"));
            let out = d.join(format!("out/f{i}.jpg"));
            put(&trashed, format!("body {i}").as_bytes());
            put(&out, format!("out {i}").as_bytes());
            items.push(ItemResult {
                source: src.clone(),
                status: ItemStatus::Done {
                    trashed: Some(TrashRef::Folder { path: trashed }),
                },
                outputs: vec![OutputResult {
                    planned: out.clone(),
                    actual: Some(out.clone()),
                    error: None,
                    warnings: vec![],
                    fell_back: false,
                    size: Some(std::fs::metadata(&out).unwrap().len()),
                    hash: Some(crate::apply::file_hash(&out).unwrap()),
                    renamed_from: None,
                    anomalies: Vec::new(),
                }],
                error: None,
                elapsed_ms: None,
                target: None,
            });
            pairs.push((src, out));
        }
        let path = d.join("plan.report.json");
        let r = Report {
            run_id: "r".into(),
            plan: d.join("plan.json"),
            started: "2026-11-04T12:00:00+00:00".into(),
            finished: "2026-11-04T12:10:00+00:00".into(),
            items,
            tool_versions: Default::default(),
            tool_warnings: vec![],
            undone: false,
            undone_sources: vec![],
            cancelled: false,
            stopped_by_anomalies: false,
            kept: 0,
        };
        write_report(&path, &r).unwrap();
        (path, pairs)
    }

    fn load(p: &Path) -> Report {
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn undo_reports_one_event_per_item() {
        let d = tempfile::tempdir().unwrap();
        let (rp, pairs) = trashed_run(d.path(), 3);
        let rec = Recorder::default();
        let s = undo_with(&rp, &rec, &Cancel::new()).unwrap();
        assert!(!s.cancelled);
        assert_eq!((s.restored, s.removed_outputs), (3, 3));
        let evs = rec.events(Stage::Undo);
        let dones: Vec<usize> = evs.iter().map(|e| e.done).collect();
        assert_eq!(dones, [1, 2, 3]);
        assert!(evs.iter().all(|e| e.total == 3));
        let cur: Vec<PathBuf> = evs.iter().filter_map(|e| e.current.clone()).collect();
        let srcs: Vec<PathBuf> = pairs.into_iter().map(|(s, _)| s).collect();
        assert_eq!(cur, srcs);
    }

    #[test]
    fn cancel_before_start_restores_nothing_and_a_rerun_finishes() {
        let d = tempfile::tempdir().unwrap();
        let (rp, pairs) = trashed_run(d.path(), 3);
        let cancel = Cancel::new();
        cancel.cancel();
        let s = undo_with(&rp, &NoProgress, &cancel).unwrap();
        assert!(s.cancelled);
        assert_eq!((s.restored, s.removed_outputs), (0, 0));
        assert_eq!(s.problems, Vec::<String>::new());
        for (src, out) in &pairs {
            assert!(!src.exists() && out.exists());
        }
        let r = load(&rp);
        assert!(!r.undone && r.undone_sources.is_empty());
        assert!(crate::apply::RunLock::acquire_for_report(&rp).is_ok());

        let s = undo(&rp).unwrap();
        assert_eq!((s.restored, s.removed_outputs), (3, 3));
        assert!(load(&rp).undone);
    }

    #[test]
    fn cancel_mid_undo_stops_between_items_and_a_rerun_finishes() {
        let d = tempfile::tempdir().unwrap();
        let (rp, pairs) = trashed_run(d.path(), 4);
        let cancel = Cancel::new();
        let c = cancel.clone();
        let progress = move |ev: ProgressEvent| {
            if ev.done >= 2 {
                c.cancel();
            }
        };
        let s = undo_with(&rp, &progress, &cancel).unwrap();
        assert!(s.cancelled);
        assert_eq!(s.problems, Vec::<String>::new());
        assert_eq!((s.restored, s.removed_outputs), (2, 2));
        for (i, (src, out)) in pairs.iter().enumerate() {
            if i < 2 {
                assert_eq!(std::fs::read_to_string(src).unwrap(), format!("body {i}"));
                assert!(!out.exists());
            } else {
                assert!(!src.exists() && out.exists(), "{src:?} untouched");
            }
        }
        let r = load(&rp);
        assert!(!r.undone, "a cancelled undo leaves the report open");
        assert_eq!(r.undone_sources, [pairs[0].0.clone(), pairs[1].0.clone()]);

        let s = undo(&rp).unwrap();
        assert_eq!(s.problems, Vec::<String>::new());
        assert_eq!((s.restored, s.removed_outputs), (2, 2));
        for (i, (src, out)) in pairs.iter().enumerate() {
            assert_eq!(std::fs::read_to_string(src).unwrap(), format!("body {i}"));
            assert!(!out.exists());
        }
        assert!(load(&rp).undone);
    }
}
