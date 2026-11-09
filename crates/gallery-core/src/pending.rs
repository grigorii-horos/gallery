//! "Pending work": how much new work the sources hold, counted quietly.
//! Scans and routes like a scan without AI (AI targets use cached answers
//! only; a file that would need a fresh answer is counted as waiting for the
//! AI), but cheaply: a small index remembers each file's size, mtime and EXIF,
//! so only new or changed files are read by exiftool, and when nothing at all
//! changed (files, config, manual decisions, AI cache) the last answer is
//! returned without routing. Reads the sources; writes only the index.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ai::cache::Cache;
use crate::config::Config;
use crate::effect::CachedOnly;
use crate::overrides::Overrides;
use crate::plan::{Plan, Side, build_plan_with, guard_last_copies};
use crate::progress::{Cancel, NoProgress};
use crate::route::{RouteCtx, route_with_overrides};
use crate::tools::Tools;
use crate::{Result, exif, scan};

/// What waits for the user.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    /// Files a run of a fresh scan would plan (not counting files waiting
    /// for the AI).
    pub to_process: usize,
    /// Files that need a fresh AI answer, not cached yet.
    pub need_ai: usize,
    /// `to_process` per target.
    pub by_target: BTreeMap<String, usize>,
    /// How many files exiftool read this time (0: the index knew them all).
    /// Not part of the answer: only for tests and the CLI's `-v`.
    #[serde(default)]
    pub exif_reads: usize,
    /// The last answer was returned as is: nothing had changed.
    #[serde(default)]
    pub fast_path: bool,
}

impl Pending {
    /// Everything that waits: ready to run and waiting for the AI.
    pub fn total(&self) -> usize {
        self.to_process + self.need_ai
    }
}

/// Where the count keeps its state and what it reads besides the sources.
pub struct Env<'a> {
    /// For files the index does not know; `None` fails only when there are
    /// such files.
    pub tools: Option<&'a Tools>,
    /// The AI answer cache.
    pub ai_cache: &'a Path,
    /// The index file (`None`: no index, every call reads EXIF of all).
    pub index: Option<&'a Path>,
}

/// `<user cache dir>/gallery/pending-index.json`.
pub fn default_index_path() -> Option<PathBuf> {
    crate::paths::cache_dir().map(|d| d.join("pending-index.json"))
}

/// [`count`] with the user's AI cache and the default index.
pub fn count_default(
    cfg: &Config,
    overrides: &Overrides,
    tools: Option<&Tools>,
    cancel: &Cancel,
) -> Result<Pending> {
    let cache =
        crate::ai::AiClassifier::default_cache_path().unwrap_or_else(|| PathBuf::from("none"));
    let index = default_index_path();
    count(
        cfg,
        overrides,
        &Env {
            tools,
            ai_cache: &cache,
            index: index.as_deref(),
        },
        cancel,
    )
}

#[derive(Serialize, Deserialize)]
struct Known {
    size: u64,
    mtime_ns: i128,
    exif: Value,
}

#[derive(Default, Serialize, Deserialize)]
struct Index {
    version: u32,
    files: HashMap<PathBuf, Known>,
    /// The state the last answer was counted for, and the answer.
    last: Option<(String, Pending)>,
}

const INDEX_VERSION: u32 = 1;

fn load_index(path: Option<&Path>) -> Index {
    path.and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Index>(&b).ok())
        .filter(|i| i.version == INDEX_VERSION)
        .unwrap_or_else(|| Index {
            version: INDEX_VERSION,
            ..Index::default()
        })
}

/// Atomic; a failure to write only costs speed next time.
fn save_index(path: &Path, index: &Index) {
    let Ok(bytes) = serde_json::to_vec(index) else {
        return;
    };
    if let Some(dir) = path.parent()
        && std::fs::create_dir_all(dir).is_err()
    {
        return;
    }
    let _ = crate::paths::write_atomic(path, &bytes);
}

/// A hash of everything the answer depends on besides EXIF.
fn state_key(
    cfg: &Config,
    files: &[scan::FileEntry],
    overrides: &Overrides,
    ai_cache: &Path,
) -> String {
    let mut h = blake3::Hasher::new();
    h.update(serde_json::to_string(cfg).unwrap_or_default().as_bytes());
    for e in overrides.entries() {
        h.update(format!("{e:?}\n").as_bytes());
    }
    let cache_meta = std::fs::metadata(ai_cache)
        .ok()
        .and_then(|m| scan::mtime_ns(&m).ok().map(|t| (m.len(), t)));
    h.update(format!("{cache_meta:?}\n").as_bytes());
    // Files apply found no smaller are planned as unchanged.
    h.update(format!("{:?}\n", crate::incompressible::stamp()).as_bytes());
    for f in files {
        h.update(
            format!(
                "{}|{}|{}|{}\n",
                f.source_id,
                f.path.display(),
                f.size,
                f.mtime_ns
            )
            .as_bytes(),
        );
    }
    h.finalize().to_hex().to_string()
}

/// A plan built from the sources the way `count` routes them: cached AI
/// answers only, files that need a fresh answer left out of the plan.
#[derive(Debug, Clone)]
pub struct Built {
    /// Without the files waiting for the AI: they must not be planned (a
    /// file that fell through to `after_ai` for want of an answer would
    /// otherwise be run as if that were its target).
    pub plan: Plan,
    /// Files that need a fresh AI answer, not cached yet.
    pub waiting: BTreeSet<PathBuf>,
    /// Routing notes (e.g. a manual decision naming an unknown target).
    pub warnings: Vec<String>,
    pub overrides_applied: usize,
    /// How many files exiftool read this time.
    pub exif_reads: usize,
}

impl Built {
    fn pending(&self) -> Pending {
        let mut out = Pending {
            need_ai: self.waiting.len(),
            exif_reads: self.exif_reads,
            ..Pending::default()
        };
        for item in &self.plan.items {
            out.to_process += 1;
            *out.by_target.entry(item.target.clone()).or_default() += 1;
        }
        out
    }
}

/// Drop what is waiting for the AI from the plan (see [`Built::plan`]).
pub fn strip_waiting(plan: &mut Plan, waiting: &BTreeSet<PathBuf>) {
    plan.items.retain(|i| !waiting.contains(&i.source));
    plan.review.retain(|r| !waiting.contains(&r.source));
    plan.unmatched.retain(|p| !waiting.contains(p));
    plan.unchanged.retain(|p| !waiting.contains(p));
}

/// Count what waits. See the module doc.
pub fn count(cfg: &Config, overrides: &Overrides, env: &Env, cancel: &Cancel) -> Result<Pending> {
    cancel.check()?;
    let files = scan::scan_sources_with(&cfg.sources, &NoProgress, cancel)?;
    let mut index = load_index(env.index);
    let key = state_key(cfg, &files, overrides, env.ai_cache);
    if let Some((k, p)) = &index.last
        && *k == key
    {
        return Ok(Pending {
            exif_reads: 0,
            fast_path: true,
            ..p.clone()
        });
    }
    let built = build_in(&mut index, cfg, overrides, env, &files, cancel)?;
    let out = built.pending();
    if let Some(path) = env.index {
        index.last = Some((key, out.clone()));
        save_index(path, &index);
    }
    Ok(out)
}

/// Build the plan over the already scanned `files` (the current plan of the
/// GUI): EXIF only for files the index does not know, the same routing as
/// [`count`]. Updates the index. For a config without AI targets it is the
/// plan of `pipeline::scan_plan` without AI.
pub fn build(
    cfg: &Config,
    overrides: &Overrides,
    env: &Env,
    files: &[scan::FileEntry],
    cancel: &Cancel,
) -> Result<Built> {
    let mut index = load_index(env.index);
    let built = build_in(&mut index, cfg, overrides, env, files, cancel)?;
    if let Some(path) = env.index {
        // The remembered answer was counted for another state.
        index.last = None;
        save_index(path, &index);
    }
    Ok(built)
}

fn build_in(
    index: &mut Index,
    cfg: &Config,
    overrides: &Overrides,
    env: &Env,
    files: &[scan::FileEntry],
    cancel: &Cancel,
) -> Result<Built> {
    let fresh = |f: &scan::FileEntry| {
        index
            .files
            .get(&f.path)
            .is_none_or(|k| k.size != f.size || k.mtime_ns != f.mtime_ns)
    };
    let new: Vec<PathBuf> = files
        .iter()
        .filter(|f| fresh(f))
        .map(|f| f.path.clone())
        .collect();
    let mut read = HashMap::new();
    if !new.is_empty() {
        let tools = env
            .tools
            .ok_or_else(|| crate::Error::Tool(crate::t!("tools.exiftool_missing")))?;
        read = exif::read_with(tools, &new, &NoProgress, cancel)?;
    }
    let exif_reads = new.len();
    let mut next = HashMap::new();
    let mut exif_map = HashMap::new();
    for f in files {
        let value = match read.remove(&f.path) {
            Some(v) => v,
            None => match index.files.remove(&f.path) {
                Some(k) if k.size == f.size && k.mtime_ns == f.mtime_ns => k.exif,
                _ => Value::Null,
            },
        };
        exif_map.insert(f.path.clone(), value.clone());
        next.insert(
            f.path.clone(),
            Known {
                size: f.size,
                mtime_ns: f.mtime_ns,
                exif: value,
            },
        );
    }
    index.files = next;

    let cache = Cache::load(env.ai_cache);
    let ai = CachedOnly {
        cache: &cache,
        model: &cfg.ai.model,
        pending: RefCell::new(HashSet::new()),
    };
    let ctx = RouteCtx::build(cfg, files, &exif_map, env.tools, &NoProgress, cancel)?;
    let routed =
        route_with_overrides(cfg, files, &ctx, &ai, false, overrides, &NoProgress, cancel)?;
    let mut plan = build_plan_with(
        cfg,
        routed.decisions,
        &exif_map,
        Side::Local,
        &NoProgress,
        cancel,
    )?;
    guard_last_copies(&mut plan, &ctx.groups, files);
    let waiting: BTreeSet<PathBuf> = ai.pending.into_inner().into_iter().collect();
    strip_waiting(&mut plan, &waiting);
    Ok(Built {
        plan,
        waiting,
        warnings: routed.warnings,
        overrides_applied: routed.overrides_applied,
        exif_reads,
    })
}

#[cfg(test)]
mod tests;
