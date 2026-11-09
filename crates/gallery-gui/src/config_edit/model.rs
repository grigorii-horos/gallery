//! The editable view of a config: sources and targets as written in the
//! file (prompt file paths, not their text; masks as lists of strings, not
//! parsed), so a draft with a typo can still be shown and fixed.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceDraft {
    pub id: String,
    pub path: String,
    #[serde(default)]
    pub server_path: Option<String>,
    #[serde(default)]
    pub recursive: bool,
    /// `None`: the key is absent and the core's default list applies.
    #[serde(default)]
    pub skip: Option<Vec<String>>,
    #[serde(default)]
    pub ai_prefix: Option<String>,
    #[serde(default)]
    pub ai_prefix_file: Option<String>,
    #[serde(default)]
    pub ai_suffix: Option<String>,
    #[serde(default)]
    pub ai_suffix_file: Option<String>,
}

/// A filter of a mask target. A mask is a list of alternatives (OR); in
/// the file one alternative may be a bare string.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterDraft {
    #[serde(default)]
    pub sources: Vec<String>,
    /// `None`: any file.
    #[serde(default, rename = "match")]
    pub matches: Option<Vec<String>>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefilterDraft {
    #[serde(default, rename = "match")]
    pub matches: Option<Vec<String>>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// One pipe step: a preset with its own mask (`mask: None`) or an override.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepDraft {
    pub preset: String,
    #[serde(default)]
    pub mask: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputDraft {
    pub dir: String,
    #[serde(default)]
    pub server_dir: Option<String>,
    #[serde(default)]
    pub content: Vec<StepDraft>,
    #[serde(default)]
    pub rename: Vec<StepDraft>,
}

/// A target of any stage: `filters` for the mask stages, `sources`,
/// `description` and `prefilter` for the AI stage (ignored elsewhere).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetDraft {
    pub name: String,
    #[serde(default)]
    pub filters: Vec<FilterDraft>,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub prefilter: Option<PrefilterDraft>,
    #[serde(default)]
    pub outputs: Vec<OutputDraft>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    BeforeAi,
    Ai,
    AfterAi,
}

impl Stage {
    pub const ALL: [Stage; 3] = [Stage::BeforeAi, Stage::Ai, Stage::AfterAi];

    /// The key under `[targets]`.
    pub fn key(self) -> &'static str {
        match self {
            Stage::BeforeAi => "before_ai",
            Stage::Ai => "ai",
            Stage::AfterAi => "after_ai",
        }
    }

    pub fn is_mask(self) -> bool {
        self != Stage::Ai
    }
}

/// Position of the first AI target (the list's length when there is none).
/// TEMPORARY (until Task 8's GUI rework): the stage-based editor and summary
/// split the ordered list here into the old three stages.
pub fn ai_cut(cfg: &gallery_core::config::Config) -> usize {
    cfg.targets
        .iter()
        .position(gallery_core::config::Target::has_ai)
        .unwrap_or(cfg.targets.len())
}

/// The ordered targets of a loaded config with the old stage and the index
/// within it that the editor (still stage-based until the GUI rework) uses
/// for them. TEMPORARY: see [`ai_cut`].
pub fn staged(
    cfg: &gallery_core::config::Config,
) -> impl Iterator<Item = (Stage, usize, &gallery_core::config::Target)> {
    let cut = ai_cut(cfg);
    let mut counts = [0usize; 3];
    cfg.targets.iter().enumerate().map(move |(i, t)| {
        let stage = if t.has_ai() {
            Stage::Ai
        } else if i < cut {
            Stage::BeforeAi
        } else {
            Stage::AfterAi
        };
        let n = &mut counts[stage as usize];
        let index = *n;
        *n += 1;
        (stage, index, t)
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetsDraft {
    pub before_ai: Vec<TargetDraft>,
    pub ai: Vec<TargetDraft>,
    pub after_ai: Vec<TargetDraft>,
}

impl TargetsDraft {
    pub fn stage(&self, s: Stage) -> &Vec<TargetDraft> {
        match s {
            Stage::BeforeAi => &self.before_ai,
            Stage::Ai => &self.ai,
            Stage::AfterAi => &self.after_ai,
        }
    }

    /// Every target with its stage and index, in stage order.
    pub fn all(&self) -> impl Iterator<Item = (Stage, usize, &TargetDraft)> {
        Stage::ALL.into_iter().flat_map(move |s| {
            self.stage(s)
                .iter()
                .enumerate()
                .map(move |(i, t)| (s, i, t))
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigDraft {
    pub sources: Vec<SourceDraft>,
    pub targets: TargetsDraft,
    /// Names of the user's own masks (`[masks]`), for `@` suggestions.
    pub masks: Vec<String>,
    /// The user's own presets and masks, in file order.
    #[serde(default)]
    pub name_presets: Vec<NamePresetDraft>,
    #[serde(default)]
    pub content_presets: Vec<ContentPresetDraft>,
    #[serde(default)]
    pub mask_defs: Vec<MaskDraft>,
}

/// Which list of `[name_presets]`, `[content_presets]`, `[masks]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetKind {
    Name,
    Content,
    Mask,
}

impl PresetKind {
    /// The table the kind lives in.
    pub fn key(self) -> &'static str {
        match self {
            PresetKind::Name => "name_presets",
            PresetKind::Content => "content_presets",
            PresetKind::Mask => "masks",
        }
    }
}

/// A user name preset as written; `None`: the key is absent (core default).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamePresetDraft {
    pub name: String,
    #[serde(default)]
    pub mask: Option<Vec<String>>,
    #[serde(default)]
    pub template: String,
    /// `capture`, `file_created` or `none`.
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub skip_if: Option<Vec<String>>,
    #[serde(default)]
    pub normalize_ext: Option<bool>,
}

/// A user content preset as written: `kind` and the keys of that kind
/// (`None`: absent, the core default). Keys of other kinds are `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContentPresetDraft {
    pub name: String,
    /// `metadata`, `photo`, `video`, `audio`, `png` or `bundle`.
    pub kind: String,
    pub mask: Option<Vec<String>>,
    // bundle: the ordered steps (same forms as a target's content pipe)
    pub steps: Option<Vec<StepDraft>>,
    // metadata
    pub write_capture_date: Option<bool>,
    pub fix_gcam_make_model: Option<bool>,
    pub strip_gcamera_xmp: Option<bool>,
    pub strip_gain_map_xmp_for_avif: Option<bool>,
    pub fill_offset_time: Option<bool>,
    pub video_make_model: Option<bool>,
    pub video_restore_gps: Option<bool>,
    // photo
    pub format: Option<String>,
    pub quality: Option<i64>,
    pub speed: Option<i64>,
    pub min_saving_pct: Option<i64>,
    // photo and video
    pub max_short_side: Option<i64>,
    pub ladder: Option<bool>,
    // video
    pub crf: Option<i64>,
    pub preset: Option<i64>,
    pub gop: Option<i64>,
    pub audio_kbps: Option<i64>,
    pub denoise: Option<String>,
    pub stabilize: Option<bool>,
    // audio
    pub codec: Option<String>,
    pub kbps: Option<i64>,
    // png
    pub level: Option<i64>,
}

/// A user mask: alternatives (OR), each a pattern or `@name`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaskDraft {
    pub name: String,
    pub items: Vec<String>,
}

/// Field path of a target, as used in errors and warnings:
/// `targets.after_ai[0]`.
pub fn target_path(stage: Stage, index: usize) -> String {
    format!("targets.{}[{index}]", stage.key())
}
