//! Scanning from the GUI: the shared core pipeline in a background thread,
//! progress as `scan-progress` events, the result as `scan-done` or
//! `scan-error`. The plan is saved to the GUI plans folder.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use gallery_core::Error;
use gallery_core::ai::Usage;
use gallery_core::pipeline::ScanOutcome;
use gallery_core::plan::Plan;
use gallery_core::progress::{ProgressEvent, Stage};
use serde::{Deserialize, Serialize};

use crate::busy::Busy;

pub const PROGRESS_EVENT: &str = "scan-progress";
pub const DONE_EVENT: &str = "scan-done";
pub const ERROR_EVENT: &str = "scan-error";

/// The running scan, as `scan_status` tells it. The scan button is always
/// the explicit AI scan, so there is nothing to choose.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanRequest {}

/// `scan-progress` payload.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ScanProgress {
    pub stage: Stage,
    pub done: usize,
    /// 0 while not known (the scan stage).
    pub total: usize,
    pub current: Option<String>,
}

impl From<ProgressEvent> for ScanProgress {
    fn from(ev: ProgressEvent) -> Self {
        ScanProgress {
            stage: ev.stage,
            done: ev.done,
            total: ev.total,
            current: ev.current.map(|p| p.display().to_string()),
        }
    }
}

/// Tokens and price of the AI stage.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AiCost {
    pub input: u64,
    pub output: u64,
    /// USD; `None` for a model without a known price.
    pub usd: Option<f64>,
}

impl AiCost {
    pub fn new(u: Usage, model: &str) -> Self {
        AiCost {
            input: u.input + u.cache_write + u.cache_read,
            output: u.output,
            usd: u.cost(model),
        }
    }
}

/// Short result of a scan, sent with `scan-done`; the plan itself is read
/// with `load_plan`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ScanSummary {
    /// Files the plan will process.
    pub files: usize,
    /// Planned files per target, by target name.
    pub targets: BTreeMap<String, usize>,
    pub review: usize,
    pub unmatched: usize,
    pub unchanged: usize,
    /// Plan warnings (series split, double encoding…).
    pub warnings: usize,
    /// Routing notes, e.g. a manual decision naming an unknown target.
    pub notes: Vec<String>,
    pub overrides_applied: usize,
    /// `None` when the AI stage did not run.
    pub ai: Option<AiCost>,
}

impl ScanSummary {
    pub fn new(out: &ScanOutcome, model: &str) -> Self {
        let plan: &Plan = &out.plan;
        let mut targets = BTreeMap::new();
        for it in &plan.items {
            *targets.entry(it.target.clone()).or_default() += 1;
        }
        ScanSummary {
            files: plan.items.len(),
            targets,
            review: plan.review.len(),
            unmatched: plan.unmatched.len(),
            unchanged: plan.unchanged.len(),
            warnings: plan.warnings.len(),
            notes: out.warnings.clone(),
            overrides_applied: out.overrides_applied,
            ai: out.ai_usage.map(|u| AiCost::new(u, model)),
        }
    }
}

/// `scan-done` payload.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ScanDone {
    pub plan_path: String,
    pub summary: ScanSummary,
}

/// `scan-error` payload.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ScanError {
    pub message: String,
    /// Stopped by "Cancel": not a failure.
    pub cancelled: bool,
}

impl From<Error> for ScanError {
    fn from(e: Error) -> Self {
        let cancelled = matches!(e, Error::Cancelled);
        ScanError {
            message: if cancelled {
                gallery_core::t!("scan.cancelled")
            } else {
                e.to_string()
            },
            cancelled,
        }
    }
}

/// Lets through the events worth drawing: the first of a stage, the last of
/// a stage (`done == total`) and otherwise at most one per `every`.
pub struct Throttle {
    every: Duration,
    last: Mutex<Option<(Stage, Instant)>>,
}

impl Throttle {
    pub fn new(every: Duration) -> Self {
        Throttle {
            every,
            last: Mutex::new(None),
        }
    }

    pub fn pass(&self, ev: &ProgressEvent, now: Instant) -> bool {
        let mut last = self.last.lock().expect("throttle lock");
        let pass = match *last {
            None => true,
            Some((stage, at)) => {
                stage != ev.stage
                    || (ev.total > 0 && ev.done >= ev.total)
                    || now.duration_since(at) >= self.every
            }
        };
        if pass {
            *last = Some((ev.stage, now));
        }
        pass
    }
}

/// Run the scan `work` in the slot already claimed from `busy` and free the
/// slot however it ends: a panic becomes a (not cancelled) [`ScanError`], so
/// the UI always gets an end event and the next operation can start.
pub fn run_guarded<T>(
    busy: &Busy,
    work: impl FnOnce() -> Result<T, ScanError>,
) -> Result<T, ScanError> {
    crate::busy::run_guarded(busy, work, |what| ScanError {
        message: gallery_core::t!("gui.internal_scan", error = what),
        cancelled: false,
    })
}

/// Default locations of a GUI scan.
pub fn default_plans_dir() -> Result<PathBuf, String> {
    crate::plans::default_dir().ok_or_else(|| gallery_core::t!("lock.no_data_dir"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::busy::{Kind, Op};
    use std::path::Path;

    use gallery_core::Error;
    use gallery_core::config::Config;
    use gallery_core::overrides::{Action, Overrides};
    use gallery_core::pipeline::{self, ScanOptions};
    use gallery_core::progress::{Cancel, Progress};
    use gallery_core::route::Classifier;
    use gallery_core::route::{AiAnswer, AiRequest, Confidence};
    use gallery_core::tools::Tools;
    use serde_json::json;

    /// Where a scan reads and writes.
    pub(crate) struct ScanEnv<'a> {
        pub config: &'a Path,
        pub plans_dir: &'a Path,
        /// The manual decisions file; `None`: no decisions.
        pub overrides: Option<&'a Path>,
        pub tools: &'a Tools,
    }

    /// Test helper: load and validate the config, run the pipeline (no AI unless
    /// `classifier` is given) and save the plan as a
    /// new file in `plans_dir`.
    pub(crate) fn run(
        env: &ScanEnv,
        classifier: Option<&dyn Classifier>,
        progress: &dyn Progress,
        cancel: &Cancel,
    ) -> Result<ScanDone, ScanError> {
        let opts = ScanOptions {
            server: false,
            accept_low: false,
            no_ai: true,
        };
        run_with_options(env, opts, classifier, progress, cancel)
    }

    /// [`run`] with explicit pipeline options (the server export plans with
    /// `server: true` into the folder the user chose).
    pub(crate) fn run_with_options(
        env: &ScanEnv,
        opts: ScanOptions,
        classifier: Option<&dyn Classifier>,
        progress: &dyn Progress,
        cancel: &Cancel,
    ) -> Result<ScanDone, ScanError> {
        let cfg = Config::load(env.config)?;
        cfg.validate()?;
        let overrides = match env.overrides {
            Some(f) => Overrides::load(f)?,
            None => Overrides::default(),
        };
        let out = match classifier {
            Some(c) => pipeline::scan_plan_using(
                &cfg,
                env.tools,
                opts,
                &overrides,
                Some(c),
                progress,
                cancel,
            )?,
            None => pipeline::scan_plan(&cfg, env.tools, opts, &overrides, progress, cancel)?,
        };
        std::fs::create_dir_all(env.plans_dir).map_err(Error::from)?;
        let path = crate::plans::new_plan_path(env.plans_dir, chrono::Local::now().naive_local());
        out.plan.save(&path)?;
        Ok(ScanDone {
            plan_path: path.display().to_string(),
            summary: ScanSummary::new(&out, &cfg.ai.model),
        })
    }

    fn ev(stage: Stage, done: usize, total: usize) -> ProgressEvent {
        ProgressEvent {
            stage,
            done,
            total,
            current: Some(PathBuf::from("/s/a.jpg")),
        }
    }

    #[test]
    fn progress_payload_shape() {
        let p: ScanProgress = ev(Stage::Exif, 3, 10).into();
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            json!({"stage": "exif", "done": 3, "total": 10, "current": "/s/a.jpg"})
        );
    }

    #[test]
    fn error_payload_tells_cancel_from_failure() {
        let c: ScanError = Error::Cancelled.into();
        assert!(c.cancelled);
        assert_eq!(c.message, "Scan cancelled");
        let f: ScanError = Error::Config("bad".into()).into();
        assert_eq!(
            serde_json::to_value(&f).unwrap(),
            json!({"message": f.message, "cancelled": false})
        );
        assert!(f.message.contains("bad"));
    }

    #[test]
    fn throttle_passes_stage_edges_and_every_interval() {
        let t = Throttle::new(Duration::from_millis(100));
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        assert!(t.pass(&ev(Stage::Scan, 1, 0), ms(0)));
        assert!(!t.pass(&ev(Stage::Scan, 2, 0), ms(10)));
        assert!(t.pass(&ev(Stage::Scan, 3, 0), ms(120)));
        // A new stage passes at once, so does its end.
        assert!(t.pass(&ev(Stage::Exif, 1, 5), ms(125)));
        assert!(!t.pass(&ev(Stage::Exif, 4, 5), ms(130)));
        assert!(t.pass(&ev(Stage::Exif, 5, 5), ms(131)));
    }

    #[test]
    fn one_scan_at_a_time() {
        let s = Busy::default();
        assert!(!s.cancel(Kind::Scan));
        assert_eq!(s.scan(), None);
        let req = ScanRequest::default();
        let c = s.begin(Op::Scan(req)).unwrap();
        // A reloaded webview learns what is running.
        assert_eq!(s.scan(), Some(req));
        assert!(s.begin(Op::Scan(ScanRequest::default())).is_err());
        assert!(s.cancel(Kind::Scan));
        assert!(c.is_cancelled());
        s.finish();
        assert_eq!(s.scan(), None);
        assert!(s.begin(Op::Scan(req)).is_ok());
    }

    #[test]
    fn a_panicking_scan_frees_the_slot_and_reports_an_error() {
        let s = Busy::default();
        s.begin(Op::Scan(ScanRequest::default())).unwrap();
        let out = run_guarded(&s, || -> Result<ScanDone, ScanError> { panic!("boom") });
        let err = out.unwrap_err();
        assert!(!err.cancelled);
        assert!(err.message.contains("boom"), "{}", err.message);
        assert_eq!(s.scan(), None);
        assert!(s.begin(Op::Scan(ScanRequest::default())).is_ok());
    }

    #[test]
    fn a_finished_scan_frees_the_slot_and_keeps_its_result() {
        let s = Busy::default();
        s.begin(Op::Scan(ScanRequest::default())).unwrap();
        let out: Result<ScanDone, ScanError> = run_guarded(&s, || {
            Err(ScanError {
                message: "x".into(),
                cancelled: true,
            })
        });
        assert_eq!(
            out,
            Err(ScanError {
                message: "x".into(),
                cancelled: true
            })
        );
        assert_eq!(s.scan(), None);
    }

    #[test]
    fn ai_cost_counts_cached_input() {
        let u = Usage {
            requests: 1,
            input: 10,
            output: 5,
            cache_write: 2,
            cache_read: 3,
        };
        let c = AiCost::new(u, "unknown-model");
        assert_eq!((c.input, c.output, c.usd), (15, 5, None));
        assert!(AiCost::new(u, "claude-haiku-5-5").usd.unwrap() > 0.0);
    }

    struct Cats;
    impl Classifier for Cats {
        fn classify(&self, batch: Vec<AiRequest>) -> gallery_core::Result<Vec<AiAnswer>> {
            Ok(batch
                .iter()
                .map(|_| AiAnswer {
                    category: Some("cats".into()),
                    reason: "a cat".into(),
                    confidence: Confidence::Low,
                })
                .collect())
        }
    }

    /// Config with source `s` (`cat.jpg`, `note.txt`, `kept.txt`), AI
    /// target `cats` for jpg and `rest` for everything after the AI.
    pub(crate) fn fixture(dir: &Path) -> PathBuf {
        let src = dir.join("src");
        std::fs::create_dir(&src).unwrap();
        for f in ["cat.jpg", "note.txt", "kept.txt"] {
            std::fs::write(src.join(f), f).unwrap();
        }
        let out = dir.join("out");
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
        let cfg = dir.join("gallery.toml");
        std::fs::write(&cfg, toml).unwrap();
        cfg
    }

    #[test]
    fn run_saves_a_plan_and_summarizes_it() {
        let dir = tempfile::tempdir().unwrap();
        let config = fixture(dir.path());
        let plans = dir.path().join("plans");
        let ovf = dir.path().join("overrides.json");
        crate::overrides::set_in(
            &ovf,
            &config,
            &dir.path().join("src/kept.txt"),
            Action::Keep,
        )
        .unwrap();
        let tools = Tools::discover().unwrap();
        let env = ScanEnv {
            config: &config,
            plans_dir: &plans,
            overrides: Some(&ovf),
            tools: &tools,
        };
        let events = Mutex::new(Vec::new());
        let sink = |e: ProgressEvent| events.lock().unwrap().push(e.stage);

        // Without AI: the jpg falls through to `rest`.
        let done = run(&env, None, &sink, &Cancel::new()).unwrap();
        assert!(done.plan_path.ends_with(".plan.json"));
        assert!(std::path::Path::new(&done.plan_path).starts_with(&plans));
        let s = serde_json::to_value(&done.summary).unwrap();
        assert_eq!(
            s,
            json!({"files": 2, "targets": {"rest": 2}, "review": 0, "unmatched": 0,
                   "unchanged": 1, "warnings": 0, "notes": [], "overrides_applied": 1,
                   "ai": null})
        );
        assert!(events.lock().unwrap().contains(&Stage::Plan));

        // With a classifier: a low answer goes to review unless accepted.
        let ai = |accept_low| ScanOptions {
            server: false,
            accept_low,
            no_ai: false,
        };
        let done = run_with_options(&env, ai(false), Some(&Cats), &sink, &Cancel::new()).unwrap();
        assert_eq!(done.summary.review, 1);
        let done = run_with_options(&env, ai(true), Some(&Cats), &sink, &Cancel::new()).unwrap();
        assert_eq!(done.summary.targets.get("cats"), Some(&1));
        let plan = Plan::load(Path::new(&done.plan_path)).unwrap();
        assert!(
            plan.items
                .iter()
                .any(|i| matches!(&i.via, gallery_core::route::Via::Ai { low: true, .. }))
        );
        // Three scans, three plans.
        assert_eq!(std::fs::read_dir(&plans).unwrap().count(), 3);
    }

    #[test]
    fn run_reports_bad_config_and_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let config = fixture(dir.path());
        let plans = dir.path().join("plans");
        let tools = Tools::discover().unwrap();
        let env = ScanEnv {
            config: &config,
            plans_dir: &plans,
            overrides: None,
            tools: &tools,
        };
        let cancel = Cancel::new();
        cancel.cancel();
        let e = run(&env, None, &|_: ProgressEvent| {}, &cancel).unwrap_err();
        assert!(e.cancelled);
        assert!(!plans.exists(), "no plan for a cancelled scan");

        let missing = dir.path().join("missing.toml");
        let env = ScanEnv {
            config: &missing,
            ..env
        };
        let e = run(&env, None, &|_: ProgressEvent| {}, &Cancel::new()).unwrap_err();
        assert!(!e.cancelled);
    }
}
