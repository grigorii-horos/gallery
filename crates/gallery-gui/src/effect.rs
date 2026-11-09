//! "Live effect in the target editor": what the editor's unsaved changes do to
//! the files. The scanned file list and the EXIF are kept per set of sources
//! (a new edit that leaves the sources alone reuses them), the diff itself is
//! `gallery_core::effect`. Reads only: no config or file is written.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use gallery_core::ai::cache::Cache;
use gallery_core::config::{Config, Source};
use gallery_core::effect::{self, EffectDiff};
use gallery_core::overrides::Overrides;
use gallery_core::progress::{Cancel, Progress};
use gallery_core::scan::{self, FileEntry};
use gallery_core::tools::Tools;
use gallery_core::{Error, exif};
use serde::Serialize;
use serde_json::Value;

use crate::config_edit::{self, EditOp};

pub const PROGRESS_EVENT: &str = "effect-progress";

/// What identifies the scanned file list: the sources' ids, folders and scan
/// switches (prompt texts do not change the files).
fn fingerprint(sources: &[Source]) -> String {
    sources
        .iter()
        .map(|s| format!("{}|{}|{}|{:?}", s.id, s.path.display(), s.recursive, s.skip))
        .collect::<Vec<_>>()
        .join("\n")
}

struct Scanned {
    key: String,
    files: Vec<FileEntry>,
    exif: HashMap<PathBuf, Value>,
}

/// Shared state: the scan kept for the sources it was made for and the
/// cancel flag of the request in flight.
#[derive(Default)]
pub struct EffectState {
    scanned: Mutex<Option<Scanned>>,
    running: Mutex<Option<Cancel>>,
}

impl EffectState {
    /// Cancel the request in flight and register a new one.
    pub fn begin(&self) -> Cancel {
        let mut r = self.running.lock().expect("effect lock");
        if let Some(c) = r.take() {
            c.cancel();
        }
        let c = Cancel::new();
        *r = Some(c.clone());
        c
    }

    /// Forget the scan (the next preview scans again).
    pub fn forget(&self) {
        *self.scanned.lock().expect("effect lock") = None;
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EffectView {
    #[serde(flatten)]
    pub diff: EffectDiff,
    /// The file list was scanned for this request (not taken from memory).
    pub rescanned: bool,
    pub scan_ms: u128,
    pub diff_ms: u128,
}

/// Where the inputs come from; tests point them at fixtures.
pub struct Env<'a> {
    pub ai_cache: &'a Path,
    pub overrides: Option<&'a Path>,
    /// The user's corrections that are part of the AI prompts.
    pub examples: Option<&'a Path>,
    /// `None`: no EXIF (names are read from the file name only).
    pub tools: Option<&'a Tools>,
}

/// The effect of `ops` on the config at `config`. `Ok(None)` when cancelled.
/// `refresh` scans again even if the sources are unchanged.
pub fn preview(
    state: &EffectState,
    env: &Env,
    config: &Path,
    ops: &[EditOp],
    refresh: bool,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<Option<EffectView>, String> {
    let (mut old, mut new) = config_edit::configs(config, ops)?;
    if let Some(f) = env.examples {
        let store = gallery_core::learn::Store::new(f);
        old.attach_examples(&store);
        new.attach_examples(&store);
    }
    match run(state, env, &old, &new, refresh, progress, cancel) {
        Err(Error::Cancelled) => Ok(None),
        r => r.map(Some).map_err(|e| e.to_string()),
    }
}

fn run(
    state: &EffectState,
    env: &Env,
    old: &Config,
    new: &Config,
    refresh: bool,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> gallery_core::Result<EffectView> {
    let key = fingerprint(&new.sources);
    let t0 = Instant::now();
    let mut slot = state.scanned.lock().expect("effect lock");
    let fresh = refresh || slot.as_ref().is_none_or(|s| s.key != key);
    if fresh {
        *slot = None;
        let files = scan::scan_sources_with(&new.sources, progress, cancel)?;
        *slot = Some(Scanned {
            key,
            files,
            exif: HashMap::new(),
        });
    }
    let s = slot.as_mut().expect("set above");
    if let Some(tools) = env.tools {
        let missing: Vec<PathBuf> = s
            .files
            .iter()
            .filter(|f| !s.exif.contains_key(&f.path))
            .map(|f| f.path.clone())
            .collect();
        if !missing.is_empty() {
            s.exif
                .extend(exif::read_with(tools, &missing, progress, cancel)?);
        }
    }
    let scan_ms = t0.elapsed().as_millis();
    // Work on copies so a slow diff does not hold the scan lock.
    let (files, exif_map) = (s.files.clone(), s.exif.clone());
    drop(slot);
    let ov = match env.overrides {
        Some(f) => Overrides::load(f).unwrap_or_default(),
        None => Overrides::default(),
    };
    let cache = Cache::load(env.ai_cache);
    let t1 = Instant::now();
    let diff = effect::diff(
        old, new, &files, &exif_map, env.tools, &ov, &cache, progress, cancel,
    )?;
    Ok(EffectView {
        diff,
        rescanned: fresh,
        scan_ms,
        diff_ms: t1.elapsed().as_millis(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gallery_core::progress::NoProgress;

    struct Fx {
        dir: tempfile::TempDir,
        cfg: PathBuf,
    }

    fn fx() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        std::fs::create_dir_all(r.join("src")).unwrap();
        std::fs::create_dir_all(r.join("src2")).unwrap();
        for n in ["20260716_211554.png", "b.png", "c.jpg"] {
            std::fs::write(r.join("src").join(n), b"x").unwrap();
        }
        std::fs::write(r.join("src2/d.png"), b"x").unwrap();
        let cfg = r.join("gallery.toml");
        std::fs::write(
            &cfg,
            format!(
                "[[sources]]\nid = \"s\"\npath = \"{r}/src\"\n\
                 [[targets.after_ai]]\nname = \"pngs\"\n[[targets.after_ai.filters]]\n\
                 sources = [\"s\"]\nmatch = \"*.png\"\n[[targets.after_ai.outputs]]\ndir = \"{r}/out\"\n",
                r = r.to_string_lossy().replace('\\', "/")
            ),
        )
        .unwrap();
        Fx { dir, cfg }
    }

    fn go(f: &Fx, st: &EffectState, ops: &[EditOp]) -> EffectView {
        let cache = f.dir.path().join("cache.jsonl");
        let env = Env {
            ai_cache: &cache,
            overrides: None,
            examples: None,
            tools: None,
        };
        preview(st, &env, &f.cfg, ops, false, &NoProgress, &Cancel::new())
            .unwrap()
            .unwrap()
    }

    #[test]
    fn empty_ops_change_nothing_and_the_scan_is_reused() {
        let f = fx();
        let st = EffectState::default();
        let before = std::fs::read(&f.cfg).unwrap();
        let a = go(&f, &st, &[]);
        assert!(a.rescanned);
        assert_eq!((a.diff.files, a.diff.changed), (3, 0));
        let b = go(&f, &st, &[]);
        assert!(!b.rescanned);
        assert_eq!(std::fs::read(&f.cfg).unwrap(), before);
    }

    #[test]
    fn changing_the_sources_invalidates_the_scan() {
        let f = fx();
        let st = EffectState::default();
        go(&f, &st, &[]);
        // New sources = another folder: the file list must be scanned again.
        let r = f.dir.path().to_string_lossy().replace('\\', "/");
        let text = std::fs::read_to_string(&f.cfg)
            .unwrap()
            .replace(&format!("{r}/src\""), &format!("{r}/src2\""));
        std::fs::write(&f.cfg, text).unwrap();
        let c = go(&f, &st, &[]);
        assert!(c.rescanned);
        assert_eq!(c.diff.files, 1);
        // A forced refresh scans again too.
        let cache = f.dir.path().join("cache.jsonl");
        let env = Env {
            ai_cache: &cache,
            overrides: None,
            examples: None,
            tools: None,
        };
        let d = preview(&st, &env, &f.cfg, &[], true, &NoProgress, &Cancel::new())
            .unwrap()
            .unwrap();
        assert!(d.rescanned);
    }

    #[test]
    fn a_newer_request_cancels_the_older_one() {
        let st = EffectState::default();
        let a = st.begin();
        let b = st.begin();
        assert!(a.is_cancelled());
        assert!(!b.is_cancelled());
    }

    #[test]
    fn cancelled_preview_is_none() {
        let f = fx();
        let st = EffectState::default();
        let cache = f.dir.path().join("cache.jsonl");
        let env = Env {
            ai_cache: &cache,
            overrides: None,
            examples: None,
            tools: None,
        };
        let c = Cancel::new();
        c.cancel();
        assert!(
            preview(&st, &env, &f.cfg, &[], false, &NoProgress, &c)
                .unwrap()
                .is_none()
        );
    }

    /// What a preview reports besides the scan: with a duplicate condition
    /// the content hashes are counted as `Stage::Plan` events (the UI shows
    /// "counting duplicates and sharpness"); without one there are none.
    #[test]
    fn counting_duplicates_reports_plan_progress_and_nothing_else_does() {
        use gallery_core::progress::{ProgressEvent, Stage};
        let f = fx();
        let plan_events = |f: &Fx| {
            let seen = Mutex::new(Vec::<ProgressEvent>::new());
            let cache = f.dir.path().join("cache.jsonl");
            let env = Env {
                ai_cache: &cache,
                overrides: None,
                examples: None,
                tools: None,
            };
            let sink = |e: ProgressEvent| seen.lock().unwrap().push(e);
            preview(
                &EffectState::default(),
                &env,
                &f.cfg,
                &[],
                false,
                &sink,
                &Cancel::new(),
            )
            .unwrap()
            .unwrap();
            seen.into_inner()
                .unwrap()
                .into_iter()
                .filter(|e| e.stage == Stage::Plan)
                .collect::<Vec<_>>()
        };
        assert!(plan_events(&f).is_empty(), "no group pass, no hint");

        let r = f.dir.path().to_string_lossy().replace('\\', "/");
        std::fs::write(
            &f.cfg,
            format!(
                "[[sources]]\nid = \"s\"\npath = \"{r}/src\"\n\
                 [[targets.after_ai]]\nname = \"dups\"\n\
                 [[targets.after_ai.filters]]\nsources = [\"s\"]\nduplicate = {{ of = [\"sources\"] }}\n\
                 [[targets.after_ai.outputs]]\ndir = \"{r}/out\"\n"
            ),
        )
        .unwrap();
        let evs = plan_events(&f);
        assert!(!evs.is_empty(), "hashing is reported");
        assert!(
            evs.iter().all(|e| e.total > 0 && e.done <= e.total),
            "{evs:?}"
        );
    }

    #[test]
    fn a_large_library_is_fast_after_the_first_scan() {
        let f = fx();
        for i in 0..5000 {
            std::fs::write(f.dir.path().join(format!("src/20260716_{i:06}.png")), b"x").unwrap();
        }
        let st = EffectState::default();
        let first = go(&f, &st, &[]);
        assert_eq!(first.diff.files, 5003);
        let second = go(&f, &st, &[]);
        eprintln!("scan {} ms, diff {} ms", first.scan_ms, second.diff_ms);
        assert!(!second.rescanned);
        // Debug build; release is about three times faster.
        // Shared CI runners are several times slower (1.5 s seen on macOS,
        // 6.1 s on Windows, where every file stat is much slower).
        let limit = if std::env::var_os("CI").is_some() {
            if cfg!(windows) { 15000 } else { 5000 }
        } else {
            1000
        };
        assert!(second.scan_ms + second.diff_ms < limit, "{second:?}");
    }
}
