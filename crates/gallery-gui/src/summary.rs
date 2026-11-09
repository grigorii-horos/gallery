//! `load_config`: a read-only JSON summary of a config for the UI.

use crate::presets::{MaskInfo, PresetInfo, content_infos, mask_infos, name_infos};
use gallery_core::config::{Config, Filter, Output, PipeStep, Source, Target, TargetKind};
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
    /// In order: the first target with a matching filter takes a file.
    pub targets: Vec<TargetSummary>,
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
pub struct TargetSummary {
    pub name: String,
    pub kind: TargetKind,
    /// Some filter asks the AI.
    pub ai: bool,
    pub filters: Vec<FilterSummary>,
    pub outputs: Vec<OutputSummary>,
}

#[derive(Debug, Serialize)]
pub struct FilterSummary {
    pub sources: Vec<String>,
    #[serde(rename = "match")]
    pub matches: Option<String>,
    pub exclude: Option<String>,
    /// AI category description.
    pub ai: Option<String>,
    /// The keys of its other conditions (`date`, `near`, ...).
    pub conditions: Vec<&'static str>,
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
        targets: cfg.targets.iter().map(target).collect(),
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

fn filter(f: &Filter) -> FilterSummary {
    let set = [
        ("expand", f.expand.is_some()),
        ("date", f.date.is_some()),
        ("geo", f.geo.is_some()),
        ("device", f.device.is_some()),
        ("props", f.props.is_some()),
        ("path", f.path.is_some()),
        ("event", f.event.is_some()),
        ("near", f.near.is_some()),
        ("duplicate", f.duplicate.is_some()),
        ("series", f.series.is_some()),
    ];
    FilterSummary {
        sources: f.sources.clone(),
        matches: f.matches.as_ref().map(MaskExpr::source),
        exclude: mask_str(&f.exclude),
        ai: f.ai.clone(),
        conditions: set
            .into_iter()
            .filter(|(_, on)| *on)
            .map(|(k, _)| k)
            .collect(),
    }
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

fn target(t: &Target) -> TargetSummary {
    TargetSummary {
        name: t.name.clone(),
        kind: t.kind,
        ai: t.has_ai(),
        filters: t.filters.iter().map(filter).collect(),
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
    fn summary_targets_in_order() {
        // The old three stages, read as one list.
        let v = summary();
        let t = &v["targets"];
        assert_eq!(
            t[0],
            json!({
                "name": "Screenshots", "kind": "folders", "ai": false,
                "filters": [{"sources": ["cam"], "match": "Screenshot_*", "exclude": "@dated",
                             "ai": null, "conditions": []}],
                "outputs": [{"dir": "/lib/shots", "server_dir": null, "content": [],
                             "rename": [{"preset": "capture-date", "mask": null}]}]
            })
        );
        let ai = &t[1];
        assert_eq!(ai["name"], "Documents");
        assert_eq!(ai["ai"], true);
        assert_eq!(ai["filters"][0]["sources"], json!(["cam"]));
        assert_eq!(ai["filters"][0]["ai"], "Photos of documents");
        assert_eq!(ai["filters"][0]["match"], "@photo");
        assert_eq!(
            ai["outputs"][0]["content"],
            json!([{"preset": "tiny", "mask": null}, {"preset": "avif-high", "mask": "*.jpg"}])
        );
        assert_eq!(ai["outputs"][0]["server_dir"], "/srv/lib/docs");
        assert_eq!(t[2]["name"], "Everything else");
        assert_eq!(t[2]["filters"][0]["exclude"], json!(null));
        assert_eq!(t.as_array().unwrap().len(), 3);
    }

    #[test]
    fn summary_kinds_and_conditions() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("g.toml");
        std::fs::write(
            &p,
            "[[sources]]\nid = \"cam\"\npath = \"/m\"\n\n[[targets]]\nname = \"Dups\"\nkind = \"trash\"\n\
             [[targets.filters]]\nsources = [\"cam\"]\nduplicate = {}\nexpand = \"event\"\n",
        )
        .unwrap();
        let v = serde_json::to_value(load_summary(&p).unwrap()).unwrap();
        assert_eq!(v["targets"][0]["kind"], "trash");
        assert_eq!(
            v["targets"][0]["filters"][0]["conditions"],
            json!(["expand", "duplicate"])
        );
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
