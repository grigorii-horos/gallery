//! "Estimate before the run": what a run of a plan would save and take, and
//! what an AI scan would cost, before the user commits. The model is in
//! `gallery_core::estimate`; this only gathers its inputs (the reports in the
//! GUI plans folder, the AI cache and the usage log). Reads only.

use std::path::Path;

use gallery_core::ai::{AiClassifier, usage_log};
use gallery_core::config::Config;
use gallery_core::estimate::{AiEstimate, Estimate, History, ai_estimate, estimate_with};
use gallery_core::overrides::Overrides;
use gallery_core::pipeline;
use gallery_core::plan::Plan;
use gallery_core::progress::{Cancel, NoProgress};

/// Estimate a run of the plan at `plan_path` with `jobs` files in parallel
/// (`None` or 0: the default), for the pilot slice of `pilot` files per target
/// when given, learning from the reports in `plans_dir`.
pub fn plan_estimate(
    plan_path: &Path,
    plans_dir: &Path,
    jobs: Option<usize>,
    pilot: Option<usize>,
) -> Result<Estimate, String> {
    let mut plan = Plan::load(plan_path).map_err(|e| e.to_string())?;
    if let Some(n) = pilot
        && let Ok((slice, _)) = crate::pilot::choose(&plan, n)
    {
        plan = slice;
    }
    let history = History::load_dir(plans_dir);
    let jobs = jobs
        .filter(|&j| j > 0)
        .unwrap_or_else(gallery_core::estimate::default_jobs);
    Ok(estimate_with(&plan, &history, jobs))
}

/// What a scan with AI would cost: files that would be sent (not cached, with
/// AI candidates), the average request from `usage_path`. No API call.
pub fn ai_scan_estimate(
    config: &Path,
    overrides: Option<&Path>,
    cache: &Path,
    usage_path: Option<&Path>,
) -> Result<AiEstimate, String> {
    let cfg = Config::load(config).map_err(|e| e.to_string())?;
    cfg.validate().map_err(|e| e.to_string())?;
    let ov = match overrides {
        Some(f) => Overrides::load(f).map_err(|e| e.to_string())?,
        None => Overrides::default(),
    };
    let dry = pipeline::ai_dry_run(&cfg, &ov, cache, &NoProgress, &Cancel::new())
        .map_err(|e| e.to_string())?;
    let rows = usage_path.map(usage_log::load).unwrap_or_default();
    Ok(ai_estimate(&dry, &cfg.ai.model, &rows))
}

/// [`ai_scan_estimate`] at the default cache and usage log.
pub fn ai_scan_estimate_default(
    config: &Path,
    overrides: Option<&Path>,
) -> Result<AiEstimate, String> {
    let cache =
        AiClassifier::default_cache_path().ok_or_else(|| gallery_core::t!("ai.no_cache_dir"))?;
    ai_scan_estimate(
        config,
        overrides,
        &cache,
        usage_log::default_path().as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_estimate_of_an_empty_history_is_rough_and_reads_only() {
        let d = tempfile::tempdir().unwrap();
        let plan_path = d.path().join("a.plan.json");
        let plan: Plan = serde_json::from_value(serde_json::json!({
            "version": 1, "created": "2026-11-05T10:00:00+02:00", "side": "local",
            "content_presets": {}, "items": [], "review": [],
            "unmatched": [], "warnings": [], "unchanged": []
        }))
        .unwrap();
        plan.save(&plan_path).unwrap();
        let e = plan_estimate(&plan_path, d.path(), Some(3), None).unwrap();
        assert_eq!((e.jobs, e.total.files), (3, 0));
        assert_eq!(e.source_label, "rough estimate");
        assert!(plan_estimate(&d.path().join("none"), d.path(), None, None).is_err());
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[test]
    fn ai_scan_estimate_counts_files_and_uses_the_usage_log() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("cat.jpg"), b"x").unwrap();
        std::fs::write(src.join("dog.jpg"), b"x").unwrap();
        let cfg = d.path().join("gallery.toml");
        std::fs::write(
            &cfg,
            format!(
                "[[sources]]\nid = \"s\"\npath = \"{r}/src\"\n\n\
                 [[targets.ai]]\nname = \"pets\"\nsources = [\"s\"]\ndescription = \"pets\"\n\
                 [[targets.ai.outputs]]\ndir = \"{r}/out\"\n",
                r = d.path().display()
            ),
        )
        .unwrap();
        let usage = d.path().join("usage.jsonl");
        let row = usage_log::UsageRow {
            model: gallery_core::config::AiSettings::default().model,
            requests: 10,
            input: 20_000,
            output: 1_000,
            cache_write: 0,
            cache_read: 0,
        };
        usage_log::append(&usage, &row).unwrap();
        let e = ai_scan_estimate(&cfg, None, &d.path().join("cache.jsonl"), Some(&usage)).unwrap();
        assert_eq!((e.candidates, e.cached, e.to_send), (2, 0, 2));
        assert_eq!(e.source_label, "from past scans (10 requests)");
        assert!(e.label.starts_with("≈ $"), "{}", e.label);
        assert!(!d.path().join("cache.jsonl").exists());
    }
}
