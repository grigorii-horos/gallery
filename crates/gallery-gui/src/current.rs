//! "Current plan": the one plan per config the GUI works with. It is kept in
//! `current/<config-hash>.plan.json` next to a small state file with the
//! fingerprint of what it was built from (config, manual decisions, the
//! sources' file index, the AI cache). Whenever the fingerprint of the
//! present state differs, the plan is rebuilt without any AI call: cached
//! answers are used, files that need a fresh answer wait for it. A real AI
//! scan is a separate, explicit action that fills the cache.
//!
//! A run never works on this file: the plan is first copied into the plans
//! folder (the archive History reads) and the copy runs, so its report lands
//! next to it. Before that, the plan is checked against the fingerprint once
//! more and a stale one is refused (and rebuilt).

use std::path::{Path, PathBuf};

use gallery_core::config::Config;
use gallery_core::learn::Store;
use gallery_core::overrides::Overrides;
use gallery_core::pending;
use gallery_core::pipeline;
use gallery_core::plan::Plan;
use gallery_core::progress::{Cancel, NoProgress, Progress};
use gallery_core::route::Classifier;
use gallery_core::scan::{self, FileEntry};
use gallery_core::tools::Tools;
use serde::{Deserialize, Serialize};

use crate::plans::{OverrideView, overrides_view};
use crate::scan::{ScanDone, ScanError, ScanSummary};

pub const UPDATED_EVENT: &str = "current-updated";
pub const REBUILDING_EVENT: &str = "current-rebuilding";
pub const ERROR_EVENT: &str = "current-error";

/// Why a run was refused because the plan is out of date.
pub const STALE_MESSAGE: &str = "Files, config or decisions changed since the check. \
     The plan was refreshed: review it again and re-run.";

/// What the current plan was built from. Equal fingerprints mean the plan
/// is what a rebuild would give. It does not cover the targets' folders:
/// what is there when the plan runs may differ from what it was built
/// against, and that is safe because a run never overwrites a file (a clash
/// gets a new name or fails that file).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    /// Hash of the config file's text.
    pub config: String,
    /// Hash of the manual decisions file's text.
    pub overrides: String,
    /// Hash of the sources' file index: path, size, mtime of every file.
    pub sources: String,
    /// Size and mtime of the AI answer cache: changes when answers are added.
    pub ai_cache: String,
    /// Size and mtime of the store of files their content pipe cannot
    /// shrink: a run that leaves such files in place changes no source file,
    /// only this store, and the plan must then drop them.
    #[serde(default)]
    pub incompressible: String,
}

/// The state file next to the plan.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Meta {
    pub fingerprint: Fingerprint,
    /// RFC 3339 local time of the build.
    pub built_at: String,
    /// Files that need a fresh AI answer (left out of the plan).
    pub waiting_ai: Vec<String>,
    pub notes: Vec<String>,
    pub overrides_applied: usize,
}

/// Where the current plan reads and writes. Tests point it at fixtures.
pub struct Env<'a> {
    /// The `current/` folder.
    pub dir: &'a Path,
    pub config: &'a Path,
    pub overrides: Option<&'a Path>,
    pub ai_cache: &'a Path,
    /// The user's corrections that go into the AI prompts (`None`: none).
    pub examples: Option<&'a Path>,
    /// The pending/effect file index (EXIF kept per file).
    pub index: Option<&'a Path>,
    /// For files the index does not know.
    pub tools: Option<&'a Tools>,
}

impl Env<'_> {
    fn key(&self) -> String {
        let h = blake3::hash(self.config.to_string_lossy().as_bytes());
        h.to_hex()[..16].to_string()
    }

    pub fn plan_path(&self) -> PathBuf {
        self.dir.join(format!("{}.plan.json", self.key()))
    }

    fn meta_path(&self) -> PathBuf {
        self.dir.join(format!("{}.state.json", self.key()))
    }

    fn pending_env(&self) -> pending::Env<'_> {
        pending::Env {
            tools: self.tools,
            ai_cache: self.ai_cache,
            index: self.index,
        }
    }
}

fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// The file's bytes; `None` when it does not exist.
fn read_opt(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn hash_opt(bytes: Option<&[u8]>) -> String {
    bytes.map_or_else(|| "none".to_string(), hash_bytes)
}

fn cache_stamp(path: &Path) -> String {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| scan::mtime_ns(&m).ok().map(|t| format!("{}|{t}", m.len())))
        .unwrap_or_else(|| "none".into())
}

fn sources_hash(files: &[FileEntry]) -> String {
    let mut h = blake3::Hasher::new();
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

/// The fingerprint of the present state, over the scanned `files` and the
/// very bytes of the config and decisions the plan is built from.
fn fingerprint(
    env: &Env,
    config: &[u8],
    overrides: Option<&[u8]>,
    files: &[FileEntry],
) -> Fingerprint {
    Fingerprint {
        config: hash_bytes(config),
        overrides: hash_opt(overrides),
        sources: sources_hash(files),
        ai_cache: cache_stamp(env.ai_cache),
        incompressible: gallery_core::incompressible::stamp()
            .map_or_else(|| "none".into(), |(len, t)| format!("{len}|{t}")),
    }
}

fn read_meta(env: &Env) -> Option<Meta> {
    let bytes = std::fs::read(env.meta_path()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Write `bytes` to `path` through a uniquely named temp file next to it, so
/// a reader never sees half and two writers never share a temp name.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    gallery_core::paths::write_atomic(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn load_config(env: &Env) -> Result<Config, String> {
    let mut cfg = Config::load(env.config).map_err(|e| e.to_string())?;
    cfg.validate().map_err(|e| e.to_string())?;
    if let Some(f) = env.examples {
        cfg.attach_examples(&Store::new(f));
    }
    Ok(cfg)
}

/// The config parsed from `bytes` (already read, and hashed by the caller).
fn parse_config(env: &Env, bytes: &[u8]) -> Result<Config, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!("{}: {e}", env.config.display()))?;
    let dir = env.config.parent().unwrap_or(Path::new(""));
    let cfg = Config::from_str_at(text, dir, env.config).map_err(|e| e.to_string())?;
    cfg.validate().map_err(|e| e.to_string())?;
    Ok(cfg)
}

fn parse_overrides(env: &Env, bytes: Option<&[u8]>) -> Result<Overrides, String> {
    match (env.overrides, bytes) {
        (Some(f), Some(b)) => {
            let text = std::str::from_utf8(b).map_err(|e| format!("{}: {e}", f.display()))?;
            Overrides::parse(text, f).map_err(|e| e.to_string())
        }
        _ => Ok(Overrides::default()),
    }
}

fn load_overrides(env: &Env) -> Result<Overrides, String> {
    match env.overrides {
        Some(f) => Overrides::load(f).map_err(|e| e.to_string()),
        None => Ok(Overrides::default()),
    }
}

/// What a freshness check found.
pub struct Checked {
    pub fresh: bool,
    cfg: Config,
    /// The decisions as read for the fingerprint: the plan is built from
    /// these, not from a second read.
    overrides: Overrides,
    files: Vec<FileEntry>,
    fingerprint: Fingerprint,
}

/// Scan the sources (no EXIF) and compare the present fingerprint with the
/// one the stored plan was built from. A missing plan is not fresh.
pub fn check(env: &Env, progress: &dyn Progress, cancel: &Cancel) -> Result<Checked, String> {
    // Read once: the hash and the parsed config are of the same bytes, so a
    // save during the scan below makes the plan stale instead of mislabelled.
    let config_bytes =
        std::fs::read(env.config).map_err(|e| format!("{}: {e}", env.config.display()))?;
    let overrides_bytes = match env.overrides {
        Some(f) => read_opt(f)?,
        None => None,
    };
    let cfg = parse_config(env, &config_bytes)?;
    let overrides = parse_overrides(env, overrides_bytes.as_deref())?;
    let files =
        scan::scan_sources_with(&cfg.sources, progress, cancel).map_err(|e| e.to_string())?;
    let fingerprint = fingerprint(env, &config_bytes, overrides_bytes.as_deref(), &files);
    let fresh =
        env.plan_path().is_file() && read_meta(env).is_some_and(|m| m.fingerprint == fingerprint);
    Ok(Checked {
        fresh,
        cfg,
        overrides,
        files,
        fingerprint,
    })
}

/// The result of [`refresh`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refreshed {
    /// The stored plan already matched the present state.
    Unchanged,
    /// A new plan was built and stored.
    Rebuilt,
}

fn build_and_store(env: &Env, c: Checked, cancel: &Cancel) -> Result<(), String> {
    let built = pending::build(&c.cfg, &c.overrides, &env.pending_env(), &c.files, cancel)
        .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(env.dir).map_err(|e| format!("{}: {e}", env.dir.display()))?;
    let plan_json = serde_json::to_vec_pretty(&built.plan).map_err(|e| e.to_string())?;
    // The state goes last: a plan without a matching state is just stale.
    let _ = std::fs::remove_file(env.meta_path());
    write_atomic(&env.plan_path(), &plan_json)?;
    let meta = Meta {
        fingerprint: c.fingerprint,
        built_at: chrono::Local::now().to_rfc3339(),
        waiting_ai: built
            .waiting
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
        notes: built.warnings,
        overrides_applied: built.overrides_applied,
    };
    let meta_json = serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?;
    write_atomic(&env.meta_path(), &meta_json)
}

/// Bring the stored plan up to date with the sources, config, decisions and
/// AI cache; `force` builds even when nothing changed. No AI call.
pub fn refresh(
    env: &Env,
    force: bool,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<Refreshed, String> {
    refresh_notify(env, force, &|| {}, progress, cancel)
}

/// [`refresh`] that calls `on_stale` once the plan is found out of date,
/// before it is rebuilt (the screen shows "refreshing plan").
pub fn refresh_notify(
    env: &Env,
    force: bool,
    on_stale: &dyn Fn(),
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<Refreshed, String> {
    let c = check(env, progress, cancel)?;
    if c.fresh && !force {
        return Ok(Refreshed::Unchanged);
    }
    on_stale();
    build_and_store(env, c, cancel)?;
    Ok(Refreshed::Rebuilt)
}

/// The places of the real app, owned so a thread can keep them.
pub struct Owned {
    dir: PathBuf,
    config: PathBuf,
    overrides: Option<PathBuf>,
    ai_cache: PathBuf,
    examples: Option<PathBuf>,
    index: Option<PathBuf>,
    tools: Option<Tools>,
}

impl Owned {
    /// The current plan of the config at `config`, with the user's decisions,
    /// AI cache and index (under `$GALLERY_DATA_DIR` in debug builds when set).
    pub fn for_config(config: &Path, tools: Option<Tools>) -> Result<Owned, String> {
        Ok(Owned {
            dir: crate::plans::current_dir().ok_or_else(|| gallery_core::t!("lock.no_data_dir"))?,
            config: config.to_path_buf(),
            overrides: crate::overrides::default_file().ok(),
            ai_cache: gallery_core::ai::AiClassifier::default_cache_path()
                .unwrap_or_else(|| PathBuf::from("none")),
            examples: crate::learn::default_file().ok(),
            index: pending::default_index_path(),
            tools,
        })
    }

    pub fn env(&self) -> Env<'_> {
        Env {
            dir: &self.dir,
            config: &self.config,
            overrides: self.overrides.as_deref(),
            ai_cache: &self.ai_cache,
            examples: self.examples.as_deref(),
            index: self.index.as_deref(),
            tools: self.tools.as_ref(),
        }
    }
}

/// The plan the screen shows: the stored plan, its decisions and counts.
#[derive(Debug, Serialize)]
pub struct CurrentView {
    /// The stored plan's file (for the commands that read a plan; never shown).
    pub path: String,
    pub built_at: String,
    pub plan: Plan,
    pub overrides: Vec<OverrideView>,
    /// Files that wait for a fresh AI answer (left out of the plan).
    pub waiting_ai: usize,
    pub notes: Vec<String>,
    /// Identity of this plan (see [`plan_id`]); a run names it.
    pub plan_id: String,
}

/// Read the stored plan; `None` when there is none yet.
pub fn load(env: &Env) -> Result<Option<CurrentView>, String> {
    let path = env.plan_path();
    let Some(meta) = read_meta(env) else {
        return Ok(None);
    };
    // One read: the plan shown and its identity are of the same bytes.
    let Some(bytes) = std::fs::read(&path).ok() else {
        return Ok(None);
    };
    let plan = parse_plan(&bytes, &path)?;
    let overrides = load_overrides(env)?;
    Ok(Some(CurrentView {
        path: path.display().to_string(),
        built_at: meta.built_at,
        plan,
        overrides: overrides_view(&overrides),
        waiting_ai: meta.waiting_ai.len(),
        notes: meta.notes,
        plan_id: hash_bytes(&bytes),
    }))
}

fn parse_plan(bytes: &[u8], path: &Path) -> Result<Plan, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    Plan::parse(text, path).map_err(|e| e.to_string())
}

/// The identity of the plan the user looked at: a hash of the stored plan
/// file. A rebuild that changes anything changes it.
pub fn plan_id(env: &Env) -> Option<String> {
    std::fs::read(env.plan_path()).ok().map(|b| hash_bytes(&b))
}

/// Identity of a pilot slice: a hash of the sources it chose, in order.
pub fn slice_id(plan: &Plan) -> String {
    let mut h = blake3::Hasher::new();
    for it in &plan.items {
        h.update(it.source.to_string_lossy().as_bytes());
        h.update(b"\n");
    }
    h.finalize().to_hex().to_string()
}

/// What the user confirmed: the plan (and for a pilot its slice) by identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Confirmed {
    pub plan_id: Option<String>,
    pub pilot_id: Option<String>,
}

pub fn pending_of(view: &CurrentView) -> pending::Pending {
    let mut p = pending::Pending {
        need_ai: view.waiting_ai,
        fast_path: true,
        ..Default::default()
    };
    for it in &view.plan.items {
        p.to_process += 1;
        *p.by_target.entry(it.target.clone()).or_default() += 1;
    }
    p
}

/// The scan with AI: asks the classifier (which fills the AI cache), then
/// rebuilds the current plan from the cache. The scan's own plan is thrown
/// away; its cost and notes come back.
pub fn ai_scan(
    env: &Env,
    tools: &Tools,
    classifier: Option<&dyn Classifier>,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<ScanDone, ScanError> {
    let cfg = load_config(env).map_err(|message| ScanError {
        message,
        cancelled: false,
    })?;
    let overrides = load_overrides(env).map_err(|message| ScanError {
        message,
        cancelled: false,
    })?;
    let opts = pipeline::ScanOptions {
        server: false,
        accept_low: false,
        no_ai: false,
    };
    let out = match classifier {
        Some(c) => {
            pipeline::scan_plan_using(&cfg, tools, opts, &overrides, Some(c), progress, cancel)?
        }
        None => pipeline::scan_plan(&cfg, tools, opts, &overrides, progress, cancel)?,
    };
    let summary = ScanSummary::new(&out, &cfg.ai.model);
    // The answers are cached now: the plan follows from the cache.
    refresh(env, true, &NoProgress, cancel).map_err(|message| ScanError {
        cancelled: cancel.is_cancelled(),
        message,
    })?;
    Ok(ScanDone {
        plan_path: env.plan_path().display().to_string(),
        summary,
    })
}

/// A plan ready to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    /// The plan file to run: the archived copy for a real run, the current
    /// plan itself for a dry run (which writes no report).
    pub plan_path: PathBuf,
    pub files: usize,
}

/// Why nothing can run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The plan was out of date; it was rebuilt, the user must look again.
    Stale,
    Other(String),
}

impl Refusal {
    pub fn message(&self) -> String {
        match self {
            Refusal::Stale => STALE_MESSAGE.into(),
            Refusal::Other(m) => m.clone(),
        }
    }
}

/// The final check before a run. A stale (or missing) plan is rebuilt and
/// refused. Otherwise: the whole plan, or its pilot with `pilot` files per
/// target, is saved as a new file in `archive` (the plans folder History
/// reads) and that copy is the plan to run; a dry run uses the current plan
/// in place (it writes no report).
pub fn prepare_run(
    env: &Env,
    archive: &Path,
    pilot: Option<usize>,
    dry_run: bool,
    confirmed: &Confirmed,
    now: chrono::NaiveDateTime,
    cancel: &Cancel,
) -> Result<Prepared, Refusal> {
    let checked = check(env, &NoProgress, cancel).map_err(Refusal::Other)?;
    if !checked.fresh {
        build_and_store(env, checked, cancel).map_err(Refusal::Other)?;
        return Err(Refusal::Stale);
    }
    let path = env.plan_path();
    // One read: the plan checked against what the user confirmed is the plan
    // that runs. A rebuild after the dialog opened (even a fresh one) is a
    // different plan.
    let bytes =
        std::fs::read(&path).map_err(|e| Refusal::Other(format!("{}: {e}", path.display())))?;
    if !dry_run
        && (confirmed.plan_id.is_none()
            || confirmed.plan_id.as_deref() != Some(hash_bytes(&bytes).as_str()))
    {
        return Err(Refusal::Stale);
    }
    let plan = parse_plan(&bytes, &path).map_err(Refusal::Other)?;
    crate::apply::ensure_local(&plan).map_err(Refusal::Other)?;
    let plan = match pilot {
        Some(n) => crate::pilot::choose(&plan, n).map_err(Refusal::Other)?.0,
        None => plan,
    };
    if !dry_run
        && pilot.is_some()
        && confirmed.pilot_id.as_deref() != Some(slice_id(&plan).as_str())
    {
        return Err(Refusal::Stale);
    }
    if plan.items.is_empty() {
        return Err(Refusal::Other(gallery_core::t!("current.no_files")));
    }
    let files = plan.items.len();
    if dry_run && pilot.is_none() {
        return Ok(Prepared {
            plan_path: path,
            files,
        });
    }
    std::fs::create_dir_all(archive)
        .map_err(|e| Refusal::Other(format!("{}: {e}", archive.display())))?;
    // A new name every time (`new_plan_path` never reuses one), so a copy
    // never has a report yet: a plan cannot be run twice from here.
    let copy = crate::plans::new_plan_path(archive, now);
    plan.save(&copy)
        .map_err(|e| Refusal::Other(e.to_string()))?;
    Ok(Prepared {
        plan_path: copy,
        files,
    })
}

#[cfg(test)]
mod tests;
