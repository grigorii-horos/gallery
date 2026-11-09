//! `load_config`: a read-only JSON summary of a config for the UI.

use crate::presets::{MaskInfo, PresetInfo, content_infos, mask_infos, name_infos};
use gallery_core::config::{Config, Filter, Output, PipeStep, Source, Target};
use gallery_core::mask::MaskExpr;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct ConfigSummary {
    pub path: String,
    /// `Config::validate` passed; otherwise `error` says why. A config that
    /// does not even parse is an `Err` of `load_config` instead.
    pub valid: bool,
    pub error: Option<String>,
    pub warnings: Vec<String>,
    pub sources: Vec<SourceSummary>,
    pub targets: TargetsSummary,
    pub presets: UserPresets,
    pub ai: AiSummary,
}

#[derive(Debug, Serialize)]
pub struct SourceSummary {
    pub id: String,
    pub path: String,
    pub server_path: Option<String>,
    pub recursive: bool,
    pub skip: Vec<String>,
    /// Prompt text before / after the AI instructions (inline or read from the file).
    pub ai_prefix: Option<String>,
    pub ai_suffix: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TargetsSummary {
    pub before_ai: Vec<TargetSummary>,
    pub ai: Vec<TargetSummary>,
    pub after_ai: Vec<TargetSummary>,
}

/// A target of any stage; `filters` for mask stages, `sources` /
/// `description` / `prefilter` for the AI stage.
#[derive(Debug, Serialize)]
pub struct TargetSummary {
    pub name: String,
    pub stage: &'static str,
    pub filters: Vec<FilterSummary>,
    pub sources: Vec<String>,
    pub description: Option<String>,
    pub prefilter: Option<FilterSummary>,
    pub outputs: Vec<OutputSummary>,
}

#[derive(Debug, Serialize)]
pub struct FilterSummary {
    pub sources: Vec<String>,
    #[serde(rename = "match")]
    pub matches: Option<String>,
    pub exclude: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OutputSummary {
    pub dir: String,
    pub server_dir: Option<String>,
    pub content: Vec<StepSummary>,
    pub rename: Vec<StepSummary>,
}

#[derive(Debug, Serialize)]
pub struct StepSummary {
    pub preset: String,
    /// Mask override; `None`: the preset's own mask.
    pub mask: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserPresets {
    pub name: Vec<PresetInfo>,
    pub content: Vec<PresetInfo>,
    pub masks: Vec<MaskInfo>,
}

#[derive(Debug, Serialize)]
pub struct AiSummary {
    pub model: String,
    pub effort: String,
    pub jobs: usize,
}

/// Load `path` and summarize it; a config that fails to load is an error,
/// one that fails validation is summarized with `valid: false`.
pub fn load_summary(path: &Path) -> Result<ConfigSummary, String> {
    let cfg = Config::load(path).map_err(|e| e.to_string())?;
    Ok(summarize(&cfg, path))
}

pub fn summarize(cfg: &Config, path: &Path) -> ConfigSummary {
    let (valid, error) = match cfg.validate() {
        Ok(()) => (true, None),
        Err(e) => (false, Some(e.to_string())),
    };
    ConfigSummary {
        path: path.display().to_string(),
        valid,
        error,
        warnings: cfg.warnings(),
        sources: cfg.sources.iter().map(source).collect(),
        targets: targets(cfg),
        presets: UserPresets {
            name: name_infos(&cfg.name_presets, cfg),
            content: content_infos(&cfg.content_presets, cfg),
            masks: mask_infos(&cfg.masks, cfg),
        },
        ai: AiSummary {
            model: cfg.ai.model.clone(),
            effort: cfg.ai.effort.clone(),
            jobs: cfg.ai.jobs,
        },
    }
}

fn source(s: &Source) -> SourceSummary {
    SourceSummary {
        id: s.id.clone(),
        path: s.path.display().to_string(),
        server_path: s.server_path.as_ref().map(|p| p.display().to_string()),
        recursive: s.recursive,
        skip: s.skip.clone(),
        ai_prefix: s.ai_prefix.clone(),
        ai_suffix: s.ai_suffix.clone(),
    }
}

fn mask_str(m: &MaskExpr) -> Option<String> {
    (!m.is_empty()).then(|| m.source())
}

/// The ordered list in the old three-stage JSON shape the UI still reads
/// (TEMPORARY, until the GUI rework): AI targets, mask targets before the
/// first of them, and mask targets after it.
fn targets(cfg: &Config) -> TargetsSummary {
    let cut = gallery_core::route::ai_cut(cfg);
    let mut out = TargetsSummary {
        before_ai: Vec::new(),
        ai: Vec::new(),
        after_ai: Vec::new(),
    };
    for (i, t) in cfg.targets.iter().enumerate() {
        if t.has_ai() {
            out.ai.push(ai_target(t));
        } else if i < cut {
            out.before_ai.push(mask_target(t, "before_ai"));
        } else {
            out.after_ai.push(mask_target(t, "after_ai"));
        }
    }
    out
}

fn filter(f: &Filter) -> FilterSummary {
    FilterSummary {
        sources: f.sources.clone(),
        matches: f.matches.as_ref().map(MaskExpr::source),
        exclude: mask_str(&f.exclude),
    }
}

/// The masks of an AI filter as the old prefilter; none when it has none.
fn prefilter(f: &Filter) -> Option<FilterSummary> {
    (f.matches.is_some() || !f.exclude.is_empty()).then(|| FilterSummary {
        sources: Vec::new(),
        matches: f.matches.as_ref().map(MaskExpr::source),
        exclude: mask_str(&f.exclude),
    })
}

fn steps(s: &[PipeStep]) -> Vec<StepSummary> {
    s.iter()
        .map(|st| StepSummary {
            preset: st.preset.clone(),
            mask: st.mask.as_ref().map(MaskExpr::source),
        })
        .collect()
}

fn output(o: &Output) -> OutputSummary {
    OutputSummary {
        dir: o.dir.clone(),
        server_dir: o.server_dir.clone(),
        content: steps(&o.content),
        rename: steps(&o.rename),
    }
}

fn mask_target(t: &Target, stage: &'static str) -> TargetSummary {
    TargetSummary {
        name: t.name.clone(),
        stage,
        filters: t.filters.iter().map(filter).collect(),
        sources: Vec::new(),
        description: None,
        prefilter: None,
        outputs: t.outputs.iter().map(output).collect(),
    }
}

/// An AI target as the old shape had it: its first AI filter is the
/// sources, description and prefilter (a migrated target has just that one).
fn ai_target(t: &Target) -> TargetSummary {
    let f = t.filters.iter().find(|f| f.ai.is_some());
    TargetSummary {
        name: t.name.clone(),
        stage: "ai",
        filters: Vec::new(),
        sources: f.map(|f| f.sources.clone()).unwrap_or_default(),
        description: f.and_then(|f| f.ai.clone()),
        prefilter: f.and_then(prefilter),
        outputs: t.outputs.iter().map(output).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CONFIG: &str = r#"
[[sources]]
id = "cam"
path = "/media/cam"
server_path = "/srv/cam"
recursive = true
skip = [".stfolder"]
ai_prefix = "Family archive."

[masks]
mine = ["@photo", "*.heic"]

[name_presets.short]
template = "{date:%Y}/{stem}"

[content_presets.tiny]
kind = "photo"
format = "avif"
quality = 40

[[targets.before_ai]]
name = "Screenshots"
[[targets.before_ai.filters]]
sources = ["cam"]
match = "Screenshot_*"
exclude = ["@dated"]
[[targets.before_ai.outputs]]
dir = "/lib/shots"
rename = ["capture-date"]

[[targets.ai]]
name = "Documents"
sources = ["cam"]
description = "Photos of documents"
prefilter = { match = "@photo" }
[[targets.ai.outputs]]
dir = "/lib/docs"
server_dir = "/srv/lib/docs"
content = ["tiny", { mask = "*.jpg", preset = "avif-high" }]
rename = ["short"]

[[targets.after_ai]]
name = "Everything else"
[[targets.after_ai.filters]]
match = "*"
"#;

    fn summary() -> serde_json::Value {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("g.toml");
        std::fs::write(&p, CONFIG).unwrap();
        serde_json::to_value(load_summary(&p).unwrap()).unwrap()
    }

    #[test]
    fn summary_top_level_and_sources() {
        let v = summary();
        assert_eq!(v["valid"], json!(true), "{}", v["error"]);
        assert_eq!(v["error"], json!(null));
        assert!(v["warnings"].is_array());
        assert_eq!(
            v["sources"],
            json!([{
                "id": "cam", "path": "/media/cam", "server_path": "/srv/cam",
                "recursive": true, "skip": [".stfolder"],
                "ai_prefix": "Family archive.", "ai_suffix": null
            }])
        );
        assert!(v["ai"]["model"].is_string() && v["ai"]["jobs"].is_number());
    }

    #[test]
    fn summary_targets_per_stage() {
        let v = summary();
        let t = &v["targets"];
        assert_eq!(
            t["before_ai"][0],
            json!({
                "name": "Screenshots", "stage": "before_ai",
                "filters": [{"sources": ["cam"], "match": "Screenshot_*", "exclude": "@dated"}],
                "sources": [], "description": null, "prefilter": null,
                "outputs": [{"dir": "/lib/shots", "server_dir": null, "content": [],
                             "rename": [{"preset": "capture-date", "mask": null}]}]
            })
        );
        let ai = &t["ai"][0];
        assert_eq!(ai["stage"], "ai");
        assert_eq!(ai["sources"], json!(["cam"]));
        assert_eq!(ai["description"], "Photos of documents");
        assert_eq!(ai["prefilter"]["match"], "@photo");
        assert_eq!(
            ai["outputs"][0]["content"],
            json!([{"preset": "tiny", "mask": null}, {"preset": "avif-high", "mask": "*.jpg"}])
        );
        assert_eq!(ai["outputs"][0]["server_dir"], "/srv/lib/docs");
        assert_eq!(t["after_ai"][0]["filters"][0]["exclude"], json!(null));
    }

    #[test]
    fn summary_user_presets() {
        let v = summary();
        let p = &v["presets"];
        assert_eq!(p["name"][0]["name"], "short");
        assert_eq!(p["content"][0]["name"], "tiny");
        assert_eq!(p["content"][0]["kind"], "photo");
        assert_eq!(p["masks"][0]["name"], "mine");
        assert!(!p["masks"][0]["expanded"].as_str().unwrap().contains('@'));
    }

    #[test]
    fn invalid_config_is_summarized_unparsable_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("g.toml");
        std::fs::write(
            &p,
            "[[targets.ai]]\nname = \"x\"\nsources = [\"nope\"]\ndescription = \"d\"\n",
        )
        .unwrap();
        let s = load_summary(&p).unwrap();
        assert!(!s.valid);
        assert!(s.error.unwrap().contains("nope"));

        std::fs::write(&p, "not = [toml").unwrap();
        assert!(load_summary(&p).is_err());
        assert!(load_summary(&dir.path().join("missing.toml")).is_err());
    }
}
