//! Editing the user's config file in place (Sources and Targets screens).
//!
//! The file is loaded as a `toml_edit` document, typed operations change
//! only what they are about, and the result is checked by the core (parsed
//! as if it were the file, so `ai_*_file` paths resolve, then validated)
//! before it is written atomically with a `.bak` of the previous text.
//! Comments, blank lines and key order of untouched places stay
//! byte-for-byte. A file changed outside since it was read is not
//! overwritten: the UI must reload it.

pub mod checks;
pub mod doc;
pub mod file;
pub mod limits;
pub mod model;
pub mod ops;
pub mod order;
pub mod presets;

#[cfg(test)]
mod preset_tests;
#[cfg(test)]
mod tests;

use std::path::Path;

use gallery_core::config::Config;
use gallery_core::mask::{MaskExpr, MaskItem};
use gallery_core::route::SourcePrompt;
use serde::{Deserialize, Serialize};
use toml_edit::DocumentMut;

pub use checks::{FieldError, FieldWarning};
pub use model::{ConfigDraft, Stage};
pub use ops::EditOp;
pub use presets::Usage;

/// Why an edit could not even be tried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// The file changed since the UI read it: reload, then edit again.
    Changed,
    /// The file cannot be read or is not TOML the editor understands.
    Read,
    /// An operation does not fit the file (its index is `op`).
    Op,
    /// Writing failed.
    Io,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EditFailure {
    pub kind: FailureKind,
    pub message: String,
    /// Index of the failed operation (`kind: op`).
    pub op: Option<usize>,
}

impl EditFailure {
    fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            op: None,
        }
    }
}

/// A target renamed by the operations. Overrides and the history refer to
/// targets by name: the UI offers to move overrides to the new name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TargetRename {
    pub stage: Stage,
    pub index: usize,
    pub from: String,
    pub to: String,
    /// Overrides still naming `from` (filled by [`with_override_warnings`]).
    pub overrides: usize,
}

/// The config after the operations: the draft to show, whether it was
/// written, and why not.
#[derive(Debug, Clone, Serialize)]
pub struct EditOutcome {
    /// Version of the file on disk (after a save: of the new text).
    pub version: String,
    pub draft: ConfigDraft,
    /// The operations changed the text.
    pub changed: bool,
    pub saved: bool,
    /// The edited config is invalid: it was not (and will not be) written.
    pub error: Option<FieldError>,
    pub warnings: Vec<FieldWarning>,
    pub renames: Vec<TargetRename>,
    /// Where each of the user's presets and masks is used.
    pub usage: Usage,
    /// The user's presets and masks with descriptions and expanded masks
    /// (when the edited config parses).
    pub own: Option<crate::summary::UserPresets>,
}

struct Applied {
    text: String,
    draft: ConfigDraft,
    result: Result<Config, FieldError>,
    renames: Vec<TargetRename>,
}

struct Edited {
    original: String,
    version: String,
    text: String,
    draft: ConfigDraft,
    result: Result<Config, FieldError>,
    renames: Vec<TargetRename>,
}

fn read_doc(text: &str) -> Result<DocumentMut, EditFailure> {
    text.parse::<DocumentMut>()
        .map_err(|e| EditFailure::new(FailureKind::Read, format!("config is not valid TOML: {e}")))
}

/// Apply `ops` to `text` (the file at `path`) and check the result.
fn edit_text(path: &Path, text: &str, ops: &[EditOp]) -> Result<Applied, EditFailure> {
    let mut doc = read_doc(text)?;
    model_of(&doc)?;
    let mut renames: Vec<TargetRename> = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        let old_name = match op {
            EditOp::UpdateTarget { stage, index, .. } => model_of(&doc)?
                .targets
                .stage(*stage)
                .get(*index)
                .map(|t| t.name.clone()),
            _ => None,
        };
        ops::apply(&mut doc, op).map_err(|message| EditFailure {
            kind: FailureKind::Op,
            message,
            op: Some(i),
        })?;
        if let (
            Some(from),
            EditOp::UpdateTarget {
                stage,
                index,
                target,
            },
        ) = (old_name, op)
            && from != target.name
        {
            // A later rename of the same target continues the chain.
            match renames
                .iter_mut()
                .find(|r| r.stage == *stage && r.index == *index && r.to == from)
            {
                Some(r) if r.from == target.name => {
                    renames.retain(|r| !(r.stage == *stage && r.index == *index));
                }
                Some(r) => r.to = target.name.clone(),
                None => renames.push(TargetRename {
                    stage: *stage,
                    index: *index,
                    from,
                    to: target.name.clone(),
                    overrides: 0,
                }),
            }
        }
    }
    let new = if ops.is_empty() {
        text.to_string()
    } else {
        doc.to_string()
    };
    let draft = model_of(&doc)?;
    let result = checks::validate(&new, path, &draft);
    Ok(Applied {
        text: new,
        draft,
        result,
        renames,
    })
}

fn model_of(doc: &DocumentMut) -> Result<ConfigDraft, EditFailure> {
    doc::read_draft(doc).map_err(|m| {
        EditFailure::new(
            FailureKind::Read,
            format!(
                "the editor does not understand this config entry ({m}); edit the file by hand"
            ),
        )
    })
}

fn edit(path: &Path, ops: &[EditOp]) -> Result<Edited, EditFailure> {
    let (original, version) = file::read(path)
        .map_err(|e| EditFailure::new(FailureKind::Read, format!("{}: {e}", path.display())))?;
    let a = edit_text(path, &original, ops)?;
    Ok(Edited {
        original,
        version,
        text: a.text,
        draft: a.draft,
        result: a.result,
        renames: a.renames,
    })
}

fn outcome(e: Edited, version: String, saved: bool) -> EditOutcome {
    let changed = e.text != e.original;
    let (error, warnings) = match &e.result {
        Ok(cfg) => (None, checks::warnings(cfg, &e.draft)),
        Err(err) => (Some(err.clone()), Vec::new()),
    };
    let own = parsed(&e.text).map(|cfg| crate::summary::UserPresets {
        name: crate::presets::name_infos(&cfg.name_presets, &cfg),
        content: crate::presets::content_infos(&cfg.content_presets, &cfg),
        masks: crate::presets::mask_infos(&cfg.masks, &cfg),
    });
    EditOutcome {
        version,
        usage: presets::usage(&e.draft),
        draft: e.draft,
        changed,
        saved,
        error,
        warnings,
        renames: e.renames,
        own,
    }
}

/// `text` parsed as a config, without reading prompt files or validating.
fn parsed(text: &str) -> Option<Config> {
    let mut doc = text.parse::<DocumentMut>().ok()?;
    // Prompt files are not needed for presets.
    if let Some(s) = doc
        .get_mut("sources")
        .and_then(|s| s.as_array_of_tables_mut())
    {
        for t in s.iter_mut() {
            t.remove("ai_prefix_file");
            t.remove("ai_suffix_file");
        }
    }
    Config::from_str_at(&doc.to_string(), Path::new(""), Path::new("draft")).ok()
}

fn changed_failure() -> EditFailure {
    EditFailure::new(
        FailureKind::Changed,
        "the file changed after the editor read it; save cancelled, file untouched",
    )
}

/// Add to `out` a warning for every renamed target that overrides in
/// `overrides` still name (and count them in `out.renames`). An unreadable
/// overrides file adds nothing.
pub fn with_override_warnings(out: &mut EditOutcome, overrides: &Path) {
    if out.renames.is_empty() {
        return;
    }
    let Ok(ov) = gallery_core::overrides::Overrides::load(overrides) else {
        return;
    };
    let still_named = |name: &str| {
        [Stage::BeforeAi, Stage::Ai, Stage::AfterAi]
            .iter()
            .any(|s| out.draft.targets.stage(*s).iter().any(|t| t.name == name))
    };
    let kept: Vec<bool> = out.renames.iter().map(|r| still_named(&r.from)).collect();
    for (r, kept) in out.renames.iter_mut().zip(kept) {
        r.overrides = ov
            .entries()
            .filter(|e| matches!(&e.action, gallery_core::overrides::Action::Target { name } if *name == r.from))
            .count();
        if r.overrides > 0 {
            let after = if kept {
                format!("the name “{}” will belong to another target", r.from)
            } else {
                "they will point to a target that no longer exists".to_string()
            };
            out.warnings.push(FieldWarning {
                path: format!("{}.name", model::target_path(r.stage, r.index)),
                message: format!(
                    "{1} manual decision(s) use target “{0}”. After the rename {2}; after saving you can move them to “{3}”.",
                    r.from, r.overrides, after, r.to
                ),
            });
        }
    }
}

/// The config at `path` with `ops` applied, checked; nothing is written.
/// With `version`, a file that no longer has it is refused (`Changed`), so
/// that the operations are not replayed on text they were not made for.
pub fn draft(
    path: &Path,
    version: Option<&str>,
    ops: &[EditOp],
) -> Result<EditOutcome, EditFailure> {
    let e = edit(path, ops)?;
    if version.is_some_and(|v| v != e.version) {
        return Err(changed_failure());
    }
    let v = e.version.clone();
    Ok(outcome(e, v, false))
}

/// The saved config at `path` and the same config with `ops` applied (the
/// editor's unsaved changes), both checked; nothing is written. `Err` is a
/// message for the user (unreadable, or the draft is invalid).
pub fn configs(path: &Path, ops: &[EditOp]) -> Result<(Config, Config), String> {
    let old = edit(path, &[]).map_err(|f| f.message)?;
    let old = old.result.map_err(|e| e.message)?;
    let new = edit(path, ops).map_err(|f| f.message)?;
    let new = new.result.map_err(|e| e.message)?;
    Ok((old, new))
}

/// Apply `ops` to the config at `path` and write it, if the file still has
/// `version` and the result is valid. An invalid result is returned with
/// its error and not written.
pub fn save(path: &Path, version: &str, ops: &[EditOp]) -> Result<EditOutcome, EditFailure> {
    let changed = changed_failure;
    let e = edit(path, ops)?;
    if e.version != version {
        return Err(changed());
    }
    if e.result.is_err() || e.text == e.original {
        let v = e.version.clone();
        return Ok(outcome(e, v, false));
    }
    file::write_atomic(path, version, &e.text).map_err(|w| match w {
        file::WriteError::Changed => changed(),
        file::WriteError::Io(err) => EditFailure::new(
            FailureKind::Io,
            format!("could not write {}: {err}", path.display()),
        ),
    })?;
    let v = file::version_of(e.text.as_bytes());
    Ok(outcome(e, v, true))
}

/// One line of a [`ChangePreview`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffLine {
    /// `ctx`, `add` or `del`.
    pub kind: &'static str,
    pub text: String,
}

/// The exact change of the file `ops` would make, as lines.
#[derive(Debug, Clone, Serialize)]
pub struct ChangePreview {
    /// Version of the file the preview was made from; save with it.
    pub version: String,
    pub changed: bool,
    pub diff: Vec<DiffLine>,
    /// The edited config is invalid: saving would be refused.
    pub error: Option<FieldError>,
    pub warnings: Vec<FieldWarning>,
}

/// Lines of context around a change in [`preview_change`].
const CONTEXT: usize = 2;

/// The text change as lines: what differs between `old` and `new` after
/// their common head and tail, with a little context.
pub fn diff_lines(old: &str, new: &str) -> Vec<DiffLine> {
    let (a, b): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let line = |kind, t: &str| DiffLine {
        kind,
        text: t.to_string(),
    };
    let mut out = Vec::new();
    out.extend(
        a[head.saturating_sub(CONTEXT)..head]
            .iter()
            .map(|t| line("ctx", t)),
    );
    out.extend(a[head..a.len() - tail].iter().map(|t| line("del", t)));
    out.extend(b[head..b.len() - tail].iter().map(|t| line("add", t)));
    let after = &a[a.len() - tail..];
    out.extend(after.iter().take(CONTEXT).map(|t| line("ctx", t)));
    out
}

/// What `ops` would change in the config at `path`; nothing is written.
/// With `version`, a file that no longer has it is refused (`Changed`).
pub fn preview_change(
    path: &Path,
    version: Option<&str>,
    ops: &[EditOp],
) -> Result<ChangePreview, EditFailure> {
    let e = edit(path, ops)?;
    if version.is_some_and(|v| v != e.version) {
        return Err(changed_failure());
    }
    let (error, warnings) = match &e.result {
        Ok(cfg) => (None, checks::warnings(cfg, &e.draft)),
        Err(err) => (Some(err.clone()), Vec::new()),
    };
    Ok(ChangePreview {
        version: e.version.clone(),
        changed: e.text != e.original,
        diff: diff_lines(&e.original, &e.text),
        error,
        warnings,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct PromptPreview {
    /// The AI system prompt for a file of the source (every AI target that
    /// takes from the source is listed; a prefilter may leave some out for
    /// a given file).
    pub prompt: String,
    pub categories: Vec<String>,
}

/// The system prompt the AI gets for files of source `source_id` in the
/// edited config, with the user's corrections from `examples`. No request
/// is made.
pub fn preview_prompt(
    path: &Path,
    ops: &[EditOp],
    source_id: &str,
    examples: Option<&Path>,
) -> Result<PromptPreview, String> {
    let (text, _) = file::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let new = edit_text(path, &text, ops).map_err(|e| e.message)?.text;
    let dir = path.parent().unwrap_or(Path::new(""));
    let label = path.file_name().map(Path::new).unwrap_or(path);
    let cfg = Config::from_str_at(&new, dir, label).map_err(|e| e.to_string())?;
    let source = cfg
        .sources
        .iter()
        .find(|s| s.id == source_id)
        .ok_or_else(|| format!("source “{source_id}” is not in the config"))?;
    let categories: Vec<(String, String)> = cfg
        .targets
        .ai
        .iter()
        .filter(|t| t.sources.iter().any(|s| s == source_id))
        .map(|t| (t.name.clone(), t.description.clone()))
        .collect();
    // The user's corrections go into the prompt after the source's own texts.
    let examples = examples
        .map(|f| {
            let mut all = gallery_core::learn::Store::new(f).load();
            all.retain(|c| c.source_id == source_id);
            gallery_core::learn::render_examples(&all, &categories, cfg.ai.examples)
        })
        .unwrap_or_default();
    let prompt = gallery_core::ai::system_prompt(
        &categories,
        &SourcePrompt {
            prefix: source.ai_prefix.clone(),
            suffix: source.ai_suffix.clone(),
            examples,
            language: cfg.ai.language.clone(),
        },
    );
    Ok(PromptPreview {
        prompt,
        categories: categories.into_iter().map(|(n, _)| n).collect(),
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct MaskTest {
    /// `None`: any name.
    #[serde(default, rename = "match")]
    pub matches: Option<Vec<String>>,
    #[serde(default)]
    pub exclude: Vec<String>,
    pub names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MaskTestResult {
    pub name: String,
    pub matched: bool,
    pub excluded: bool,
    /// Taken: matched and not excluded.
    pub taken: bool,
}

fn parse_mask(items: &[String]) -> Result<MaskExpr, String> {
    items
        .iter()
        .map(|m| MaskItem::parse(m).map_err(|e| format!("mask “{m}”: {e}")))
        .collect::<Result<Vec<_>, _>>()
        .map(MaskExpr)
}

/// Whether sample file names pass a filter (`match` and not `exclude`),
/// with the user's masks of the edited config (built-ins only when it does
/// not parse).
pub fn mask_test(path: &Path, ops: &[EditOp], t: &MaskTest) -> Result<Vec<MaskTestResult>, String> {
    let scope = file::read(path)
        .ok()
        .and_then(|(text, _)| edit_text(path, &text, ops).ok())
        .and_then(|a| {
            let new = a.text;
            let dir = path.parent().unwrap_or(Path::new(""));
            Config::from_str_at(&new, dir, Path::new("draft")).ok()
        })
        .unwrap_or_default();
    let m = t.matches.as_deref().map(parse_mask).transpose()?;
    let x = parse_mask(&t.exclude)?;
    Ok(t.names
        .iter()
        .filter(|n| !n.trim().is_empty())
        .map(|n| {
            let name = n.trim();
            let matched = m.as_ref().is_none_or(|m| m.matches(name, &scope));
            let excluded = !x.is_empty() && x.matches(name, &scope);
            MaskTestResult {
                name: name.to_string(),
                matched,
                excluded,
                taken: matched && !excluded,
            }
        })
        .collect())
}

/// A name preset to try: one of the config (or a built-in) by name, or a
/// draft from the form.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum PresetRef {
    Named(String),
    Draft(model::NamePresetDraft),
}

#[derive(Debug, Clone, Deserialize)]
pub struct NameSample {
    pub preset: PresetRef,
    pub names: Vec<String>,
    /// The capture (and file) date the files are taken to have,
    /// `2026-11-05 10:15:00`; `None`: no date.
    #[serde(default)]
    pub date: Option<String>,
    /// The source id for `{source}`.
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NameStatus {
    Renamed,
    /// The preset applies but gives the same name.
    Same,
    /// The preset's mask does not take the name.
    NotMatched,
    /// `skip_if` matched.
    Skipped,
    /// The template needs a date the file does not have.
    NoDate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NameResult {
    pub name: String,
    /// The relative path after the preset (`folders/name`).
    pub result: String,
    pub status: NameStatus,
}

/// The edited config as a mask scope (the built-ins only when it does not
/// parse).
fn scope_of(path: &Path, ops: &[EditOp]) -> Config {
    file::read(path)
        .ok()
        .and_then(|(text, _)| edit_text(path, &text, ops).ok())
        .and_then(|a| {
            let dir = path.parent().unwrap_or(Path::new(""));
            Config::from_str_at(&a.text, dir, Path::new("draft")).ok()
        })
        .unwrap_or_default()
}

fn parse_date(s: &str) -> Result<chrono::NaiveDateTime, String> {
    let s = s.trim();
    [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
    ]
    .iter()
    .find_map(|f| chrono::NaiveDateTime::parse_from_str(s, f).ok())
    .or_else(|| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
    })
    .ok_or_else(|| format!("date “{s}” is not valid: expected the form 2026-11-05 10:15:00"))
}

/// A name preset draft as the core reads it (template checked).
fn core_name_preset(
    p: &model::NamePresetDraft,
) -> Result<gallery_core::config::NamePreset, String> {
    let mut t = toml_edit::Table::new();
    presets::write_name(&mut t, &model::NamePresetDraft::default(), p);
    let mut doc = DocumentMut::new();
    let mut g = toml_edit::Table::new();
    g.set_implicit(true);
    g.insert("preview", toml_edit::Item::Table(t));
    doc.insert("name_presets", toml_edit::Item::Table(g));
    let cfg = Config::from_str_at(&doc.to_string(), Path::new(""), Path::new("preset"))
        .map_err(|e| e.to_string())?;
    cfg.name_presets
        .get("preview")
        .cloned()
        .ok_or_else(|| "preset could not be parsed".to_string())
}

/// What name preset `s.preset` makes of each sample name with the date
/// `s.date`, by the core's name pipe, with the masks of the edited config.
pub fn preview_name(
    path: &Path,
    ops: &[EditOp],
    s: &NameSample,
) -> Result<Vec<NameResult>, String> {
    let scope = scope_of(path, ops);
    let date = s
        .date
        .as_deref()
        .filter(|d| !d.trim().is_empty())
        .map(parse_date)
        .transpose()?;
    let preset = match &s.preset {
        PresetRef::Draft(p) => core_name_preset(p)?,
        PresetRef::Named(n) => scope
            .name_preset(n)
            .cloned()
            .ok_or_else(|| format!("there is no preset named “{n}”"))?,
    };
    let ctx = gallery_core::name::NameCtx {
        capture: date,
        created: date,
        source_id: s.source.clone().unwrap_or_else(|| "source".into()),
    };
    let mask = preset.mask_or_default();
    let has_date = match preset.date {
        gallery_core::config::DateSource::None => false,
        _ => date.is_some(),
    };
    s.names
        .iter()
        .map(|n| n.trim())
        .filter(|n| !n.is_empty())
        .map(|name| {
            let tr = gallery_core::name::apply_name_pipe(
                &scope,
                &[(mask.clone(), "preview", &preset)],
                name,
                &ctx,
            )?;
            let result = tr.rel_path().to_string_lossy().replace('\\', "/");
            let status = if !tr.steps[0].matched {
                NameStatus::NotMatched
            } else if preset
                .skip_if
                .as_ref()
                .is_some_and(|m| m.matches(name, &scope))
            {
                NameStatus::Skipped
            } else if preset.template.contains("{date") && !has_date {
                NameStatus::NoDate
            } else if result == name {
                NameStatus::Same
            } else {
                NameStatus::Renamed
            };
            Ok(NameResult {
                name: name.to_string(),
                result,
                status,
            })
        })
        .collect()
}
