//! Plan building: the JSON contract between `scan` and `apply`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use chrono::{Local, NaiveDateTime};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::apply::{ItemResult, ItemStatus, Report, file_hash, same_file};
use crate::config::{Config, ContentPreset, Output, expand_tilde};
use crate::datetime::{capture_date, file_created};
use crate::media::{kind_of, split_name};
use crate::name::{NameCtx, Pipe, TraceStep, apply_name_pipe, reserve};
use crate::progress::{Cancel, NoProgress, Progress, ProgressEvent, Stage};
use crate::route::{Decision, Via};
use crate::scan::FileEntry;
use crate::{Error, Result};

pub const PLAN_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Local,
    Server,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub version: u32,
    pub created: String,
    pub side: Side,
    /// IANA zone of the planning machine: capture times without a zone (from
    /// file names, EXIF) are wall-clock times there, whatever machine applies.
    #[serde(default)]
    pub tz: Option<String>,
    /// Only the presets that planned items use: apply on a server has no config.
    pub content_presets: BTreeMap<String, ContentPreset>,
    pub items: Vec<PlanItem>,
    pub review: Vec<ReviewItem>,
    pub unmatched: Vec<PathBuf>,
    pub warnings: Vec<Warning>,
    /// Routed sources whose every output would leave them exactly as they
    /// are (same path, no content step), and sources only an
    /// exclude mask turned away: nothing to do.
    #[serde(default)]
    pub unchanged: Vec<PathBuf>,
    /// `run_id` of the report whose failed files this plan retries
    /// (`undo::prepare_retry`); `None` for a scanned plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
    pub source: PathBuf,
    pub source_root: PathBuf,
    pub size: u64,
    pub mtime_ns: i128,
    pub target: String,
    pub via: Via,
    pub outputs: Vec<PlannedOutput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedOutput {
    pub dest: PathBuf,
    /// What apply does for this output.
    #[serde(default)]
    pub op: OutputOp,
    /// Content presets whose mask matched, in pipe order.
    pub steps: Vec<String>,
    /// Content pipe steps, then name pipe steps.
    pub trace: Vec<TraceStep>,
}

/// How an output comes into being.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputOp {
    /// A new file is written at `dest` (a processed file or a byte copy).
    #[default]
    Write,
    /// The source itself is moved to `dest`, in its own folder or in another
    /// folder inside its source root; it is not copied and does not go to
    /// the trash.
    Rename,
    /// The source already is `dest`: nothing to do for this output.
    Keep,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewItem {
    pub source: PathBuf,
    pub size: u64,
    pub mtime_ns: i128,
    pub suggested: String,
    pub reason: String,
    /// When the file was captured (`YYYY-MM-DDTHH:MM:SS`, local), if known:
    /// the GUI groups review items into series by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured: Option<String>,
    /// The root of the source the file came from (absent in older plans).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_root: Option<PathBuf>,
    /// The file is held back because its name could not be built (`reason`
    /// says why), not because the AI was unsure: another target does not
    /// help. Absent (false) in older plans.
    #[serde(default, skip_serializing_if = "is_false")]
    pub name_error: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Warning {
    SeriesSplit { a: PathBuf, b: PathBuf },
    DoubleEncode { source: PathBuf, dest: PathBuf },
    MissingServerPath { what: String },
}

/// Planning state of one destination directory.
struct DirState {
    /// Lowercased names already planned into the directory.
    planned: HashSet<String>,
    /// Lowercased names present on disk (Local side only).
    on_disk: HashSet<String>,
}

fn read_dir_names(dir: &Path) -> HashSet<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return HashSet::new();
    };
    rd.filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_lowercase())
        .collect()
}

fn target_outputs<'a>(cfg: &'a Config, name: &str) -> Option<&'a [Output]> {
    let t = &cfg.targets;
    t.before_ai
        .iter()
        .chain(&t.after_ai)
        .find(|m| m.name == name)
        .map(|m| m.outputs.as_slice())
        .or_else(|| {
            t.ai.iter()
                .find(|a| a.name == name)
                .map(|a| a.outputs.as_slice())
        })
}

/// Render `{YYYY} {MM} {DD} {source}`; without a capture date the date tokens
/// become `unknown`.
fn render_dir(tpl: &str, capture: Option<NaiveDateTime>, source_id: &str) -> String {
    let part = |f: &str| capture.map_or_else(|| "unknown".to_string(), |d| d.format(f).to_string());
    tpl.replace("{YYYY}", &part("%Y"))
        .replace("{MM}", &part("%m"))
        .replace("{DD}", &part("%d"))
        .replace("{source}", source_id)
}

/// Source path and source root as seen on `side`. On the server side the
/// relative path is re-rooted at `server_path`; a missing one is recorded.
fn locate(
    cfg: &Config,
    e: &FileEntry,
    side: Side,
    missing: &mut BTreeSet<String>,
) -> (PathBuf, PathBuf) {
    let src = cfg.sources.iter().find(|s| s.id == e.source_id);
    match side {
        Side::Local => {
            let root = match src {
                Some(s) => expand_tilde(&s.path),
                None => e
                    .path
                    .ancestors()
                    .nth(e.rel.components().count())
                    .unwrap_or(Path::new(""))
                    .to_path_buf(),
            };
            (e.path.clone(), root)
        }
        Side::Server => match src.and_then(|s| s.server_path.clone()) {
            Some(root) => (root.join(&e.rel), root),
            None => {
                missing.insert(crate::t!("plan.no_server_path", source = e.source_id));
                (e.path.clone(), PathBuf::new())
            }
        },
    }
}

/// `p` with `.` components, repeated and trailing separators dropped. `..`
/// stays: lexically it may cross a symlink.
pub(crate) fn normalize(p: &Path) -> PathBuf {
    p.components().collect()
}

/// Whether `a` and `b` name the same directory: equal once `~` is expanded
/// on both and they are normalized or, when `on_disk`, once resolved on this
/// machine (symlinks, `..`).
pub(crate) fn same_dir(a: &Path, b: &Path, on_disk: bool) -> bool {
    if normalize(&expand_tilde(a)) == normalize(&expand_tilde(b)) {
        return true;
    }
    on_disk
        && matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(x), Ok(y)) if x == y
        )
}

/// Whether directory `dir` is `root` or lies below it: compared once `~` is
/// expanded on both and they are normalized, with no `..` below the root. When
/// `on_disk`, the paths as resolved on this machine decide (symlinks; a `dir`
/// not made yet is resolved through its nearest existing ancestor), and the
/// lexical answer only when they cannot be resolved.
pub fn inside_root(dir: &Path, root: &Path, on_disk: bool) -> bool {
    let (d, r) = (
        normalize(&expand_tilde(dir)),
        normalize(&expand_tilde(root)),
    );
    if r.as_os_str().is_empty() {
        return false;
    }
    let lexical = d
        .strip_prefix(&r)
        .is_ok_and(|rest| rest.components().all(|c| matches!(c, Component::Normal(_))));
    if !on_disk {
        return lexical;
    }
    match (resolve(&d), std::fs::canonicalize(&r)) {
        (Some(d), Ok(r)) => d.starts_with(r),
        _ => lexical,
    }
}

/// `p` with its longest existing ancestor resolved on disk and the missing
/// rest appended; `None` when nothing resolves or the rest is not plain names.
fn resolve(p: &Path) -> Option<PathBuf> {
    let mut rest = Vec::new();
    let mut cur = p;
    loop {
        if let Ok(mut real) = std::fs::canonicalize(cur) {
            real.extend(rest.iter().rev());
            return Some(real);
        }
        match cur.components().next_back()? {
            Component::Normal(n) => rest.push(n),
            _ => return None,
        }
        cur = cur.parent()?;
    }
}

/// Whether `dir` already holds a byte copy of `source` (another regular file
/// of its size and blake3). `hash` caches the source's blake3 (`None` inside
/// when it cannot be read); only same-size candidates are hashed.
fn has_copy(dir: &Path, source: &Path, hash: &mut Option<Option<String>>) -> bool {
    let Ok(len) = std::fs::metadata(source).map(|m| m.len()) else {
        return false;
    };
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        // Leftovers of interrupted runs are deleted by the next one.
        if p == source || name.starts_with(".gallery-tmp-") || name.starts_with(".gallery-undo-") {
            continue;
        }
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.is_file() && m.len() == len => {}
            _ => continue,
        }
        let Some(want) = hash.get_or_insert_with(|| file_hash(source).ok()) else {
            return false;
        };
        if file_hash(&p).is_ok_and(|h| &h == want) {
            return true;
        }
    }
    false
}

fn plan_err(msg: String) -> Error {
    Error::Plan(msg)
}

/// The content pipe of one output run "dry" on `file_name`.
pub(crate) struct ContentRun {
    /// The name after the matching steps (only the extension changes).
    pub name: String,
    /// Presets whose mask matched, in pipe order.
    pub steps: Vec<String>,
    pub trace: Vec<TraceStep>,
    /// Lossy re-encodes among the matched steps.
    pub reencodes: usize,
}

pub(crate) fn run_content_pipe(
    cfg: &Config,
    target: &str,
    o: &Output,
    file_name: &str,
) -> Result<ContentRun> {
    let mut name = file_name.to_string();
    let mut steps = Vec::new();
    let mut trace = Vec::new();
    let mut reencodes = 0;
    let expanded = cfg.expand_content(target, &o.content).map_err(plan_err)?;
    for (index, ex) in expanded.iter().enumerate() {
        let st = &ex.step;
        let preset = cfg.content_preset(&st.preset).ok_or_else(|| {
            plan_err(crate::config::unknown_preset(target, "content", &st.preset))
        })?;
        let mask = st.mask.clone().unwrap_or_else(|| preset.mask_or_default());
        let matched = mask.matches(&name, cfg);
        if matched {
            let (stem, ext) = split_name(&name);
            let new_ext = preset.output_ext(&ext);
            name = if new_ext.is_empty() {
                stem
            } else {
                format!("{stem}.{new_ext}")
            };
            steps.push(st.preset.clone());
            reencodes += usize::from(preset.reencodes());
        }
        trace.push(TraceStep {
            index,
            preset: st.preset.clone(),
            matched,
            after: name.clone(),
            pipe: Some(Pipe::Content),
            bundle: ex.bundle.clone(),
        });
    }
    Ok(ContentRun {
        name,
        steps,
        trace,
        reencodes,
    })
}

/// Build the plan for routed files. Every output runs its content pipe "dry"
/// (each matching step changes the extension), then the name pipe, then the
/// directory template; names are reserved per directory, against files on
/// disk too on the local side.
pub fn build_plan(
    cfg: &Config,
    routed: Vec<(FileEntry, Decision)>,
    exif: &HashMap<PathBuf, Value>,
    side: Side,
) -> Result<Plan> {
    build_plan_with(cfg, routed, exif, side, &NoProgress, &Cancel::new())
}

/// [`build_plan`] with a [`Stage::Plan`] event per routed file; a cancel
/// (checked per file) ends it with `Error::Cancelled`. Planning only reads.
pub fn build_plan_with(
    cfg: &Config,
    routed: Vec<(FileEntry, Decision)>,
    exif: &HashMap<PathBuf, Value>,
    side: Side,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<Plan> {
    let total = routed.len();
    // Recorded in the plan; Unix-time names are read in it too, as apply will.
    let zone = local_zone();
    let tz = zone
        .as_deref()
        .and_then(|z| z.parse::<chrono_tz::Tz>().ok());
    let mut missing = BTreeSet::new();
    let mut dirs: HashMap<PathBuf, DirState> = HashMap::new();
    let mut used_presets = BTreeSet::new();
    let mut items = Vec::new();
    let mut review = Vec::new();
    let mut unmatched = Vec::new();
    let mut unchanged = Vec::new();
    let mut warnings = Vec::new();
    // (capture time, target, source) of planned items, for SeriesSplit.
    let mut timeline = Vec::new();

    'files: for (done, (e, decision)) in routed.into_iter().enumerate() {
        cancel.check()?;
        // Counted as the file starts: its `continue`s leave from many places.
        progress.step(ProgressEvent {
            stage: Stage::Plan,
            done: done + 1,
            total,
            current: Some(e.path.clone()),
        });
        let (source, source_root) = locate(cfg, &e, side, &mut missing);
        let file_name = e
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let capture = || {
            let ex = exif.get(&e.path).unwrap_or(&Value::Null);
            capture_date(file_name, kind_of(&e.path), ex, tz)
        };
        let captured =
            |c: Option<NaiveDateTime>| c.map(|t| t.format("%Y-%m-%dT%H:%M:%S").to_string());
        let (target, via) = match decision {
            Decision::Target { name, via } => (name, via),
            Decision::Review { suggested, reason } => {
                review.push(ReviewItem {
                    source,
                    size: e.size,
                    mtime_ns: e.mtime_ns,
                    suggested,
                    reason,
                    captured: captured(capture()),
                    source_root: Some(source_root),
                    name_error: false,
                });
                continue;
            }
            Decision::Unmatched => {
                unmatched.push(source);
                continue;
            }
            // Left alone on purpose by an exclude mask: nothing to do.
            // Kept by a manual decision: likewise nothing to do.
            Decision::Excluded | Decision::Kept => {
                unchanged.push(source);
                continue;
            }
        };
        let outputs_cfg = target_outputs(cfg, &target)
            .ok_or_else(|| plan_err(crate::t!("plan.unknown_target", target = target)))?;

        let capture = capture();
        let ctx = NameCtx {
            capture,
            created: file_created(&e.path),
            source_id: e.source_id.clone(),
        };

        // Every output's directory, content steps, trace and wanted name.
        let mut pending = Vec::with_capacity(outputs_cfg.len());
        for (oi, o) in outputs_cfg.iter().enumerate() {
            // (1) Content pipe, dry: only the extension changes.
            let ContentRun {
                name,
                steps,
                mut trace,
                reencodes,
            } = run_content_pipe(cfg, &target, o, file_name)?;

            // (2) Name pipe on the resulting name.
            let name_steps = o
                .rename
                .iter()
                .map(|st| {
                    cfg.name_preset(&st.preset)
                        .map(|p| {
                            let mask = st.mask.clone().unwrap_or_else(|| p.mask_or_default());
                            (mask, st.preset.as_str(), p)
                        })
                        .ok_or_else(|| {
                            plan_err(crate::config::unknown_preset(&target, "name", &st.preset))
                        })
                })
                .collect::<Result<Vec<_>>>()?;
            // A name this file cannot have (an empty `{ext}` in a folder
            // part, say) only holds this file back: it goes to review and
            // stays where it is.
            let named = match apply_name_pipe(cfg, &name_steps, &name, &ctx) {
                Ok(n) => n,
                Err(why) => {
                    review.push(ReviewItem {
                        source,
                        size: e.size,
                        mtime_ns: e.mtime_ns,
                        suggested: target,
                        reason: why,
                        captured: captured(capture),
                        source_root: Some(source_root),
                        name_error: true,
                    });
                    continue 'files;
                }
            };
            trace.extend(named.steps);

            // (3) Directory.
            let tpl = match side {
                Side::Local => o.dir.as_str(),
                Side::Server => match &o.server_dir {
                    Some(d) => d.as_str(),
                    None => {
                        missing.insert(crate::t!("plan.no_server_dir", target = target, n = oi));
                        continue;
                    }
                },
            };
            let rendered = PathBuf::from(render_dir(tpl, capture, &e.source_id));
            // `~` stays literal on the server side: it is the server's home.
            let mut dir = match side {
                Side::Local => expand_tilde(&rendered),
                Side::Server => rendered,
            };
            // Folders made by the name pipe go below the output directory.
            dir.extend(&named.subdir);
            dir = normalize(&dir);
            // The source's own folder, however written, is named as the
            // source's path names it.
            if let Some(src_dir) = source.parent()
                && same_dir(&dir, src_dir, side == Side::Local)
            {
                dir = src_dir.to_path_buf();
            }
            // This machine's view of the folder: on the server side the local
            // `dir` is its Syncthing mirror.
            let mut local_dir =
                expand_tilde(&PathBuf::from(render_dir(&o.dir, capture, &e.source_id)));
            local_dir.extend(&named.subdir);
            let local_dir = normalize(&local_dir);
            pending.push((dir, local_dir, steps, trace, reencodes, named.final_name));
        }

        // (4) One output may consume the source: the first that runs no
        // content step and stays inside the source's root (its own folder or
        // a folder below the root). It moves the source there (or keeps it,
        // when the path does not change) instead of copying it.
        let src_dir = source.parent();
        let consumer = pending.iter().position(|(dir, _, steps, ..)| {
            steps.is_empty() && inside_root(dir, &source_root, side == Side::Local)
        });

        // (5) Free name in the directory.
        let mut outputs = Vec::with_capacity(pending.len());
        let src_name = source
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase());
        let mut source_hash = None;
        for (k, (dir, local_dir, steps, trace, reencodes, wanted)) in
            pending.into_iter().enumerate()
        {
            // A source that stays needs a byte copy only once: an earlier run
            // already wrote it when the folder holds the same bytes. Checked
            // here, with the scanned file and the local (mirrored) folder.
            if consumer.is_some_and(|c| c != k)
                && steps.is_empty()
                && has_copy(&local_dir, &e.path, &mut source_hash)
            {
                continue;
            }
            let state = dirs.entry(dir.clone()).or_insert_with(|| DirState {
                planned: HashSet::new(),
                on_disk: match side {
                    Side::Local => read_dir_names(&dir),
                    Side::Server => HashSet::new(),
                },
            });
            let on_disk = &state.on_disk;
            // The source's name is free for the output that consumes the
            // source, or for any output when none does (an in-place write);
            // never for a copy next to a source that stays.
            let source_free = consumer.is_none_or(|c| c == k);
            // The consumer may also take its own name in another case
            // (`IMG.JPG` -> `IMG.jpg`); apply falls back to the next name where
            // the file system does not tell the two apart.
            let is_source = |n: &str| {
                source_free
                    && (dir.join(n) == source
                        || consumer == Some(k)
                            && Some(dir.as_path()) == src_dir
                            && Some(n.to_lowercase()) == src_name)
            };
            let final_name = reserve(
                &mut state.planned,
                |n| on_disk.contains(&n.to_lowercase()) && !is_source(n),
                &wanted,
            );
            let dest = dir.join(final_name);
            let op = match consumer {
                // Also when `dest` is another name of the source itself (a
                // case-insensitive file system): a rename would only find the
                // name taken and pick the next one.
                Some(c)
                    if c == k
                        && (dest == source
                            || cfg!(unix) && side == Side::Local && same_file(&dest, &source)) =>
                {
                    OutputOp::Keep
                }
                Some(c) if c == k => OutputOp::Rename,
                _ => OutputOp::Write,
            };

            if reencodes >= 2 {
                warnings.push(Warning::DoubleEncode {
                    source: source.clone(),
                    dest: dest.clone(),
                });
            }
            used_presets.extend(steps.iter().cloned());
            outputs.push(PlannedOutput {
                dest,
                op,
                steps,
                trace,
            });
        }

        if !outputs.is_empty() && outputs.iter().all(|o| o.op == OutputOp::Keep) {
            unchanged.push(source);
            continue;
        }
        timeline.push((capture, target.clone(), source.clone()));
        items.push(PlanItem {
            source,
            source_root,
            size: e.size,
            mtime_ns: e.mtime_ns,
            target,
            via,
            outputs,
        });
    }

    if !missing.is_empty() {
        let list: Vec<String> = missing.into_iter().collect();
        return Err(plan_err(crate::t!(
            "plan.server_needs_paths",
            list = list.join("; ")
        )));
    }

    // Neighbouring captures (<= 60 s apart) that went to different targets.
    let mut timeline: Vec<_> = timeline
        .into_iter()
        .filter_map(|(t, target, src)| t.map(|t| (t, target, src)))
        .collect();
    timeline.sort_by_key(|(t, _, _)| *t);
    for w in timeline.windows(2) {
        let ((ta, xa, a), (tb, xb, b)) = (&w[0], &w[1]);
        if xa != xb && (*tb - *ta).num_seconds() <= 60 {
            warnings.push(Warning::SeriesSplit {
                a: a.clone(),
                b: b.clone(),
            });
        }
    }

    let content_presets = used_presets
        .into_iter()
        .filter_map(|n| cfg.content_preset(&n).map(|p| (n, p.clone())))
        .collect();

    Ok(Plan {
        version: PLAN_VERSION,
        created: Local::now().to_rfc3339(),
        side,
        tz: zone,
        content_presets,
        items,
        review,
        unmatched,
        warnings,
        unchanged,
        retry_of: None,
    })
}

/// IANA name of this machine's zone: `TZ` when it names one, else the
/// system setting.
pub fn local_zone() -> Option<String> {
    std::env::var("TZ")
        .ok()
        .map(|z| z.trim_start_matches(':').to_string())
        .filter(|z| z.parse::<chrono_tz::Tz>().is_ok())
        .or_else(|| iana_time_zone::get_timezone().ok())
}

/// The plan to retry the items `report` lists as failed: the same items with
/// the same outputs, kept only while each source is still a regular file with
/// the planned size and mtime. Same side, zone and content presets; nothing
/// for review, unmatched or unchanged.
///
/// Only items that never touched their source qualify: `Failed` with no trash
/// reference (a trashed source's path may hold its processed in-place output)
/// and no output that is the source renamed. Their already placed outputs are
/// still on disk; see `undo::prepare_retry` to remove them first.
pub fn subplan(plan: &Plan, report: &Report) -> Plan {
    // The last result per source wins (a report holds one per item).
    let results: HashMap<&Path, &ItemResult> = report
        .items
        .iter()
        .map(|r| (r.source.as_path(), r))
        .collect();
    let untouched_failure = |r: &&ItemResult| {
        matches!(r.status, ItemStatus::Failed { trashed: None })
            && r.outputs.iter().all(|o| o.renamed_from.is_none())
    };
    let unchanged_on_disk = |it: &PlanItem| {
        std::fs::symlink_metadata(&it.source).is_ok_and(|m| {
            m.is_file()
                && m.len() == it.size
                && crate::scan::mtime_ns(&m).is_ok_and(|t| t == it.mtime_ns)
        })
    };
    let items = plan
        .items
        .iter()
        .filter(|it| {
            results
                .get(it.source.as_path())
                .is_some_and(untouched_failure)
        })
        .filter(|it| unchanged_on_disk(it))
        .cloned()
        .collect();
    Plan {
        version: PLAN_VERSION,
        created: Local::now().to_rfc3339(),
        side: plan.side,
        tz: plan.tz.clone(),
        content_presets: plan.content_presets.clone(),
        items,
        review: Vec::new(),
        unmatched: Vec::new(),
        warnings: Vec::new(),
        unchanged: Vec::new(),
        retry_of: None,
    }
}

/// Split `plan` for a trial run: the pilot takes up to `per_target` items of
/// each target, preferring different extensions (one per extension, in plan
/// order, then the remaining items in plan order); the rest is everything
/// else. Both keep the plan's order, side, zone and content presets, so the
/// two together cover every item exactly once; the rest also carries the
/// review, unmatched and unchanged lists and the warnings (the pilot has none).
pub fn pilot(plan: &Plan, per_target: usize) -> (Plan, Plan) {
    // Indexes of the items per target, in plan order.
    let mut by_target: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, it) in plan.items.iter().enumerate() {
        by_target.entry(it.target.as_str()).or_default().push(i);
    }
    let ext_of = |i: usize| {
        plan.items[i]
            .source
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    let mut chosen = vec![false; plan.items.len()];
    for idx in by_target.values() {
        let mut seen = HashSet::new();
        let mut picked = Vec::new();
        for &i in idx {
            if picked.len() < per_target && seen.insert(ext_of(i)) {
                picked.push(i);
            }
        }
        for &i in idx {
            if picked.len() < per_target && !picked.contains(&i) {
                picked.push(i);
            }
        }
        for i in picked {
            chosen[i] = true;
        }
    }
    let created = Local::now().to_rfc3339();
    let part = |want: bool| Plan {
        version: PLAN_VERSION,
        created: created.clone(),
        side: plan.side,
        tz: plan.tz.clone(),
        content_presets: plan.content_presets.clone(),
        items: plan
            .items
            .iter()
            .zip(&chosen)
            .filter(|(_, c)| **c == want)
            .map(|(it, _)| it.clone())
            .collect(),
        review: if want {
            Vec::new()
        } else {
            plan.review.clone()
        },
        unmatched: if want {
            Vec::new()
        } else {
            plan.unmatched.clone()
        },
        warnings: if want {
            Vec::new()
        } else {
            plan.warnings.clone()
        },
        unchanged: if want {
            Vec::new()
        } else {
            plan.unchanged.clone()
        },
        retry_of: plan.retry_of.clone(),
    };
    (part(true), part(false))
}

impl Plan {
    /// Write the plan as pretty JSON.
    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| plan_err(format!("{}: {e}", path.display())))?;
        crate::paths::write_atomic(path, json.as_bytes())?;
        Ok(())
    }

    /// Read a plan; only version 1 is understood. A value this build does
    /// not know (e.g. a manual decision, `Via::Manual`, read by an older
    /// server binary) gets a hint to update gallery.
    pub fn load(path: &Path) -> Result<Plan> {
        let text = std::fs::read_to_string(path)?;
        Plan::parse(&text, path)
    }

    /// [`Plan::load`] over text already read (`path` is only for messages),
    /// so a caller can hash and parse the very same bytes.
    pub fn parse(text: &str, path: &Path) -> Result<Plan> {
        let plan: Plan = serde_json::from_str(text).map_err(|e| {
            let msg = e.to_string();
            let hint = if msg.contains("unknown variant") {
                crate::t!("plan.newer_version")
            } else {
                String::new()
            };
            plan_err(format!("{}: {msg}{hint}", path.display()))
        })?;
        if plan.version != PLAN_VERSION {
            return Err(plan_err(crate::t!(
                "plan.unsupported_version",
                path = path.display(),
                version = plan.version
            )));
        }
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AudioPreset, DateSource, MaskTarget, MetadataPreset, NamePreset, Output, PhotoPreset,
        PipeStep, Source, VideoPreset,
    };
    use crate::mask::MaskExpr;

    fn step(mask: &str, preset: &str) -> PipeStep {
        PipeStep {
            mask: Some(MaskExpr::parse(mask).unwrap()),
            preset: preset.into(),
        }
    }

    fn output(dir: &Path, content: Vec<PipeStep>, name: Vec<PipeStep>) -> Output {
        Output {
            dir: dir.to_str().unwrap().into(),
            server_dir: None,
            content,
            rename: name,
        }
    }

    /// Two sources (`phone`, `cam`) under `root`, one target `t` with `outputs`.
    fn cfg(root: &Path, outputs: Vec<Output>) -> Config {
        let mut c = Config::default();
        for id in ["phone", "cam"] {
            c.sources.push(Source {
                id: id.into(),
                path: root.join(id),
                server_path: None,
                recursive: false,
                skip: vec![],
                ai_prefix: None,
                ai_prefix_file: None,
                ai_suffix: None,
                ai_suffix_file: None,
            });
        }
        c.content_presets.insert(
            "Metadata".into(),
            ContentPreset::Metadata(MetadataPreset::full()),
        );
        c.content_presets.insert(
            "AVIF".into(),
            ContentPreset::Photo(PhotoPreset::avif_high()),
        );
        c.content_presets
            .insert("Unused".into(), ContentPreset::Video(VideoPreset::high()));
        c.name_presets.insert(
            "capture-date".into(),
            NamePreset {
                mask: None,
                template: "{date:YYYYMMDD_HHMMSS}.{ext}".into(),
                date: DateSource::Capture,
                skip_if: None,
                normalize_ext: true,
            },
        );
        c.name_presets.insert(
            "ext-lower".into(),
            NamePreset {
                mask: None,
                template: "{stem}.{ext}".into(),
                date: DateSource::None,
                skip_if: None,
                normalize_ext: true,
            },
        );
        c.name_presets.insert(
            "Prefix".into(),
            NamePreset {
                mask: None,
                template: "x_{stem}.{ext}".into(),
                date: DateSource::None,
                skip_if: None,
                normalize_ext: false,
            },
        );
        c.name_presets.insert(
            "capture-date-dirs".into(),
            NamePreset {
                mask: None,
                template: "{date:YYYY}/{date:MM}/{date:YYYYMMDD_HHMMSS}.{ext}".into(),
                date: DateSource::Capture,
                skip_if: None,
                normalize_ext: true,
            },
        );
        c.targets.after_ai.push(MaskTarget {
            name: "t".into(),
            filters: vec![],
            outputs,
        });
        c
    }

    fn entry(root: &Path, src: &str, name: &str) -> FileEntry {
        FileEntry {
            source_id: src.into(),
            path: root.join(src).join(name),
            rel: PathBuf::from(name),
            size: 7,
            mtime_ns: 42,
        }
    }

    fn to(target: &str) -> Decision {
        Decision::Target {
            name: target.into(),
            via: Via::Mask,
        }
    }

    fn local(c: &Config, routed: Vec<(FileEntry, Decision)>) -> Plan {
        build_plan(c, routed, &HashMap::new(), Side::Local).unwrap()
    }

    #[test]
    fn content_preset_output_ext() {
        let m = ContentPreset::Metadata(MetadataPreset::full());
        assert_eq!(m.output_ext("JPG"), "JPG");
        assert!(!m.reencodes());
        let p = ContentPreset::Photo(PhotoPreset::webp_high());
        assert_eq!(p.output_ext("jpg"), "webp");
        assert!(p.reencodes());
        assert_eq!(
            ContentPreset::Video(VideoPreset::high()).output_ext("mov"),
            "av1.mp4"
        );
        assert_eq!(
            ContentPreset::Audio(AudioPreset::aac(96)).output_ext("wav"),
            "m4a"
        );
        assert!(ContentPreset::Audio(AudioPreset::opus(96)).reencodes());
        assert!(!ContentPreset::Audio(AudioPreset::flac()).reencodes());
        let png = ContentPreset::Png(Default::default());
        assert_eq!(png.output_ext("png"), "png");
        assert!(!png.reencodes());
    }

    #[test]
    fn plan_embeds_a_builtin_content_preset() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        let mut c = cfg(
            &out,
            vec![output(&out, vec![step("*.jpg", "avif-max")], vec![])],
        );
        assert!(!c.content_presets.contains_key("avif-max"));
        c.sources[0].path = tmp.path().join("phone");
        let e = entry(tmp.path(), "phone", "IMG_20260412_103314.jpg");
        let p = local(&c, vec![(e, to("t"))]);
        assert_eq!(p.content_presets.keys().collect::<Vec<_>>(), ["avif-max"]);
        let ContentPreset::Photo(ph) = &p.content_presets["avif-max"] else {
            panic!("embedded preset is a photo preset")
        };
        assert_eq!(*ph, PhotoPreset::avif_max());
    }

    #[test]
    fn plan_content_then_name() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        let c = cfg(
            tmp.path(),
            vec![output(
                &out,
                vec![step("*", "Metadata"), step("*.jpg", "AVIF")],
                vec![step("*", "capture-date"), step("*.png", "Prefix")],
            )],
        );
        let e = entry(tmp.path(), "phone", "IMG_20260412_103314.jpg");
        let p = local(&c, vec![(e.clone(), to("t"))]);
        assert_eq!(p.version, 1);
        assert_eq!(p.side, Side::Local);
        assert!(chrono::DateTime::parse_from_rfc3339(&p.created).is_ok());
        let item = &p.items[0];
        assert_eq!(item.source, e.path);
        assert_eq!(item.source_root, tmp.path().join("phone"));
        assert_eq!((item.size, item.mtime_ns), (7, 42));
        assert_eq!(item.target, "t");
        let o = &item.outputs[0];
        assert_eq!(o.dest, out.join("20260412_103314.avif"));
        assert_eq!(o.steps, vec!["Metadata", "AVIF"]);
        let names: Vec<&str> = o.trace.iter().map(|s| s.preset.as_str()).collect();
        assert_eq!(names, ["Metadata", "AVIF", "capture-date", "Prefix"]);
        assert_eq!(o.trace[1].after, "IMG_20260412_103314.avif");
        assert!(!o.trace[3].matched);
        // Each step says which pipe it belongs to, so a UI can split them.
        let pipes: Vec<Option<Pipe>> = o.trace.iter().map(|s| s.pipe).collect();
        assert_eq!(
            pipes,
            [
                Some(Pipe::Content),
                Some(Pipe::Content),
                Some(Pipe::Name),
                Some(Pipe::Name)
            ]
        );
        assert_eq!(
            p.content_presets.keys().collect::<Vec<_>>(),
            ["AVIF", "Metadata"]
        );
        assert!(p.warnings.is_empty());
    }

    #[test]
    fn plan_trace_with_short_steps() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        let c = cfg(
            tmp.path(),
            vec![output(
                &out,
                vec![PipeStep::named("meta-full"), PipeStep::named("avif-high")],
                vec![],
            )],
        );
        let e = entry(tmp.path(), "phone", "IMG_20260412_103314.jpg");
        let p = local(&c, vec![(e, to("t"))]);
        let o = &p.items[0].outputs[0];
        assert_eq!(o.dest, out.join("IMG_20260412_103314.avif"));
        assert_eq!(o.steps, vec!["meta-full", "avif-high"]);
        assert!(o.trace.iter().all(|s| s.matched));
        assert_eq!(o.trace[1].after, "IMG_20260412_103314.avif");
    }

    #[test]
    fn short_photo_step_skips_a_video() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        let c = cfg(
            tmp.path(),
            vec![output(&out, vec![PipeStep::named("avif-high")], vec![])],
        );
        let e = entry(tmp.path(), "phone", "VID_20260412_103314.mp4");
        let p = local(&c, vec![(e, to("t"))]);
        let o = &p.items[0].outputs[0];
        assert!(!o.trace[0].matched);
        assert_eq!(o.dest, out.join("VID_20260412_103314.mp4"));
    }

    #[test]
    fn plan_mask_sees_previous_ext() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        let c = cfg(
            tmp.path(),
            vec![output(
                &out,
                vec![step("*.jpg", "AVIF"), step("*.avif", "AVIF")],
                vec![],
            )],
        );
        let e = entry(tmp.path(), "phone", "a.jpg");
        let p = local(&c, vec![(e.clone(), to("t"))]);
        let o = &p.items[0].outputs[0];
        assert_eq!(o.steps, vec!["AVIF", "AVIF"]);
        assert_eq!(
            p.warnings,
            vec![Warning::DoubleEncode {
                source: e.path,
                dest: out.join("a.avif"),
            }]
        );
    }

    #[test]
    fn plan_empty_content_keeps_ext() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        let c = cfg(tmp.path(), vec![output(&out, vec![], vec![])]);
        let p = local(&c, vec![(entry(tmp.path(), "phone", "Clip.MOV"), to("t"))]);
        let o = &p.items[0].outputs[0];
        assert_eq!(o.dest, out.join("Clip.MOV"));
        assert!(o.steps.is_empty() && o.trace.is_empty());
        assert!(p.content_presets.is_empty());
    }

    #[test]
    fn plan_dir_template_year() {
        let tmp = tempfile::tempdir().unwrap();
        let tpl = tmp.path().join("{source}/{YYYY}/{MM}-{DD}");
        let c = cfg(tmp.path(), vec![output(&tpl, vec![], vec![])]);
        let p = local(
            &c,
            vec![
                (
                    entry(tmp.path(), "phone", "IMG_20260412_103314.jpg"),
                    to("t"),
                ),
                (entry(tmp.path(), "cam", "nodate.jpg"), to("t")),
            ],
        );
        assert_eq!(
            p.items[0].outputs[0].dest,
            tmp.path().join("phone/2026/04-12/IMG_20260412_103314.jpg")
        );
        assert_eq!(
            p.items[1].outputs[0].dest,
            tmp.path().join("cam/unknown/unknown-unknown/nodate.jpg")
        );
    }

    #[test]
    fn plan_dir_expands_tilde() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = cfg(tmp.path(), vec![]);
        c.targets.after_ai[0].outputs.push(Output {
            dir: "~/gallery-plan-test-{YYYY}".into(),
            server_dir: None,
            content: vec![],
            rename: vec![],
        });
        let p = local(
            &c,
            vec![(entry(tmp.path(), "phone", "nodate.jpg"), to("t"))],
        );
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert_eq!(
            p.items[0].outputs[0].dest,
            home.join("gallery-plan-test-unknown/nodate.jpg")
        );
    }

    #[test]
    fn plan_reserves_colliding_names() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        let c = cfg(
            tmp.path(),
            vec![output(&out, vec![step("*.jpg", "AVIF")], vec![])],
        );
        let p = local(
            &c,
            vec![
                (entry(tmp.path(), "phone", "x.jpg"), to("t")),
                (entry(tmp.path(), "cam", "x.jpg"), to("t")),
            ],
        );
        assert_eq!(p.items[0].outputs[0].dest, out.join("x.avif"));
        assert_eq!(p.items[1].outputs[0].dest, out.join("x_1.avif"));
    }

    #[test]
    fn plan_reserve_sees_disk_case_insensitively() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("Y.AVIF"), b"").unwrap();
        let c = cfg(
            tmp.path(),
            vec![output(&out, vec![step("*.jpg", "AVIF")], vec![])],
        );
        let p = local(&c, vec![(entry(tmp.path(), "phone", "y.jpg"), to("t"))]);
        assert_eq!(p.items[0].outputs[0].dest, out.join("y_1.avif"));
    }

    #[test]
    fn plan_dest_equal_to_source_is_not_collision() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("phone");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.png"), b"").unwrap();
        // A content step: the processed file replaces the source in place.
        let c = cfg(
            tmp.path(),
            vec![output(&src, vec![step("*", "Metadata")], vec![])],
        );
        let p = local(&c, vec![(entry(tmp.path(), "phone", "a.png"), to("t"))]);
        assert_eq!(p.items[0].outputs[0].dest, src.join("a.png"));
        assert_eq!(p.items[0].outputs[0].op, OutputOp::Write);
    }

    /// Source folder `phone` with `files` on disk.
    fn phone_with(root: &Path, files: &[&str]) -> PathBuf {
        let dir = root.join("phone");
        std::fs::create_dir_all(&dir).unwrap();
        for f in files {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        dir
    }

    fn ops(o: &[PlannedOutput]) -> Vec<(OutputOp, PathBuf)> {
        o.iter().map(|o| (o.op, o.dest.clone())).collect()
    }

    #[test]
    fn plan_same_dir_name_change_is_rename() {
        let tmp = tempfile::tempdir().unwrap();
        let shot = "IMG_20260412_103314.jpg";
        let dir = phone_with(tmp.path(), &[shot, "20260412_103314.JPG"]);
        // The same folder, written another way.
        let c = cfg(
            tmp.path(),
            vec![output(
                &dir.join("."),
                vec![step("*.png", "Metadata")],
                vec![step("*", "capture-date")],
            )],
        );
        let p = local(&c, vec![(entry(tmp.path(), "phone", shot), to("t"))]);
        // The name on disk (another case) is taken: the next one is reserved.
        assert_eq!(
            ops(&p.items[0].outputs),
            [(OutputOp::Rename, dir.join("20260412_103314_1.jpg"))]
        );
        assert!(p.unchanged.is_empty());
    }

    #[test]
    fn plan_already_named_is_keep_and_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let name = "20260412_103314.jpg";
        let dir = phone_with(tmp.path(), &[name]);
        let c = cfg(
            tmp.path(),
            vec![output(&dir, vec![], vec![step("*", "capture-date")])],
        );
        let e = entry(tmp.path(), "phone", name);
        let p = local(&c, vec![(e.clone(), to("t"))]);
        assert!(p.items.is_empty(), "{:?}", p.items);
        assert_eq!(p.unchanged, vec![e.path]);
    }

    #[test]
    fn plan_av1_output_is_not_taken_again() {
        let tmp = tempfile::tempdir().unwrap();
        let name = "20260727_200006.av1.mp4";
        let dir = phone_with(tmp.path(), &[name]);
        let av1 = PipeStep {
            mask: None, // the preset's own mask: @video-original
            preset: "av1-high".into(),
        };
        let c = cfg(
            tmp.path(),
            vec![output(&dir, vec![av1], vec![step("*", "capture-date")])],
        );
        let e = entry(tmp.path(), "phone", name);
        let p = local(&c, vec![(e.clone(), to("t"))]);
        // No re-encode, no new name: nothing to do.
        assert!(p.items.is_empty(), "{:?}", p.items);
        assert_eq!(p.unchanged, vec![e.path]);
        // A fresh original with the same date is still re-encoded.
        let orig = "20260727_200007.mp4";
        let dir = phone_with(tmp.path(), &[orig]);
        let av1 = PipeStep {
            mask: None,
            preset: "av1-high".into(),
        };
        let c = cfg(tmp.path(), vec![output(&dir, vec![av1], vec![])]);
        let p = local(&c, vec![(entry(tmp.path(), "phone", orig), to("t"))]);
        assert_eq!(p.items.len(), 1);
    }

    #[test]
    fn plan_audio_output_is_not_taken_again() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = phone_with(tmp.path(), &["note.opus", "song.flac"]);
        let step = |preset: &str| PipeStep {
            mask: None, // the preset's own mask: @audio & !its output
            preset: preset.into(),
        };
        let c = cfg(
            tmp.path(),
            vec![output(&dir, vec![step("opus-128")], vec![])],
        );
        let opus = entry(tmp.path(), "phone", "note.opus");
        let p = local(&c, vec![(opus.clone(), to("t"))]);
        assert!(p.items.is_empty(), "{:?}", p.items);
        assert_eq!(p.unchanged, vec![opus.path]);
        // Another format is still encoded.
        let p = local(&c, vec![(entry(tmp.path(), "phone", "song.flac"), to("t"))]);
        assert_eq!(p.items.len(), 1);
    }

    #[test]
    fn plan_keep_with_backup_is_planned() {
        let tmp = tempfile::tempdir().unwrap();
        let name = "20260412_103314.jpg";
        let dir = phone_with(tmp.path(), &[name]);
        let backup = tmp.path().join("backup");
        let c = cfg(
            tmp.path(),
            vec![
                output(&dir, vec![], vec![step("*", "capture-date")]),
                output(&backup, vec![], vec![]),
            ],
        );
        let p = local(&c, vec![(entry(tmp.path(), "phone", name), to("t"))]);
        assert!(p.unchanged.is_empty());
        assert_eq!(
            ops(&p.items[0].outputs),
            [
                (OutputOp::Keep, dir.join(name)),
                (OutputOp::Write, backup.join(name)),
            ]
        );
    }

    #[test]
    fn plan_only_one_output_consumes_the_source() {
        let tmp = tempfile::tempdir().unwrap();
        let shot = "IMG_20260412_103314.jpg";
        let dir = phone_with(tmp.path(), &[shot]);
        let c = cfg(
            tmp.path(),
            vec![
                output(&dir, vec![], vec![step("*", "capture-date")]),
                output(&dir, vec![], vec![step("*", "capture-date")]),
            ],
        );
        let p = local(&c, vec![(entry(tmp.path(), "phone", shot), to("t"))]);
        assert_eq!(
            ops(&p.items[0].outputs),
            [
                (OutputOp::Rename, dir.join("20260412_103314.jpg")),
                (OutputOp::Write, dir.join("20260412_103314_1.jpg")),
            ]
        );
    }

    #[test]
    fn plan_write_never_targets_a_consumed_source() {
        // The first output keeps the source; the second, a processed copy in
        // the same folder, must not take the source's name.
        let tmp = tempfile::tempdir().unwrap();
        let dir = phone_with(tmp.path(), &["a.jpg"]);
        let c = cfg(
            tmp.path(),
            vec![
                output(&dir, vec![], vec![]),
                output(&dir, vec![step("*", "Metadata")], vec![]),
            ],
        );
        let p = local(&c, vec![(entry(tmp.path(), "phone", "a.jpg"), to("t"))]);
        assert_eq!(
            ops(&p.items[0].outputs),
            [
                (OutputOp::Keep, dir.join("a.jpg")),
                (OutputOp::Write, dir.join("a_1.jpg")),
            ]
        );
    }

    /// `phone/<name>` with `data`, and its scan entry.
    fn source_file(root: &Path, name: &str, data: &[u8]) -> FileEntry {
        let dir = phone_with(root, &[]);
        std::fs::write(dir.join(name), data).unwrap();
        FileEntry {
            size: data.len() as u64,
            ..entry(root, "phone", name)
        }
    }

    #[test]
    fn plan_existing_backup_copy_is_not_written_again() {
        let tmp = tempfile::tempdir().unwrap();
        let name = "20260412_103314.jpg";
        let e = source_file(tmp.path(), name, b"1234567");
        let dir = tmp.path().join("phone");
        let backup = tmp.path().join("backup");
        std::fs::create_dir_all(&backup).unwrap();
        // Same size, other content: not a copy of this source.
        std::fs::write(backup.join("other.jpg"), b"7654321").unwrap();
        let c = cfg(
            tmp.path(),
            vec![
                output(&dir, vec![], vec![step("*", "capture-date")]),
                output(&backup, vec![], vec![step("*", "capture-date")]),
            ],
        );
        let p = local(&c, vec![(e.clone(), to("t"))]);
        assert_eq!(
            ops(&p.items[0].outputs),
            [
                (OutputOp::Keep, dir.join(name)),
                (OutputOp::Write, backup.join(name)),
            ]
        );
        // Once the copy is there (under any name), the item has nothing to do.
        std::fs::write(backup.join("20260412_103314_1.jpg"), b"1234567").unwrap();
        let p = local(&c, vec![(e.clone(), to("t"))]);
        assert!(p.items.is_empty(), "{:?}", p.items);
        assert_eq!(p.unchanged, vec![e.path]);
    }

    #[test]
    fn plan_server_existing_backup_copy_is_seen_in_the_local_mirror() {
        // Syncthing mirrors the server's folders: the local `dir` of an
        // output shows what its `server_dir` holds.
        let tmp = tempfile::tempdir().unwrap();
        let name = "20260412_103314.jpg";
        let e = source_file(tmp.path(), name, b"1234567");
        let backup = tmp.path().join("backup");
        std::fs::create_dir_all(&backup).unwrap();
        let mut c = cfg(
            tmp.path(),
            vec![
                Output {
                    dir: tmp.path().join("phone").to_str().unwrap().into(),
                    server_dir: Some("/srv/phone".into()),
                    content: vec![],
                    rename: vec![step("*", "capture-date")],
                },
                Output {
                    dir: backup.to_str().unwrap().into(),
                    server_dir: Some("/srv/backup".into()),
                    content: vec![],
                    rename: vec![],
                },
            ],
        );
        c.sources[0].server_path = Some("/srv/phone".into());
        let server = |c: &Config| {
            build_plan(c, vec![(e.clone(), to("t"))], &HashMap::new(), Side::Server).unwrap()
        };
        let p = server(&c);
        assert_eq!(
            ops(&p.items[0].outputs),
            [
                (OutputOp::Keep, PathBuf::from("/srv/phone").join(name)),
                (OutputOp::Write, PathBuf::from("/srv/backup").join(name)),
            ]
        );
        std::fs::write(backup.join(name), b"1234567").unwrap();
        let p = server(&c);
        assert!(p.items.is_empty(), "{:?}", p.items);
        assert_eq!(p.unchanged, vec![PathBuf::from("/srv/phone").join(name)]);
    }

    #[test]
    fn plan_existing_copy_does_not_drop_a_write_of_a_moved_source() {
        // Without an output that keeps the source, a copy is always written:
        // the source goes to the trash afterwards.
        let tmp = tempfile::tempdir().unwrap();
        let e = source_file(tmp.path(), "a.jpg", b"1234567");
        let backup = tmp.path().join("backup");
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(backup.join("a.jpg"), b"1234567").unwrap();
        let c = cfg(tmp.path(), vec![output(&backup, vec![], vec![])]);
        let p = local(&c, vec![(e, to("t"))]);
        assert_eq!(
            ops(&p.items[0].outputs),
            [(OutputOp::Write, backup.join("a_1.jpg"))]
        );
    }

    #[test]
    #[cfg_attr(
        any(target_os = "macos", windows),
        ignore = "case-insensitive file system: the case-only target is the file itself"
    )]
    fn plan_case_only_rename_keeps_the_name() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = phone_with(tmp.path(), &["IMG.JPG"]);
        let c = cfg(
            tmp.path(),
            vec![output(&dir, vec![], vec![step("*", "ext-lower")])],
        );
        let p = local(&c, vec![(entry(tmp.path(), "phone", "IMG.JPG"), to("t"))]);
        assert_eq!(
            ops(&p.items[0].outputs),
            [(OutputOp::Rename, dir.join("IMG.jpg"))]
        );
    }

    #[test]
    fn plan_server_same_dir_sees_through_tilde() {
        let tmp = tempfile::tempdir().unwrap();
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let mut c = cfg(
            tmp.path(),
            vec![Output {
                dir: "/local".into(),
                // The source's server folder, spelled without `~`.
                server_dir: Some(home.join("srv-shots").to_str().unwrap().into()),
                content: vec![],
                rename: vec![step("*", "capture-date")],
            }],
        );
        c.sources[0].server_path = Some("~/srv-shots".into());
        let p = build_plan(
            &c,
            vec![(
                entry(tmp.path(), "phone", "IMG_20260412_103314.jpg"),
                to("t"),
            )],
            &HashMap::new(),
            Side::Server,
        )
        .unwrap();
        assert_eq!(
            ops(&p.items[0].outputs),
            [(
                OutputOp::Rename,
                PathBuf::from("~/srv-shots/20260412_103314.jpg")
            )]
        );
    }

    #[test]
    fn plan_content_step_in_same_dir_is_write() {
        let tmp = tempfile::tempdir().unwrap();
        let shot = "IMG_20260412_103314.jpg";
        let dir = phone_with(tmp.path(), &[shot]);
        let c = cfg(
            tmp.path(),
            vec![output(
                &dir,
                vec![step("*", "Metadata")],
                vec![step("*", "capture-date")],
            )],
        );
        let p = local(&c, vec![(entry(tmp.path(), "phone", shot), to("t"))]);
        assert_eq!(
            ops(&p.items[0].outputs),
            [(OutputOp::Write, dir.join("20260412_103314.jpg"))]
        );
    }

    #[test]
    fn plan_move_into_subfolder_of_own_source_is_rename() {
        let tmp = tempfile::tempdir().unwrap();
        let shot = "IMG_20260412_103314.JPG";
        let dir = phone_with(tmp.path(), &[shot]);
        let c = cfg(
            tmp.path(),
            vec![output(
                &dir,
                vec![step("*.png", "Metadata")],
                vec![step("*", "capture-date-dirs"), step("*", "ext-lower")],
            )],
        );
        let p = local(&c, vec![(entry(tmp.path(), "phone", shot), to("t"))]);
        let o = &p.items[0].outputs[0];
        assert_eq!(
            (o.op, &o.dest),
            (OutputOp::Rename, &dir.join("2026/04/20260412_103314.jpg"))
        );
        let after: Vec<&str> = o.trace.iter().map(|s| s.after.as_str()).collect();
        assert_eq!(
            after,
            [
                "IMG_20260412_103314.JPG",
                "2026/04/20260412_103314.jpg",
                "2026/04/20260412_103314.jpg"
            ]
        );
    }

    #[test]
    fn plan_dir_template_and_name_subdirs_combine() {
        let tmp = tempfile::tempdir().unwrap();
        let shot = "IMG_20260412_103314.jpg";
        let dir = phone_with(tmp.path(), &[shot]);
        let c = cfg(
            tmp.path(),
            vec![output(
                &dir.join("{source}"),
                vec![],
                vec![step("*", "capture-date-dirs")],
            )],
        );
        let p = local(&c, vec![(entry(tmp.path(), "phone", shot), to("t"))]);
        assert_eq!(
            ops(&p.items[0].outputs),
            [(
                OutputOp::Rename,
                dir.join("phone/2026/04/20260412_103314.jpg")
            )]
        );
    }

    #[test]
    fn plan_subfolder_outside_source_root_is_write() {
        let tmp = tempfile::tempdir().unwrap();
        let shot = "IMG_20260412_103314.jpg";
        phone_with(tmp.path(), &[shot]);
        // A sibling whose name only starts like the source's.
        let near = tmp.path().join("phone-archive");
        let c = cfg(
            tmp.path(),
            vec![output(&near, vec![], vec![step("*", "capture-date-dirs")])],
        );
        let p = local(&c, vec![(entry(tmp.path(), "phone", shot), to("t"))]);
        assert_eq!(
            ops(&p.items[0].outputs),
            [(OutputOp::Write, near.join("2026/04/20260412_103314.jpg"))]
        );
    }

    #[test]
    fn plan_subfolder_with_content_step_is_write() {
        let tmp = tempfile::tempdir().unwrap();
        let shot = "IMG_20260412_103314.jpg";
        let dir = phone_with(tmp.path(), &[shot]);
        let c = cfg(
            tmp.path(),
            vec![output(
                &dir,
                vec![step("*", "Metadata")],
                vec![step("*", "capture-date-dirs")],
            )],
        );
        let p = local(&c, vec![(entry(tmp.path(), "phone", shot), to("t"))]);
        assert_eq!(
            ops(&p.items[0].outputs),
            [(OutputOp::Write, dir.join("2026/04/20260412_103314.jpg"))]
        );
    }

    #[test]
    fn plan_subfolder_collision_gets_the_next_name() {
        let tmp = tempfile::tempdir().unwrap();
        // The source already has the name; the subfolder holds another file
        // of that name in another case.
        let name = "20260412_103314.jpg";
        let dir = phone_with(tmp.path(), &[name]);
        let sub = dir.join("2026/04");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("20260412_103314.JPG"), b"other").unwrap();
        let c = cfg(
            tmp.path(),
            vec![output(&dir, vec![], vec![step("*", "capture-date-dirs")])],
        );
        let p = local(&c, vec![(entry(tmp.path(), "phone", name), to("t"))]);
        assert_eq!(
            ops(&p.items[0].outputs),
            [(OutputOp::Rename, sub.join("20260412_103314_1.jpg"))]
        );
    }

    #[test]
    fn plan_recursive_rescan_of_moved_files_is_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = phone_with(tmp.path(), &[]);
        let mut c = cfg(
            tmp.path(),
            vec![output(&dir, vec![], vec![step("*", "capture-date-dirs")])],
        );
        c.sources[0].recursive = true;
        let mut routed = Vec::new();
        for rel in [
            "2026/04/20260412_103314.jpg",
            "2025/12/20251231_235959.png",
            "nodate.jpg",
        ] {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"x").unwrap();
            let e = FileEntry {
                path,
                rel: rel.into(),
                ..entry(tmp.path(), "phone", "x")
            };
            routed.push((e, to("t")));
        }
        let p = local(&c, routed);
        assert!(p.items.is_empty(), "{:?}", p.items);
        assert_eq!(p.unchanged.len(), 3);
    }

    #[test]
    fn plan_server_subfolder_of_server_source_is_rename() {
        let tmp = tempfile::tempdir().unwrap();
        let shot = "IMG_20260412_103314.jpg";
        let server = |server_dir: &str| {
            let mut c = cfg(
                tmp.path(),
                vec![Output {
                    dir: "/local".into(),
                    server_dir: Some(server_dir.into()),
                    content: vec![],
                    rename: vec![step("*", "capture-date-dirs")],
                }],
            );
            c.sources[0].server_path = Some("/srv/phone".into());
            let p = build_plan(
                &c,
                vec![(entry(tmp.path(), "phone", shot), to("t"))],
                &HashMap::new(),
                Side::Server,
            )
            .unwrap();
            ops(&p.items[0].outputs)
        };
        assert_eq!(
            server("/srv/phone/"),
            [(
                OutputOp::Rename,
                PathBuf::from("/srv/phone/2026/04/20260412_103314.jpg")
            )]
        );
        assert_eq!(
            server("/srv/other"),
            [(
                OutputOp::Write,
                PathBuf::from("/srv/other/2026/04/20260412_103314.jpg")
            )]
        );
    }

    #[test]
    fn plan_bad_rendered_path_sends_only_that_file_to_review() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        let mut c = cfg(
            tmp.path(),
            vec![output(&out, vec![], vec![step("*", "By extension")])],
        );
        // Fine for `a.jpg`; an extensionless file renders `/README`.
        c.name_presets.insert(
            "By extension".into(),
            NamePreset {
                mask: None,
                template: "{ext}/{stem}".into(),
                date: DateSource::None,
                skip_if: None,
                normalize_ext: false,
            },
        );
        let bad = entry(tmp.path(), "phone", "README");
        let p = local(
            &c,
            vec![
                (entry(tmp.path(), "phone", "a.jpg"), to("t")),
                (bad.clone(), to("t")),
                (entry(tmp.path(), "cam", "b.png"), to("t")),
            ],
        );
        let dests: Vec<_> = p.items.iter().map(|i| i.outputs[0].dest.clone()).collect();
        assert_eq!(dests, [out.join("jpg/a"), out.join("png/b")]);
        assert_eq!(p.review.len(), 1, "{:?}", p.review);
        let r = &p.review[0];
        assert_eq!(r.source, bad.path);
        assert_eq!(r.suggested, "t");
        assert!(r.reason.contains("By extension"), "{}", r.reason);
        assert!(r.name_error);
    }

    #[test]
    fn review_items_record_capture_time_and_source_root() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        let mut c = cfg(
            tmp.path(),
            vec![output(&out, vec![], vec![step("*", "By extension")])],
        );
        c.name_presets.insert(
            "By extension".into(),
            NamePreset {
                mask: None,
                template: "{ext}/{stem}".into(),
                date: DateSource::None,
                skip_if: None,
                normalize_ext: false,
            },
        );
        let review = |why: &str| Decision::Review {
            suggested: "t".into(),
            reason: why.into(),
        };
        let p = local(
            &c,
            vec![
                (
                    entry(tmp.path(), "phone", "IMG_20260412_103314.jpg"),
                    review("low"),
                ),
                (entry(tmp.path(), "cam", "undated.jpg"), review("low")),
                // A name error: no extension for `{ext}`.
                (entry(tmp.path(), "cam", "VID_20260412_103400"), to("t")),
            ],
        );
        assert_eq!(p.review.len(), 3, "{:?}", p.review);
        let r = &p.review[0];
        assert_eq!(r.captured.as_deref(), Some("2026-04-12T10:33:14"));
        assert_eq!(
            r.source_root.as_deref(),
            Some(tmp.path().join("phone").as_path())
        );
        assert_eq!(p.review[1].captured, None);
        assert_eq!(p.review[1].source_root, Some(tmp.path().join("cam")));
        assert_eq!(p.review[2].captured.as_deref(), Some("2026-04-12T10:34:00"));
        assert_eq!(p.review[2].source_root, Some(tmp.path().join("cam")));

        // Plans written before these fields load; absent ones stay absent.
        let old: ReviewItem = serde_json::from_value(serde_json::json!({
            "source": "/s/a.jpg", "size": 1, "mtime_ns": 2,
            "suggested": "t", "reason": "r"
        }))
        .unwrap();
        assert_eq!((old.captured, old.source_root), (None, None));
        assert!(!old.name_error);
        let v = serde_json::to_value(&p.review[1]).unwrap();
        assert!(v.get("captured").is_none(), "{v}");
    }

    #[test]
    #[cfg_attr(
        any(target_os = "macos", windows),
        ignore = "case-insensitive file system: the case-only target is the file itself"
    )]
    fn plan_dest_that_is_the_source_under_another_name_is_keep() {
        // A case-insensitive file system: `IMG.jpg` opens `IMG.JPG`. Two
        // names of one file stand in for it here.
        let tmp = tempfile::tempdir().unwrap();
        let dir = phone_with(tmp.path(), &["IMG.JPG"]);
        std::fs::hard_link(dir.join("IMG.JPG"), dir.join("IMG.jpg")).unwrap();
        let c = cfg(
            tmp.path(),
            vec![output(&dir, vec![], vec![step("*", "ext-lower")])],
        );
        let e = entry(tmp.path(), "phone", "IMG.JPG");
        let p = local(&c, vec![(e.clone(), to("t"))]);
        assert!(p.items.is_empty(), "{:?}", p.items);
        assert_eq!(p.unchanged, vec![e.path]);
    }

    #[test]
    fn plan_server_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = cfg(
            tmp.path(),
            vec![Output {
                dir: "/local/{YYYY}".into(),
                server_dir: Some("/srv/gallery/{YYYY}".into()),
                content: vec![],
                rename: vec![],
            }],
        );
        c.sources[0].server_path = Some("/srv/phone".into());
        let e = entry(tmp.path(), "phone", "IMG_20260412_103314.jpg");
        let review = FileEntry {
            rel: "sub/r.jpg".into(),
            path: tmp.path().join("phone/sub/r.jpg"),
            ..e.clone()
        };
        let p = build_plan(
            &c,
            vec![
                (e, to("t")),
                (
                    review,
                    Decision::Review {
                        suggested: "t".into(),
                        reason: "why".into(),
                    },
                ),
            ],
            &HashMap::new(),
            Side::Server,
        )
        .unwrap();
        assert_eq!(p.side, Side::Server);
        let item = &p.items[0];
        assert_eq!(
            item.source,
            PathBuf::from("/srv/phone/IMG_20260412_103314.jpg")
        );
        assert_eq!(item.source_root, PathBuf::from("/srv/phone"));
        assert_eq!(
            item.outputs[0].dest,
            PathBuf::from("/srv/gallery/2026/IMG_20260412_103314.jpg")
        );
        assert_eq!(p.review[0].source, PathBuf::from("/srv/phone/sub/r.jpg"));
        assert_eq!(p.review[0].suggested, "t");
    }

    #[test]
    fn plan_server_reports_all_missing_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        let c = cfg(tmp.path(), vec![output(&out, vec![], vec![])]);
        let r = build_plan(
            &c,
            vec![
                (entry(tmp.path(), "phone", "a.jpg"), to("t")),
                (entry(tmp.path(), "cam", "b.jpg"), to("t")),
            ],
            &HashMap::new(),
            Side::Server,
        );
        match r {
            Err(Error::Plan(m)) => {
                assert!(m.contains("phone"), "{m}");
                assert!(m.contains("cam"), "{m}");
                assert!(m.contains("server_dir"), "{m}");
            }
            other => panic!("expected Plan error, got {other:?}"),
        }
    }

    #[test]
    fn plan_series_split_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        let mut c = cfg(tmp.path(), vec![output(&out, vec![], vec![])]);
        let mut u = c.targets.after_ai[0].clone();
        u.name = "u".into();
        c.targets.after_ai.push(u);
        let a = entry(tmp.path(), "phone", "IMG_20260412_103314.jpg");
        let b = entry(tmp.path(), "phone", "IMG_20260412_103400.jpg");
        let far = entry(tmp.path(), "phone", "IMG_20260412_120000.jpg");
        let same = entry(tmp.path(), "phone", "IMG_20260412_103410.jpg");
        let p = local(
            &c,
            vec![
                (far.clone(), to("t")),
                (same.clone(), to("u")),
                (b.clone(), to("u")),
                (a.clone(), to("t")),
            ],
        );
        assert_eq!(
            p.warnings,
            vec![Warning::SeriesSplit {
                a: a.path,
                b: b.path
            }]
        );
    }

    #[test]
    fn plan_json_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        let c = cfg(
            tmp.path(),
            vec![output(
                &out,
                vec![step("*.jpg", "AVIF"), step("*.avif", "AVIF")],
                vec![step("*", "capture-date")],
            )],
        );
        let p = build_plan(
            &c,
            vec![
                (
                    entry(tmp.path(), "phone", "IMG_20260412_103314.jpg"),
                    Decision::Target {
                        name: "t".into(),
                        via: Via::Ai {
                            reason: "cat".into(),
                            low: false,
                        },
                    },
                ),
                (
                    entry(tmp.path(), "cam", "r.jpg"),
                    Decision::Review {
                        suggested: "t".into(),
                        reason: "why".into(),
                    },
                ),
                (entry(tmp.path(), "cam", "u.txt"), Decision::Unmatched),
                (entry(tmp.path(), "cam", "x.jpg"), Decision::Excluded),
                (entry(tmp.path(), "cam", "k.jpg"), Decision::Kept),
            ],
            &HashMap::new(),
            Side::Local,
        )
        .unwrap();
        let path = tmp.path().join("plan.json");
        p.save(&path).unwrap();
        let q = Plan::load(&path).unwrap();
        assert_eq!(q.items, p.items);
        assert_eq!(q.review, p.review);
        assert_eq!(q.unmatched, vec![tmp.path().join("cam/u.txt")]);
        // Rejected by an exclude mask only, or kept by a manual decision:
        // nothing to do, not unmatched.
        assert_eq!(
            q.unchanged,
            vec![tmp.path().join("cam/x.jpg"), tmp.path().join("cam/k.jpg")]
        );
        assert_eq!(q.warnings, p.warnings);
        assert_eq!(
            (q.version, q.side, &q.created),
            (1, Side::Local, &p.created)
        );
        // The planning machine's zone is recorded and read back; a plan
        // written before the field existed still loads.
        let zone = p.tz.clone().expect("this machine's zone");
        assert!(zone.parse::<chrono_tz::Tz>().is_ok(), "{zone}");
        assert_eq!(q.tz, p.tz);
        let mut v = serde_json::to_value(&q).unwrap();
        v.as_object_mut().unwrap().remove("tz");
        std::fs::write(&path, v.to_string()).unwrap();
        assert_eq!(Plan::load(&path).unwrap().tz, None);
        // Older plans have no output op (all were writes) and no `unchanged`.
        let mut v = serde_json::to_value(&q).unwrap();
        v.as_object_mut().unwrap().remove("unchanged");
        for it in v["items"].as_array_mut().unwrap() {
            for o in it["outputs"].as_array_mut().unwrap() {
                o.as_object_mut().unwrap().remove("op");
            }
        }
        std::fs::write(&path, v.to_string()).unwrap();
        let old = Plan::load(&path).unwrap();
        assert!(old.unchanged.is_empty());
        assert_eq!(old.items[0].outputs[0].op, OutputOp::Write);
        let json = serde_json::to_value(&q).unwrap();
        assert_eq!(json["items"][0]["outputs"][0]["op"], "write");
        assert_eq!(
            serde_json::to_value(&q.content_presets).unwrap(),
            serde_json::to_value(&p.content_presets).unwrap()
        );
    }

    #[test]
    fn plan_load_rejects_other_version() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = local(&cfg(tmp.path(), vec![]), vec![]);
        p.version = 2;
        let path = tmp.path().join("plan.json");
        p.save(&path).unwrap();
        assert!(matches!(Plan::load(&path), Err(Error::Plan(_))));
    }

    #[test]
    fn plan_reports_one_event_per_file_and_stops_on_cancel() {
        use crate::progress::testing::Recorder;
        use crate::progress::{Cancel, NoProgress, Stage};
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        let mut c = cfg(&out, vec![output(&out, vec![], vec![])]);
        c.sources[0].path = tmp.path().join("phone");
        let routed = vec![
            (entry(tmp.path(), "phone", "a.jpg"), to("t")),
            (entry(tmp.path(), "phone", "b.jpg"), Decision::Unmatched),
        ];
        let rec = Recorder::default();
        let p = build_plan_with(
            &c,
            routed.clone(),
            &HashMap::new(),
            Side::Local,
            &rec,
            &Cancel::new(),
        )
        .unwrap();
        assert_eq!((p.items.len(), p.unmatched.len()), (1, 1));
        let evs = rec.events(Stage::Plan);
        let pairs: Vec<(usize, usize)> = evs.iter().map(|e| (e.done, e.total)).collect();
        assert_eq!(pairs, [(1, 2), (2, 2)]);

        let cancel = Cancel::new();
        cancel.cancel();
        let r = build_plan_with(
            &c,
            routed,
            &HashMap::new(),
            Side::Local,
            &NoProgress,
            &cancel,
        );
        assert!(matches!(r, Err(Error::Cancelled)), "{:?}", r.map(|_| ()));
    }

    fn subplan_item(path: &Path, size: u64, mtime_ns: i128) -> PlanItem {
        PlanItem {
            source: path.to_path_buf(),
            source_root: path.parent().unwrap().to_path_buf(),
            size,
            mtime_ns,
            target: "t".into(),
            via: Via::Mask,
            outputs: vec![PlannedOutput {
                dest: path.with_extension("out.jpg"),
                op: OutputOp::Write,
                steps: vec!["AVIF".into()],
                trace: vec![],
            }],
        }
    }

    #[test]
    fn subplan_keeps_failed_items_whose_sources_are_unchanged() {
        use crate::apply::{ItemResult, ItemStatus, Report};
        use crate::scan::mtime_ns;
        use crate::trash::TrashRef;
        let tmp = tempfile::tempdir().unwrap();
        let mk = |name: &str| {
            let p = tmp.path().join(name);
            std::fs::write(&p, b"0123456789").unwrap();
            let m = std::fs::metadata(&p).unwrap();
            subplan_item(&p, m.len(), mtime_ns(&m).unwrap())
        };
        let failed = mk("failed.jpg");
        let done = mk("done.jpg");
        let grown = mk("grown.jpg");
        let touched = mk("touched.jpg");
        let gone = mk("gone.jpg");
        let stale = mk("stale.jpg");
        let not_started = mk("not_started.jpg");
        // Its source went to the trash; the path holds the processed in-place
        // output, which happens to have the planned size and mtime.
        let trashed = mk("trashed.jpg");
        // Renamed by apply before failing: the path is free or reused.
        let renamed = mk("renamed.jpg");
        let dir = mk("dir.jpg");
        std::fs::write(&grown.source, b"0123456789+").unwrap();
        let mut t = touched.clone();
        t.mtime_ns -= 1_000_000_000;
        let touched = t;
        std::fs::remove_file(&gone.source).unwrap();
        std::fs::remove_file(&dir.source).unwrap();
        std::fs::create_dir(&dir.source).unwrap();

        let mut presets = BTreeMap::new();
        presets.insert(
            "AVIF".to_string(),
            ContentPreset::Photo(PhotoPreset::avif_high()),
        );
        let plan = Plan {
            version: PLAN_VERSION,
            created: "2026-11-01T00:00:00+00:00".into(),
            side: Side::Local,
            tz: Some("Europe/Berlin".into()),
            content_presets: presets.clone(),
            items: vec![
                failed.clone(),
                done.clone(),
                grown.clone(),
                touched.clone(),
                gone.clone(),
                stale.clone(),
                not_started.clone(),
                trashed.clone(),
                renamed.clone(),
                dir.clone(),
            ],
            review: vec![ReviewItem {
                source: tmp.path().join("r.jpg"),
                size: 1,
                mtime_ns: 0,
                suggested: "t".into(),
                reason: "?".into(),
                captured: None,
                source_root: None,
                name_error: false,
            }],
            unmatched: vec![tmp.path().join("u.txt")],
            warnings: vec![Warning::MissingServerPath { what: "x".into() }],
            unchanged: vec![tmp.path().join("k.jpg")],
            retry_of: None,
        };
        let result = |it: &PlanItem, status: ItemStatus| ItemResult {
            source: it.source.clone(),
            status,
            outputs: vec![],
            error: Some("boom".into()),
            elapsed_ms: None,
        };
        let fail = ItemStatus::Failed { trashed: None };
        let report = Report {
            run_id: "20261101-000000".into(),
            plan: tmp.path().join("plan.json"),
            started: String::new(),
            finished: String::new(),
            items: vec![
                result(&failed, fail.clone()),
                result(&done, ItemStatus::Done { trashed: None }),
                result(&grown, fail.clone()),
                result(&touched, fail.clone()),
                result(&gone, fail.clone()),
                result(&stale, ItemStatus::Stale),
                result(
                    &trashed,
                    ItemStatus::Failed {
                        trashed: Some(TrashRef::Folder {
                            path: tmp.path().join(".trash/trashed.jpg"),
                        }),
                    },
                ),
                ItemResult {
                    outputs: vec![crate::apply::OutputResult {
                        planned: tmp.path().join("renamed2.jpg"),
                        actual: Some(tmp.path().join("renamed2.jpg")),
                        error: None,
                        warnings: vec![],
                        fell_back: false,
                        size: None,
                        hash: None,
                        renamed_from: Some(renamed.source.clone()),
                        anomalies: Vec::new(),
                    }],
                    ..result(&renamed, fail.clone())
                },
                result(&dir, fail.clone()),
            ],
            tool_versions: BTreeMap::new(),
            tool_warnings: vec![],
            undone: false,
            undone_sources: vec![],
            cancelled: true,
            stopped_by_anomalies: false,
        };
        let sub = subplan(&plan, &report);
        assert_eq!(sub.items, vec![failed]);
        assert_eq!(sub.side, Side::Local);
        assert_eq!(sub.tz, plan.tz);
        assert_eq!(sub.version, PLAN_VERSION);
        assert_eq!(
            sub.content_presets.keys().collect::<Vec<_>>(),
            presets.keys().collect::<Vec<_>>()
        );
        assert!(sub.review.is_empty());
        assert!(sub.unmatched.is_empty());
        assert!(sub.unchanged.is_empty());
        assert!(sub.warnings.is_empty());

        let mut server = plan.clone();
        server.side = Side::Server;
        assert_eq!(subplan(&server, &report).side, Side::Server);
    }

    #[test]
    fn load_hints_at_updating_on_an_unknown_variant() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("plan.json");
        let mut item = subplan_item(&tmp.path().join("a.jpg"), 1, 0);
        item.via = Via::Manual;
        let plan = Plan {
            version: PLAN_VERSION,
            created: String::new(),
            side: Side::Server,
            tz: None,
            content_presets: BTreeMap::new(),
            items: vec![item],
            review: vec![],
            unmatched: vec![],
            warnings: vec![],
            unchanged: vec![],
            retry_of: None,
        };
        // As an older binary sees it: a `via` kind it does not know.
        let text = serde_json::to_string(&plan)
            .unwrap()
            .replace(r#""kind":"manual""#, r#""kind":"from_the_future""#);
        std::fs::write(&path, text).unwrap();
        let e = Plan::load(&path).unwrap_err().to_string();
        assert!(e.contains("from_the_future"), "{e}");
        assert!(e.contains("update gallery"), "{e}");
        // Other parse errors carry no such hint.
        std::fs::write(&path, "{").unwrap();
        let e = Plan::load(&path).unwrap_err().to_string();
        assert!(!e.contains("update"), "{e}");
    }

    fn pilot_item(target: &str, name: &str) -> PlanItem {
        PlanItem {
            source: PathBuf::from(format!("/src/{target}/{name}")),
            source_root: PathBuf::from("/src"),
            size: 1,
            mtime_ns: 1,
            target: target.into(),
            via: Via::Mask,
            outputs: vec![PlannedOutput {
                dest: PathBuf::from(format!("/out/{target}/{name}")),
                op: OutputOp::Write,
                steps: vec![],
                trace: vec![],
            }],
        }
    }

    fn pilot_plan(items: Vec<PlanItem>) -> Plan {
        let mut content_presets = BTreeMap::new();
        content_presets.insert(
            "M".to_string(),
            ContentPreset::Metadata(MetadataPreset::full()),
        );
        Plan {
            version: PLAN_VERSION,
            created: String::new(),
            side: Side::Server,
            tz: Some("Europe/Berlin".into()),
            content_presets,
            items,
            review: vec![],
            unmatched: vec![PathBuf::from("/x")],
            warnings: vec![],
            unchanged: vec![PathBuf::from("/y")],
            retry_of: None,
        }
    }

    fn sources(p: &Plan) -> Vec<String> {
        p.items
            .iter()
            .map(|i| i.source.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn pilot_takes_n_per_target_preferring_extension_variety() {
        let plan = pilot_plan(vec![
            pilot_item("a", "1.jpg"),
            pilot_item("a", "2.jpg"),
            pilot_item("a", "3.JPG"),
            pilot_item("a", "4.mp4"),
            pilot_item("a", "5.png"),
            pilot_item("b", "6.jpg"),
            pilot_item("b", "7.jpg"),
        ]);
        let (p, rest) = pilot(&plan, 3);
        // Target a: one of each extension first; b has only two items.
        assert_eq!(sources(&p), ["1.jpg", "4.mp4", "5.png", "6.jpg", "7.jpg"]);
        assert_eq!(sources(&rest), ["2.jpg", "3.JPG"]);
        let (p, _) = pilot(&plan, 4);
        // The fourth slot of a goes to a repeated extension, in plan order.
        assert_eq!(
            sources(&p),
            ["1.jpg", "2.jpg", "4.mp4", "5.png", "6.jpg", "7.jpg"]
        );
    }

    #[test]
    fn pilot_and_rest_cover_every_item_once_and_keep_the_settings() {
        let items: Vec<PlanItem> = (0..23)
            .map(|i| {
                pilot_item(
                    ["a", "b", "c"][i % 3],
                    &format!("{i}.{}", ["jpg", "mp4"][i % 2]),
                )
            })
            .collect();
        let plan = pilot_plan(items);
        for n in [0, 1, 2, 5, 100] {
            let (p, rest) = pilot(&plan, n);
            let mut all = sources(&p);
            all.extend(sources(&rest));
            all.sort();
            let mut want = sources(&plan);
            want.sort();
            assert_eq!(all, want, "n={n}");
            for part in [&p, &rest] {
                assert_eq!(part.side, plan.side);
                assert_eq!(part.tz, plan.tz);
                assert_eq!(
                    part.content_presets.keys().collect::<Vec<_>>(),
                    plan.content_presets.keys().collect::<Vec<_>>()
                );
            }
            for t in ["a", "b", "c"] {
                let count = p.items.iter().filter(|i| i.target == t).count();
                assert_eq!(
                    count,
                    n.min(plan.items.iter().filter(|i| i.target == t).count())
                );
            }
            // Informational lists stay with the rest.
            assert!(p.unmatched.is_empty() && p.unchanged.is_empty());
            assert_eq!(rest.unmatched, plan.unmatched);
        }
    }

    #[test]
    fn pilot_keeps_rename_and_keep_items() {
        let mut keep = pilot_item("a", "1.jpg");
        keep.outputs[0].op = OutputOp::Keep;
        let mut ren = pilot_item("a", "2.jpg");
        ren.outputs[0].op = OutputOp::Rename;
        let (p, rest) = pilot(&pilot_plan(vec![keep, ren]), 1);
        assert_eq!(p.items.len(), 1);
        assert_eq!(rest.items.len(), 1);
    }

    fn bundle(steps: Vec<PipeStep>) -> ContentPreset {
        ContentPreset::Bundle(crate::config::BundlePreset { mask: None, steps })
    }

    /// Plan `names` through one output with a `content` pipe; per file the
    /// matched step names and the destination file name.
    fn plan_names(c: &Config, tmp: &Path, names: &[&str]) -> (Plan, Vec<(Vec<String>, String)>) {
        let routed = names
            .iter()
            .map(|n| (entry(tmp, "phone", n), to("t")))
            .collect();
        let p = local(c, routed);
        let got = p
            .items
            .iter()
            .map(|i| {
                let o = &i.outputs[0];
                (
                    o.steps.clone(),
                    o.dest.file_name().unwrap().to_string_lossy().into_owned(),
                )
            })
            .collect();
        (p, got)
    }

    #[test]
    fn bundle_expands_in_place_and_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        let mut c = cfg(
            tmp.path(),
            vec![output(
                &out,
                vec![
                    PipeStep::named("Metadata"),
                    PipeStep::named("pack"),
                    PipeStep::named("Unused"),
                ],
                vec![],
            )],
        );
        c.content_presets.insert(
            "pack".into(),
            bundle(vec![PipeStep::named("AVIF"), PipeStep::named("Metadata")]),
        );
        let (p, got) = plan_names(&c, tmp.path(), &["a.jpg"]);
        assert_eq!(got[0].0, ["Metadata", "AVIF", "Metadata"]);
        let t = &p.items[0].outputs[0].trace;
        let tr: Vec<_> = t.iter().map(|s| (s.index, s.bundle.as_deref())).collect();
        assert_eq!(
            tr,
            [(0, None), (1, Some("pack")), (2, Some("pack")), (3, None)]
        );
        // The plan stores expanded presets only, never the bundle.
        assert!(!p.content_presets.contains_key("pack"));
        assert!(p.content_presets.contains_key("AVIF"));
    }

    #[test]
    fn bundle_step_mask_override_wins_over_the_preset_mask() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        let mut c = cfg(
            tmp.path(),
            vec![output(&out, vec![PipeStep::named("pack")], vec![])],
        );
        c.content_presets.insert(
            "pack".into(),
            bundle(vec![step("*.png", "AVIF"), PipeStep::named("Unused")]),
        );
        let (p, got) = plan_names(&c, tmp.path(), &["a.png", "b.jpg"]);
        assert_eq!(got[0], (vec!["AVIF".to_string()], "a.avif".to_string()));
        // The `*.png` override keeps the jpg out (the preset's own mask takes it).
        assert!(got[1].0.is_empty());
        assert!(p.warnings.is_empty());
    }

    #[test]
    fn nested_bundles_expand_and_a_cycle_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        let mut c = cfg(
            tmp.path(),
            vec![output(&out, vec![PipeStep::named("outer")], vec![])],
        );
        c.content_presets.insert(
            "inner".into(),
            bundle(vec![PipeStep::named("Metadata"), PipeStep::named("AVIF")]),
        );
        c.content_presets.insert(
            "outer".into(),
            bundle(vec![PipeStep::named("inner"), PipeStep::named("Unused")]),
        );
        c.check_bundles().unwrap();
        let (_, got) = plan_names(&c, tmp.path(), &["a.jpg"]);
        assert_eq!(got[0].0, ["Metadata", "AVIF"]);

        c.content_presets
            .insert("inner".into(), bundle(vec![PipeStep::named("outer")]));
        let e = c.check_bundles().unwrap_err();
        assert!(e.contains("bundle cycle: "), "{e}");
        let e = build_plan(
            &c,
            vec![(entry(tmp.path(), "phone", "a.jpg"), to("t"))],
            &HashMap::new(),
            Side::Local,
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("bundle cycle"), "{e}");
    }

    #[test]
    fn compress_all_gives_each_file_type_exactly_one_encoder() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        let c = cfg(
            tmp.path(),
            vec![output(&out, vec![PipeStep::named("compress-all")], vec![])],
        );
        c.check_bundles().unwrap();
        let (p, got) = plan_names(
            &c,
            tmp.path(),
            &["a.png", "b.jpg", "c.webp", "d.mp4", "e.av1.mp4", "f.txt"],
        );
        let by: Vec<_> = got.iter().map(|(s, d)| (s.join("+"), d.as_str())).collect();
        assert_eq!(
            by,
            [
                ("png-avif".to_string(), "a.avif"),
                ("avif-high".to_string(), "b.avif"),
                ("webp-avif".to_string(), "c.avif"),
                ("av1-high".to_string(), "d.av1.mp4"),
                // AV1 output is never taken again (@video-original).
                (String::new(), "e.av1.mp4"),
                (String::new(), "f.txt"),
            ]
        );
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        // JSON: expanded presets only.
        let v = serde_json::to_value(&p).unwrap();
        let keys: Vec<_> = v["content_presets"].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["av1-high", "avif-high", "png-avif", "webp-avif"]);
        let steps = &v["items"][0]["outputs"][0]["steps"];
        assert_eq!(steps, &serde_json::json!(["png-avif"]));
    }

    #[test]
    fn stacked_encoders_in_a_user_bundle_still_warn() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        let mut c = cfg(
            tmp.path(),
            vec![output(&out, vec![PipeStep::named("pack")], vec![])],
        );
        c.content_presets.insert(
            "pack".into(),
            bundle(vec![step("*.jpg", "AVIF"), step("*.avif", "AVIF")]),
        );
        let (p, _) = plan_names(&c, tmp.path(), &["a.jpg"]);
        assert!(matches!(p.warnings[0], Warning::DoubleEncode { .. }));
    }

    #[test]
    fn save_is_atomic_and_round_trips_through_parse() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trip.2026.json");
        let plan = Plan {
            version: PLAN_VERSION,
            created: "c".into(),
            side: Side::Local,
            tz: None,
            content_presets: BTreeMap::new(),
            items: vec![],
            review: vec![],
            unmatched: vec![],
            warnings: vec![],
            unchanged: vec![],
            retry_of: None,
        };
        plan.save(&path).unwrap();
        plan.save(&path).unwrap();
        let names: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("trip.2026.json")]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(Plan::parse(&text, &path).unwrap().created, "c");
    }
}
