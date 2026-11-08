//! Checks of an edited config: the core's own validation (with the field it
//! is about, where it can be told) and the editor's warnings.

use std::path::Path;

use gallery_core::config::{Config, Output};
use gallery_core::mask::{MaskExpr, MaskItem};
use serde::Serialize;

use super::limits::{KINDS, choices, limit_for};
use super::model::{
    ConfigDraft, ContentPresetDraft, NamePresetDraft, Stage, TargetDraft, target_path,
};
use super::presets::{Slot, kind_keys};

/// Why the config cannot be saved; `path` is the field it is about
/// (`sources[1]`, `targets.after_ai[0].filters[1].match`), when known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldError {
    pub message: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldWarning {
    /// Empty: about the config as a whole.
    pub path: String,
    pub message: String,
}

fn err(path: String, message: impl Into<String>) -> FieldError {
    FieldError {
        message: message.into(),
        path: Some(path),
    }
}

fn check_mask(path: String, items: &[String]) -> Result<(), FieldError> {
    if items.is_empty() {
        return Err(err(path, "empty mask: add at least one"));
    }
    for m in items {
        MaskItem::parse(m).map_err(|e| err(path.clone(), format!("mask “{m}”: {e}")))?;
    }
    Ok(())
}

fn check_target(path: &str, stage: Stage, t: &TargetDraft) -> Result<(), FieldError> {
    if t.name.trim().is_empty() {
        return Err(err(format!("{path}.name"), "the target has no name"));
    }
    if stage.is_mask() {
        for (i, f) in t.filters.iter().enumerate() {
            let fp = format!("{path}.filters[{i}]");
            if let Some(m) = &f.matches {
                check_mask(format!("{fp}.match"), m)?;
            }
            if !f.exclude.is_empty() {
                check_mask(format!("{fp}.exclude"), &f.exclude)?;
            }
        }
    } else {
        if t.description.as_deref().is_none_or(|d| d.trim().is_empty()) {
            return Err(err(
                format!("{path}.description"),
                "the AI target has no description: the AI will not know what to put in it",
            ));
        }
        if let Some(p) = &t.prefilter {
            if let Some(m) = &p.matches {
                check_mask(format!("{path}.prefilter.match"), m)?;
            }
            if !p.exclude.is_empty() {
                check_mask(format!("{path}.prefilter.exclude"), &p.exclude)?;
            }
        }
    }
    for (i, o) in t.outputs.iter().enumerate() {
        let op = format!("{path}.outputs[{i}]");
        if o.dir.trim().is_empty() {
            return Err(err(format!("{op}.dir"), "the output has no folder"));
        }
        for (pipe, steps) in [("content", &o.content), ("rename", &o.rename)] {
            for (j, s) in steps.iter().enumerate() {
                let sp = format!("{op}.{pipe}[{j}]");
                if s.preset.trim().is_empty() {
                    return Err(err(sp, "step without a preset"));
                }
                if let Some(m) = &s.mask {
                    check_mask(format!("{sp}.mask"), m)?;
                }
            }
        }
    }
    Ok(())
}

fn check_name_preset(p: &NamePresetDraft) -> Result<(), FieldError> {
    let base = format!("name_presets.{}", p.name);
    if p.template.trim().is_empty() {
        return Err(err(
            format!("{base}.template"),
            "the name preset has no template",
        ));
    }
    if let Some(d) = &p.date
        && !["capture", "file_created", "none"].contains(&d.as_str())
    {
        return Err(err(
            format!("{base}.date"),
            format!("date “{d}”: capture, file_created or none"),
        ));
    }
    for (key, m) in [("mask", &p.mask), ("skip_if", &p.skip_if)] {
        if let Some(m) = m {
            check_mask(format!("{base}.{key}"), m)?;
        }
    }
    Ok(())
}

fn check_content_preset(p: &ContentPresetDraft) -> Result<(), FieldError> {
    let base = format!("content_presets.{}", p.name);
    if !KINDS.contains(&p.kind.as_str()) {
        return Err(err(
            format!("{base}.kind"),
            format!("kind “{}”: {}", p.kind, KINDS.join(", ")),
        ));
    }
    if p.kind == "bundle" {
        if p.mask.is_some() {
            return Err(err(
                format!("{base}.mask"),
                "a set has no mask of its own: its steps define the masks",
            ));
        }
        let steps = p.steps.as_deref().unwrap_or_default();
        if steps.is_empty() {
            return Err(err(format!("{base}.steps"), "the set has no steps"));
        }
        for (i, s) in steps.iter().enumerate() {
            if let Some(m) = &s.mask {
                check_mask(format!("{base}.steps[{i}]"), m)?;
            }
        }
    }
    for key in kind_keys(&p.kind) {
        match p.slot(key) {
            Some(Slot::I(Some(v))) => {
                if let Some(l) = limit_for(p, key)
                    && !(l.min..=l.max).contains(&v)
                {
                    return Err(err(
                        format!("{base}.{key}"),
                        format!("{key} = {v}: allowed range is {} to {}", l.min, l.max),
                    ));
                }
            }
            Some(Slot::S(Some(v))) => {
                if let Some(c) = choices(&p.kind, key)
                    && !c.contains(&v.as_str())
                {
                    return Err(err(
                        format!("{base}.{key}"),
                        format!("{key} = “{v}”: one of {}", c.join(", ")),
                    ));
                }
            }
            _ => {}
        }
    }
    if let Some(m) = &p.mask {
        check_mask(format!("{base}.mask"), m)?;
    }
    Ok(())
}

/// What the editor checks itself, with exact field paths, before the core.
pub fn precheck(d: &ConfigDraft) -> Result<(), FieldError> {
    for (i, s) in d.sources.iter().enumerate() {
        let p = format!("sources[{i}]");
        if s.id.trim().is_empty() {
            return Err(err(format!("{p}.id"), "the source has no id"));
        }
        if d.sources[..i].iter().any(|o| o.id == s.id) {
            return Err(err(
                format!("{p}.id"),
                format!("id “{}” is already used by another source", s.id),
            ));
        }
        if s.path.trim().is_empty() {
            return Err(err(format!("{p}.path"), "the source has no path"));
        }
    }
    for (stage, i, t) in d.targets.all() {
        check_target(&target_path(stage, i), stage, t)?;
    }
    for p in &d.name_presets {
        check_name_preset(p)?;
    }
    for p in &d.content_presets {
        check_content_preset(p)?;
    }
    for m in &d.mask_defs {
        check_mask(format!("masks.{}", m.name), &m.items)?;
    }
    Ok(())
}

/// The text between the quotes after `marker` in `m`.
fn quoted<'a>(m: &'a str, marker: &str) -> Option<&'a str> {
    let rest = &m[m.find(marker)? + marker.len()..];
    rest.find('\'').map(|e| &rest[..e])
}

/// The field a core error message is about: the target or source it names.
pub fn locate(message: &str, d: &ConfigDraft) -> Option<String> {
    for marker in ["duplicate target name '", "target '"] {
        if let Some(name) = quoted(message, marker) {
            // A duplicate: the second one with the name.
            let skip = usize::from(marker.starts_with("duplicate"));
            if let Some((s, i, _)) = d.targets.all().filter(|(_, _, t)| t.name == name).nth(skip) {
                return Some(target_path(s, i));
            }
        }
    }
    for marker in ["source id '", "source '"] {
        if let Some(id) = quoted(message, marker)
            && let Some(i) = d.sources.iter().position(|s| s.id == id)
        {
            return Some(format!("sources[{i}]"));
        }
    }
    for (marker, base, names) in [
        (
            "name preset '",
            "name_presets",
            d.name_presets.iter().map(|p| &p.name).collect::<Vec<_>>(),
        ),
        (
            "content preset '",
            "content_presets",
            d.content_presets.iter().map(|p| &p.name).collect(),
        ),
        (
            "mask '",
            "masks",
            d.mask_defs.iter().map(|m| &m.name).collect(),
        ),
    ] {
        if let Some(n) = quoted(message, marker)
            && names.iter().any(|x| *x == n)
        {
            return Some(format!("{base}.{n}"));
        }
    }
    if let Some(rest) = message.split("mask cycle: ").nth(1) {
        let first = rest.split(" -> ").next().unwrap_or("").trim();
        if d.mask_defs.iter().any(|m| m.name == first) {
            return Some(format!("masks.{first}"));
        }
    }
    None
}

/// Parse and validate `text` as the config file at `config`. The parsed
/// config when it is valid, else the error.
pub fn validate(text: &str, config: &Path, d: &ConfigDraft) -> Result<Config, FieldError> {
    precheck(d)?;
    let dir = config.parent().unwrap_or(Path::new(""));
    let label = config.file_name().map(Path::new).unwrap_or(config);
    let cfg = Config::from_str_at(text, dir, label).map_err(|e| {
        let message = e.to_string();
        FieldError {
            path: locate(&message, d),
            message,
        }
    })?;
    cfg.validate().map_err(|e| {
        let message = e.to_string();
        FieldError {
            path: locate(&message, d),
            message,
        }
    })?;
    Ok(cfg)
}

/// Extensions tried when looking for a double re-encode.
const SAMPLE_EXTS: &[&str] = &[
    "jpg", "jpeg", "heic", "heif", "webp", "avif", "tif", "tiff", "bmp", "gif", "png", "mp4",
    "m4v", "mov", "mkv", "webm", "avi", "3gp", "mp3", "m4a", "aac", "flac", "wav", "ogg", "opus",
    "cr2", "nef", "arw", "dng",
];

fn matches_opt(m: &Option<MaskExpr>, name: &str, cfg: &Config) -> bool {
    m.as_ref().is_none_or(|m| m.matches(name, cfg))
}

/// The content steps a file named `name` goes through and how many of them
/// re-encode, as the planner runs the pipe.
fn content_run(o: &Output, name: &str, cfg: &Config) -> (Vec<String>, usize) {
    let mut name = name.to_string();
    let mut steps = Vec::new();
    let mut reencodes = 0;
    for st in &o.content {
        let Some(preset) = cfg.content_preset(&st.preset) else {
            continue;
        };
        let mask = st.mask.clone().unwrap_or_else(|| preset.mask_or_default());
        if mask.matches(&name, cfg) {
            let stem = name
                .rsplit_once('.')
                .map_or(name.as_str(), |(s, _)| s)
                .to_string();
            let ext = name.rsplit_once('.').map_or("", |(_, e)| e).to_string();
            let new_ext = preset.output_ext(&ext);
            name = if new_ext.is_empty() {
                stem
            } else {
                format!("{stem}.{new_ext}")
            };
            steps.push(st.preset.clone());
            reencodes += usize::from(preset.reencodes());
        }
    }
    (steps, reencodes)
}

/// The fixed part of an output folder template: up to the folder that
/// holds the first `{placeholder}`.
fn fixed_dir(dir: &str) -> &str {
    match dir.find('{') {
        None => dir,
        Some(i) => dir[..i].rfind('/').map_or("", |j| &dir[..j]),
    }
}

struct TargetView<'a> {
    stage: Stage,
    index: usize,
    sources: Vec<&'a str>,
    accepts: Box<dyn Fn(&str) -> bool + 'a>,
    outputs: &'a [Output],
}

fn views(cfg: &Config) -> Vec<TargetView<'_>> {
    let t = &cfg.targets;
    let mut out = Vec::new();
    for (stage, list) in [
        (Stage::BeforeAi, &t.before_ai),
        (Stage::AfterAi, &t.after_ai),
    ] {
        for (index, m) in list.iter().enumerate() {
            out.push(TargetView {
                stage,
                index,
                sources: m
                    .filters
                    .iter()
                    .flat_map(|f| f.sources.iter().map(String::as_str))
                    .collect(),
                accepts: Box::new(move |name| {
                    m.filters.iter().any(|f| {
                        matches_opt(&f.matches, name, cfg)
                            && (f.exclude.is_empty() || !f.exclude.matches(name, cfg))
                    })
                }),
                outputs: &m.outputs,
            });
        }
    }
    for (index, a) in t.ai.iter().enumerate() {
        out.push(TargetView {
            stage: Stage::Ai,
            index,
            sources: a.sources.iter().map(String::as_str).collect(),
            accepts: Box::new(move |name| match &a.prefilter {
                None => true,
                Some(p) => {
                    matches_opt(&p.matches, name, cfg)
                        && (p.exclude.is_empty() || !p.exclude.matches(name, cfg))
                }
            }),
            outputs: &a.outputs,
        });
    }
    out
}

/// Warnings of a valid config: the core's, and per target a missing
/// output, a filter without sources, an output that re-encodes a file
/// twice, an output folder inside a source (files are moved, not copied).
pub fn warnings(cfg: &Config, d: &ConfigDraft) -> Vec<FieldWarning> {
    let mut w: Vec<FieldWarning> = cfg
        .warnings()
        .into_iter()
        .map(|message| FieldWarning {
            path: String::new(),
            message,
        })
        .collect();
    let mut views = views(cfg);
    views.sort_by_key(|v| (v.stage as usize, v.index));
    for v in &views {
        let path = target_path(v.stage, v.index);
        let draft = &d.targets.stage(v.stage).get(v.index);
        if v.outputs.is_empty() {
            w.push(FieldWarning {
                path: format!("{path}.outputs"),
                message: "No outputs: matched files will stay in the source.".into(),
            });
        }
        if v.stage.is_mask() {
            for (i, f) in draft
                .map(|t| &t.filters[..])
                .unwrap_or_default()
                .iter()
                .enumerate()
            {
                if f.sources.is_empty() {
                    w.push(FieldWarning {
                        path: format!("{path}.filters[{i}].sources"),
                        message: "A filter without sources will not take any files.".into(),
                    });
                }
            }
        } else if v.sources.is_empty() {
            w.push(FieldWarning {
                path: format!("{path}.sources"),
                message: "No sources set: the AI will not see any files for this target.".into(),
            });
        }
        for (oi, o) in v.outputs.iter().enumerate() {
            let op = format!("{path}.outputs[{oi}]");
            let twice: Vec<String> = SAMPLE_EXTS
                .iter()
                .map(|e| format!("IMG_0001.{e}"))
                .filter(|n| (v.accepts)(n))
                .filter_map(|n| {
                    let (steps, re) = content_run(o, &n, cfg);
                    (re >= 2).then(|| {
                        format!(
                            "*.{} ({})",
                            n.rsplit('.').next().unwrap_or(""),
                            steps.join(" → ")
                        )
                    })
                })
                .collect();
            if !twice.is_empty() {
                w.push(FieldWarning {
                    path: format!("{op}.content"),
                    message: format!(
                        "Double lossy re-encoding: {}. Check the step masks.",
                        twice.join(", ")
                    ),
                });
            }
            let fixed = gallery_core::config::expand_tilde(Path::new(fixed_dir(&o.dir)));
            for s in cfg
                .sources
                .iter()
                .filter(|s| v.sources.contains(&s.id.as_str()))
            {
                if !fixed.as_os_str().is_empty()
                    && gallery_core::plan::inside_root(&fixed, &s.path, true)
                {
                    w.push(FieldWarning {
                        path: format!("{op}.dir"),
                        message: format!(
                            "Folder inside source “{}”: a file that needs no content step \
will be moved or renamed in place, not copied (no trash).",
                            s.id
                        ),
                    });
                    break;
                }
            }
        }
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_dir_stops_before_placeholders() {
        assert_eq!(fixed_dir("~/Media/{YYYY}/{MM}"), "~/Media");
        assert_eq!(fixed_dir("~/Media/x{YYYY}"), "~/Media");
        assert_eq!(
            fixed_dir("~/Captures/Screenshots"),
            "~/Captures/Screenshots"
        );
        assert_eq!(fixed_dir("{source}"), "");
    }

    #[test]
    fn locate_finds_targets_and_sources() {
        let mut d = ConfigDraft::default();
        d.sources.push(super::super::model::SourceDraft {
            id: "cam".into(),
            ..Default::default()
        });
        d.targets.after_ai.push(TargetDraft {
            name: "T".into(),
            ..Default::default()
        });
        d.targets.after_ai.push(TargetDraft {
            name: "T".into(),
            ..Default::default()
        });
        assert_eq!(
            locate("target 'T': unknown source 'x'", &d).as_deref(),
            Some("targets.after_ai[0]")
        );
        assert_eq!(
            locate("duplicate target name 'T'", &d).as_deref(),
            Some("targets.after_ai[1]")
        );
        assert_eq!(
            locate("source id 'cam': must be", &d).as_deref(),
            Some("sources[0]")
        );
        assert_eq!(locate("something else", &d), None);
    }
}
