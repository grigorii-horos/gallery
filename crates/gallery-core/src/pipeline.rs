//! The scan pipeline shared by the CLI and the GUI: scan the sources, read
//! EXIF, route (manual decisions first, then masks and the AI) and build the
//! plan. Only reads; the caller saves the plan.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::ai::cache::Cache;
use crate::ai::client::is_cached;
use crate::ai::usage_log;
use crate::ai::{AiClassifier, Usage};
use crate::config::Config;
use crate::overrides::Overrides;
use crate::plan::{Plan, Side, build_plan_with};
use crate::progress::{Cancel, Progress};
use crate::route::{AiAnswer, AiRequest, Classifier, Confidence, RouteCtx, route_with_overrides};
use crate::tools::Tools;
use crate::{Result, exif, scan};

/// Scan switches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanOptions {
    /// Plan with the sources' and outputs' server paths.
    pub server: bool,
    /// Accept low-confidence AI answers instead of sending them to review.
    pub accept_low: bool,
    /// Skip the AI: AI filters take nothing, files walk on to later targets.
    pub no_ai: bool,
}

/// A finished scan.
#[derive(Debug, Clone)]
pub struct ScanOutcome {
    pub plan: Plan,
    /// Routing notes (e.g. a manual decision naming an unknown target).
    pub warnings: Vec<String>,
    /// Files decided by a manual decision.
    pub overrides_applied: usize,
    /// Tokens the AI classifier used; `None` when the AI stage did not run.
    pub ai_usage: Option<Usage>,
}

/// Classifier for a scan without AI: no category, so files fall through to
/// `after_ai`.
pub struct NoAi;

impl Classifier for NoAi {
    fn classify(&self, batch: Vec<AiRequest>) -> Result<Vec<AiAnswer>> {
        Ok(batch
            .into_iter()
            .map(|_| AiAnswer {
                category: None,
                reason: String::new(),
                confidence: Confidence::Ok,
            })
            .collect())
    }
}

/// Scan, EXIF, routing and planning with progress and cancellation (a
/// cancel ends it with `Error::Cancelled`). The AI classifier is the
/// production one (key from the environment or the key file, answers
/// cached); it is not created without AI targets or with `no_ai`.
pub fn scan_plan(
    cfg: &Config,
    tools: &Tools,
    opts: ScanOptions,
    overrides: &Overrides,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<ScanOutcome> {
    let ai = if opts.no_ai || !cfg.has_ai() {
        None
    } else {
        Some(AiClassifier::from_settings(&cfg.ai, tools.clone())?)
    };
    let classifier = ai.as_ref().map(|a| a as &dyn Classifier);
    let mut out = scan_plan_using(cfg, tools, opts, overrides, classifier, progress, cancel)?;
    out.ai_usage = ai.map(|a| a.usage());
    // Remembered for the next estimate; failing to write it never fails a scan.
    if let (Some(u), Some(path)) = (out.ai_usage, usage_log::default_path()) {
        let _ = usage_log::append(&path, &usage_log::UsageRow::new(&cfg.ai.model, u));
    }
    Ok(out)
}

/// How many files an AI scan would send, counted without any AI call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct AiDry {
    /// Files with at least one AI candidate (not decided by an earlier
    /// stage).
    pub candidates: usize,
    /// Of them, answered by the cache already.
    pub cached: usize,
    /// Of them, requests that would be paid for.
    pub to_send: usize,
}

/// Records the batches it is given and answers "none" to all of them.
struct Counting(RefCell<Vec<AiRequest>>);

impl Classifier for Counting {
    fn classify(&self, batch: Vec<AiRequest>) -> Result<Vec<AiAnswer>> {
        let answers = NoAi.classify(batch.clone())?;
        self.0.borrow_mut().extend(batch);
        Ok(answers)
    }
}

/// Scan and route like a scan with AI would (manual decisions and the
/// targets before the AI ones first), then count the AI candidates and how
/// many the cache at `cache_path` already answers. Reads only: no EXIF, no
/// thumbnails, no API call, nothing written; so conditions on metadata or
/// groups of files do not hold here and the count is an estimate for them.
pub fn ai_dry_run(
    cfg: &Config,
    overrides: &Overrides,
    cache_path: &Path,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<AiDry> {
    if !cfg.has_ai() {
        return Ok(AiDry::default());
    }
    cancel.check()?;
    let files = scan::scan_sources_with(&cfg.sources, progress, cancel)?;
    let counting = Counting(RefCell::new(Vec::new()));
    let ctx = RouteCtx::empty(&files);
    route_with_overrides(
        cfg, &files, &ctx, &counting, false, overrides, progress, cancel,
    )?;
    let batch = counting.0.into_inner();
    let cache = Cache::load(cache_path);
    let cached = batch
        .iter()
        .filter(|r| is_cached(&cache, &cfg.ai.model, r))
        .count();
    Ok(AiDry {
        candidates: batch.len(),
        cached,
        to_send: batch.len() - cached,
    })
}

/// [`scan_plan`] with the given classifier (`None`, or `opts.no_ai`, means
/// no AI). `ai_usage` of the result is `None`: only the caller knows the
/// classifier's usage.
pub fn scan_plan_using(
    cfg: &Config,
    tools: &Tools,
    opts: ScanOptions,
    overrides: &Overrides,
    classifier: Option<&dyn Classifier>,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<ScanOutcome> {
    cancel.check()?;
    let files = scan::scan_sources_with(&cfg.sources, progress, cancel)?;
    let paths: Vec<PathBuf> = files.iter().map(|f| f.path.clone()).collect();
    let exif = exif::read_with(tools, &paths, progress, cancel)?;
    let ctx = RouteCtx::build(cfg, &files, &exif, Some(tools), progress, cancel)?;
    let classifier: &dyn Classifier = match classifier {
        Some(c) if !opts.no_ai => c,
        _ => &NoAi,
    };
    let routed = route_with_overrides(
        cfg,
        &files,
        &ctx,
        classifier,
        opts.accept_low,
        overrides,
        progress,
        cancel,
    )?;
    let side = if opts.server {
        Side::Server
    } else {
        Side::Local
    };
    let plan = build_plan_with(cfg, routed.decisions, &exif, side, progress, cancel)?;
    Ok(ScanOutcome {
        plan,
        warnings: routed.warnings,
        overrides_applied: routed.overrides_applied,
        ai_usage: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use crate::overrides::Action;
    use crate::progress::Stage;
    use crate::progress::testing::Recorder;
    use crate::route::Via;
    use std::path::Path;

    struct Fixture {
        _dir: tempfile::TempDir,
        src: PathBuf,
        cfg: Config,
    }

    /// Source `s` with `cat.jpg` and `note.txt`; AI target `cats`, then
    /// `rest` takes everything after the AI.
    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("cat.jpg"), b"not really a jpeg").unwrap();
        std::fs::write(src.join("note.txt"), b"text").unwrap();
        let out = dir.path().join("out");
        let toml = format!(
            r#"
[[sources]]
id = "s"
path = "{src}"

[[targets.ai]]
name = "cats"
sources = ["s"]
description = "cats"
prefilter = {{ match = "*.jpg" }}

[[targets.ai.outputs]]
dir = "{out}/cats"

[[targets.after_ai]]
name = "rest"

[[targets.after_ai.filters]]
sources = ["s"]
match = "*"

[[targets.after_ai.outputs]]
dir = "{out}/rest"
"#,
            src = src.to_string_lossy().replace('\\', "/"),
            out = out.to_string_lossy().replace('\\', "/")
        );
        let path = dir.path().join("gallery.toml");
        std::fs::write(&path, toml).unwrap();
        let cfg = Config::load(&path).unwrap();
        cfg.validate().unwrap();
        Fixture {
            _dir: dir,
            src,
            cfg,
        }
    }

    fn tools() -> Tools {
        Tools::discover().unwrap()
    }

    fn target_of<'a>(plan: &'a Plan, path: &Path) -> Option<&'a str> {
        plan.items
            .iter()
            .find(|i| i.source == path)
            .map(|i| i.target.as_str())
    }

    struct Cats;
    impl Classifier for Cats {
        fn classify(&self, batch: Vec<AiRequest>) -> Result<Vec<AiAnswer>> {
            Ok(batch
                .iter()
                .map(|_| AiAnswer {
                    category: Some("cats".into()),
                    reason: "a cat".into(),
                    confidence: Confidence::Ok,
                })
                .collect())
        }
    }

    struct Panics;
    impl Classifier for Panics {
        fn classify(&self, _: Vec<AiRequest>) -> Result<Vec<AiAnswer>> {
            panic!("classifier must not be called");
        }
    }

    #[test]
    fn stages_run_in_order_and_ai_routes() {
        let f = fixture();
        let rec = Recorder::default();
        let out = scan_plan_using(
            &f.cfg,
            &tools(),
            ScanOptions::default(),
            &Overrides::default(),
            Some(&Cats),
            &rec,
            &Cancel::new(),
        )
        .unwrap();
        let cat = f.src.join("cat.jpg");
        assert_eq!(target_of(&out.plan, &cat), Some("cats"));
        let item = out.plan.items.iter().find(|i| i.source == cat).unwrap();
        assert!(matches!(&item.via, Via::Ai { reason, .. } if reason == "a cat"));
        assert_eq!(target_of(&out.plan, &f.src.join("note.txt")), Some("rest"));
        assert_eq!(out.overrides_applied, 0);
        assert!(out.ai_usage.is_none());
        // Every stage reported, in pipeline order.
        let order: Vec<Stage> = rec.0.lock().unwrap().iter().map(|e| e.stage).collect();
        let first = |s: Stage| order.iter().position(|&x| x == s).unwrap();
        assert!(first(Stage::Scan) < first(Stage::Exif));
        assert!(first(Stage::Exif) < first(Stage::Ai));
        assert!(first(Stage::Ai) < first(Stage::Plan));
        assert_eq!(rec.events(Stage::Scan).last().unwrap().done, 2);
        assert_eq!(rec.events(Stage::Plan).len(), 2);
    }

    #[test]
    fn no_ai_never_calls_the_classifier() {
        let f = fixture();
        let opts = ScanOptions {
            no_ai: true,
            ..Default::default()
        };
        let out = scan_plan_using(
            &f.cfg,
            &tools(),
            opts,
            &Overrides::default(),
            Some(&Panics),
            &Recorder::default(),
            &Cancel::new(),
        )
        .unwrap();
        assert_eq!(target_of(&out.plan, &f.src.join("cat.jpg")), Some("rest"));
        // The production entry point does not build a classifier either.
        let out = scan_plan(
            &f.cfg,
            &tools(),
            opts,
            &Overrides::default(),
            &Recorder::default(),
            &Cancel::new(),
        )
        .unwrap();
        assert!(out.ai_usage.is_none());
        assert_eq!(out.plan.items.len(), 2);
    }

    #[test]
    fn manual_decisions_come_first() {
        let f = fixture();
        let cat = f.src.join("cat.jpg");
        let note = f.src.join("note.txt");
        let meta = |p: &Path| {
            let m = std::fs::metadata(p).unwrap();
            (m.len(), scan::mtime_ns(&m).unwrap())
        };
        let mut ov = Overrides::default();
        let (size, mtime) = meta(&cat);
        ov.set(cat.clone(), size, mtime, Action::Keep);
        let (size, mtime) = meta(&note);
        ov.set(
            note.clone(),
            size,
            mtime,
            Action::Target {
                name: "cats".into(),
            },
        );
        let out = scan_plan_using(
            &f.cfg,
            &tools(),
            ScanOptions::default(),
            &ov,
            Some(&Panics),
            &Recorder::default(),
            &Cancel::new(),
        )
        .unwrap();
        assert_eq!(out.overrides_applied, 2);
        assert_eq!(out.plan.unchanged, vec![cat]);
        let item = out.plan.items.iter().find(|i| i.source == note).unwrap();
        assert_eq!((item.target.as_str(), &item.via), ("cats", &Via::Manual));
    }

    #[test]
    fn ai_dry_run_counts_candidates_and_cache_hits_without_calling_anything() {
        use crate::ai::cache::{self, Stored};
        let f = fixture();
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("cache.jsonl");
        let run = |ov: &Overrides| {
            ai_dry_run(
                &f.cfg,
                ov,
                &cache_path,
                &Recorder::default(),
                &Cancel::new(),
            )
            .unwrap()
        };
        // Only cat.jpg passes the prefilter; note.txt has no candidate.
        let d = run(&Overrides::default());
        assert_eq!((d.candidates, d.cached, d.to_send), (1, 0, 1));

        // An answer in the cache makes it free.
        let files = scan::scan_sources(&f.cfg.sources).unwrap();
        let cat = files.iter().find(|e| e.path.ends_with("cat.jpg")).unwrap();
        let cats = vec![("cats".to_string(), "cats".to_string())];
        let hash = cache::prompt_hash(&crate::ai::system_prompt(
            &cats,
            &crate::route::SourcePrompt::default(),
        ));
        let mut c = Cache::load(&cache_path);
        c.put(
            cache::key(cat, &f.cfg.ai.model, &hash),
            Stored {
                category: "cats".into(),
                reason: "r".into(),
                confidence: "OK".into(),
            },
        )
        .unwrap();
        let d = run(&Overrides::default());
        assert_eq!((d.candidates, d.cached, d.to_send), (1, 1, 0));

        // A cached answer of another model does not count.
        let mut other = f.cfg.clone();
        other.ai.model = "claude-haiku-5-5".into();
        let d = ai_dry_run(
            &other,
            &Overrides::default(),
            &cache_path,
            &Recorder::default(),
            &Cancel::new(),
        )
        .unwrap();
        assert_eq!((d.cached, d.to_send), (0, 1));

        // A manual decision takes the file out of the AI's hands.
        let mut ov = Overrides::default();
        ov.set(cat.path.clone(), cat.size, cat.mtime_ns, Action::Keep);
        assert_eq!(run(&ov), AiDry::default());
        // Nothing was written but the cache the test made itself.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn ai_dry_run_without_ai_targets_scans_nothing() {
        let mut f = fixture();
        f.cfg.targets.retain(|t| !t.has_ai());
        let rec = Recorder::default();
        let d = ai_dry_run(
            &f.cfg,
            &Overrides::default(),
            Path::new("/nonexistent/cache.jsonl"),
            &rec,
            &Cancel::new(),
        )
        .unwrap();
        assert_eq!(d, AiDry::default());
        assert!(rec.0.lock().unwrap().is_empty());
    }

    #[test]
    fn cancelled_before_start_reads_nothing() {
        let f = fixture();
        let cancel = Cancel::new();
        cancel.cancel();
        let rec = Recorder::default();
        let r = scan_plan_using(
            &f.cfg,
            &tools(),
            ScanOptions::default(),
            &Overrides::default(),
            Some(&Panics),
            &rec,
            &cancel,
        );
        assert!(matches!(r, Err(Error::Cancelled)));
        assert!(rec.0.lock().unwrap().is_empty());
    }

    #[test]
    fn conditions_and_event_expansion_see_the_whole_scan() {
        // Capture times come from the file names (the files have no EXIF).
        let dir = tempfile::tempdir().unwrap();
        let (cam, partner, out) = (
            dir.path().join("cam"),
            dir.path().join("partner"),
            dir.path().join("out"),
        );
        for (d, n) in [
            (&cam, "IMG_20260801_100000.jpg"),
            (&cam, "IMG_20260801_103000.jpg"),
            (&cam, "IMG_20260805_100000.jpg"),
            (&partner, "IMG_20260801_100500.jpg"),
        ] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join(n), n.as_bytes()).unwrap();
        }
        let p = |d: &Path| d.to_string_lossy().replace('\\', "/");
        let toml = format!(
            r#"
[[sources]]
id = "cam"
path = "{cam}"

[[sources]]
id = "partner"
path = "{partner}"

[[targets]]
name = "together"
[[targets.filters]]
sources = ["cam"]
near = {{ sources = ["partner"], time = "20m" }}
expand = "event"
[[targets.outputs]]
dir = "{out}/together"

[[targets]]
name = "rest"
[[targets.filters]]
sources = ["cam", "partner"]
[[targets.outputs]]
dir = "{out}/rest"
"#,
            cam = p(&cam),
            partner = p(&partner),
            out = p(&out)
        );
        let path = dir.path().join("gallery.toml");
        std::fs::write(&path, toml).unwrap();
        let cfg = Config::load(&path).unwrap();
        cfg.validate().unwrap();
        let out = scan_plan_using(
            &cfg,
            &tools(),
            ScanOptions::default(),
            &Overrides::default(),
            Some(&Panics),
            &crate::progress::NoProgress,
            &Cancel::new(),
        )
        .unwrap();
        let at = |d: &Path, n: &str| target_of(&out.plan, &d.join(n));
        // Near the partner's shot (5 min).
        assert_eq!(at(&cam, "IMG_20260801_100000.jpg"), Some("together"));
        // 25 min from it, but the same outing.
        assert_eq!(at(&cam, "IMG_20260801_103000.jpg"), Some("together"));
        assert_eq!(at(&cam, "IMG_20260805_100000.jpg"), Some("rest"));
        assert_eq!(at(&partner, "IMG_20260801_100500.jpg"), Some("rest"));
    }
}
