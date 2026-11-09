//! Plan execution: run every item's content pipes, place the outputs without
//! ever overwriting, then move the source to the trash, and report.
//!
//! Safety rules:
//! - the source is only read until every output of its item is in place;
//! - the source goes to the trash only after that, and only if it is still
//!   the file the plan saw;
//! - outputs are staged as `.gallery-tmp-<run_id>-*` in their destination
//!   directory and placed with a no-overwrite link/rename;
//! - a source consumed by a Rename or Keep output never goes to the trash: it
//!   is renamed in its folder or moved into a folder inside its source root
//!   (no-overwrite, never copied) only after every written output is in place
//!   and only if it is still the file the plan saw;
//! - a processed output that comes out as the source's very bytes (the pipe
//!   could not make the file smaller) never replaces the source: at the
//!   source's own path the source just stays, under another name in its
//!   folder the source is renamed; either way it is not trashed, and the
//!   file is remembered (see [`crate::incompressible`]) so that planning
//!   keeps it in place next time;
//! - a `trash` target's file has no outputs: it goes to the trash directly,
//!   after every placement of the run, and only if it is still the file the
//!   plan saw; a `keep` target's file is not in the items at all.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use chrono::Local;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{AudioCodec, ContentPreset, TargetKind, expand_tilde};
use crate::content::{StepCtx, run_pipe};
use crate::datetime::capture_date;
use crate::media::{kind_of, split_name};
use crate::name::reserve;
use crate::plan::{OutputOp, Plan, PlanItem, PlannedOutput, inside_root, same_dir};
use crate::progress::{Cancel, NoProgress, Progress, ProgressEvent, Stage};
use crate::route::Via;
use crate::scan::mtime_ns;
use crate::syncthing::SyncPause;
use crate::tools::{Capabilities, Tools};
use crate::trash::{TrashMode, TrashRef, move_to_folder, move_to_trash};
use crate::verify::{self, Expected};
use crate::{Error, Result, exif};

/// Prefix of staged outputs and per-output work directories.
const TMP_PREFIX: &str = ".gallery-tmp-";
/// The report on disk is refreshed after this many finished items.
const REPORT_EVERY: usize = 50;
/// Item error of a target without outputs.
fn no_outputs() -> String {
    crate::t!("apply.no_outputs")
}

#[derive(Debug, Clone)]
pub struct ApplyOptions {
    /// Only check (capabilities, missing and stale sources); write nothing.
    pub dry_run: bool,
    /// Items processed in parallel.
    pub jobs: usize,
    pub trash: TrashMode,
    /// The run stops like a cancel once this many items failed verification
    /// (see [`crate::verify`]); 0 never stops it.
    pub max_anomalies: usize,
    /// Place outputs that differ from their source in what the pipe should
    /// keep (recorded, not failed). A deliberate override.
    pub allow_anomalies: bool,
    /// Pause the local Syncthing folders the plan touches before a real run
    /// and leave them paused (see [`crate::syncthing`]). A failure to pause
    /// is a warning in the report, never a reason not to run.
    pub pause_sync: Option<SyncPause>,
}

/// Default of [`ApplyOptions::max_anomalies`].
pub const DEFAULT_MAX_ANOMALIES: usize = 3;

impl Default for ApplyOptions {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        ApplyOptions {
            dry_run: false,
            jobs: (cpus / 2).max(1),
            trash: TrashMode::default(),
            max_anomalies: DEFAULT_MAX_ANOMALIES,
            allow_anomalies: false,
            pause_sync: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// `YYYYmmdd-HHMMSS`; also names the trash folder of this run.
    pub run_id: String,
    pub plan: PathBuf,
    pub started: String,
    /// Empty while the run is in progress.
    pub finished: String,
    /// Finished items, in plan order.
    pub items: Vec<ItemResult>,
    /// Version banner of each tool (tool -> first line), for the reader;
    /// older reports kept these among `tool_warnings`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_versions: BTreeMap<String, String>,
    /// Real problems of the run as a whole (unknown zone, journal errors).
    pub tool_warnings: Vec<String>,
    /// Set by an undo that finished without problems; a report is undone at
    /// most once.
    #[serde(default)]
    pub undone: bool,
    /// Sources of the items an undo already finished without problems; a
    /// rerun of undo skips them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub undone_sources: Vec<PathBuf>,
    /// The run was cancelled: items it never started are absent from
    /// `items` (their sources were not touched).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cancelled: bool,
    /// The run stopped because `max_anomalies` items failed verification:
    /// like a cancel, items it never started are absent from `items`. When
    /// the limit was reached by the last items in flight nothing is absent
    /// and `cancelled` stays false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stopped_by_anomalies: bool,
    /// Files the plan's `keep` targets took: apply leaves them untouched,
    /// they are only counted here.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub kept: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemResult {
    pub source: PathBuf,
    pub status: ItemStatus,
    pub outputs: Vec<OutputResult>,
    /// Item-level problem (no outputs, trash failure, changed source, ...).
    #[serde(default)]
    pub error: Option<String>,
    /// Wall time the item took on its worker, in milliseconds (absent in
    /// older reports and for items that were not processed): the estimate
    /// learns throughput from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    /// The plan target that routed the file (absent in older reports): a
    /// partial undo by target relies on it, since the plan file may have
    /// been rescanned since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ItemStatus {
    /// Every output is in place. `trashed` is where the source went; `None`
    /// when an output consumed the source (renamed or kept it in place).
    /// Older reports always hold a trash reference here.
    Done {
        #[serde(default)]
        trashed: Option<TrashRef>,
    },
    /// Something failed. `trashed` is set only when the source already went
    /// to the trash and its in-place output could not be placed afterwards.
    Failed {
        #[serde(default)]
        trashed: Option<TrashRef>,
    },
    /// The source differs from the plan (size or mtime); nothing was done.
    Stale,
    /// The source does not exist; nothing was done.
    Missing,
    /// Dry run: the item would be processed.
    DryRun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputResult {
    pub planned: PathBuf,
    /// Where the output really is (another name on collision or fallback).
    pub actual: Option<PathBuf>,
    pub error: Option<String>,
    pub warnings: Vec<String>,
    pub fell_back: bool,
    /// Size of the file placed at `actual`; undo keeps a file whose size changed.
    #[serde(default)]
    pub size: Option<u64>,
    /// blake3 (hex) of the file placed at `actual`; undo keeps a file whose
    /// content changed.
    #[serde(default)]
    pub hash: Option<String>,
    /// Set when this output is the source itself, renamed in its folder from
    /// this path to `actual`; undo renames it back and never deletes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renamed_from: Option<PathBuf>,
    /// What differed from the source (capture date, mtime, GPS, size,
    /// duration); such an output is not placed unless anomalies are allowed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anomalies: Vec<String>,
}

/// The plan's name without a trailing `.json`: `trip.2026.json` and
/// `trip.2026` are both `trip.2026`. Report, journal and lock names all start
/// from it, so a dotted name never collides with a shorter one.
fn plan_stem(plan_path: &Path) -> String {
    let name = plan_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.strip_suffix(".json").map(String::from).unwrap_or(name)
}

/// `<plan stem>.report.json` next to the plan.
pub fn report_path(plan_path: &Path) -> PathBuf {
    plan_path.with_file_name(format!("{}.report.json", plan_stem(plan_path)))
}

/// `<report stem>.jsonl` next to the report: one line per finished item (and
/// per trashed source), synced at once, so that undo knows every trashed
/// source even when the run dies between two report writes.
pub fn journal_path(report: &Path) -> PathBuf {
    report.with_extension("jsonl")
}

/// `<plan stem>.lock` next to the plan: held by an apply or undo of it.
pub fn lock_path(plan_path: &Path) -> PathBuf {
    plan_path.with_file_name(format!("{}.lock", plan_stem(plan_path)))
}

/// The lock of the plan whose report (current or archived) is `report`:
/// `<plan stem>.report[.<run_id>].json` locks `<plan stem>.lock`, the same
/// file [`lock_path`] gives an apply of the plan.
pub fn lock_path_for_report(report: &Path) -> PathBuf {
    let name = report
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match name.rfind(".report.") {
        Some(i) => report.with_file_name(format!("{}.lock", &name[..i])),
        None => lock_path(report),
    }
}

/// Exclusive hold of a plan's lock file (an advisory flock): released when
/// dropped or when the process dies.
#[allow(dead_code)] // held for its Drop
pub(crate) struct RunLock(crate::lock::FileLock);

impl RunLock {
    /// Take the lock of the plan at `plan_path`; another run holding it is
    /// an error.
    pub(crate) fn acquire(plan_path: &Path) -> Result<RunLock> {
        RunLock::acquire_at(&lock_path(plan_path))
    }

    fn acquire_at(path: &Path) -> Result<RunLock> {
        crate::lock::FileLock::acquire(path, |pid, p| {
            format!(
                "plan is already being applied or undone (pid {}): {}; \
                 if there is no such process, delete the lock file",
                pid.trim(),
                p.display()
            )
        })
        .map(RunLock)
    }

    pub(crate) fn acquire_for_report(report: &Path) -> Result<RunLock> {
        RunLock::acquire_at(&lock_path_for_report(report))
    }
}

/// Execute the plan at `plan_path`; the report is also written next to it.
pub fn apply(plan_path: &Path, tools: &Tools, opts: &ApplyOptions) -> Result<Report> {
    apply_with(plan_path, tools, opts, &NoProgress, &Cancel::new())
}

/// [`apply`] with an [`Stage::Apply`] event per finished item and
/// cancellation: once `cancel` is set no new item starts, items in flight
/// finish (each on its own safe path), and the report (marked `cancelled`)
/// lists only the items that ran.
pub fn apply_with(
    plan_path: &Path,
    tools: &Tools,
    opts: &ApplyOptions,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<Report> {
    apply_observed(
        plan_path,
        tools,
        opts,
        progress,
        cancel,
        &|_: &ItemResult| {},
    )
}

/// [`apply_with`] that also hands every finished item's result to
/// `on_item` (from worker threads), just before that item's progress event:
/// a caller can count failures as they happen.
pub fn apply_observed(
    plan_path: &Path,
    tools: &Tools,
    opts: &ApplyOptions,
    progress: &dyn Progress,
    cancel: &Cancel,
    on_item: &(dyn Fn(&ItemResult) + Sync),
) -> Result<Report> {
    apply_impl(
        plan_path,
        tools,
        &tools.capabilities(),
        opts,
        progress,
        cancel,
        on_item,
    )
}

#[cfg(test)]
fn apply_with_caps(
    plan_path: &Path,
    tools: &Tools,
    caps: &Capabilities,
    opts: &ApplyOptions,
) -> Result<Report> {
    apply_impl(
        plan_path,
        tools,
        caps,
        opts,
        &NoProgress,
        &Cancel::new(),
        &|_: &ItemResult| {},
    )
}

fn apply_impl(
    plan_path: &Path,
    tools: &Tools,
    caps: &Capabilities,
    opts: &ApplyOptions,
    progress: &dyn Progress,
    cancel: &Cancel,
    on_item: &(dyn Fn(&ItemResult) + Sync),
) -> Result<Report> {
    let mut plan = Plan::load(plan_path)?;
    expand_paths(&mut plan);
    let tool_versions = check_capabilities(&plan, caps, tools.optipng.is_some())?;
    let mut tool_warnings = Vec::new();
    // Wall-clock capture times are the planning machine's: convert them (and
    // run every tool) in its zone, wherever this runs.
    let mut tools = tools.clone();
    match plan.tz.as_deref() {
        Some(z) if z.parse::<chrono_tz::Tz>().is_ok() => tools.tz = Some(z.to_string()),
        Some(z) => tool_warnings.push(crate::t!("apply.bad_tz", zone = z)),
        None => tool_warnings.push(crate::t!("apply.no_tz")),
    }
    let tools = &tools;
    // A dry run writes nothing, so it needs no lock.
    let _lock = (!opts.dry_run)
        .then(|| RunLock::acquire(plan_path))
        .transpose()?;
    if let (Some(sync), false) = (&opts.pause_sync, opts.dry_run) {
        // Before the first file moves; never a reason not to run.
        if let Err(e) = sync.pause_for(plan_path, &plan) {
            tool_warnings.push(crate::t!("apply.sync_not_paused", error = e));
        }
    }

    let now = Local::now();
    let mut report = Report {
        run_id: now.format("%Y%m%d-%H%M%S").to_string(),
        plan: plan_path.to_path_buf(),
        started: now.to_rfc3339(),
        finished: String::new(),
        items: Vec::new(),
        tool_versions,
        tool_warnings,
        undone: false,
        undone_sources: Vec::new(),
        cancelled: false,
        stopped_by_anomalies: false,
        kept: plan.kept.len(),
    };
    let out_path = report_path(plan_path);
    let journal = if opts.dry_run {
        None
    } else {
        // Fail before touching anything when the report cannot be written.
        archive_report(&out_path, plan_path)?;
        write_report(&out_path, &report)?;
        let j = Journal::create(&journal_path(&out_path), &report.run_id)?;
        remove_leftovers(&plan);
        Some(j)
    };

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.jobs.max(1))
        .build()
        .map_err(|e| Error::Tool(crate::t!("apply.thread_pool", error = e)))?;
    let run = Run {
        plan: &plan,
        tools,
        run_id: &report.run_id,
        opts,
        dirs: Mutex::new(HashMap::new()),
        journal: journal.as_ref(),
        incompressible: Mutex::new(Vec::new()),
    };
    let collected = Mutex::new(Collected::new(
        report.clone(),
        plan.items.len(),
        (!opts.dry_run).then(|| out_path.clone()),
    ));
    let total = plan.items.len();
    let done = AtomicUsize::new(0);
    let anomalous = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let one = |i: usize, item: &PlanItem| {
        // Checked only before an item starts: an item in flight always
        // finishes on its own path, and one never started stays absent
        // from the report (its source untouched).
        if cancel.is_cancelled() || stop.load(Ordering::SeqCst) {
            return;
        }
        let started = Instant::now();
        let mut r = run.item(item);
        if matches!(
            r.status,
            ItemStatus::Done { .. } | ItemStatus::Failed { .. }
        ) {
            r.elapsed_ms = Some(started.elapsed().as_millis() as u64);
        }
        if !opts.allow_anomalies
            && r.outputs.iter().any(|o| !o.anomalies.is_empty())
            && anomalous.fetch_add(1, Ordering::SeqCst) + 1 >= opts.max_anomalies
            && opts.max_anomalies > 0
        {
            stop.store(true, Ordering::SeqCst);
        }
        run.journal_result(item, &r);
        on_item(&r);
        collected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record(i, r);
        progress.step(ProgressEvent {
            stage: Stage::Apply,
            done: done.fetch_add(1, Ordering::SeqCst) + 1,
            total,
            current: Some(item.source.clone()),
        });
    };
    let is_trash = |item: &PlanItem| item.kind == TargetKind::Trash;
    pool.install(|| {
        // Every placement first; the files of `trash` targets go to the
        // trash only after that, in the phase where originals are trashed.
        plan.items
            .par_iter()
            .enumerate()
            .filter(|(_, item)| !is_trash(item))
            .for_each(|(i, item)| one(i, item));
        plan.items
            .par_iter()
            .enumerate()
            .filter(|(_, item)| is_trash(item))
            .for_each(|(i, item)| one(i, item));
    });

    // One write for the whole run (cancelled or stopped too: what finished).
    let found = run
        .incompressible
        .into_inner()
        .unwrap_or_else(|e| e.into_inner());
    crate::incompressible::remember_many(&found);
    let collected = collected.into_inner().unwrap_or_else(|e| e.into_inner());
    report.cancelled = collected.results.iter().any(Option::is_none);
    report.stopped_by_anomalies = stop.load(Ordering::SeqCst);
    report.items = collected.results.into_iter().flatten().collect();
    report.finished = Local::now().to_rfc3339();
    if let Some(j) = journal {
        report.tool_warnings.extend(j.into_errors());
        if let Err(e) = write_report(&out_path, &report) {
            // The undo record must not be lost: hand it over on stderr. The
            // journal stays for undo, next to the unfinished report.
            if let Ok(json) = serde_json::to_string_pretty(&report) {
                eprintln!("{json}");
            }
            return Err(e);
        }
        // The finished report holds everything the journal did.
        let _ = std::fs::remove_file(journal_path(&out_path));
    }
    Ok(report)
}

/// One journal line.
#[derive(Serialize, Deserialize)]
struct JournalLine {
    run_id: String,
    item: ItemResult,
}

/// Append-only journal of item results; every line is synced at once.
struct Journal {
    file: Mutex<std::fs::File>,
    run_id: String,
    /// Lines that could not be written, for the report.
    errors: Mutex<Vec<String>>,
}

impl Journal {
    /// A new journal at `path`; an existing file is never reused.
    fn create(path: &Path, run_id: &str) -> Result<Journal> {
        let file = std::fs::OpenOptions::new()
            .append(true)
            .create_new(true)
            .open(path)
            .map_err(|e| Error::Plan(format!("{}: {e}", path.display())))?;
        Ok(Journal {
            file: Mutex::new(file),
            run_id: run_id.to_string(),
            errors: Mutex::new(Vec::new()),
        })
    }

    fn append(&self, item: &ItemResult) {
        let line = JournalLine {
            run_id: self.run_id.clone(),
            item: item.clone(),
        };
        let res = serde_json::to_string(&line)
            .map_err(std::io::Error::other)
            .and_then(|mut text| {
                text.push('\n');
                let mut f = self.file.lock().unwrap_or_else(|e| e.into_inner());
                std::io::Write::write_all(&mut *f, text.as_bytes())?;
                f.sync_data()
            });
        if let Err(e) = res {
            self.errors
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(crate::t!(
                    "apply.journal",
                    path = item.source.display(),
                    error = e
                ));
        }
    }

    fn into_errors(self) -> Vec<String> {
        self.errors.into_inner().unwrap_or_else(|e| e.into_inner())
    }
}

/// Item results of run `run_id` in the journal at `path`, the last line of
/// each source winning, in first-seen order. A torn last line (the run died
/// while writing it) and lines of other runs are skipped.
pub(crate) fn read_journal(path: &Path, run_id: &str) -> Result<Vec<ItemResult>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut items: Vec<ItemResult> = Vec::new();
    let mut at: HashMap<PathBuf, usize> = HashMap::new();
    for line in text.lines() {
        let Ok(l) = serde_json::from_str::<JournalLine>(line) else {
            continue;
        };
        if l.run_id != run_id {
            continue;
        }
        match at.get(&l.item.source) {
            Some(&i) => items[i] = l.item,
            None => {
                at.insert(l.item.source.clone(), items.len());
                items.push(l.item);
            }
        }
    }
    Ok(items)
}

/// A plan written for another machine may hold `~`: it is this machine's home.
fn expand_paths(plan: &mut Plan) {
    for it in &mut plan.items {
        it.source = expand_tilde(&it.source);
        it.source_root = expand_tilde(&it.source_root);
        for o in &mut it.outputs {
            o.dest = expand_tilde(&o.dest);
        }
    }
}

/// Refuse when a used preset needs something the tools lack. Returns the
/// tool version banners for the report: a plan carries no versions to compare
/// against, so the report records them for the reader.
pub fn check_capabilities(
    plan: &Plan,
    caps: &Capabilities,
    has_optipng: bool,
) -> Result<BTreeMap<String, String>> {
    let used: BTreeSet<&str> = plan
        .items
        .iter()
        .flat_map(|it| &it.outputs)
        .flat_map(|o| &o.steps)
        .map(String::as_str)
        .collect();
    let mut missing = Vec::new();
    for name in used {
        let preset = plan
            .content_presets
            .get(name)
            .ok_or_else(|| Error::Plan(crate::t!("apply.unknown_preset", name = name)))?;
        match preset {
            ContentPreset::Video(v) => {
                if !caps.svtav1 {
                    missing.push(crate::t!("apply.no_svtav1", name = name));
                }
                if v.stabilize && !caps.vidstab {
                    missing.push(crate::t!("apply.no_vidstab", name = name));
                }
            }
            ContentPreset::Audio(a) if a.codec == AudioCodec::Opus && !caps.opus => {
                missing.push(crate::t!("apply.no_opus", name = name));
            }
            ContentPreset::Png(_) if !has_optipng => {
                missing.push(crate::t!("apply.no_optipng", name = name));
            }
            _ => {}
        }
    }
    if !missing.is_empty() {
        return Err(Error::Tool(missing.join("; ")));
    }
    Ok(caps.versions.clone())
}

/// Delete `.gallery-tmp-*` entries an interrupted run left in destination dirs.
fn remove_leftovers(plan: &Plan) {
    let sources: HashSet<&Path> = plan.items.iter().map(|it| it.source.as_path()).collect();
    let dirs: BTreeSet<&Path> = plan
        .items
        .iter()
        .flat_map(|it| &it.outputs)
        .filter_map(|o| o.dest.parent())
        .collect();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in rd.flatten() {
            let path = e.path();
            if !e.file_name().to_string_lossy().starts_with(TMP_PREFIX)
                || sources.contains(path.as_path())
            {
                continue;
            }
            let _ = match e.file_type() {
                Ok(t) if t.is_dir() => std::fs::remove_dir_all(&path),
                _ => std::fs::remove_file(&path),
            };
        }
    }
}

/// Keep an earlier run's report and journal (its undo record): move them to
/// `<plan stem>.report.<its run_id>.json` / `.jsonl` (`_N` added if taken),
/// never overwriting. Without a readable run_id the file's mtime names it.
fn archive_report(path: &Path, plan_path: &Path) -> Result<()> {
    let journal = journal_path(path);
    let report_meta = std::fs::symlink_metadata(path).ok();
    let journal_meta = std::fs::symlink_metadata(&journal).ok();
    let Some(meta) = report_meta.as_ref().or(journal_meta.as_ref()) else {
        return Ok(());
    };
    let id = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("run_id")?.as_str().map(String::from))
        .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .unwrap_or_else(|| {
            let t: chrono::DateTime<Local> =
                meta.modified().map_or_else(|_| Local::now(), Into::into);
            t.format("%Y%m%d-%H%M%S").to_string()
        });
    let taken = |p: &Path| std::fs::symlink_metadata(p).is_ok();
    let mut n = 0;
    let target = loop {
        let suffix = if n == 0 {
            id.clone()
        } else {
            format!("{id}_{n}")
        };
        let target =
            plan_path.with_file_name(format!("{}.report.{suffix}.json", plan_stem(plan_path)));
        if !taken(&target) && !taken(&journal_path(&target)) {
            break target;
        }
        n += 1;
    };
    if report_meta.is_some() {
        move_noreplace(path, &target)?;
    }
    if journal_meta.is_some() {
        move_noreplace(&journal, &journal_path(&target))?;
    }
    Ok(())
}

/// Move `from` to `to` unless `to` exists.
pub(crate) fn move_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    link_noreplace(from, to)?;
    // After a hard link the old name is still there; after a rename it is not.
    if std::fs::symlink_metadata(from).is_ok() {
        std::fs::remove_file(from)?;
    }
    Ok(())
}

/// blake3 of a file's content, as lowercase hex.
pub(crate) fn file_hash(path: &Path) -> std::io::Result<String> {
    let mut h = blake3::Hasher::new();
    h.update_reader(std::fs::File::open(path)?)?;
    Ok(h.finalize().to_hex().to_string())
}

/// Atomic report write: a temp file next to it, then rename over it.
pub(crate) fn write_report(path: &Path, report: &Report) -> Result<()> {
    let json = serde_json::to_string_pretty(report)
        .map_err(|e| Error::Plan(crate::t!("apply.report", error = e)))?;
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let mut tmp = tempfile::Builder::new()
        .prefix(".report-")
        .tempfile_in(dir)?;
    std::io::Write::write_all(&mut tmp, json.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| Error::Io(e.error))?;
    Ok(())
}

/// Results collected from the worker threads; refreshes the report on disk.
struct Collected {
    base: Report,
    results: Vec<Option<ItemResult>>,
    done: usize,
    path: Option<PathBuf>,
}

impl Collected {
    fn new(base: Report, items: usize, path: Option<PathBuf>) -> Self {
        Collected {
            base,
            results: vec![None; items],
            done: 0,
            path,
        }
    }

    fn record(&mut self, i: usize, r: ItemResult) {
        self.results[i] = Some(r);
        self.done += 1;
        if self.done.is_multiple_of(REPORT_EVERY)
            && let Some(path) = &self.path
        {
            let mut snap = self.base.clone();
            snap.items = self.results.iter().flatten().cloned().collect();
            // Best effort: the final write reports a persistent problem.
            let _ = write_report(path, &snap);
        }
    }
}

/// A finished output that waits under a temp name for its final place.
struct Staged {
    path: PathBuf,
    wanted: PathBuf,
    len: u64,
    /// blake3 (hex) of the staged file.
    hash: String,
}

struct Run<'a> {
    plan: &'a Plan,
    tools: &'a Tools,
    run_id: &'a str,
    opts: &'a ApplyOptions,
    /// Lowercased names per destination directory: what is on disk plus what
    /// this run placed. Guarded so that placing a name is race-free.
    dirs: Mutex<HashMap<PathBuf, HashSet<String>>>,
    /// None in a dry run.
    journal: Option<&'a Journal>,
    /// Files no pipe made smaller, written to the store once at the end.
    incompressible: Mutex<Vec<crate::incompressible::Record>>,
}

fn same_file_state(item: &PlanItem, meta: &std::fs::Metadata) -> bool {
    meta.len() == item.size && mtime_ns(meta).is_ok_and(|m| m == item.mtime_ns)
}

impl Run<'_> {
    fn journal(&self, r: &ItemResult) {
        if let Some(j) = self.journal {
            j.append(r);
        }
    }

    /// Journal the result of `item`, unless [`Run::trash_source`] did at
    /// once (a trashed `trash` item): one line per item is enough.
    fn journal_result(&self, item: &PlanItem, r: &ItemResult) {
        let journaled =
            item.kind == TargetKind::Trash && matches!(r.status, ItemStatus::Done { .. });
        if !journaled {
            self.journal(r);
        }
    }

    fn item(&self, item: &PlanItem) -> ItemResult {
        let mut res = ItemResult {
            source: item.source.clone(),
            status: ItemStatus::Failed { trashed: None },
            outputs: item
                .outputs
                .iter()
                .map(|o| OutputResult {
                    planned: o.dest.clone(),
                    actual: None,
                    error: None,
                    warnings: Vec::new(),
                    fell_back: false,
                    size: None,
                    hash: None,
                    renamed_from: None,
                    anomalies: Vec::new(),
                })
                .collect(),
            error: None,
            elapsed_ms: None,
            target: Some(item.target.clone()),
        };
        let meta = match std::fs::symlink_metadata(&item.source) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                res.status = ItemStatus::Missing;
                return res;
            }
            Err(e) => {
                res.error = Some(e.to_string());
                return res;
            }
        };
        if !meta.is_file() {
            res.error = Some(crate::t!("apply.not_regular"));
            return res;
        }
        if !same_file_state(item, &meta) {
            res.status = ItemStatus::Stale;
            return res;
        }
        if item.kind == TargetKind::Trash {
            if !item.outputs.is_empty() {
                // A trash item never places anything: a plan saying
                // otherwise is not one this build wrote.
                res.error = Some(crate::t!("apply.trash_with_outputs"));
            } else if self.opts.dry_run {
                res.status = ItemStatus::DryRun;
            } else {
                self.trash_source(item, &mut res);
            }
            return res;
        }
        if item.outputs.is_empty() {
            // Vacuously "all outputs done" must never trash a source.
            res.error = Some(no_outputs());
            return res;
        }
        if self.opts.dry_run {
            res.status = ItemStatus::DryRun;
            return res;
        }
        self.process(item, &mut res);
        res
    }

    /// A `trash` target's file, already checked to be the file the plan saw:
    /// nothing to place or verify, so it goes to the trash directly and is
    /// journaled at once, like a trashed original, so undo finds it.
    fn trash_source(&self, item: &PlanItem, res: &mut ItemResult) {
        let trashed = match self.opts.trash {
            TrashMode::System => move_to_trash(&item.source, &item.source_root, self.run_id),
            TrashMode::Folder => move_to_folder(&item.source, &item.source_root, self.run_id),
        };
        match trashed {
            Ok(t) => {
                res.status = ItemStatus::Done { trashed: Some(t) };
                self.journal(res);
                #[cfg(test)]
                if CRASH_AFTER_TRASH.lock().unwrap().contains(&item.source) {
                    panic!("test: the run dies after the trash move");
                }
            }
            Err(e) => res.error = Some(crate::t!("apply.trash", error = e)),
        }
    }

    /// Produce and place every output, then trash the source; or, when an
    /// output consumes the source (Rename/Keep), rename or keep it instead:
    /// such a source never goes to the trash.
    fn process(&self, item: &PlanItem, res: &mut ItemResult) {
        let consumes = item.outputs.iter().any(|o| o.op != OutputOp::Write);
        let writes = item.outputs.iter().any(|o| o.op == OutputOp::Write);
        // Written outputs carry the source's dates: without its EXIF they
        // would lose them, and the source then goes to the trash. Nothing
        // has been touched yet, so the item fails and the source stays.
        let exif = if writes {
            let read =
                exif::read(self.tools, std::slice::from_ref(&item.source)).and_then(|mut m| {
                    m.remove(&item.source)
                        .ok_or_else(|| Error::Tool(crate::t!("apply.exif_no_data")))
                });
            match read {
                Ok(v) => v,
                Err(e) => {
                    res.error = Some(crate::t!("apply.exif_not_read", error = e));
                    return;
                }
            }
        } else {
            Value::Null
        };
        let file_name = item
            .source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ctx_capture = capture_date(
            &file_name,
            kind_of(&item.source),
            &exif,
            self.tools.tz.as_deref().and_then(|z| z.parse().ok()),
        );

        // Outputs replacing the source itself wait until it is in the trash.
        let mut in_place: Vec<(usize, Staged)> = Vec::new();
        // Produced, verified outputs waiting to be placed.
        let mut ready: Vec<(usize, Staged)> = Vec::new();
        let mut failed = false;
        // blake3 of the source, read once for the byte copies (empty pipes).
        let mut source_hash: Option<String> = None;
        for (k, o) in item.outputs.iter().enumerate() {
            let out = &mut res.outputs[k];
            if failed {
                out.error = Some(crate::t!("apply.earlier_failed"));
                continue;
            }
            match o.op {
                OutputOp::Write => {}
                // Renamed once every write is in place.
                OutputOp::Rename => continue,
                OutputOp::Keep => {
                    out.warnings.push(crate::t!("apply.kept_in_place"));
                    continue;
                }
            }
            let staged = match self.produce(item, o, &exif, ctx_capture) {
                Ok((staged, warnings, fell_back)) => {
                    out.warnings = warnings;
                    out.fell_back = fell_back;
                    staged
                }
                Err(e) => {
                    out.error = Some(e.to_string());
                    failed = true;
                    continue;
                }
            };
            if !o.steps.is_empty() {
                // Before anything is placed or trashed: what the pipe should
                // keep must still match the source.
                match self.verify(item, o, &exif, ctx_capture, &staged) {
                    Ok(found) if found.is_empty() => {}
                    Ok(found) if self.opts.allow_anomalies => {
                        for a in &found {
                            out.warnings
                                .push(crate::t!("apply.discrepancy_allowed", message = a.message));
                        }
                        out.anomalies = found.into_iter().map(|a| a.message).collect();
                    }
                    Ok(found) => {
                        let _ = std::fs::remove_file(&staged.path);
                        let msgs: Vec<String> = found.into_iter().map(|a| a.message).collect();
                        out.error =
                            Some(crate::t!("apply.result_differs", details = msgs.join("; ")));
                        out.anomalies = msgs;
                        failed = true;
                        continue;
                    }
                    Err(e) => {
                        let _ = std::fs::remove_file(&staged.path);
                        let msg = crate::t!("apply.check_not_performed", error = e);
                        // A tool failure counts toward max_anomalies too.
                        out.anomalies = vec![msg.clone()];
                        out.error = Some(crate::t!("apply.source_untouched", message = msg));
                        failed = true;
                        continue;
                    }
                }
            }
            if o.steps.is_empty() {
                // A byte copy must be the source exactly.
                let want = match &source_hash {
                    Some(h) => Ok(h.clone()),
                    None => file_hash(&item.source).inspect(|h| source_hash = Some(h.clone())),
                };
                let err = match want {
                    Ok(h) if h == staged.hash => None,
                    Ok(h) => Some(crate::t!(
                        "apply.copy_differs",
                        got = staged.hash,
                        expected = h
                    )),
                    Err(e) => Some(crate::t!("apply.source_blake3", error = e)),
                };
                if let Some(err) = err {
                    let _ = std::fs::remove_file(&staged.path);
                    out.error = Some(err);
                    failed = true;
                    continue;
                }
            }
            ready.push((k, staged));
        }

        let discard = |staged: &[(usize, Staged)]| {
            for (_, s) in staged {
                let _ = std::fs::remove_file(&s.path);
            }
        };
        // Every output is produced and verified before any is placed: one
        // bad output leaves nothing behind.
        if failed {
            discard(&ready);
            return;
        }
        // An output that is the source's own bytes (a pipe that could not
        // make the file smaller fell back to it) never replaces the source:
        // wanted at the source's own path, the source just stays (nothing
        // written, nothing trashed); wanted under another name in the
        // source's folder, the source is renamed instead of copied and
        // trashed, unless another output already consumes it. Only for
        // processed outputs: a byte copy's place is decided by the plan
        // (a Keep or Rename output when it is the source).
        let renames = item.outputs.iter().any(|o| o.op == OutputOp::Rename);
        let src_dir = item.source.parent();
        let mut identical = |staged: &Staged| {
            staged.len == item.size
                && match &source_hash {
                    Some(h) => *h == staged.hash,
                    None => file_hash(&item.source)
                        .inspect(|h| source_hash = Some(h.clone()))
                        .is_ok_and(|h| h == staged.hash),
                }
        };
        let at_source = |staged: &Staged| {
            staged.wanted == item.source || same_file(&staged.wanted, &item.source)
        };
        let in_src_dir = |staged: &Staged| {
            let w = staged.wanted.parent();
            w.zip(src_dir).is_some_and(|(w, s)| same_dir(w, s, true))
        };
        let processed = |k: usize| !item.outputs[k].steps.is_empty();
        let mut stays_at: Vec<usize> = Vec::new();
        let mut rename_to: Option<(usize, PathBuf)> = None;
        for (k, staged) in &ready {
            if processed(*k) && !renames && at_source(staged) && identical(staged) {
                stays_at.push(*k);
            }
        }
        if !consumes && stays_at.is_empty() {
            rename_to = ready
                .iter()
                .find(|(k, s)| processed(*k) && in_src_dir(s) && identical(s))
                .map(|(k, s)| (*k, s.wanted.clone()));
        }
        let source_stays = consumes || !stays_at.is_empty() || rename_to.is_some();
        let mut ready_rest = Vec::with_capacity(ready.len());
        for (k, staged) in ready {
            let kept = stays_at.contains(&k);
            if kept || rename_to.as_ref().is_some_and(|(r, _)| *r == k) {
                let _ = std::fs::remove_file(&staged.path);
                let out = &mut res.outputs[k];
                out.warnings.push(crate::t!("apply.already_smallest"));
                if kept {
                    // The output is the source itself, where it already is.
                    out.actual = Some(item.source.clone());
                    out.size = Some(item.size);
                    out.hash = Some(staged.hash.clone());
                }
                continue;
            }
            ready_rest.push((k, staged));
        }
        // Decided by where the result goes: a fallback that keeps the
        // source's extension can land on the source path too. A source
        // that stays (renamed or kept) is never replaced: such an output
        // gets the next free name instead.
        let mut ready = ready_rest.into_iter();
        while let Some((k, staged)) = ready.next() {
            let out = &mut res.outputs[k];
            if !source_stays && staged.wanted == item.source {
                in_place.push((k, staged));
                continue;
            }
            let (actual, err) = self.place(&staged, None);
            out.size = actual.as_ref().map(|_| staged.len);
            out.hash = actual.as_ref().map(|_| staged.hash.clone());
            out.actual = actual;
            if err.is_some() {
                out.error = err;
                let rest: Vec<(usize, Staged)> = ready.collect();
                for (j, _) in &rest {
                    res.outputs[*j].error = Some(crate::t!("apply.earlier_failed"));
                }
                discard(&rest);
                discard(&in_place);
                return;
            }
        }
        #[cfg(test)]
        if CHANGE_BEFORE_RENAME.lock().unwrap().contains(&item.source) {
            use std::io::Write;
            let _ = std::fs::OpenOptions::new()
                .append(true)
                .open(&item.source)
                .and_then(|mut f| f.write_all(b"changed"));
        }
        if source_stays {
            let renamed = rename_to.as_ref().map(|(k, _)| *k);
            self.consume(item, res, source_hash, rename_to);
            // Remember what could not be made smaller, where it now is, so
            // the next scan does not encode it again.
            if matches!(res.status, ItemStatus::Done { .. }) {
                for k in stays_at.iter().copied().chain(renamed) {
                    let at = res.outputs[k].actual.clone();
                    self.remember_incompressible(item, &item.outputs[k], at.as_deref());
                }
            }
            return;
        }
        // The source may have changed while it was being processed.
        match std::fs::symlink_metadata(&item.source) {
            Ok(m) if same_file_state(item, &m) => {}
            _ => {
                discard(&in_place);
                res.error = Some(crate::t!("apply.source_changed"));
                return;
            }
        }
        let trashed = match self.opts.trash {
            TrashMode::System => move_to_trash(&item.source, &item.source_root, self.run_id),
            TrashMode::Folder => move_to_folder(&item.source, &item.source_root, self.run_id),
        };
        let trashed = match trashed {
            Ok(t) => {
                // Undo must find this source even if the run dies now.
                self.journal(&ItemResult {
                    status: ItemStatus::Failed {
                        trashed: Some(t.clone()),
                    },
                    error: Some(crate::t!("apply.interrupted_trash")),
                    ..res.clone()
                });
                #[cfg(test)]
                if CRASH_AFTER_TRASH.lock().unwrap().contains(&item.source) {
                    panic!("test: the run dies after the trash move");
                }
                t
            }
            Err(e) => {
                discard(&in_place);
                res.error = Some(crate::t!("apply.trash", error = e));
                return;
            }
        };

        // The source path is free now: place the in-place outputs.
        let mut ok = true;
        for (k, staged) in &in_place {
            let out = &mut res.outputs[*k];
            let (actual, err) = self.place(staged, Some(&item.source));
            out.size = Some(staged.len);
            out.hash = Some(staged.hash.clone());
            let Some(mut err) = err else {
                out.actual = actual;
                continue;
            };
            ok = false;
            // The original is in the trash: never drop the processed file.
            out.actual = actual.or_else(|| {
                if std::fs::symlink_metadata(&staged.path).is_err() {
                    return None;
                }
                Some(self.keep(staged).unwrap_or_else(|| {
                    err.push_str("; left under its temp name, which the next run deletes");
                    staged.path.clone()
                }))
            });
            out.error = Some(err);
        }
        res.status = if ok {
            ItemStatus::Done {
                trashed: Some(trashed),
            }
        } else {
            res.error = Some(crate::t!("apply.not_placed_trashed"));
            ItemStatus::Failed {
                trashed: Some(trashed),
            }
        };
        if ok {
            self.remember_fallbacks(item, res);
        }
    }

    /// A pipe whose encoder fell back but whose later step (a metadata
    /// step) still changed the bytes replaced the source with a new file in
    /// its folder: remember that file, as placed, so the next scan keeps it
    /// instead of running the same pipe on it again (and again).
    fn remember_fallbacks(&self, item: &PlanItem, res: &ItemResult) {
        let Some(src_dir) = item.source.parent() else {
            return;
        };
        for (o, out) in item.outputs.iter().zip(&res.outputs) {
            let Some(at) = out.actual.as_deref() else {
                continue;
            };
            if !out.fell_back || o.steps.is_empty() || out.error.is_some() {
                continue;
            }
            if !at.parent().is_some_and(|d| same_dir(d, src_dir, true)) {
                continue;
            }
            let Ok(meta) = std::fs::symlink_metadata(at) else {
                continue;
            };
            let (Ok(mtime), true) = (mtime_ns(&meta), meta.is_file()) else {
                continue;
            };
            let pipe =
                crate::incompressible::fingerprint(&o.steps, |n| self.plan.content_presets.get(n));
            if let Some(pipe) = pipe {
                self.incompressible
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(crate::incompressible::Record {
                        path: at.to_path_buf(),
                        size: meta.len(),
                        mtime_ns: mtime,
                        pipe,
                    });
            }
        }
    }

    /// Every write of an item that keeps its source is in place: give the
    /// source the name of its Rename output, if it has one. The source is
    /// never trashed and never overwritten; a taken name gets the next one.
    /// `rename_to` is a written output found to be the source's own bytes
    /// under another name in its folder: the source takes that name instead.
    fn consume(
        &self,
        item: &PlanItem,
        res: &mut ItemResult,
        source_hash: Option<String>,
        rename_to: Option<(usize, PathBuf)>,
    ) {
        let planned = item
            .outputs
            .iter()
            .position(|o| o.op == OutputOp::Rename)
            .map(|k| (k, item.outputs[k].dest.clone()));
        let Some((k, dest)) = planned.or(rename_to) else {
            // Only Keep outputs besides the writes: nothing more to do.
            res.status = ItemStatus::Done { trashed: None };
            return;
        };
        let dest = &dest;
        let (Some(src_dir), Some(dest_dir), Some(wanted)) =
            (item.source.parent(), dest.parent(), dest.file_name())
        else {
            res.outputs[k].error = Some(crate::t!("apply.bad_rename", dest = dest.display()));
            return;
        };
        // A move never leaves the source's root: it stays a source file.
        if dest_dir.as_os_str().is_empty() || !inside_root(dest_dir, &item.source_root, true) {
            res.outputs[k].error = Some(crate::t!(
                "apply.rename_outside_root",
                root = item.source_root.display(),
                dest = dest.display()
            ));
            return;
        }
        // The source's own folder, however written, is named as the source's
        // path names it.
        let dir = if same_dir(dest_dir, src_dir, true) {
            src_dir
        } else {
            dest_dir
        };
        let hash = match source_hash.map_or_else(|| file_hash(&item.source), Ok) {
            Ok(h) => h,
            Err(e) => {
                res.outputs[k].error = Some(crate::t!("apply.source_blake3", error = e));
                return;
            }
        };
        // The source may have changed while it was being processed (or hashed).
        match std::fs::symlink_metadata(&item.source) {
            Ok(m) if same_file_state(item, &m) => {}
            _ => {
                res.error = Some("source changed during apply".into());
                return;
            }
        }
        let (actual, err) = self.rename_source(&item.source, dir, &wanted.to_string_lossy());
        let out = &mut res.outputs[k];
        if let Some(actual) = actual {
            out.renamed_from = Some(item.source.clone());
            out.size = Some(item.size);
            out.hash = Some(hash);
            out.actual = Some(actual);
            // Undo must find the new name even if the run dies now.
            self.journal(&ItemResult {
                status: ItemStatus::Failed { trashed: None },
                error: Some(crate::t!("apply.interrupted_rename")),
                ..res.clone()
            });
        }
        let out = &mut res.outputs[k];
        if err.is_some() {
            out.error = err;
            return;
        }
        res.status = ItemStatus::Done { trashed: None };
    }

    /// Record that the pipe of `o` could not make the source smaller, now
    /// that the file is at `at` (the source path, or its new name), still
    /// the file the plan saw.
    fn remember_incompressible(&self, item: &PlanItem, o: &PlannedOutput, at: Option<&Path>) {
        let Some(at) = at else {
            return;
        };
        if o.steps.is_empty()
            || !std::fs::symlink_metadata(at)
                .is_ok_and(|m| m.is_file() && same_file_state(item, &m))
        {
            return;
        }
        let pipe =
            crate::incompressible::fingerprint(&o.steps, |n| self.plan.content_presets.get(n));
        if let Some(pipe) = pipe {
            // The plan's size and mtime: the file was just checked to be them.
            self.incompressible
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(crate::incompressible::Record {
                    path: at.to_path_buf(),
                    size: item.size,
                    mtime_ns: item.mtime_ns,
                    pipe,
                });
        }
    }

    /// Move `source` to `wanted` in folder `dir` (its own folder or one inside
    /// its root, made if missing), or to the next free name; never
    /// overwrites, never copies. Returns where the file is (if it moved) and
    /// an error; a moved file whose check failed comes with both.
    fn rename_source(
        &self,
        source: &Path,
        dir: &Path,
        wanted: &str,
    ) -> (Option<PathBuf>, Option<String>) {
        let len = match std::fs::metadata(source) {
            Ok(m) => m.len(),
            Err(e) => return (None, Some(e.to_string())),
        };
        let src_dir = source.parent().unwrap_or(Path::new("."));
        let same_folder = src_dir == dir;
        if !same_folder && let Err(e) = std::fs::create_dir_all(dir) {
            return (None, Some(format!("{}: {e}", dir.display())));
        }
        let mut dirs = self.dirs.lock().unwrap_or_else(|e| e.into_inner());
        // The source's own name stays reserved in its folder: nothing else of
        // this run may take it, so a rename back on undo finds it free.
        if let Some(n) = source.file_name() {
            dirs.entry(src_dir.to_path_buf())
                .or_insert_with(|| read_dir_names(src_dir))
                .insert(n.to_string_lossy().to_lowercase());
        }
        let names = dirs
            .entry(dir.to_path_buf())
            .or_insert_with(|| read_dir_names(dir));
        // A new name in the same folder that differs from the source's only
        // in case is tried as is first; where the file system does not tell
        // them apart it exists, and the next name is used.
        let case_only = same_folder
            && source.file_name().is_some_and(|n| {
                let n = n.to_string_lossy();
                n != wanted && n.to_lowercase() == wanted.to_lowercase()
            });
        if case_only {
            let target = dir.join(wanted);
            match rename_noreplace(source, &target) {
                Ok(()) => {
                    drop(dirs);
                    let err = check_len(&target, len);
                    return (Some(target), err);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return (None, Some(e.to_string())),
            }
        }
        let target = loop {
            let target = dir.join(reserve(names, |_| false, wanted));
            match rename_noreplace(source, &target) {
                Ok(()) => break target,
                // Someone else created it: `reserve` recorded it, try the next.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return (None, Some(e.to_string())),
            }
        };
        drop(dirs);
        let err = check_len(&target, len);
        (Some(target), err)
    }

    /// Run the content pipe of `o` in its own work dir and stage the result
    /// in the destination directory.
    fn produce(
        &self,
        item: &PlanItem,
        o: &PlannedOutput,
        exif: &Value,
        capture: Option<chrono::NaiveDateTime>,
    ) -> Result<(Staged, Vec<String>, bool)> {
        let (dir, planned_name) = match (o.dest.parent(), o.dest.file_name()) {
            (Some(d), Some(n)) if !d.as_os_str().is_empty() => (d, n.to_string_lossy()),
            _ => {
                return Err(Error::Plan(crate::t!(
                    "apply.bad_dest",
                    dest = o.dest.display()
                )));
            }
        };
        std::fs::create_dir_all(dir)?;
        let prefix = format!("{TMP_PREFIX}{}-", self.run_id);
        let work = tempfile::Builder::new().prefix(&prefix).tempdir_in(dir)?;
        let steps = o
            .steps
            .iter()
            .map(|s| {
                self.plan
                    .content_presets
                    .get(s)
                    .ok_or_else(|| Error::Plan(crate::t!("apply.unknown_preset", name = s)))
            })
            .collect::<Result<Vec<_>>>()?;
        let ctx = StepCtx {
            tools: self.tools,
            original: &item.source,
            original_exif: exif,
            capture,
            tmp_dir: work.path(),
        };
        let mut r = run_pipe(&ctx, &steps)?;
        // The AI's keywords go into processed outputs only: a byte copy (no
        // steps) stays the source exactly. `r.path` is the pipe's own file.
        if let Some(meta) = item.ai_meta.as_ref().filter(|_| {
            !steps.is_empty() && matches!(item.via, Via::Ai { .. }) && r.path != item.source
        }) {
            match crate::content::metadata::write_ai_meta(self.tools, &r.path, meta) {
                Ok(()) => {}
                // exiftool leaves the file untouched when it fails.
                Err(Error::Unwritable(_)) => r.warnings.push(crate::t!(
                    "apply.ai_meta_unwritable",
                    container = r.ext.rsplit('.').next().unwrap_or_default()
                )),
                Err(e) => r
                    .warnings
                    .push(crate::t!("apply.ai_meta_failed", error = e)),
            }
        }

        let (_, staged) = tempfile::Builder::new()
            .prefix(&prefix)
            .tempfile_in(dir)?
            .keep()
            .map_err(|e| Error::Io(e.error))?;
        if let Err(e) = std::fs::rename(&r.path, &staged) {
            let _ = std::fs::remove_file(&staged);
            return Err(e.into());
        }
        drop(work);
        #[cfg(test)]
        if CORRUPT_STAGED.lock().unwrap().contains(&o.dest) {
            use std::io::Write;
            std::fs::OpenOptions::new()
                .append(true)
                .open(&staged)?
                .write_all(b"corrupted")?;
        }
        // On disk before it takes its place and the source goes to the trash.
        let file = std::fs::File::options().write(true).open(&staged)?;
        file.sync_all()?;
        // Encoders write through 0600 temp files; outputs must stay as
        // readable as their source (synced shares, other users). Set on the
        // open handle, after the writes a read-only mode would block.
        #[cfg(unix)]
        if let Err(e) = copy_mode(&item.source, &file, &staged) {
            let _ = std::fs::remove_file(&staged);
            return Err(e.into());
        }
        // Encoders stamp "now", which breaks galleries sorted by mtime: the
        // final file carries the capture date when a metadata step writes it
        // (whatever the step order), else the source's time.
        let stamped = self
            .output_mtime(&item.source, capture, &steps)
            .and_then(|t| file.set_modified(t).map_err(Into::into));
        if let Err(e) = stamped {
            let _ = std::fs::remove_file(&staged);
            return Err(e);
        }
        drop(file);
        #[cfg(test)]
        if let Some((_, f)) = TAMPER_STAGED
            .lock()
            .unwrap()
            .iter()
            .find(|(d, _)| *d == o.dest)
        {
            f(&staged);
        }
        let len = std::fs::metadata(&staged)?.len();
        let hash = file_hash(&staged)?;
        let name = if r.fell_back {
            let stem = split_name(&planned_name).0;
            if r.ext.is_empty() {
                stem
            } else {
                format!("{stem}.{}", r.ext)
            }
        } else {
            planned_name.into_owned()
        };
        Ok((
            Staged {
                path: staged,
                wanted: dir.join(name),
                len,
                hash,
            },
            r.warnings,
            r.fell_back,
        ))
    }

    /// The mtime the finished output must carry: the capture date when a
    /// metadata step writes it (whatever the step order), else the source's.
    fn output_mtime(
        &self,
        source: &Path,
        capture: Option<chrono::NaiveDateTime>,
        steps: &[&ContentPreset],
    ) -> Result<std::time::SystemTime> {
        match capture.filter(|_| sets_capture_mtime(steps)) {
            Some(d) => {
                crate::content::metadata::local_time(self.tools, d).map(std::time::SystemTime::from)
            }
            None => Ok(std::fs::metadata(source).and_then(|m| m.modified())?),
        }
    }

    /// Compare a staged output with its source (see [`crate::verify`]).
    fn verify(
        &self,
        item: &PlanItem,
        o: &PlannedOutput,
        exif: &Value,
        capture: Option<chrono::NaiveDateTime>,
        staged: &Staged,
    ) -> Result<Vec<verify::Anomaly>> {
        let steps = o
            .steps
            .iter()
            .map(|s| {
                self.plan
                    .content_presets
                    .get(s)
                    .ok_or_else(|| Error::Plan(crate::t!("apply.unknown_preset", name = s)))
            })
            .collect::<Result<Vec<_>>>()?;
        let resizes = steps.iter().any(|s| match s {
            ContentPreset::Photo(p) => p.max_short_side.is_some(),
            ContentPreset::Video(v) => v.max_short_side.is_some(),
            _ => false,
        });
        let name = staged
            .wanted
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ext = split_name(&name).1;
        let expected = Expected {
            source_exif: exif,
            mtime: self.output_mtime(&item.source, capture, &steps)?,
            // A metadata step writes the capture date it was given.
            capture: capture.filter(|_| sets_capture_mtime(&steps)),
            resizes,
            ext: &ext,
        };
        Ok(verify::compare(
            self.tools,
            &item.source,
            &staged.path,
            &expected,
        ))
    }

    /// Move a staged output to its wanted name, or the next free one; never
    /// overwrites. `freed` is a path this run just vacated (the trashed source).
    /// Returns where the file is (if it was placed at all) and an error; a
    /// placed file whose check failed comes with both, so undo can remove it.
    fn place(&self, staged: &Staged, freed: Option<&Path>) -> (Option<PathBuf>, Option<String>) {
        #[cfg(test)]
        if FAIL_PLACE.lock().unwrap().contains(&staged.wanted) {
            return (None, Some("test: placement fails".into()));
        }
        let dir = staged.wanted.parent().unwrap_or(Path::new("."));
        let wanted = staged
            .wanted
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut dirs = self.dirs.lock().unwrap_or_else(|e| e.into_inner());
        let names = dirs
            .entry(dir.to_path_buf())
            .or_insert_with(|| read_dir_names(dir));
        if let Some(f) = freed.filter(|f| f.parent() == Some(dir))
            && let Some(n) = f.file_name()
        {
            // A real collision is still caught by the no-overwrite link below.
            names.remove(&n.to_string_lossy().to_lowercase());
        }
        let target = loop {
            let name = reserve(names, |_| false, &wanted);
            let target = dir.join(name);
            match link_noreplace(&staged.path, &target) {
                Ok(()) => break target,
                // Someone else created it: `reserve` recorded it, try the next.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return (None, Some(e.to_string())),
            }
        };
        drop(dirs);
        let _ = std::fs::remove_file(&staged.path);
        let err = match std::fs::metadata(&target) {
            Ok(m) if m.len() == staged.len => None,
            Ok(m) => Some(crate::t!(
                "apply.size_differs",
                path = target.display(),
                got = m.len(),
                expected = staged.len
            )),
            Err(e) => Some(format!("{}: {e}", target.display())),
        };
        (Some(target), err)
    }

    /// Give a staged file a regular sibling name (`<stem>_kept.<ext>`, or the
    /// next free one) so that leftover cleanup never deletes it.
    fn keep(&self, staged: &Staged) -> Option<PathBuf> {
        let dir = staged.wanted.parent().unwrap_or(Path::new("."));
        let name = staged.wanted.file_name()?.to_string_lossy().into_owned();
        let (stem, ext) = split_name(&name);
        let wanted = if ext.is_empty() {
            format!("{stem}_kept")
        } else {
            format!("{stem}_kept.{ext}")
        };
        let mut dirs = self.dirs.lock().unwrap_or_else(|e| e.into_inner());
        let names = dirs
            .entry(dir.to_path_buf())
            .or_insert_with(|| read_dir_names(dir));
        loop {
            let target = dir.join(reserve(names, |_| false, &wanted));
            match link_noreplace(&staged.path, &target) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&staged.path);
                    return Some(target);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return None,
            }
        }
    }
}

/// Whether a metadata step of the pipe sets the mtime from the capture date.
fn sets_capture_mtime(steps: &[&ContentPreset]) -> bool {
    steps
        .iter()
        .any(|s| matches!(s, ContentPreset::Metadata(m) if m.write_capture_date))
}

/// Give `to` (open file at `to_path`) the read/write bits of `from`: never
/// execute bits (FAT/NTFS mounts show 0777). When `from` cannot be read, use
/// what a new file gets under the current umask, capped at 0644, probed with
/// a sibling of `to_path` (a temp name, so leftover cleanup covers it).
#[cfg(unix)]
fn copy_mode(from: &Path, to: &std::fs::File, to_path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = match std::fs::metadata(from) {
        Ok(m) => m.permissions().mode() & 0o666,
        Err(_) => {
            let mut probe = to_path.as_os_str().to_owned();
            probe.push("-mode");
            let m = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&probe)?
                .metadata()?
                .permissions()
                .mode();
            let _ = std::fs::remove_file(&probe);
            m & 0o644
        }
    };
    to.set_permissions(std::fs::Permissions::from_mode(mode))
}

/// Test hook: placing to these wanted paths fails before anything happens.
#[cfg(test)]
static FAIL_PLACE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Test hook: the run dies right after these sources went to the trash.
#[cfg(test)]
static CRASH_AFTER_TRASH: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Test hook: these sources change right before their rename.
#[cfg(test)]
static CHANGE_BEFORE_RENAME: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Test hook: hard links from these paths fail as if the target were on
/// another file system (EXDEV).
#[cfg(test)]
static LINK_EXDEV: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Test hook: hard links from these paths fail as unsupported, so the
/// rename fallback runs.
#[cfg(test)]
static LINK_UNSUPPORTED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Test hook: another program creates these paths right after the
/// fallback's existence check.
#[cfg(test)]
static RACE_BEFORE_RENAME: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Test hook: the staged file of these planned outputs is altered by the
/// function right after its final mtime was stamped.
#[cfg(test)]
type Tamper = fn(&Path);
#[cfg(test)]
static TAMPER_STAGED: Mutex<Vec<(PathBuf, Tamper)>> = Mutex::new(Vec::new());

/// Test hook: the staged file of these planned outputs gets damaged.
#[cfg(test)]
static CORRUPT_STAGED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

#[cfg(test)]
fn fail_place_to(wanted: &Path) {
    FAIL_PLACE.lock().unwrap().push(wanted.to_path_buf());
}

/// Lowercased names in `dir`; empty when it cannot be listed.
fn read_dir_names(dir: &Path) -> HashSet<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return HashSet::new();
    };
    rd.filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_lowercase())
        .collect()
}

/// Rename `from` to `to` unless `to` exists. With hard links the new name is
/// added first and the old one dropped only once both name the same file; if
/// the old name cannot be dropped, the new one is taken back. Without hard
/// links an existence check and a rename are used.
pub(crate) fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    if from == to {
        return Err(std::io::ErrorKind::AlreadyExists.into());
    }
    let before = nlink(from);
    match hard_link(from, to) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(e),
        // A rename never becomes a copy: across file systems it fails.
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            return Err(std::io::Error::new(
                e.kind(),
                crate::t!(
                    "apply.other_fs",
                    dir = to.parent().unwrap_or(to).display(),
                    from = from.display()
                ),
            ));
        }
        Err(_) => return rename_fallback(from, to),
    }
    // Both names must be separate entries of one file: a file system that
    // let `to` alias `from` itself (case folding) would lose it below. When
    // that is not certain, nothing is removed.
    link_verdict(before, nlink(from), || Ok(listed(from)? && listed(to)?))?;
    if !same_file(from, to) {
        // `from` was replaced in between: its old content is at `to` now;
        // the newcomer at `from` is not ours to remove.
        return Err(std::io::Error::other(format!(
            "{} was replaced during the rename; its old content is at {}",
            from.display(),
            to.display()
        )));
    }
    if let Err(e) = std::fs::remove_file(from) {
        // `to` is a second name of the same file: dropping it loses nothing.
        let _ = std::fs::remove_file(to);
        return Err(e);
    }
    Ok(())
}

/// `std::fs::hard_link`, with a test hook that fails it as across devices.
fn hard_link(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if LINK_EXDEV.lock().unwrap().iter().any(|p| p == from) {
        return Err(std::io::ErrorKind::CrossesDevices.into());
    }
    #[cfg(test)]
    if LINK_UNSUPPORTED.lock().unwrap().iter().any(|p| p == from) {
        return Err(std::io::ErrorKind::Unsupported.into());
    }
    std::fs::hard_link(from, to)
}

/// Rename `from` to `to` unless `to` exists, for file systems without hard
/// links. On Linux `renameat2(RENAME_NOREPLACE)` makes that atomic; where the
/// file system or kernel lacks it, an existence check and a rename are used.
fn rename_fallback(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(std::io::ErrorKind::AlreadyExists.into());
    }
    #[cfg(test)]
    if RACE_BEFORE_RENAME.lock().unwrap().iter().any(|p| p == to) {
        std::fs::write(to, b"racer")?;
    }
    #[cfg(target_os = "linux")]
    match rename_exclusive(from, to) {
        Err(e) if matches!(e.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS)) => {}
        r => return r,
    }
    rename_last_resort(from, to)
}

/// The final, non-atomic-where-unavoidable step of [`rename_fallback`].
#[cfg(windows)]
fn rename_last_resort(from: &Path, to: &Path) -> std::io::Result<()> {
    rename_exclusive_windows(from, to)
}

#[cfg(target_os = "macos")]
fn rename_last_resort(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = |p: &Path| CString::new(p.as_os_str().as_bytes()).map_err(std::io::Error::other);
    let (f, t) = (c(from)?, c(to)?);
    // SAFETY: both are valid NUL-terminated paths, alive for the call.
    let r = unsafe { libc::renamex_np(f.as_ptr(), t.as_ptr(), libc::RENAME_EXCL) };
    if r == 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    // A file system without exclusive rename: the check-then-rename below.
    if e.raw_os_error() == Some(libc::ENOTSUP) {
        return std::fs::rename(from, to);
    }
    Err(e)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn rename_last_resort(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

/// `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` (what `std::fs::rename`
/// always sets): atomic, fails with `AlreadyExists` instead of overwriting,
/// and, with no `MOVEFILE_COPY_ALLOWED`, never turns into a copy across
/// volumes. The only way to place a file on FAT/exFAT without ever
/// overwriting; NTFS goes through the hard link path first.
#[cfg(windows)]
fn rename_exclusive_windows(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let wide = |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain(Some(0)).collect() };
    let (f, t) = (wide(from), wide(to));
    // SAFETY: both are NUL-terminated UTF-16 paths alive for the call.
    let ok =
        unsafe { windows_sys::Win32::Storage::FileSystem::MoveFileExW(f.as_ptr(), t.as_ptr(), 0) };
    if ok != 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    // ERROR_ALREADY_EXISTS (183) and ERROR_FILE_EXISTS (80) are AlreadyExists
    // to std; ERROR_NOT_SAME_DEVICE (17) is CrossesDevices.
    Err(e)
}

/// `renameat2(from, to, RENAME_NOREPLACE)`: fails with `AlreadyExists`
/// instead of replacing `to`.
#[cfg(target_os = "linux")]
fn rename_exclusive(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = |p: &Path| CString::new(p.as_os_str().as_bytes()).map_err(std::io::Error::other);
    let (from, to) = (c(from)?, c(to)?);
    // SAFETY: both are valid NUL-terminated paths, alive for the call.
    let r = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Whether a hard link that reported success really added a second, distinct
/// name, so that dropping the old one cannot drop the file's only name. Yes
/// when the link count went up by exactly one; otherwise (counts unreadable,
/// another count, a link racing in) only when the folder listing shows both
/// names. A listing without both means the new name aliases the old one (a
/// case-folding file system): `AlreadyExists`, so the caller tries the next.
fn link_verdict(
    before: Option<u64>,
    after: Option<u64>,
    listed_both: impl FnOnce() -> std::io::Result<bool>,
) -> std::io::Result<()> {
    match (before, after) {
        (Some(b), Some(a)) if a == b + 1 => Ok(()),
        _ => match listed_both() {
            Ok(true) => Ok(()),
            Ok(false) => Err(std::io::ErrorKind::AlreadyExists.into()),
            Err(e) => Err(std::io::Error::other(crate::t!(
                "apply.cannot_verify_name",
                error = e
            ))),
        },
    }
}

/// Link count of `p` where the platform tells it.
fn nlink(p: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(p).ok().map(|m| m.nlink())
    }
    #[cfg(not(unix))]
    {
        let _ = p;
        None
    }
}

/// Whether `p`'s folder lists exactly `p`'s name (byte for byte).
fn listed(p: &Path) -> std::io::Result<bool> {
    let (Some(dir), Some(name)) = (p.parent(), p.file_name()) else {
        return Ok(false);
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    for e in std::fs::read_dir(dir)? {
        if e?.file_name() == name {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Why the file at `p` is not `len` bytes long, if it is not.
fn check_len(p: &Path, len: u64) -> Option<String> {
    match std::fs::metadata(p) {
        Ok(m) if m.len() == len => None,
        Ok(m) => Some(format!("{}: size {}, expected {len}", p.display(), m.len())),
        Err(e) => Some(format!("{}: {e}", p.display())),
    }
}

/// Whether `a` and `b` are names of one file (not following symlinks).
pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
    let (Ok(x), Ok(y)) = (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        x.dev() == y.dev() && x.ino() == y.ino()
    }
    #[cfg(not(unix))]
    {
        x.len() == y.len() && x.modified().ok() == y.modified().ok()
    }
}

/// Give `from` the name `to` unless `to` exists. A hard link is atomic; on a
/// file system without hard links an existence check and a rename are used
/// (the caller holds the directory lock, so only other programs could race).
pub(crate) fn link_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    match hard_link(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(e),
        Err(_) => rename_fallback(from, to),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AudioPreset, MetadataPreset, PngPreset, VideoPreset};
    use crate::plan::{PLAN_VERSION, Side};
    use crate::route::Via;
    use std::collections::BTreeMap;

    fn fake_tools() -> Tools {
        let p = PathBuf::from("/nonexistent/tool");
        // The shell-script stand-in needs unix; elsewhere the tool is absent.
        #[cfg(unix)]
        let exiftool = fake_exiftool();
        #[cfg(not(unix))]
        let exiftool = p.clone();
        Tools {
            ffmpeg: p.clone(),
            ffprobe: p.clone(),
            exiftool,
            avifenc: p.clone(),
            magick: p,
            optipng: None,
            tz: None,
        }
    }

    fn all_caps() -> Capabilities {
        Capabilities {
            svtav1: true,
            opus: true,
            vidstab: true,
            versions: BTreeMap::new(),
        }
    }

    /// One existing source with one output using `preset`; a leftover temp
    /// file sits in the destination dir.
    fn setup(d: &Path, preset: ContentPreset) -> (PathBuf, PathBuf, PathBuf) {
        let src = d.join("src/a.mov");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, b"x").unwrap();
        let out = d.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let leftover = out.join(".gallery-tmp-19990101-000000-x");
        std::fs::write(&leftover, b"x").unwrap();
        let meta = std::fs::metadata(&src).unwrap();
        let plan = Plan {
            version: PLAN_VERSION,
            created: String::new(),
            side: Side::Local,
            tz: None,
            content_presets: BTreeMap::from([("P".to_string(), preset)]),
            items: vec![PlanItem {
                ai_meta: None,
                source: src.clone(),
                source_root: d.join("src"),
                size: meta.len(),
                mtime_ns: mtime_ns(&meta).unwrap(),
                target: "t".into(),
                via: Via::Mask,
                outputs: vec![PlannedOutput {
                    dest: out.join("a.out"),
                    op: OutputOp::Write,
                    steps: vec!["P".into()],
                    trace: vec![],
                }],
                kind: Default::default(),
                captured: None,
            }],
            review: vec![],
            unmatched: vec![],
            warnings: vec![],
            unchanged: vec![],
            retry_of: None,
            kept: Vec::new(),
        };
        let path = d.join("plan.json");
        plan.save(&path).unwrap();
        (path, src, leftover)
    }

    fn refused(preset: ContentPreset, caps: Capabilities, needle: &str) {
        let d = tempfile::tempdir().unwrap();
        let (plan, src, leftover) = setup(d.path(), preset);
        let r = apply_with_caps(&plan, &fake_tools(), &caps, &ApplyOptions::default());
        match r {
            Err(Error::Tool(m)) => assert!(m.contains(needle), "{m}"),
            other => panic!("expected Tool error, got {other:?}"),
        }
        // Refused before any action.
        assert!(src.exists() && leftover.exists());
        assert!(!report_path(&plan).exists());
    }

    #[test]
    fn apply_refuses_without_svtav1() {
        let caps = Capabilities {
            svtav1: false,
            ..all_caps()
        };
        refused(ContentPreset::Video(VideoPreset::high()), caps, "libsvtav1");
    }

    #[test]
    fn apply_refuses_without_opus() {
        let caps = Capabilities {
            opus: false,
            ..all_caps()
        };
        refused(ContentPreset::Audio(AudioPreset::opus(96)), caps, "libopus");
    }

    #[test]
    fn apply_refuses_stabilize_without_vidstab() {
        let caps = Capabilities {
            vidstab: false,
            ..all_caps()
        };
        refused(
            ContentPreset::Video(VideoPreset::action_cam()),
            caps,
            "vidstabdetect",
        );
    }

    #[test]
    fn apply_refuses_png_without_optipng() {
        refused(
            ContentPreset::Png(PngPreset::default()),
            all_caps(),
            "optipng",
        );
    }

    #[test]
    fn capabilities_not_needed_are_not_required() {
        let d = tempfile::tempdir().unwrap();
        let (plan, _, _) = setup(d.path(), ContentPreset::Video(VideoPreset::high()));
        let mut p = Plan::load(&plan).unwrap();
        // Without stabilization vidstab is not needed; opus is never needed here.
        let caps = Capabilities {
            vidstab: false,
            opus: false,
            versions: BTreeMap::from([("ffmpeg".to_string(), "ffmpeg version 8".to_string())]),
            ..all_caps()
        };
        assert_eq!(
            check_capabilities(&p, &caps, false).unwrap(),
            BTreeMap::from([("ffmpeg".to_string(), "ffmpeg version 8".to_string())])
        );
        p.content_presets
            .insert("P".into(), ContentPreset::Metadata(MetadataPreset::full()));
        assert!(check_capabilities(&p, &Capabilities::default(), false).is_ok());
        p.items[0].outputs[0].steps = vec!["nope".into()];
        assert!(matches!(
            check_capabilities(&p, &all_caps(), true),
            Err(Error::Plan(_))
        ));
    }

    fn result(i: usize) -> ItemResult {
        ItemResult {
            source: PathBuf::from(format!("/s/{i}")),
            status: ItemStatus::Missing,
            outputs: vec![],
            error: None,
            elapsed_ms: None,
            target: None,
        }
    }

    #[test]
    fn report_is_refreshed_every_50_items() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("plan.report.json");
        let base = Report {
            run_id: "r".into(),
            plan: d.path().join("plan.json"),
            started: "s".into(),
            finished: String::new(),
            items: vec![],
            tool_versions: BTreeMap::new(),
            tool_warnings: vec![],
            undone: false,
            undone_sources: vec![],
            cancelled: false,
            stopped_by_anomalies: false,
            kept: 0,
        };
        let mut p = Collected::new(base, 120, Some(path.clone()));
        // Out of order, as worker threads finish.
        for i in (0..49).rev() {
            p.record(i, result(i));
        }
        assert!(!path.exists());
        p.record(100, result(100));
        let snap: Report = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(snap.items.len(), 50);
        assert_eq!(snap.items[0].source, PathBuf::from("/s/0"));
        assert_eq!(snap.items[49].source, PathBuf::from("/s/100"));
        assert!(snap.finished.is_empty());
        // Atomic write: no temp file stays behind.
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[test]
    fn report_round_trips() {
        let r = Report {
            run_id: "r".into(),
            plan: "p".into(),
            started: "s".into(),
            finished: "f".into(),
            items: vec![
                ItemResult {
                    status: ItemStatus::Done {
                        trashed: Some(TrashRef::System {
                            original: "/a".into(),
                        }),
                    },
                    ..result(0)
                },
                ItemResult {
                    status: ItemStatus::Failed {
                        trashed: Some(TrashRef::Folder { path: "/t".into() }),
                    },
                    outputs: vec![OutputResult {
                        planned: "/o".into(),
                        actual: Some("/o".into()),
                        error: None,
                        warnings: vec!["w".into()],
                        fell_back: true,
                        size: Some(5),
                        hash: Some("h".into()),
                        renamed_from: None,
                        anomalies: Vec::new(),
                    }],
                    ..result(1)
                },
                ItemResult {
                    status: ItemStatus::Stale,
                    ..result(2)
                },
            ],
            tool_versions: BTreeMap::from([("ffmpeg".into(), "ffmpeg version 8".into())]),
            tool_warnings: vec!["w".into()],
            undone: true,
            undone_sources: vec!["/a".into()],
            cancelled: true,
            stopped_by_anomalies: false,
            kept: 0,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<Report>(&json).unwrap(), r);
        // Reports written before the split have no `tool_versions`.
        let mut v: Value = serde_json::from_str(&json).unwrap();
        v.as_object_mut().unwrap().remove("tool_versions");
        let old: Report = serde_json::from_value(v).unwrap();
        assert!(old.tool_versions.is_empty());
        assert_eq!(old.tool_warnings, ["w"]);
    }

    #[test]
    fn done_status_reads_old_and_new_reports() {
        let read = |json: &str| -> Value {
            let st: ItemStatus = serde_json::from_str(json).unwrap();
            serde_json::to_value(&st).unwrap()
        };
        // A report written before an output could consume its source.
        let old = r#"{"state":"done","trashed":{"kind":"folder","path":"/t/a.jpg"}}"#;
        assert_eq!(
            read(old),
            serde_json::json!({"state": "done", "trashed": {"kind": "folder", "path": "/t/a.jpg"}})
        );
        let old = r#"{"state":"done","trashed":{"kind":"system","original":"/s/a.jpg"}}"#;
        assert_eq!(read(old)["trashed"]["original"], "/s/a.jpg");
        // A renamed source is done without a trash reference.
        for json in [r#"{"state":"done","trashed":null}"#, r#"{"state":"done"}"#] {
            assert_eq!(
                read(json),
                serde_json::json!({"state": "done", "trashed": null})
            );
        }
        // An output result without `renamed_from` is not a rename.
        let o: OutputResult = serde_json::from_str(
            r#"{"planned":"/o","actual":"/o","error":null,"warnings":[],"fell_back":false}"#,
        )
        .unwrap();
        let v = serde_json::to_value(&o).unwrap();
        assert!(v.get("renamed_from").is_none_or(Value::is_null), "{v}");
    }

    fn in_place_plan(d: &Path) -> (PathBuf, PathBuf) {
        let root = d.join("src");
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("a.jpg");
        std::fs::write(&src, b"original").unwrap();
        let meta = std::fs::metadata(&src).unwrap();
        let plan = Plan {
            version: PLAN_VERSION,
            created: String::new(),
            side: Side::Local,
            tz: None,
            content_presets: BTreeMap::new(),
            items: vec![PlanItem {
                ai_meta: None,
                source: src.clone(),
                source_root: root,
                size: meta.len(),
                mtime_ns: mtime_ns(&meta).unwrap(),
                target: "t".into(),
                via: Via::Mask,
                outputs: vec![PlannedOutput {
                    dest: src.clone(),
                    op: OutputOp::Write,
                    steps: vec![],
                    trace: vec![],
                }],
                kind: Default::default(),
                captured: None,
            }],
            review: vec![],
            unmatched: vec![],
            warnings: vec![],
            unchanged: vec![],
            retry_of: None,
            kept: Vec::new(),
        };
        let path = d.join("plan.json");
        plan.save(&path).unwrap();
        (path, src)
    }

    fn folder_opts() -> ApplyOptions {
        ApplyOptions {
            dry_run: false,
            jobs: 1,
            trash: TrashMode::Folder,
            ..Default::default()
        }
    }

    #[test]
    #[cfg_attr(not(unix), ignore = "needs the sh exiftool stand-in")]
    fn in_place_failure_keeps_processed_output() {
        let d = tempfile::tempdir().unwrap();
        let (plan, src) = in_place_plan(d.path());
        fail_place_to(&src);
        // Empty pipe: only the (fake) exiftool read runs.
        let rep = apply_with_caps(&plan, &fake_tools(), &all_caps(), &folder_opts()).unwrap();
        let r = &rep.items[0];
        let trashed = match &r.status {
            ItemStatus::Failed {
                trashed: Some(TrashRef::Folder { path }),
            } => path.clone(),
            other => panic!("{other:?} / {r:?}"),
        };
        assert_eq!(std::fs::read(&trashed).unwrap(), b"original");
        assert!(r.outputs[0].error.is_some());
        let kept = r.outputs[0]
            .actual
            .clone()
            .expect("processed output recorded");
        assert_eq!(std::fs::read(&kept).unwrap(), b"original");
        assert!(
            !kept
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(TMP_PREFIX),
            "{kept:?} would be deleted by the next run"
        );
        let names: Vec<String> = std::fs::read_dir(src.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().all(|n| !n.starts_with(TMP_PREFIX)),
            "{names:?}"
        );
    }

    #[test]
    #[cfg_attr(not(unix), ignore = "needs the sh exiftool stand-in")]
    fn damaged_byte_copy_is_not_placed() {
        let d = tempfile::tempdir().unwrap();
        let (plan_path, src) = in_place_plan(d.path());
        let mut plan = Plan::load(&plan_path).unwrap();
        let out = d.path().join("out/a.jpg");
        plan.items[0].outputs[0].dest = out.clone();
        plan.save(&plan_path).unwrap();
        CORRUPT_STAGED.lock().unwrap().push(out.clone());
        let rep = apply_with_caps(&plan_path, &fake_tools(), &all_caps(), &folder_opts()).unwrap();
        let r = &rep.items[0];
        assert!(
            matches!(r.status, ItemStatus::Failed { trashed: None }),
            "{r:?}"
        );
        let e = r.outputs[0].error.as_deref().unwrap_or_default();
        assert!(e.contains("blake3"), "{e}");
        assert!(r.outputs[0].actual.is_none());
        assert_eq!(std::fs::read(&src).unwrap(), b"original");
        // Nothing placed, nothing staged left behind.
        assert_eq!(std::fs::read_dir(d.path().join("out")).unwrap().count(), 0);
    }

    #[test]
    #[cfg_attr(not(unix), ignore = "needs the sh exiftool stand-in")]
    fn run_interrupted_after_the_trash_can_be_undone() {
        let d = tempfile::tempdir().unwrap();
        let (plan, src) = in_place_plan(d.path());
        let mut p = Plan::load(&plan).unwrap();
        // A second source, finished before the crash.
        let other = d.path().join("src/b.jpg");
        std::fs::write(&other, b"other").unwrap();
        let mut it = p.items[0].clone();
        it.source = other.clone();
        it.size = 5;
        it.mtime_ns = mtime_ns(&std::fs::metadata(&other).unwrap()).unwrap();
        it.outputs[0].dest = d.path().join("out/b.jpg");
        p.items.insert(0, it);
        p.save(&plan).unwrap();
        CRASH_AFTER_TRASH.lock().unwrap().push(src.clone());
        let opts = ApplyOptions {
            jobs: 1,
            ..folder_opts()
        };
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            apply_with_caps(&plan, &fake_tools(), &all_caps(), &opts)
        }));
        assert!(crashed.is_err());
        assert!(
            !src.exists() && !other.exists(),
            "both sources went to the trash"
        );
        // The periodic report knows nothing yet; the journal does.
        let rp = report_path(&plan);
        let on_disk: Report = serde_json::from_str(&std::fs::read_to_string(&rp).unwrap()).unwrap();
        assert!(on_disk.items.is_empty() && on_disk.finished.is_empty());
        assert!(journal_path(&rp).exists());
        assert!(
            RunLock::acquire(&plan).is_ok(),
            "the lock is released with the run"
        );

        let s = crate::undo::undo(&rp).unwrap();
        assert_eq!(s.problems, Vec::<String>::new());
        assert_eq!(s.restored, 2);
        assert_eq!(std::fs::read(&src).unwrap(), b"original");
        assert_eq!(std::fs::read(&other).unwrap(), b"other");
        assert!(!d.path().join("out/b.jpg").exists());
    }

    #[test]
    fn an_undo_takes_the_same_lock_as_an_apply_of_its_plan() {
        let d = tempfile::tempdir().unwrap();
        for name in [
            "20261105-101010.plan.json",
            "plan.json",
            "plan",
            "trip.2026.json",
            "trip.2026",
            "my.report.2026.json",
        ] {
            let plan = d.path().join(name);
            let rp = report_path(&plan);
            let archived = plan.with_file_name(format!(
                "{}.report.20260101-000000_1.json",
                plan_stem(&plan)
            ));
            for report in [&rp, &archived] {
                let held = RunLock::acquire(&plan).unwrap();
                assert!(
                    RunLock::acquire_for_report(report).is_err(),
                    "{name}: {report:?} must wait for the apply"
                );
                drop(held);
                let held = RunLock::acquire_for_report(report).unwrap();
                assert!(RunLock::acquire(&plan).is_err(), "{name}");
                drop(held);
                assert!(RunLock::acquire(&plan).is_ok(), "{name}");
            }
        }
    }

    #[test]
    fn dotted_plan_names_do_not_share_locks_or_reports() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("trip.2026.json");
        let b = d.path().join("trip.2027.json");
        assert_ne!(lock_path(&a), lock_path(&b));
        assert_ne!(report_path(&a), report_path(&b));
        assert_eq!(lock_path(&a), d.path().join("trip.2026.lock"));
        assert_eq!(report_path(&a), d.path().join("trip.2026.report.json"));
        // Extensionless dotted names too.
        let c = d.path().join("trip.2026");
        assert_eq!(lock_path_for_report(&report_path(&c)), lock_path(&c));
        assert_ne!(lock_path(&c), lock_path(&d.path().join("trip.2027")));
    }

    #[test]
    fn a_held_plan_lock_is_refused_and_a_leftover_file_is_not() {
        let d = tempfile::tempdir().unwrap();
        let plan = d.path().join("p.plan.json");
        let lp = lock_path(&plan);
        std::fs::write(&lp, "4194999").unwrap();
        let held = RunLock::acquire(&plan).unwrap();
        assert!(RunLock::acquire(&plan).is_err());
        drop(held);
        assert!(RunLock::acquire(&plan).is_ok());
    }

    #[test]
    fn interrupted_runs_journal_is_archived_with_its_report() {
        let d = tempfile::tempdir().unwrap();
        let plan = d.path().join("plan.json");
        let rp = report_path(&plan);
        std::fs::write(&rp, r#"{"run_id": "20260101-000000"}"#).unwrap();
        std::fs::write(journal_path(&rp), "line\n").unwrap();
        // Taken: the next free suffix is used for both.
        std::fs::write(d.path().join("plan.report.20260101-000000.jsonl"), "x").unwrap();
        archive_report(&rp, &plan).unwrap();
        assert!(!rp.exists() && !journal_path(&rp).exists());
        let kept = d.path().join("plan.report.20260101-000000_1.json");
        assert_eq!(
            std::fs::read_to_string(&kept).unwrap(),
            r#"{"run_id": "20260101-000000"}"#
        );
        assert_eq!(
            std::fs::read_to_string(journal_path(&kept)).unwrap(),
            "line\n"
        );
    }

    #[test]
    #[cfg_attr(not(unix), ignore = "needs the sh exiftool stand-in")]
    fn finished_run_leaves_no_journal() {
        let d = tempfile::tempdir().unwrap();
        let (plan, _) = in_place_plan(d.path());
        let rep = apply_with_caps(&plan, &fake_tools(), &all_caps(), &folder_opts()).unwrap();
        assert!(matches!(rep.items[0].status, ItemStatus::Done { .. }));
        assert!(!journal_path(&report_path(&plan)).exists());
    }

    #[test]
    fn concurrent_run_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let (plan, src) = in_place_plan(d.path());
        let lock = d.path().join("plan.lock");
        // Another apply (or undo) of this plan is running.
        let me = std::process::id().to_string();
        let held = RunLock::acquire(&plan).unwrap();
        let before: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        match apply_with_caps(&plan, &fake_tools(), &all_caps(), &folder_opts()) {
            Err(Error::Plan(m)) => {
                assert!(m.contains("plan.lock"), "{m}");
                // Windows locks are mandatory: the holder's pid is unreadable.
                assert!(!cfg!(unix) || m.contains(&me), "{m}");
            }
            other => panic!("expected a lock error, got {other:?}"),
        }
        let after: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(before, after);
        assert_eq!(std::fs::read(&src).unwrap(), b"original");
        // Windows locks are mandatory: the file cannot be read while held.
        #[cfg(unix)]
        assert_eq!(std::fs::read_to_string(&lock).unwrap(), me);
        #[cfg(not(unix))]
        let _ = &lock;
        // Undo of the same plan waits for it too.
        let rp = report_path(&plan);
        std::fs::write(&rp, "{}").unwrap();
        assert!(matches!(
            crate::undo::undo(&rp),
            Err(Error::Plan(m)) if m.contains("plan.lock") && (!cfg!(unix) || m.contains(&me))
        ));
        std::fs::remove_file(&rp).unwrap();

        // Once it is released, a run takes the lock and releases it at the end.
        drop(held);
        apply_with_caps(&plan, &fake_tools(), &all_caps(), &folder_opts()).unwrap();
        assert!(RunLock::acquire(&plan).is_ok());
    }

    #[test]
    fn place_records_actual_when_check_fails() {
        let d = tempfile::tempdir().unwrap();
        let (plan_path, _) = in_place_plan(d.path());
        let plan = Plan::load(&plan_path).unwrap();
        let tools = fake_tools();
        let opts = folder_opts();
        let run = Run {
            plan: &plan,
            tools: &tools,
            run_id: "r",
            opts: &opts,
            dirs: Mutex::new(HashMap::new()),
            journal: None,
            incompressible: Mutex::new(Vec::new()),
        };
        let staged = d.path().join(".gallery-tmp-r-x");
        std::fs::write(&staged, b"12345").unwrap();
        let s = Staged {
            path: staged,
            wanted: d.path().join("out.jpg"),
            // Wrong on purpose: the post-link check fails.
            len: 99,
            hash: String::new(),
        };
        let (actual, err) = run.place(&s, None);
        assert_eq!(actual.as_deref(), Some(d.path().join("out.jpg").as_path()));
        assert!(err.is_some());
    }

    /// `src/IMG_1.png` ("original") with outputs `(dest, op)`, all byte copies.
    fn rename_plan(d: &Path, outs: Vec<(PathBuf, OutputOp)>) -> (PathBuf, PathBuf) {
        let (path, src) = in_place_plan(d);
        let shot = d.join("src/IMG_1.png");
        std::fs::rename(&src, &shot).unwrap();
        let mut plan = Plan::load(&path).unwrap();
        plan.items[0].source = shot.clone();
        plan.items[0].outputs = outs
            .into_iter()
            .map(|(dest, op)| PlannedOutput {
                dest,
                op,
                steps: vec![],
                trace: vec![],
            })
            .collect();
        plan.save(&path).unwrap();
        (path, shot)
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    #[test]
    fn rename_in_place_moves_nothing_to_the_trash() {
        let d = tempfile::tempdir().unwrap();
        let new = d.path().join("src/20260101_120000.png");
        let (plan, shot) = rename_plan(d.path(), vec![(new.clone(), OutputOp::Rename)]);
        let rep = apply_with_caps(&plan, &fake_tools(), &all_caps(), &folder_opts()).unwrap();
        let r = &rep.items[0];
        assert_eq!(r.status, ItemStatus::Done { trashed: None }, "{r:?}");
        // No copy left behind, no trash folder: only the new name.
        assert_eq!(names_in(&d.path().join("src")), ["20260101_120000.png"]);
        assert_eq!(std::fs::read(&new).unwrap(), b"original");
        let o = &r.outputs[0];
        assert_eq!(o.actual.as_deref(), Some(new.as_path()));
        assert_eq!(o.renamed_from.as_deref(), Some(shot.as_path()));
        assert_eq!(o.size, Some(8));
        assert_eq!(o.hash.as_deref(), Some(file_hash(&new).unwrap().as_str()));
        assert!(o.error.is_none());
    }

    #[test]
    #[cfg_attr(not(unix), ignore = "needs the sh exiftool stand-in")]
    fn rename_waits_for_every_write() {
        let d = tempfile::tempdir().unwrap();
        let new = d.path().join("src/20260101_120000.png");
        let backup = d.path().join("backup/IMG_1.png");
        // The rename comes first in the plan, yet happens last.
        let (plan, shot) = rename_plan(
            d.path(),
            vec![
                (new.clone(), OutputOp::Rename),
                (backup.clone(), OutputOp::Write),
            ],
        );
        let rep = apply_with_caps(&plan, &fake_tools(), &all_caps(), &folder_opts()).unwrap();
        let r = &rep.items[0];
        assert_eq!(r.status, ItemStatus::Done { trashed: None }, "{r:?}");
        assert_eq!(std::fs::read(&backup).unwrap(), b"original");
        assert_eq!(std::fs::read(&new).unwrap(), b"original");
        assert!(!shot.exists());
        assert_eq!(names_in(&d.path().join("src")), ["20260101_120000.png"]);
        assert_eq!(r.outputs[1].actual.as_deref(), Some(backup.as_path()));
        assert!(r.outputs[1].renamed_from.is_none());
    }

    #[test]
    #[cfg_attr(not(unix), ignore = "needs the sh exiftool stand-in")]
    fn failed_write_leaves_the_source_unrenamed() {
        let d = tempfile::tempdir().unwrap();
        let new = d.path().join("src/20260101_120000.png");
        let backup = d.path().join("backup/IMG_1.png");
        let (plan, shot) = rename_plan(
            d.path(),
            vec![
                (new.clone(), OutputOp::Rename),
                (backup.clone(), OutputOp::Write),
            ],
        );
        fail_place_to(&backup);
        let rep = apply_with_caps(&plan, &fake_tools(), &all_caps(), &folder_opts()).unwrap();
        let r = &rep.items[0];
        assert_eq!(r.status, ItemStatus::Failed { trashed: None }, "{r:?}");
        assert_eq!(std::fs::read(&shot).unwrap(), b"original");
        assert_eq!(names_in(&d.path().join("src")), ["IMG_1.png"]);
        assert!(r.outputs[0].actual.is_none() && r.outputs[0].renamed_from.is_none());
        assert!(r.outputs[1].error.is_some());
    }

    #[test]
    fn taken_rename_target_gets_the_next_name() {
        let d = tempfile::tempdir().unwrap();
        let new = d.path().join("src/20260101_120000.png");
        let (plan, shot) = rename_plan(d.path(), vec![(new.clone(), OutputOp::Rename)]);
        // Someone took the planned name after the scan.
        std::fs::write(&new, b"intruder").unwrap();
        let rep = apply_with_caps(&plan, &fake_tools(), &all_caps(), &folder_opts()).unwrap();
        let r = &rep.items[0];
        assert_eq!(r.status, ItemStatus::Done { trashed: None }, "{r:?}");
        let next = d.path().join("src/20260101_120000_1.png");
        assert_eq!(r.outputs[0].actual.as_deref(), Some(next.as_path()));
        assert_eq!(std::fs::read(&new).unwrap(), b"intruder");
        assert_eq!(std::fs::read(&next).unwrap(), b"original");
        assert!(!shot.exists());
    }

    #[test]
    fn stale_source_is_not_renamed() {
        let d = tempfile::tempdir().unwrap();
        let new = d.path().join("src/20260101_120000.png");
        let (plan, shot) = rename_plan(d.path(), vec![(new.clone(), OutputOp::Rename)]);
        std::fs::write(&shot, b"edited after the scan").unwrap();
        let rep = apply_with_caps(&plan, &fake_tools(), &all_caps(), &folder_opts()).unwrap();
        assert_eq!(rep.items[0].status, ItemStatus::Stale);
        assert_eq!(names_in(&d.path().join("src")), ["IMG_1.png"]);
    }

    #[test]
    #[cfg_attr(not(unix), ignore = "needs the sh exiftool stand-in")]
    fn source_changed_after_the_writes_is_not_renamed() {
        let d = tempfile::tempdir().unwrap();
        let new = d.path().join("src/20260101_120000.png");
        let backup = d.path().join("backup/IMG_1.png");
        let (plan, shot) = rename_plan(
            d.path(),
            vec![
                (new.clone(), OutputOp::Rename),
                (backup.clone(), OutputOp::Write),
            ],
        );
        CHANGE_BEFORE_RENAME.lock().unwrap().push(shot.clone());
        let rep = apply_with_caps(&plan, &fake_tools(), &all_caps(), &folder_opts()).unwrap();
        let r = &rep.items[0];
        assert_eq!(r.status, ItemStatus::Failed { trashed: None }, "{r:?}");
        assert!(
            r.error.as_deref().unwrap_or_default().contains("changed"),
            "{r:?}"
        );
        assert_eq!(names_in(&d.path().join("src")), ["IMG_1.png"]);
        assert!(r.outputs[0].actual.is_none());
    }

    #[test]
    #[cfg_attr(not(unix), ignore = "needs the sh exiftool stand-in")]
    fn keep_with_backup_writes_the_backup_only() {
        let d = tempfile::tempdir().unwrap();
        let backup = d.path().join("backup/IMG_1.png");
        let shot = d.path().join("src/IMG_1.png");
        let (plan, _) = rename_plan(
            d.path(),
            vec![
                (shot.clone(), OutputOp::Keep),
                (backup.clone(), OutputOp::Write),
            ],
        );
        let rep = apply_with_caps(&plan, &fake_tools(), &all_caps(), &folder_opts()).unwrap();
        let r = &rep.items[0];
        assert_eq!(r.status, ItemStatus::Done { trashed: None }, "{r:?}");
        assert_eq!(names_in(&d.path().join("src")), ["IMG_1.png"]);
        assert_eq!(std::fs::read(&shot).unwrap(), b"original");
        assert_eq!(std::fs::read(&backup).unwrap(), b"original");
        // Nothing recorded at the kept source: undo must never touch it.
        assert!(r.outputs[0].actual.is_none() && r.outputs[0].renamed_from.is_none());
    }

    #[test]
    #[cfg_attr(not(unix), ignore = "needs the sh exiftool stand-in")]
    fn rename_with_backup_is_undone() {
        let d = tempfile::tempdir().unwrap();
        let new = d.path().join("src/20260101_120000.png");
        let backup = d.path().join("backup/IMG_1.png");
        let (plan, shot) = rename_plan(
            d.path(),
            vec![
                (new.clone(), OutputOp::Rename),
                (backup.clone(), OutputOp::Write),
            ],
        );
        apply_with_caps(&plan, &fake_tools(), &all_caps(), &folder_opts()).unwrap();
        assert!(!shot.exists() && new.exists() && backup.exists());
        let s = crate::undo::undo(&report_path(&plan)).unwrap();
        assert_eq!(s.problems, Vec::<String>::new());
        assert_eq!((s.restored, s.removed_outputs), (1, 1));
        assert_eq!(std::fs::read(&shot).unwrap(), b"original");
        assert_eq!(names_in(&d.path().join("src")), ["IMG_1.png"]);
        assert!(names_in(&d.path().join("backup")).is_empty());
    }

    #[test]
    #[cfg_attr(
        any(target_os = "macos", windows),
        ignore = "case-insensitive file system: the case-only target is the file itself"
    )]
    fn case_only_rename_keeps_the_name() {
        let d = tempfile::tempdir().unwrap();
        let new = d.path().join("src/IMG.jpg");
        let (plan, shot) = rename_plan(d.path(), vec![(new.clone(), OutputOp::Rename)]);
        let upper = d.path().join("src/IMG.JPG");
        std::fs::rename(&shot, &upper).unwrap();
        let mut p = Plan::load(&plan).unwrap();
        p.items[0].source = upper.clone();
        p.save(&plan).unwrap();
        let rep = apply_with_caps(&plan, &fake_tools(), &all_caps(), &folder_opts()).unwrap();
        let r = &rep.items[0];
        assert_eq!(r.status, ItemStatus::Done { trashed: None }, "{r:?}");
        assert_eq!(r.outputs[0].actual.as_deref(), Some(new.as_path()));
        assert_eq!(names_in(&d.path().join("src")), ["IMG.jpg"]);
        assert_eq!(std::fs::read(&new).unwrap(), b"original");
        let s = crate::undo::undo(&report_path(&plan)).unwrap();
        assert_eq!(s.problems, Vec::<String>::new());
        assert_eq!(names_in(&d.path().join("src")), ["IMG.JPG"]);
    }

    #[cfg(unix)]
    fn ino(p: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(p).unwrap().ino()
    }

    #[test]
    fn rename_into_new_nested_subfolder_moves_the_file() {
        let d = tempfile::tempdir().unwrap();
        let new = d.path().join("src/2026/01/20260101_120000.png");
        let (plan, shot) = rename_plan(d.path(), vec![(new.clone(), OutputOp::Rename)]);
        #[cfg(unix)]
        let before = ino(&shot);
        let rep = apply_with_caps(&plan, &fake_tools(), &all_caps(), &folder_opts()).unwrap();
        let r = &rep.items[0];
        assert_eq!(r.status, ItemStatus::Done { trashed: None }, "{r:?}");
        // The folders were made; no copy and no trash folder are left behind.
        assert_eq!(names_in(&d.path().join("src")), ["2026"]);
        assert_eq!(
            names_in(&d.path().join("src/2026/01")),
            ["20260101_120000.png"]
        );
        assert_eq!(std::fs::read(&new).unwrap(), b"original");
        #[cfg(unix)]
        assert_eq!(ino(&new), before, "the same file, not a copy");
        let o = &r.outputs[0];
        assert_eq!(o.actual.as_deref(), Some(new.as_path()));
        assert_eq!(o.renamed_from.as_deref(), Some(shot.as_path()));
        // Undo moves it back; the made folders stay.
        let s = crate::undo::undo(&report_path(&plan)).unwrap();
        assert_eq!(s.problems, Vec::<String>::new());
        assert_eq!(std::fs::read(&shot).unwrap(), b"original");
        assert!(!new.exists());
    }

    #[test]
    fn taken_subfolder_target_gets_the_next_name() {
        let d = tempfile::tempdir().unwrap();
        let new = d.path().join("src/2026/01/20260101_120000.png");
        let (plan, shot) = rename_plan(d.path(), vec![(new.clone(), OutputOp::Rename)]);
        std::fs::create_dir_all(new.parent().unwrap()).unwrap();
        std::fs::write(&new, b"intruder").unwrap();
        let rep = apply_with_caps(&plan, &fake_tools(), &all_caps(), &folder_opts()).unwrap();
        let r = &rep.items[0];
        assert_eq!(r.status, ItemStatus::Done { trashed: None }, "{r:?}");
        let next = d.path().join("src/2026/01/20260101_120000_1.png");
        assert_eq!(r.outputs[0].actual.as_deref(), Some(next.as_path()));
        assert_eq!(std::fs::read(&new).unwrap(), b"intruder");
        assert_eq!(std::fs::read(&next).unwrap(), b"original");
        assert!(!shot.exists());
    }

    #[test]
    fn rename_across_file_systems_fails_and_leaves_the_source() {
        let d = tempfile::tempdir().unwrap();
        let new = d.path().join("src/mnt/20260101_120000.png");
        let (plan, shot) = rename_plan(d.path(), vec![(new.clone(), OutputOp::Rename)]);
        // As if `src/mnt` were another file system mounted inside the source.
        LINK_EXDEV.lock().unwrap().push(shot.clone());
        let rep = apply_with_caps(&plan, &fake_tools(), &all_caps(), &folder_opts()).unwrap();
        let r = &rep.items[0];
        assert_eq!(r.status, ItemStatus::Failed { trashed: None }, "{r:?}");
        let err = r.outputs[0].error.as_deref().unwrap_or_default();
        assert!(err.contains("another file system"), "{r:?}");
        assert_eq!(std::fs::read(&shot).unwrap(), b"original");
        assert!(!new.exists());
        assert!(r.outputs[0].actual.is_none() && r.outputs[0].renamed_from.is_none());
        assert!(!d.path().join("src/.gallery-trash").exists());
    }

    #[test]
    fn rename_outside_the_source_root_is_refused() {
        let d = tempfile::tempdir().unwrap();
        for new in [
            d.path().join("elsewhere/20260101_120000.png"),
            d.path().join("src/../elsewhere/20260101_120000.png"),
        ] {
            let (plan, shot) = rename_plan(d.path(), vec![(new.clone(), OutputOp::Rename)]);
            let rep = apply_with_caps(&plan, &fake_tools(), &all_caps(), &folder_opts()).unwrap();
            let r = &rep.items[0];
            assert_eq!(r.status, ItemStatus::Failed { trashed: None }, "{r:?}");
            let err = r.outputs[0].error.as_deref().unwrap_or_default();
            assert!(err.contains("outside the source root"), "{r:?}");
            assert_eq!(std::fs::read(&shot).unwrap(), b"original");
            assert!(!d.path().join("elsewhere").exists());
            // Back to `a.jpg` for the next round.
            std::fs::rename(&shot, d.path().join("src/a.jpg")).unwrap();
            let _ = std::fs::remove_file(report_path(&plan));
        }
    }

    #[test]
    fn rename_fallback_never_overwrites_a_racing_target() {
        // Without hard links the fallback renames; a file that appears at
        // the target after the existence check must survive.
        let d = tempfile::tempdir().unwrap();
        for (k, f) in [
            rename_noreplace as fn(&Path, &Path) -> std::io::Result<()>,
            link_noreplace,
        ]
        .into_iter()
        .enumerate()
        {
            let from = d.path().join(format!("from{k}"));
            let to = d.path().join(format!("to{k}"));
            std::fs::write(&from, b"mine").unwrap();
            LINK_UNSUPPORTED.lock().unwrap().push(from.clone());
            RACE_BEFORE_RENAME.lock().unwrap().push(to.clone());
            let e = f(&from, &to).unwrap_err();
            assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists, "{e}");
            assert_eq!(std::fs::read(&to).unwrap(), b"racer");
            assert_eq!(std::fs::read(&from).unwrap(), b"mine");
        }
    }

    #[test]
    fn link_verdict_needs_one_more_link_or_both_names_listed() {
        let never = || -> std::io::Result<bool> { panic!("no listing needed") };
        let kind = |r: std::io::Result<()>| r.map_err(|e| e.kind());
        use std::io::ErrorKind::AlreadyExists;
        assert_eq!(kind(link_verdict(Some(1), Some(2), never)), Ok(()));
        assert_eq!(kind(link_verdict(Some(3), Some(4), never)), Ok(()));
        // The source had other links already and the count did not move: the
        // "new" name may be the old one in another case.
        assert_eq!(
            kind(link_verdict(Some(2), Some(2), || Ok(false))),
            Err(AlreadyExists)
        );
        assert_eq!(
            kind(link_verdict(Some(1), Some(1), || Ok(false))),
            Err(AlreadyExists)
        );
        // Another link raced in (+2), but the listing shows both names.
        assert_eq!(kind(link_verdict(Some(1), Some(3), || Ok(true))), Ok(()));
        // No counts (not unix): the listing decides.
        assert_eq!(
            kind(link_verdict(None, None, || Ok(false))),
            Err(AlreadyExists)
        );
        assert_eq!(kind(link_verdict(None, None, || Ok(true))), Ok(()));
        // Unverifiable: an error, but not one that makes the caller retry.
        let e = link_verdict(None, Some(2), || Err(std::io::Error::other("no list"))).unwrap_err();
        assert_ne!(e.kind(), AlreadyExists);
    }

    #[test]
    fn rename_noreplace_keeps_an_existing_target() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let e = rename_noreplace(&a, &b).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&a).unwrap(), b"a");
        assert_eq!(std::fs::read(&b).unwrap(), b"b");
        let c = d.path().join("c");
        rename_noreplace(&a, &c).unwrap();
        assert!(!a.exists());
        assert_eq!(std::fs::read(&c).unwrap(), b"a");
    }

    #[cfg(unix)]
    #[test]
    fn copy_mode_falls_back_to_at_most_0644() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let to = d.path().join(".gallery-tmp-x");
        std::fs::write(&to, b"x").unwrap();
        std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o600)).unwrap();
        let f = std::fs::File::options().write(true).open(&to).unwrap();
        copy_mode(&d.path().join("gone.jpg"), &f, &to).unwrap();
        let m = std::fs::metadata(&to).unwrap().permissions().mode() & 0o777;
        assert_eq!(m & !0o644, 0, "{m:o}");
        assert_ne!(m & 0o400, 0, "{m:o}");
        assert_eq!(names_in(d.path()), [".gallery-tmp-x"]);
    }

    #[test]
    fn link_noreplace_refuses_existing() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let e = link_noreplace(&a, &b).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&b).unwrap(), b"b");
    }
}

/// A stand-in exiftool for tests: answers every file of the `-@` argfile
/// with an entry holding only its `SourceFile`.
#[cfg(all(test, unix))]
pub(crate) fn fake_exiftool() -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    static PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap().keep();
        let p = dir.join("exiftool");
        std::fs::write(
            &p,
            "#!/bin/sh\nfor a; do f=$a; done\nprintf '['; sep=''\n\
             while IFS= read -r l; do printf '%s{\"SourceFile\":\"%s\"}' \"$sep\" \"$l\"; sep=','; done < \"$f\"\n\
             printf ']\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    })
    .clone()
}

#[cfg(all(test, unix))]
mod cancel_tests {
    use super::*;
    use crate::plan::{PLAN_VERSION, Side};
    use crate::progress::testing::Recorder;
    use crate::progress::{Cancel, NoProgress, ProgressEvent, Stage};
    use crate::route::Via;

    fn fake_tools() -> Tools {
        let p = PathBuf::from("/nonexistent/tool");
        Tools {
            ffmpeg: p.clone(),
            ffprobe: p.clone(),
            exiftool: fake_exiftool(),
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

    /// `n` sources `src/f<i>.jpg`, each byte-copied (empty pipe) to
    /// `out/f<i>.jpg`, the source then trashed.
    fn copy_plan(d: &Path, n: usize) -> (PathBuf, Vec<PathBuf>) {
        let root = d.join("src");
        std::fs::create_dir_all(&root).unwrap();
        let mut items = Vec::new();
        let mut sources = Vec::new();
        for i in 0..n {
            let src = root.join(format!("f{i}.jpg"));
            std::fs::write(&src, format!("body {i}")).unwrap();
            let meta = std::fs::metadata(&src).unwrap();
            items.push(PlanItem {
                ai_meta: None,
                source: src.clone(),
                source_root: root.clone(),
                size: meta.len(),
                mtime_ns: mtime_ns(&meta).unwrap(),
                target: "t".into(),
                via: Via::Mask,
                outputs: vec![PlannedOutput {
                    dest: d.join(format!("out/f{i}.jpg")),
                    op: OutputOp::Write,
                    steps: vec![],
                    trace: vec![],
                }],
                kind: Default::default(),
                captured: None,
            });
            sources.push(src);
        }
        let plan = Plan {
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
            retry_of: None,
            kept: Vec::new(),
        };
        let path = d.join("plan.json");
        plan.save(&path).unwrap();
        (path, sources)
    }

    fn on_disk(plan: &Path) -> Report {
        serde_json::from_str(&std::fs::read_to_string(report_path(plan)).unwrap()).unwrap()
    }

    #[test]
    fn apply_reports_one_event_per_item() {
        let d = tempfile::tempdir().unwrap();
        let (plan, sources) = copy_plan(d.path(), 3);
        let rec = Recorder::default();
        let rep = apply_impl(
            &plan,
            &fake_tools(),
            &caps(),
            &opts(),
            &rec,
            &Cancel::new(),
            &|_: &ItemResult| {},
        )
        .unwrap();
        assert!(!rep.cancelled);
        assert_eq!(rep.items.len(), 3);
        let evs = rec.events(Stage::Apply);
        assert_eq!(evs.len(), 3, "{evs:?}");
        let mut dones: Vec<usize> = evs.iter().map(|e| e.done).collect();
        dones.sort();
        assert_eq!(dones, [1, 2, 3]);
        assert!(evs.iter().all(|e| e.total == 3));
        let mut cur: Vec<PathBuf> = evs.iter().filter_map(|e| e.current.clone()).collect();
        cur.sort();
        assert_eq!(cur, sources);
    }

    #[test]
    fn apply_tells_each_item_result_before_its_event() {
        let d = tempfile::tempdir().unwrap();
        let (plan, sources) = copy_plan(d.path(), 3);
        // The second source changed after planning: Stale.
        std::fs::write(&sources[1], "changed body").unwrap();
        let seen = Mutex::new(Vec::new());
        let on_item = |r: &ItemResult| seen.lock().unwrap().push(r.clone());
        let events = Mutex::new(0usize);
        let progress = |_: ProgressEvent| {
            let n = seen.lock().unwrap().len();
            let mut e = events.lock().unwrap();
            *e += 1;
            // The item of this event was already told.
            assert!(n >= *e);
        };
        let rep = apply_impl(
            &plan,
            &fake_tools(),
            &caps(),
            &opts(),
            &progress,
            &Cancel::new(),
            &on_item,
        )
        .unwrap();
        let mut seen = seen.into_inner().unwrap();
        seen.sort_by(|a, b| a.source.cmp(&b.source));
        assert_eq!(seen, rep.items);
        assert!(matches!(seen[1].status, ItemStatus::Stale));
        assert_eq!(*events.lock().unwrap(), 3);
    }

    #[test]
    fn cancel_before_start_touches_nothing() {
        let d = tempfile::tempdir().unwrap();
        let (plan, sources) = copy_plan(d.path(), 3);
        let cancel = Cancel::new();
        cancel.cancel();
        let rep = apply_impl(
            &plan,
            &fake_tools(),
            &caps(),
            &opts(),
            &NoProgress,
            &cancel,
            &|_: &ItemResult| {},
        )
        .unwrap();
        assert!(rep.cancelled);
        assert!(rep.items.is_empty(), "{:?}", rep.items);
        assert!(!rep.finished.is_empty());
        for (i, s) in sources.iter().enumerate() {
            assert_eq!(std::fs::read_to_string(s).unwrap(), format!("body {i}"));
        }
        assert!(!d.path().join("out").exists());
        // The report on disk is valid and final; no journal or lock is left.
        let r = on_disk(&plan);
        assert!(r.cancelled && r.items.is_empty() && !r.finished.is_empty());
        assert!(!journal_path(&report_path(&plan)).exists());
        assert!(RunLock::acquire(&plan).is_ok());
        // And its undo is a no-op without problems.
        let s = crate::undo::undo(&report_path(&plan)).unwrap();
        assert_eq!(s.problems, Vec::<String>::new());
        assert_eq!((s.restored, s.removed_outputs), (0, 0));
    }

    #[test]
    fn cancel_mid_apply_stops_between_items_and_can_be_undone() {
        let d = tempfile::tempdir().unwrap();
        let (plan, sources) = copy_plan(d.path(), 6);
        let cancel = Cancel::new();
        let c = cancel.clone();
        // Cancel once the second item is finished.
        let progress = move |ev: ProgressEvent| {
            if ev.done >= 2 {
                c.cancel();
            }
        };
        let rep = apply_impl(
            &plan,
            &fake_tools(),
            &caps(),
            &opts(),
            &progress,
            &cancel,
            &|_: &ItemResult| {},
        )
        .unwrap();
        assert!(rep.cancelled);
        assert_eq!(rep.items.len(), 2, "{:?}", rep.items);
        assert!(
            rep.items
                .iter()
                .all(|it| matches!(it.status, ItemStatus::Done { .. })),
            "{:?}",
            rep.items
        );
        let processed: HashSet<&PathBuf> = rep.items.iter().map(|it| &it.source).collect();
        for (i, s) in sources.iter().enumerate() {
            let out = d.path().join(format!("out/f{i}.jpg"));
            if processed.contains(s) {
                assert!(!s.exists(), "{s:?} trashed");
                assert_eq!(std::fs::read_to_string(&out).unwrap(), format!("body {i}"));
            } else {
                assert_eq!(std::fs::read_to_string(s).unwrap(), format!("body {i}"));
                assert!(!out.exists(), "{out:?}");
            }
        }
        let r = on_disk(&plan);
        assert_eq!(r, rep);
        assert!(!journal_path(&report_path(&plan)).exists());

        let s = crate::undo::undo(&report_path(&plan)).unwrap();
        assert_eq!(s.problems, Vec::<String>::new());
        assert_eq!((s.restored, s.removed_outputs), (2, 2));
        for (i, s) in sources.iter().enumerate() {
            assert_eq!(std::fs::read_to_string(s).unwrap(), format!("body {i}"));
        }
        assert_eq!(std::fs::read_dir(d.path().join("out")).unwrap().count(), 0);
    }

    #[test]
    fn cancelled_dry_run_lists_only_checked_items() {
        let d = tempfile::tempdir().unwrap();
        let (plan, _) = copy_plan(d.path(), 3);
        let cancel = Cancel::new();
        cancel.cancel();
        let o = ApplyOptions {
            dry_run: true,
            ..opts()
        };
        let rep = apply_impl(
            &plan,
            &fake_tools(),
            &caps(),
            &o,
            &NoProgress,
            &cancel,
            &|_: &ItemResult| {},
        )
        .unwrap();
        assert!(rep.cancelled && rep.items.is_empty());
    }
}

#[cfg(all(test, unix))]
mod exif_failure_tests {
    use super::*;
    use crate::plan::{PLAN_VERSION, Side};
    use crate::route::Via;
    use std::os::unix::fs::PermissionsExt;

    fn tools_with_exiftool(exiftool: PathBuf) -> Tools {
        let p = PathBuf::from("/nonexistent/tool");
        Tools {
            ffmpeg: p.clone(),
            ffprobe: p.clone(),
            exiftool,
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

    /// One source byte-copied to `out/a.jpg`, then trashed.
    fn copy_plan(d: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let root = d.join("src");
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("a.jpg");
        std::fs::write(&src, b"original").unwrap();
        let meta = std::fs::metadata(&src).unwrap();
        let out = d.join("out/a.jpg");
        let plan = Plan {
            version: PLAN_VERSION,
            created: String::new(),
            side: Side::Local,
            tz: None,
            content_presets: BTreeMap::new(),
            items: vec![PlanItem {
                ai_meta: None,
                source: src.clone(),
                source_root: root,
                size: meta.len(),
                mtime_ns: mtime_ns(&meta).unwrap(),
                target: "t".into(),
                via: Via::Mask,
                outputs: vec![PlannedOutput {
                    dest: out.clone(),
                    op: OutputOp::Write,
                    steps: vec![],
                    trace: vec![],
                }],
                kind: Default::default(),
                captured: None,
            }],
            review: vec![],
            unmatched: vec![],
            warnings: vec![],
            unchanged: vec![],
            retry_of: None,
            kept: Vec::new(),
        };
        let path = d.join("plan.json");
        plan.save(&path).unwrap();
        (path, src, out)
    }

    fn assert_failed_untouched(exiftool: PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let (plan, src, out) = copy_plan(d.path());
        let opts = ApplyOptions {
            dry_run: false,
            jobs: 1,
            trash: TrashMode::Folder,
            ..Default::default()
        };
        let rep = apply_with_caps(&plan, &tools_with_exiftool(exiftool), &caps(), &opts).unwrap();
        let r = &rep.items[0];
        assert!(
            matches!(r.status, ItemStatus::Failed { trashed: None }),
            "{r:?}"
        );
        let e = r.error.as_deref().unwrap_or_default();
        assert!(e.contains("EXIF"), "{e}");
        assert_eq!(std::fs::read(&src).unwrap(), b"original");
        assert!(!out.exists());
    }

    #[test]
    fn missing_exiftool_fails_the_item_and_keeps_the_source() {
        assert_failed_untouched(PathBuf::from("/nonexistent/exiftool"));
    }

    #[test]
    fn broken_exiftool_output_fails_the_item_and_keeps_the_source() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("exiftool");
        std::fs::write(&p, "#!/bin/sh\necho boom\nexit 2\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_failed_untouched(p);
    }

    #[test]
    fn exiftool_without_an_entry_for_the_source_fails_the_item() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("exiftool");
        std::fs::write(&p, "#!/bin/sh\necho '[]'\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_failed_untouched(p);
    }
}

#[cfg(all(test, unix))]
mod mtime_tests {
    use super::*;
    use crate::config::{MetadataPreset, PhotoPreset};
    use crate::plan::{PLAN_VERSION, Side};
    use crate::route::Via;
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, SystemTime};

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// avifenc writes 1 byte, magick succeeds, ffprobe says "AV1 video".
    fn tools(d: &Path) -> Tools {
        let bin = d.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let probe = r#"echo '{"streams":[{"codec_type":"video","codec_name":"av1","width":64,"height":64}],"format":{"duration":"1"}}'"#;
        Tools {
            ffmpeg: PathBuf::from("/nonexistent/ffmpeg"),
            ffprobe: script(&bin, "ffprobe", probe),
            exiftool: fake_exiftool(),
            avifenc: script(&bin, "avifenc", "for a; do o=$a; done; printf x > \"$o\""),
            magick: script(&bin, "magick", "exit 0"),
            optipng: None,
            tz: Some("UTC".into()),
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

    fn old() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000)
    }

    fn mtime_of(p: &Path) -> SystemTime {
        std::fs::metadata(p).unwrap().modified().unwrap()
    }

    /// Apply one output `out_name` of source `src_name` (100 bytes, mtime
    /// 2001) through `steps`; returns (source mtime, output path).
    fn run(
        src_name: &str,
        out_name: &str,
        op: OutputOp,
        steps: Vec<ContentPreset>,
    ) -> (SystemTime, PathBuf, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("src");
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join(src_name);
        std::fs::write(&src, [7u8; 100]).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&src)
            .unwrap()
            .set_modified(old())
            .unwrap();
        let meta = std::fs::metadata(&src).unwrap();
        let names: Vec<String> = (0..steps.len()).map(|i| format!("S{i}")).collect();
        let out = match op {
            OutputOp::Write => d.path().join("out").join(out_name),
            _ => root.join(out_name),
        };
        let plan = Plan {
            version: PLAN_VERSION,
            created: String::new(),
            side: Side::Local,
            tz: None,
            content_presets: names.iter().cloned().zip(steps).collect(),
            items: vec![PlanItem {
                ai_meta: None,
                source: src.clone(),
                source_root: root,
                size: meta.len(),
                mtime_ns: mtime_ns(&meta).unwrap(),
                target: "t".into(),
                via: Via::Mask,
                outputs: vec![PlannedOutput {
                    dest: out.clone(),
                    op,
                    steps: names,
                    trace: vec![],
                }],
                kind: Default::default(),
                captured: None,
            }],
            review: vec![],
            unmatched: vec![],
            warnings: vec![],
            unchanged: vec![],
            retry_of: None,
            kept: Vec::new(),
        };
        let path = d.path().join("plan.json");
        plan.save(&path).unwrap();
        let opts = ApplyOptions {
            dry_run: false,
            jobs: 1,
            trash: TrashMode::Folder,
            // The fake tools cannot show the written dates: only mtimes count here.
            allow_anomalies: true,
            ..Default::default()
        };
        let rep = apply_with_caps(&path, &tools(d.path()), &caps(), &opts).unwrap();
        let r = &rep.items[0];
        assert!(!matches!(r.status, ItemStatus::Failed { .. }), "{r:?}");
        let placed = r.outputs[0].actual.clone().expect("output placed");
        (old(), placed, d)
    }

    fn photo(p: PhotoPreset) -> ContentPreset {
        ContentPreset::Photo(p)
    }

    #[test]
    fn encoded_avif_gets_the_source_mtime() {
        let (want, out, _d) = run(
            "a.jpg",
            "a.avif",
            OutputOp::Write,
            vec![photo(PhotoPreset::avif_high())],
        );
        assert_eq!(std::fs::metadata(&out).unwrap().len(), 1, "encoded");
        assert_eq!(mtime_of(&out), want);
    }

    #[test]
    fn png_avif_gets_the_source_mtime() {
        let (want, out, _d) = run(
            "a.png",
            "a.avif",
            OutputOp::Write,
            vec![photo(PhotoPreset::png_avif())],
        );
        assert_eq!(mtime_of(&out), want);
    }

    #[test]
    fn av1_fallback_copy_gets_the_source_mtime() {
        let steps = vec![ContentPreset::Video(crate::config::VideoPreset::high())];
        let (want, out, _d) = run("a.mp4", "a.av1.mp4", OutputOp::Write, steps);
        assert_eq!(std::fs::metadata(&out).unwrap().len(), 100, "fell back");
        assert_eq!(mtime_of(&out), want);
    }

    #[test]
    fn byte_copy_keeps_the_source_mtime() {
        let (want, out, _d) = run("a.jpg", "a.jpg", OutputOp::Write, vec![]);
        assert_eq!(mtime_of(&out), want);
    }

    #[test]
    fn capture_date_from_metadata_step_wins() {
        let steps = vec![ContentPreset::Metadata(MetadataPreset::full())];
        let (_, out, _d) = run("IMG_20200102_030405.jpg", "a.jpg", OutputOp::Write, steps);
        let want = SystemTime::UNIX_EPOCH + Duration::from_secs(1_577_934_245); // 2020-01-02 03:04:05 UTC
        assert_eq!(mtime_of(&out), want);
    }

    #[test]
    fn metadata_without_capture_date_keeps_the_source_mtime() {
        let steps = vec![ContentPreset::Metadata(MetadataPreset::full())];
        let (want, out, _d) = run("a.jpg", "a.jpg", OutputOp::Write, steps);
        assert_eq!(mtime_of(&out), want);
    }

    #[test]
    fn rename_keeps_the_source_mtime() {
        let (want, out, _d) = run("a.jpg", "b.jpg", OutputOp::Rename, vec![]);
        assert_eq!(mtime_of(&out), want);
    }

    const CAPTURE: u64 = 1_577_934_245; // 2020-01-02 03:04:05 UTC

    fn meta_full() -> ContentPreset {
        ContentPreset::Metadata(MetadataPreset::full())
    }

    fn avif() -> ContentPreset {
        photo(PhotoPreset::avif_high())
    }

    fn capture_time() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(CAPTURE)
    }

    #[test]
    fn metadata_then_encoder_gets_the_capture_date() {
        let (_, out, _d) = run(
            "IMG_20200102_030405.jpg",
            "a.avif",
            OutputOp::Write,
            vec![meta_full(), avif()],
        );
        assert_eq!(std::fs::metadata(&out).unwrap().len(), 1, "encoded");
        assert_eq!(mtime_of(&out), capture_time());
    }

    #[test]
    fn encoder_then_metadata_gets_the_capture_date() {
        let (_, out, _d) = run(
            "IMG_20200102_030405.jpg",
            "a.avif",
            OutputOp::Write,
            vec![avif(), meta_full()],
        );
        assert_eq!(mtime_of(&out), capture_time());
    }

    #[test]
    fn encoder_alone_keeps_the_source_mtime_despite_capture_name() {
        let (want, out, _d) = run(
            "IMG_20200102_030405.jpg",
            "a.avif",
            OutputOp::Write,
            vec![avif()],
        );
        assert_eq!(mtime_of(&out), want);
    }
}

/// Verification of outputs against their source, with real tools: a damaged
/// output fails its item before placement and the source stays.
#[cfg(all(test, unix))]
mod verification {
    use super::*;
    use crate::config::MetadataPreset;
    use crate::plan::{PLAN_VERSION, Side};
    use crate::route::Via;
    use std::collections::BTreeMap;
    use std::process::Command;

    fn have_tools() -> bool {
        ["ffmpeg", "ffprobe", "exiftool", "magick"]
            .iter()
            .all(|t| crate::tools::find(t).is_some())
    }

    macro_rules! need_tools {
        () => {
            if !have_tools() {
                eprintln!("skip: tools not found");
                return;
            }
        };
    }

    fn tool(name: &str, args: &[&str], file: &Path) {
        let st = Command::new(name).args(args).arg(file).status().unwrap();
        assert!(st.success(), "{name} {args:?}");
    }

    fn jpeg(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        let st = Command::new("magick")
            .args(["-size", "64x48", "gradient:red-blue"])
            .arg(&p)
            .status()
            .unwrap();
        assert!(st.success());
        tool(
            "exiftool",
            &[
                "-overwrite_original",
                "-q",
                "-DateTimeOriginal=2026:04:12 10:33:14",
                "-GPSLatitude=10",
                "-GPSLatitudeRef=N",
                "-GPSLongitude=20",
                "-GPSLongitudeRef=E",
            ],
            &p,
        );
        p
    }

    fn mp4(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        let st = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", "testsrc2=size=160x120:rate=10"])
            .args(["-t", "3", "-c:v", "libx264", "-preset", "ultrafast"])
            .args(["-pix_fmt", "yuv420p"])
            .arg(&p)
            .status()
            .unwrap();
        assert!(st.success());
        p
    }

    fn tools() -> Tools {
        Tools::discover().unwrap()
    }

    /// Plan of `sources`, each written to `out/<name>` through the metadata
    /// preset. Returns the plan path and the output paths.
    fn plan_for(d: &Path, sources: &[PathBuf]) -> (PathBuf, Vec<PathBuf>) {
        plan_steps(d, sources, &["Metadata"])
    }

    /// Like `plan_for`, with the given pipe (`Metadata`, `AVIF`).
    fn plan_steps(d: &Path, sources: &[PathBuf], steps: &[&str]) -> (PathBuf, Vec<PathBuf>) {
        let root = d.join("src");
        let outs: Vec<PathBuf> = sources
            .iter()
            .map(|s| d.join("out").join(s.file_name().unwrap()))
            .collect();
        let items = sources
            .iter()
            .zip(&outs)
            .map(|(s, o)| {
                let meta = std::fs::metadata(s).unwrap();
                PlanItem {
                    ai_meta: None,
                    source: s.clone(),
                    source_root: root.clone(),
                    size: meta.len(),
                    mtime_ns: mtime_ns(&meta).unwrap(),
                    target: "t".into(),
                    via: Via::Mask,
                    outputs: vec![PlannedOutput {
                        dest: o.clone(),
                        op: OutputOp::Write,
                        steps: steps.iter().map(|s| s.to_string()).collect(),
                        trace: vec![],
                    }],
                    kind: Default::default(),
                    captured: None,
                }
            })
            .collect();
        let mut presets = BTreeMap::new();
        presets.insert(
            "Metadata".to_string(),
            ContentPreset::Metadata(MetadataPreset::full()),
        );
        presets.insert(
            "AVIF".to_string(),
            ContentPreset::Photo(crate::config::PhotoPreset::avif_fast()),
        );
        let plan = Plan {
            version: PLAN_VERSION,
            created: String::new(),
            side: Side::Local,
            tz: None,
            content_presets: presets,
            items,
            review: vec![],
            unmatched: vec![],
            warnings: vec![],
            unchanged: vec![],
            retry_of: None,
            kept: Vec::new(),
        };
        let path = d.join("plan.json");
        plan.save(&path).unwrap();
        (path, outs)
    }

    fn opts() -> ApplyOptions {
        ApplyOptions {
            jobs: 1,
            trash: TrashMode::Folder,
            ..Default::default()
        }
    }

    fn setup(d: &Path, n: usize) -> Vec<PathBuf> {
        let src = d.join("src");
        std::fs::create_dir_all(&src).unwrap();
        (0..n).map(|i| jpeg(&src, &format!("{i}.jpg"))).collect()
    }

    fn tamper_date(p: &Path) {
        tool(
            "exiftool",
            &["-overwrite_original", "-q", "-P", "-AllDates="],
            p,
        );
    }

    fn tamper_gps(p: &Path) {
        tool(
            "exiftool",
            &["-overwrite_original", "-q", "-P", "-gps:all="],
            p,
        );
    }

    fn tamper_mtime(p: &Path) {
        std::fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(std::time::SystemTime::now())
            .unwrap();
    }

    fn tamper_duration(p: &Path) {
        let m = std::fs::metadata(p).unwrap().modified().unwrap();
        let cut = p.with_extension("cut.mp4");
        let st = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(p)
            .args(["-t", "1", "-c", "copy", "-f", "mp4"])
            .arg(&cut)
            .status()
            .unwrap();
        assert!(st.success());
        std::fs::rename(&cut, p).unwrap();
        std::fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(m)
            .unwrap();
    }

    fn assert_failed_and_kept(rep: &Report, src: &Path, out: &Path, original: &[u8]) {
        let r = rep.items.iter().find(|r| r.source == src).unwrap();
        assert!(
            matches!(r.status, ItemStatus::Failed { trashed: None }),
            "{r:?}"
        );
        let o = &r.outputs[0];
        assert!(!o.anomalies.is_empty(), "{o:?}");
        let e = o.error.as_deref().unwrap();
        assert!(e.contains("source untouched"), "{e}");
        assert!(o.actual.is_none());
        assert_eq!(std::fs::read(src).unwrap(), original);
        assert!(!out.exists());
        // Nothing staged is left behind either.
        let left = std::fs::read_dir(out.parent().unwrap()).map_or(0, Iterator::count);
        assert_eq!(left, 0);
    }

    fn damaged(tamper: Tamper, needle: &str) {
        need_tools!();
        let d = tempfile::tempdir().unwrap();
        let srcs = setup(d.path(), 1);
        let original = std::fs::read(&srcs[0]).unwrap();
        let (plan, outs) = plan_for(d.path(), &srcs);
        TAMPER_STAGED
            .lock()
            .unwrap()
            .push((outs[0].clone(), tamper));
        let rep = apply(&plan, &tools(), &opts()).unwrap();
        assert_failed_and_kept(&rep, &srcs[0], &outs[0], &original);
        let a = rep.items[0].outputs[0].anomalies.join("|");
        assert!(a.contains(needle), "{a}");
        assert!(!rep.stopped_by_anomalies);
    }

    #[test]
    fn clean_run_on_real_tools_has_no_anomalies() {
        need_tools!();
        let d = tempfile::tempdir().unwrap();
        let srcs = setup(d.path(), 2);
        let (plan, outs) = plan_for(d.path(), &srcs);
        let rep = apply(&plan, &tools(), &opts()).unwrap();
        for (r, o) in rep.items.iter().zip(&outs) {
            assert!(matches!(r.status, ItemStatus::Done { .. }), "{r:?}");
            assert!(r.outputs[0].anomalies.is_empty());
            assert!(o.exists());
        }
        assert!(!rep.stopped_by_anomalies && !rep.cancelled);
    }

    #[test]
    fn lost_capture_date_fails_the_item() {
        damaged(tamper_date, "capture date");
    }

    #[test]
    fn wrong_mtime_fails_the_item() {
        damaged(tamper_mtime, "mtime");
    }

    #[test]
    fn lost_gps_fails_the_item() {
        damaged(tamper_gps, "GPS");
    }

    #[test]
    fn video_duration_mismatch_fails_the_item() {
        need_tools!();
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let srcs = vec![mp4(&src, "v.mp4")];
        let original = std::fs::read(&srcs[0]).unwrap();
        let (plan, outs) = plan_for(d.path(), &srcs);
        TAMPER_STAGED
            .lock()
            .unwrap()
            .push((outs[0].clone(), tamper_duration));
        let rep = apply(&plan, &tools(), &opts()).unwrap();
        assert_failed_and_kept(&rep, &srcs[0], &outs[0], &original);
        let a = rep.items[0].outputs[0].anomalies.join("|");
        assert!(a.contains("duration"), "{a}");
    }

    #[test]
    fn max_anomalies_stops_the_run_like_a_cancel() {
        need_tools!();
        let d = tempfile::tempdir().unwrap();
        let srcs = setup(d.path(), 5);
        let (plan, outs) = plan_for(d.path(), &srcs);
        for o in &outs {
            TAMPER_STAGED
                .lock()
                .unwrap()
                .push((o.clone(), tamper_mtime));
        }
        let o = ApplyOptions {
            max_anomalies: 2,
            ..opts()
        };
        let rep = apply(&plan, &tools(), &o).unwrap();
        assert_eq!(rep.items.len(), 2, "{:?}", rep.items);
        assert!(rep.stopped_by_anomalies && rep.cancelled);
        assert!(srcs.iter().all(|s| s.exists()));
        assert!(outs.iter().all(|o| !o.exists()));
        // The report on disk says so too.
        let disk: Report =
            serde_json::from_str(&std::fs::read_to_string(report_path(&plan)).unwrap()).unwrap();
        assert!(disk.stopped_by_anomalies);
    }

    #[test]
    fn allow_anomalies_places_the_output_and_records_them() {
        need_tools!();
        let d = tempfile::tempdir().unwrap();
        let srcs = setup(d.path(), 1);
        let (plan, outs) = plan_for(d.path(), &srcs);
        TAMPER_STAGED
            .lock()
            .unwrap()
            .push((outs[0].clone(), tamper_gps));
        let o = ApplyOptions {
            allow_anomalies: true,
            ..opts()
        };
        let rep = apply(&plan, &tools(), &o).unwrap();
        let r = &rep.items[0];
        assert!(matches!(r.status, ItemStatus::Done { .. }), "{r:?}");
        assert!(outs[0].exists());
        assert!(!r.outputs[0].anomalies.is_empty());
        assert!(!rep.stopped_by_anomalies);
    }

    #[test]
    fn old_reports_load_without_anomaly_fields() {
        let rep = r#"{"run_id":"r","plan":"p.json","started":"s","finished":"f",
            "items":[{"source":"/a","status":{"state":"failed"},
            "outputs":[{"planned":"/o","actual":null,"error":null,"warnings":[],"fell_back":false}]}],
            "tool_warnings":[]}"#;
        let r: Report = serde_json::from_str(rep).unwrap();
        assert!(!r.stopped_by_anomalies);
        assert!(r.items[0].outputs[0].anomalies.is_empty());
        let text = serde_json::to_string(&r).unwrap();
        assert!(!text.contains("anomalies"), "{text}");
    }

    #[test]
    fn name_date_written_by_a_metadata_step_is_not_an_anomaly() {
        need_tools!();
        if crate::tools::find("avifenc").is_none() {
            eprintln!("skip: avifenc not found");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        // The name says 10:33:10, the EXIF 10:33:14: the name wins.
        let photo = jpeg(&src, "IMG_20260412_103310.jpg");
        // A video named by date whose own CreateDate is another day.
        let clip = mp4(&src, "VID_20260412_103310.mp4");
        tool(
            "exiftool",
            &[
                "-overwrite_original",
                "-q",
                "-QuickTime:CreateDate=2020:01:01 00:00:00",
            ],
            &clip,
        );
        let (plan, _) = plan_steps(d.path(), &[photo], &["Metadata", "AVIF"]);
        let rep = apply(&plan, &tools(), &opts()).unwrap();
        let o = &rep.items[0].outputs[0];
        assert!(o.anomalies.is_empty(), "{o:?}");
        assert!(
            matches!(rep.items[0].status, ItemStatus::Done { .. }),
            "{:?}",
            rep.items[0]
        );

        let d2 = tempfile::tempdir().unwrap();
        let src2 = d2.path().join("src");
        std::fs::create_dir_all(&src2).unwrap();
        std::fs::copy(&clip, src2.join("VID_20260412_103310.mp4")).unwrap();
        let (plan, _) = plan_for(d2.path(), &[src2.join("VID_20260412_103310.mp4")]);
        let rep = apply(&plan, &tools(), &opts()).unwrap();
        let o = &rep.items[0].outputs[0];
        assert!(o.anomalies.is_empty(), "{o:?}");
    }

    #[test]
    fn second_output_failing_verification_places_nothing() {
        need_tools!();
        let d = tempfile::tempdir().unwrap();
        let srcs = setup(d.path(), 1);
        let original = std::fs::read(&srcs[0]).unwrap();
        let (plan_path, _) = plan_for(d.path(), &srcs);
        let mut plan = Plan::load(&plan_path).unwrap();
        let second = d.path().join("out2/0.jpg");
        let mut o2 = plan.items[0].outputs[0].clone();
        o2.dest = second.clone();
        let first = plan.items[0].outputs[0].dest.clone();
        plan.items[0].outputs.push(o2);
        plan.save(&plan_path).unwrap();
        TAMPER_STAGED
            .lock()
            .unwrap()
            .push((second.clone(), tamper_gps));
        let rep = apply(&plan_path, &tools(), &opts()).unwrap();
        let r = &rep.items[0];
        assert!(
            matches!(r.status, ItemStatus::Failed { trashed: None }),
            "{r:?}"
        );
        assert!(r.outputs.iter().all(|o| o.actual.is_none()));
        assert!(!first.exists() && !second.exists());
        assert_eq!(std::fs::read(&srcs[0]).unwrap(), original);
        for dir in [first.parent().unwrap(), second.parent().unwrap()] {
            assert_eq!(std::fs::read_dir(dir).map_or(0, Iterator::count), 0);
        }
    }
}

#[cfg(test)]
mod trash_tests;
