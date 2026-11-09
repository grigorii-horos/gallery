//! Configuration model: sources, places, ordered targets, presets, AI settings.
//! Old configs with three target stages are migrated on load.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::filter::facts::{DateCond, DeviceCond, GeoCond, PropsCond};
use crate::filter::groups::{DuplicateCond, EventCond, NearCond, SeriesCond};
use crate::mask::{MAX_REF_DEPTH, MaskExpr, MaskItem, MaskScope};
use crate::{Error, Result};

/// Expand a leading `~` / `~/` to the home directory: `$HOME` when set (also
/// what tests use), else the platform's (Windows has no `$HOME`).
pub fn expand_tilde(p: &Path) -> PathBuf {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir);
    match (home, p.strip_prefix("~")) {
        (Some(home), Ok(rest)) => home.join(rest),
        _ => p.to_path_buf(),
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(from = "RawConfig")]
pub struct Config {
    pub sources: Vec<Source>,
    /// Named points for geo conditions (`[places]`).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub places: BTreeMap<String, Place>,
    /// Ordered: the first target with a matching filter takes a file.
    pub targets: Vec<Target>,
    /// The user's own name presets; the built-ins (`crate::presets`) are not
    /// stored here, look names up with `Config::name_preset`.
    pub name_presets: BTreeMap<String, NamePreset>,
    /// The user's own content presets; see `name_presets`.
    pub content_presets: BTreeMap<String, ContentPreset>,
    /// The user's own named masks, referred to as `@name`; the built-ins
    /// (`crate::presets::builtin_masks`) are not stored here.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub masks: BTreeMap<String, MaskExpr>,
    pub ai: AiSettings,
    /// The user's corrections per source id (see [`crate::learn`]); not part
    /// of the file: filled by `Config::attach_examples`.
    #[serde(skip)]
    pub ai_examples: BTreeMap<String, Vec<crate::learn::Correction>>,
    /// The file used the old three-stage shape (`targets.before_ai` / `ai` /
    /// `after_ai`); `targets` holds them migrated, in that order.
    #[serde(skip)]
    pub was_staged: bool,
}

/// The file as written; `Config` is made from it (old target stages
/// migrated).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    sources: Vec<Source>,
    #[serde(default)]
    places: BTreeMap<String, Place>,
    #[serde(default)]
    targets: RawTargets,
    #[serde(default)]
    name_presets: BTreeMap<String, NamePreset>,
    #[serde(default)]
    content_presets: BTreeMap<String, ContentPreset>,
    #[serde(default)]
    masks: BTreeMap<String, MaskExpr>,
    #[serde(default)]
    ai: AiSettings,
}

impl From<RawConfig> for Config {
    fn from(r: RawConfig) -> Self {
        let (targets, was_staged) = match r.targets {
            RawTargets::List(l) => (l, false),
            // An empty `[targets]` table names no stage: not the old shape.
            RawTargets::Staged(s) => {
                let staged = s.names_a_stage();
                (s.into_list(), staged)
            }
        };
        Config {
            sources: r.sources,
            places: r.places,
            targets,
            name_presets: r.name_presets,
            content_presets: r.content_presets,
            masks: r.masks,
            ai: r.ai,
            ai_examples: BTreeMap::new(),
            was_staged,
        }
    }
}

fn default_skip() -> Vec<String> {
    [
        ".stfolder",
        ".stversions",
        ".gallery-trash",
        ".gallery-tmp-*",
    ]
    .map(String::from)
    .into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: String,
    pub path: PathBuf,
    #[serde(default)]
    pub server_path: Option<PathBuf>,
    #[serde(default)]
    pub recursive: bool,
    /// Globs of entries to ignore while scanning.
    #[serde(default = "default_skip")]
    pub skip: Vec<String>,
    /// Text placed in the AI system prompt before the category list of this
    /// source's files; changing it re-asks the AI for them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_prefix: Option<String>,
    /// File holding `ai_prefix` (relative to the config file, `~` expanded);
    /// read by `Config::load` into `ai_prefix`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_prefix_file: Option<PathBuf>,
    /// Text placed after the category list, before the answer format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_suffix: Option<String>,
    /// File holding `ai_suffix`, like `ai_prefix_file`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_suffix_file: Option<PathBuf>,
}

/// What a target does with the files it takes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    /// Place the file into the target's outputs.
    #[default]
    Folders,
    /// Move the file to the trash (no outputs).
    Trash,
    /// Leave the file in its source, untouched (no outputs).
    Keep,
}

impl TargetKind {
    pub fn is_folders(&self) -> bool {
        *self == TargetKind::Folders
    }

    /// The value as written in a config file.
    pub fn as_str(&self) -> &'static str {
        match self {
            TargetKind::Folders => "folders",
            TargetKind::Trash => "trash",
            TargetKind::Keep => "keep",
        }
    }
}

/// One entry of the ordered `[[targets]]` list: the first target one of
/// whose filters matches a file takes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub name: String,
    #[serde(default, skip_serializing_if = "TargetKind::is_folders")]
    pub kind: TargetKind,
    /// Any one of them takes a file (OR).
    #[serde(default)]
    pub filters: Vec<Filter>,
    /// Only for `kind = "folders"`.
    #[serde(default)]
    pub outputs: Vec<Output>,
}

impl Target {
    /// Some filter asks the AI.
    pub fn has_ai(&self) -> bool {
        self.filters.iter().any(|f| f.ai.is_some())
    }
}

/// Grow a filter's match to whole groups of files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Expand {
    /// Every file of the event a matched file belongs to.
    Event,
}

/// `sources` plus conditions; every condition set must hold (AND).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    #[serde(default)]
    pub sources: Vec<String>,
    /// Mask of file names to match; `None` matches any file.
    #[serde(default, rename = "match")]
    pub matches: Option<MaskExpr>,
    /// The filter does not match a file whose name matches any of these.
    #[serde(default, skip_serializing_if = "MaskExpr::is_empty")]
    pub exclude: MaskExpr,
    /// AI category description: the file must be classified into this target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expand: Option<Expand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<DateCond>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geo: Option<GeoCond>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceCond>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub props: Option<PropsCond>,
    /// Mask on the path relative to the source root (`/` separators).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<MaskExpr>,
    /// Conditions on the event the file belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<EventCond>,
    /// Files of other sources taken around the same time and place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub near: Option<NearCond>,
    /// Byte-identical copies in the scan or in output folders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicate: Option<DuplicateCond>,
    /// Burst shots and re-takes; holds for all but the best shot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<SeriesCond>,
}

/// A named point for geo conditions (`[places]`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Place {
    pub lat: f64,
    pub lon: f64,
    /// Radius in kilometres.
    pub km: f64,
}

/// `targets` as written in a file: the `[[targets]]` list, or the old
/// three stages (`targets.before_ai` / `ai` / `after_ai`).
enum RawTargets {
    List(Vec<Target>),
    Staged(StagedTargets),
}

impl Default for RawTargets {
    fn default() -> Self {
        RawTargets::List(Vec::new())
    }
}

// Not `#[serde(untagged)]`: that would replace every error inside the
// targets (unknown field, bad mask, ...) with "did not match any variant".
impl<'de> Deserialize<'de> for RawTargets {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = RawTargets;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a list of [[targets]] or the tables targets.before_ai / ai / after_ai")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                seq: A,
            ) -> std::result::Result<RawTargets, A::Error> {
                Vec::<Target>::deserialize(serde::de::value::SeqAccessDeserializer::new(seq))
                    .map(RawTargets::List)
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<RawTargets, A::Error> {
                StagedTargets::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                    .map(RawTargets::Staged)
            }
        }
        de.deserialize_any(V)
    }
}

/// The old shape, read only to be migrated.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StagedTargets {
    #[serde(default)]
    before_ai: Vec<StagedMaskTarget>,
    #[serde(default)]
    ai: Vec<StagedAiTarget>,
    #[serde(default)]
    after_ai: Vec<StagedMaskTarget>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StagedMaskTarget {
    name: String,
    #[serde(default)]
    filters: Vec<StagedFilter>,
    #[serde(default)]
    outputs: Vec<Output>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StagedFilter {
    #[serde(default)]
    sources: Vec<String>,
    #[serde(default, rename = "match")]
    matches: Option<MaskExpr>,
    #[serde(default)]
    exclude: MaskExpr,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StagedAiTarget {
    name: String,
    #[serde(default)]
    sources: Vec<String>,
    #[serde(default)]
    prefilter: Option<StagedPrefilter>,
    description: String,
    #[serde(default)]
    outputs: Vec<Output>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StagedPrefilter {
    #[serde(default, rename = "match")]
    matches: Option<MaskExpr>,
    #[serde(default)]
    exclude: MaskExpr,
}

impl StagedTargets {
    fn names_a_stage(&self) -> bool {
        !(self.before_ai.is_empty() && self.ai.is_empty() && self.after_ai.is_empty())
    }

    /// `before_ai`, `ai`, `after_ai`, in that order; an AI target becomes
    /// one filter `{ sources, match/exclude of the prefilter, ai }`.
    fn into_list(self) -> Vec<Target> {
        let mask = |m: StagedMaskTarget| Target {
            name: m.name,
            kind: TargetKind::Folders,
            filters: m
                .filters
                .into_iter()
                .map(|f| Filter {
                    sources: f.sources,
                    matches: f.matches,
                    exclude: f.exclude,
                    ..Filter::default()
                })
                .collect(),
            outputs: m.outputs,
        };
        let ai = |a: StagedAiTarget| {
            let (matches, exclude) = a
                .prefilter
                .map(|p| (p.matches, p.exclude))
                .unwrap_or_default();
            Target {
                name: a.name,
                kind: TargetKind::Folders,
                filters: vec![Filter {
                    sources: a.sources,
                    matches,
                    exclude,
                    ai: Some(a.description),
                    expand: None,
                    ..Filter::default()
                }],
                outputs: a.outputs,
            }
        };
        let mut out: Vec<Target> = self.before_ai.into_iter().map(mask).collect();
        out.extend(self.ai.into_iter().map(ai));
        out.extend(self.after_ai.into_iter().map(mask));
        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    /// Destination directory template.
    pub dir: String,
    #[serde(default)]
    pub server_dir: Option<String>,
    /// Content pipe; empty means a byte-for-byte copy.
    #[serde(default)]
    pub content: Vec<PipeStep>,
    /// Name pipe (TOML key `rename`).
    #[serde(default)]
    pub rename: Vec<PipeStep>,
}

/// One step of an output pipe. In a config file it is either a bare preset
/// name (`"avif-high"`, the preset's own mask) or a table
/// `{ mask = "...", preset = "..." }` that overrides the mask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipeStep {
    /// `None`: use the preset's mask (see `ContentPreset::mask_or_default`).
    pub mask: Option<MaskExpr>,
    pub preset: String,
}

impl PipeStep {
    /// A step that uses the preset's own mask.
    pub fn named(preset: &str) -> Self {
        Self {
            mask: None,
            preset: preset.into(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PipeStepTable {
    #[serde(default)]
    mask: Option<MaskExpr>,
    preset: String,
}

impl Serialize for PipeStep {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        match &self.mask {
            None => ser.serialize_str(&self.preset),
            Some(m) => {
                let mut st = ser.serialize_struct("PipeStep", 2)?;
                st.serialize_field("mask", m)?;
                st.serialize_field("preset", &self.preset)?;
                st.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for PipeStep {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = PipeStep;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a preset name or a table { mask, preset }")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<PipeStep, E> {
                Ok(PipeStep::named(v))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<PipeStep, A::Error> {
                let t =
                    PipeStepTable::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(PipeStep {
                    mask: t.mask,
                    preset: t.preset,
                })
            }
        }
        de.deserialize_any(V)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamePreset {
    /// Which files the preset applies to by default; the kind default if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<MaskExpr>,
    pub template: String,
    #[serde(default = "DateSource::default_source")]
    pub date: DateSource,
    #[serde(default)]
    pub skip_if: Option<MaskExpr>,
    #[serde(default)]
    pub normalize_ext: bool,
}

impl NamePreset {
    /// The preset's own mask, else `*`.
    pub fn mask_or_default(&self) -> MaskExpr {
        self.mask
            .clone()
            .unwrap_or_else(|| MaskExpr::parse("*").expect("`*` is a valid mask"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateSource {
    Capture,
    FileCreated,
    None,
}

impl DateSource {
    fn default_source() -> Self {
        DateSource::Capture
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ContentPreset {
    Metadata(MetadataPreset),
    Photo(PhotoPreset),
    Video(VideoPreset),
    Audio(AudioPreset),
    Png(PngPreset),
    /// One name for an ordered list of presets; expanded at plan time.
    Bundle(BundlePreset),
}

/// A named, ordered list of content steps (bundle). It has no mask of its
/// own: every step keeps its preset's mask or its own override. `mask` is
/// parsed only to refuse it with a clear message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundlePreset {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<MaskExpr>,
    pub steps: Vec<PipeStep>,
}

/// One step of a content pipe after bundles are expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandedStep {
    pub step: PipeStep,
    /// The pipe step (bundle) this step came from.
    pub bundle: Option<String>,
}

impl ContentPreset {
    /// The preset's own mask, else the default for its kind.
    pub fn mask_or_default(&self) -> MaskExpr {
        // An audio preset leaves files already in its output format alone,
        // or each scan would re-encode its own results.
        if let ContentPreset::Audio(p) = self
            && p.mask.is_none()
        {
            let own = format!("!*.{}", p.output_ext());
            return MaskExpr(vec![
                MaskItem::parse("@audio").expect("valid"),
                MaskItem::parse(&own).expect("valid"),
            ]);
        }
        let (own, default) = match self {
            ContentPreset::Metadata(p) => (&p.mask, "*"),
            ContentPreset::Photo(p) => (&p.mask, "@photo-original"),
            ContentPreset::Video(p) => (&p.mask, "@video-original"),
            ContentPreset::Audio(p) => (&p.mask, "@audio"),
            ContentPreset::Png(p) => (&p.mask, "@png"),
            ContentPreset::Bundle(p) => (&p.mask, "*"),
        };
        own.clone()
            .unwrap_or_else(|| MaskExpr::parse(default).expect("kind default mask is valid"))
    }

    /// Extension of the file this preset writes for an input with `input_ext`.
    pub fn output_ext(&self, input_ext: &str) -> String {
        match self {
            ContentPreset::Metadata(_) => input_ext,
            ContentPreset::Photo(p) => p.output_ext(),
            ContentPreset::Video(p) => p.output_ext(),
            ContentPreset::Audio(p) => p.output_ext(),
            ContentPreset::Png(p) => p.output_ext(),
            ContentPreset::Bundle(_) => input_ext,
        }
        .to_string()
    }

    /// Whether the preset re-encodes lossily.
    pub fn reencodes(&self) -> bool {
        match self {
            ContentPreset::Metadata(_) => false,
            ContentPreset::Photo(p) => p.reencodes(),
            ContentPreset::Video(p) => p.reencodes(),
            ContentPreset::Audio(p) => p.codec != AudioCodec::Flac,
            ContentPreset::Png(p) => p.reencodes(),
            ContentPreset::Bundle(_) => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct MetadataPreset {
    /// Which files the preset applies to by default; the kind default if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<MaskExpr>,
    pub write_capture_date: bool,
    pub fix_gcam_make_model: bool,
    pub strip_gcamera_xmp: bool,
    pub strip_gain_map_xmp_for_avif: bool,
    pub fill_offset_time: bool,
    pub video_make_model: bool,
    pub video_restore_gps: bool,
}

impl MetadataPreset {
    /// "meta-full": every fix enabled.
    pub fn full() -> Self {
        Self {
            mask: None,
            write_capture_date: true,
            fix_gcam_make_model: true,
            strip_gcamera_xmp: true,
            strip_gain_map_xmp_for_avif: true,
            fill_offset_time: true,
            video_make_model: true,
            video_restore_gps: true,
        }
    }

    /// "meta-dates": only the capture date.
    pub fn dates_only() -> Self {
        Self {
            mask: None,
            write_capture_date: true,
            fix_gcam_make_model: false,
            strip_gcamera_xmp: false,
            strip_gain_map_xmp_for_avif: false,
            fill_offset_time: false,
            video_make_model: false,
            video_restore_gps: false,
        }
    }
}

impl Default for MetadataPreset {
    fn default() -> Self {
        Self::full()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PhotoFormat {
    Avif,
    Webp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct PhotoPreset {
    /// Which files the preset applies to by default; the kind default if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<MaskExpr>,
    pub format: PhotoFormat,
    pub quality: u8,
    /// AVIF speed or WebP method, depending on `format`.
    pub speed: u8,
    pub max_short_side: Option<u32>,
    pub ladder: bool,
    pub min_saving_pct: u8,
}

impl PhotoPreset {
    fn make(format: PhotoFormat, quality: u8, speed: u8) -> Self {
        Self {
            mask: None,
            format,
            quality,
            speed,
            max_short_side: None,
            ladder: true,
            min_saving_pct: 0,
        }
    }
    /// PNG (screenshots) to AVIF: a bit higher quality than photos, since
    /// text and flat UI areas show artefacts sooner.
    pub fn png_avif() -> Self {
        Self {
            mask: Some(MaskExpr::parse("@png").expect("valid mask")),
            ..Self::make(PhotoFormat::Avif, 80, 6)
        }
    }
    /// WebP to AVIF: same level as `avif-high`.
    pub fn webp_avif() -> Self {
        Self {
            mask: Some(MaskExpr::parse("*.webp").expect("valid mask")),
            ..Self::make(PhotoFormat::Avif, 72, 6)
        }
    }
    pub fn avif_low() -> Self {
        Self::make(PhotoFormat::Avif, 55, 8)
    }
    pub fn avif_medium() -> Self {
        Self::make(PhotoFormat::Avif, 65, 6)
    }
    pub fn avif_high() -> Self {
        Self::make(PhotoFormat::Avif, 72, 6)
    }
    pub fn avif_max() -> Self {
        Self::make(PhotoFormat::Avif, 82, 4)
    }
    /// "avif-fast": high quality, fastest speed.
    pub fn avif_fast() -> Self {
        Self::make(PhotoFormat::Avif, 72, 10)
    }
    pub fn webp_low() -> Self {
        Self::make(PhotoFormat::Webp, 70, 4)
    }
    pub fn webp_medium() -> Self {
        Self::make(PhotoFormat::Webp, 80, 5)
    }
    pub fn webp_high() -> Self {
        Self::make(PhotoFormat::Webp, 87, 6)
    }
    pub fn webp_max() -> Self {
        Self::make(PhotoFormat::Webp, 93, 6)
    }
    pub fn output_ext(&self) -> &'static str {
        match self.format {
            PhotoFormat::Avif => "avif",
            PhotoFormat::Webp => "webp",
        }
    }
    pub fn reencodes(&self) -> bool {
        true
    }
}

impl Default for PhotoPreset {
    fn default() -> Self {
        Self::avif_high()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Denoise {
    VeryLow,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct VideoPreset {
    /// Which files the preset applies to by default; the kind default if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<MaskExpr>,
    pub crf: u8,
    pub preset: u8,
    pub gop: u32,
    pub max_short_side: Option<u32>,
    pub audio_kbps: u32,
    pub denoise: Option<Denoise>,
    pub stabilize: bool,
    pub ladder: bool,
}

impl VideoPreset {
    fn make(crf: u8, preset: u8) -> Self {
        Self {
            mask: None,
            crf,
            preset,
            gop: 240,
            max_short_side: Some(1080),
            audio_kbps: 160,
            denoise: None,
            stabilize: false,
            ladder: true,
        }
    }
    pub fn max() -> Self {
        Self::make(27, 2)
    }
    pub fn high() -> Self {
        Self::make(30, 3)
    }
    pub fn medium() -> Self {
        Self::make(32, 4)
    }
    pub fn low() -> Self {
        Self::make(36, 4)
    }
    /// "Fast": fast encoder preset.
    pub fn fast() -> Self {
        Self::make(32, 8)
    }
    /// "av1-action-cam": high + stabilization + medium denoise.
    pub fn action_cam() -> Self {
        Self {
            stabilize: true,
            denoise: Some(Denoise::Medium),
            ..Self::high()
        }
    }
    pub fn output_ext(&self) -> &'static str {
        "av1.mp4"
    }
    pub fn reencodes(&self) -> bool {
        true
    }
}

impl Default for VideoPreset {
    fn default() -> Self {
        Self::high()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioCodec {
    Opus,
    Mp3,
    Aac,
    Flac,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioPreset {
    /// Which files the preset applies to by default; the kind default if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<MaskExpr>,
    pub codec: AudioCodec,
    /// Ignored for lossless codecs.
    #[serde(default)]
    pub kbps: Option<u32>,
}

impl AudioPreset {
    pub fn opus(kbps: u32) -> Self {
        Self {
            mask: None,
            codec: AudioCodec::Opus,
            kbps: Some(kbps),
        }
    }
    pub fn mp3(kbps: u32) -> Self {
        Self {
            mask: None,
            codec: AudioCodec::Mp3,
            kbps: Some(kbps),
        }
    }
    pub fn aac(kbps: u32) -> Self {
        Self {
            mask: None,
            codec: AudioCodec::Aac,
            kbps: Some(kbps),
        }
    }
    /// Lossless; no bitrate.
    pub fn flac() -> Self {
        Self {
            mask: None,
            codec: AudioCodec::Flac,
            kbps: None,
        }
    }
    pub fn output_ext(&self) -> &'static str {
        match self.codec {
            AudioCodec::Opus => "opus",
            AudioCodec::Mp3 => "mp3",
            AudioCodec::Aac => "m4a",
            AudioCodec::Flac => "flac",
        }
    }
}

impl Default for AudioPreset {
    fn default() -> Self {
        Self::opus(128)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct PngPreset {
    /// Which files the preset applies to by default; the kind default if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<MaskExpr>,
    /// optipng `-oN` level.
    pub level: u8,
}

impl PngPreset {
    pub fn output_ext(&self) -> &'static str {
        "png"
    }
    /// PNG optimisation is lossless.
    pub fn reencodes(&self) -> bool {
        false
    }
}

impl Default for PngPreset {
    fn default() -> Self {
        Self {
            mask: None,
            level: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[serde(deny_unknown_fields)]
pub struct AiSettings {
    pub model: String,
    pub effort: String,
    pub jobs: usize,
    /// How many of the user's most recent corrections of a source go into
    /// its AI prompt as examples; 0 turns the learning off.
    pub examples: usize,
    /// Language the model writes each `reason`, keywords and description in: a name or code ("en",
    /// "ru", "Ukrainian"); blank means the language of the interface.
    pub language: String,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            model: "claude-sonnet-5-5".into(),
            effort: "low".into(),
            jobs: 8,
            examples: 20,
            language: String::new(),
        }
    }
}

/// One prompt slot of source `id`: inline `text` or the contents of `file`
/// (relative to `dir`, `~` expanded); both is an error, blank is none.
fn load_ai_text(
    dir: &Path,
    id: &str,
    slot: &str,
    text: Option<String>,
    file: Option<PathBuf>,
) -> std::result::Result<Option<String>, String> {
    let text = match (text, file) {
        (Some(_), Some(_)) => {
            return Err(crate::t!("config.ai_both", id = id, slot = slot));
        }
        (Some(t), None) => t,
        (None, Some(f)) => {
            let f = dir.join(expand_tilde(&f));
            std::fs::read_to_string(&f).map_err(|e| {
                crate::t!(
                    "config.ai_file",
                    id = id,
                    slot = slot,
                    path = f.display(),
                    error = e
                )
            })?
        }
        (None, None) => return Ok(None),
    };
    Ok(Some(text).filter(|t| !t.trim().is_empty()))
}

/// The error for a step that names no preset; an old (pre-rename) built-in
/// name gets a hint with the new one.
pub(crate) fn unknown_preset(target: &str, kind: &str, name: &str) -> String {
    let mut m = if kind == "name" {
        crate::t!("config.unknown_name_preset", target = target, name = name)
    } else {
        crate::t!(
            "config.unknown_content_preset",
            target = target,
            name = name
        )
    };
    if let Some(new) = crate::presets::renamed_builtin(name) {
        m.push_str(&crate::t!("config.preset_renamed", new = new));
    }
    m
}

impl MaskScope for Config {
    fn named_mask(&self, name: &str) -> Option<&MaskExpr> {
        self.masks
            .get(name)
            .or_else(|| crate::presets::builtin_mask(name))
    }
}

impl Config {
    /// Read and parse a TOML config file; the sources' `ai_*_file` are read
    /// here into `ai_prefix` / `ai_suffix`.
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)?;
        let dir = path.parent().unwrap_or(Path::new(""));
        Self::from_str_at(&text, dir, path)
    }

    /// Parse config `text` as if it were a file in `base_dir`: relative
    /// `ai_*_file` paths are read from there. `label` names the text in
    /// errors (the file path for `load`). Used to check an edited config
    /// before it is written.
    pub fn from_str_at(text: &str, base_dir: &Path, label: &Path) -> Result<Config> {
        let path = label;
        let mut c: Config = toml::from_str(text).map_err(|e| {
            let mut m = format!("{}: {e}", path.display());
            if m.contains("unknown field `instructions") {
                if !m.ends_with('\n') {
                    m.push('\n');
                }
                m.push_str(&crate::t!("config.hint_instructions"));
            }
            if m.contains("unknown field `kinds`") {
                if !m.ends_with('\n') {
                    m.push('\n');
                }
                m.push_str(&crate::t!("config.hint_kinds"));
            }
            if ["before_ai", "ai", "after_ai"]
                .iter()
                .any(|k| m.contains(&format!("unknown field `{k}`")))
            {
                if !m.ends_with('\n') {
                    m.push('\n');
                }
                m.push_str(&crate::t!("config.hint_mixed_targets"));
            }
            if m.contains("unknown field `name`, expected") {
                let hint = if m.contains("`rename`") {
                    crate::t!("config.hint_name_rename")
                } else if m.contains("`match`") {
                    crate::t!("config.hint_name_match")
                } else {
                    String::new()
                };
                if !hint.is_empty() {
                    if !m.ends_with('\n') {
                        m.push('\n');
                    }
                    m.push_str(&hint);
                }
            }
            Error::Config(m)
        })?;
        let dir = base_dir;
        for s in &mut c.sources {
            s.ai_prefix = load_ai_text(
                dir,
                &s.id,
                "ai_prefix",
                s.ai_prefix.take(),
                s.ai_prefix_file.take(),
            )
            .map_err(|m| Error::Config(format!("{}: {m}", path.display())))?;
            s.ai_suffix = load_ai_text(
                dir,
                &s.id,
                "ai_suffix",
                s.ai_suffix.take(),
                s.ai_suffix_file.take(),
            )
            .map_err(|m| Error::Config(format!("{}: {m}", path.display())))?;
        }
        c.check_builtin_clash()
            .and_then(|()| c.check_name_templates())
            .map_err(|m| Error::Config(format!("{}: {m}", path.display())))?;
        Ok(c)
    }

    /// A target by name.
    pub fn target(&self, name: &str) -> Option<&Target> {
        self.targets.iter().find(|t| t.name == name)
    }

    /// Some target asks the AI.
    pub fn has_ai(&self) -> bool {
        self.targets.iter().any(Target::has_ai)
    }

    /// A name preset by name: the user's own, else a built-in.
    pub fn name_preset(&self, name: &str) -> Option<&NamePreset> {
        self.name_presets
            .get(name)
            .or_else(|| crate::presets::builtin_name_preset(name))
    }

    /// A content preset by name: the user's own, else a built-in.
    pub fn content_preset(&self, name: &str) -> Option<&ContentPreset> {
        self.content_presets
            .get(name)
            .or_else(|| crate::presets::builtin_content_preset(name))
    }

    /// Expand bundles in a content pipe, in place and in order. A bundle
    /// step may not carry a mask (its own steps do); bundles may nest.
    pub fn expand_content(
        &self,
        target: &str,
        steps: &[PipeStep],
    ) -> std::result::Result<Vec<ExpandedStep>, String> {
        let mut out = Vec::new();
        for st in steps {
            match self.content_preset(&st.preset) {
                None => return Err(unknown_preset(target, "content", &st.preset)),
                Some(ContentPreset::Bundle(_)) => {
                    let mut open = vec![st.preset.as_str()];
                    self.expand_bundle(st, &st.preset, &mut open, &mut out)?;
                }
                Some(_) => out.push(ExpandedStep {
                    step: st.clone(),
                    bundle: None,
                }),
            }
        }
        Ok(out)
    }

    fn expand_bundle<'a>(
        &'a self,
        st: &'a PipeStep,
        top: &str,
        open: &mut Vec<&'a str>,
        out: &mut Vec<ExpandedStep>,
    ) -> std::result::Result<(), String> {
        let name = st.preset.as_str();
        if st.mask.is_some() {
            return Err(crate::t!("config.bundle_with_mask", name = name));
        }
        let Some(ContentPreset::Bundle(b)) = self.content_preset(name) else {
            return Err(crate::t!("config.unknown_bundle", name = name));
        };
        if b.mask.is_some() {
            return Err(crate::t!("config.bundle_has_mask", name = name));
        }
        if b.steps.is_empty() {
            return Err(crate::t!("config.bundle_no_steps", name = name));
        }
        for inner in &b.steps {
            match self.content_preset(&inner.preset) {
                None => {
                    return Err(crate::t!(
                        "config.bundle_unknown_preset",
                        name = name,
                        preset = inner.preset
                    ));
                }
                Some(ContentPreset::Bundle(_)) => {
                    if let Some(i) = open.iter().position(|n| *n == inner.preset) {
                        let mut cycle = open[i..].to_vec();
                        cycle.push(&inner.preset);
                        return Err(crate::t!("config.bundle_cycle", cycle = cycle.join(" -> ")));
                    }
                    if open.len() >= MAX_REF_DEPTH {
                        return Err(crate::t!(
                            "config.bundle_too_deep",
                            name = name,
                            depth = MAX_REF_DEPTH
                        ));
                    }
                    open.push(&inner.preset);
                    self.expand_bundle(inner, top, open, out)?;
                    open.pop();
                }
                Some(_) => out.push(ExpandedStep {
                    step: inner.clone(),
                    bundle: Some(top.to_string()),
                }),
            }
        }
        Ok(())
    }

    /// A user preset must not reuse the name of a built-in one.
    fn check_builtin_clash(&self) -> std::result::Result<(), String> {
        let clash = |kind: &str, name: &str| {
            let kind = if kind == "name" {
                crate::t!("config.kind_name")
            } else {
                crate::t!("config.kind_content")
            };
            crate::t!("config.builtin_preset_clash", kind = kind, name = name)
        };
        if let Some(n) = self
            .name_presets
            .keys()
            .find(|n| crate::presets::builtin_name_preset(n).is_some())
        {
            return Err(clash("name", n));
        }
        if let Some(n) = self
            .content_presets
            .keys()
            .find(|n| crate::presets::builtin_content_preset(n).is_some())
        {
            return Err(clash("content", n));
        }
        match self
            .masks
            .keys()
            .find(|n| crate::presets::builtin_mask(n).is_some())
        {
            Some(n) => Err(crate::t!("config.builtin_mask_clash", name = n)),
            None => Ok(()),
        }
    }

    /// Every `@name` in `expr` is a known mask.
    /// `exclude` / `skip_if` only turn files away: a `!` item there would
    /// rescue files, so it is refused.
    fn check_veto_mask(
        &self,
        whose: &str,
        field: &str,
        expr: &MaskExpr,
    ) -> std::result::Result<(), String> {
        if let Some(i) = expr.0.iter().find(|i| i.is_negated()) {
            return Err(crate::t!(
                "config.veto_negated",
                whose = whose,
                item = i.as_string(),
                field = field
            ));
        }
        self.check_mask_refs(whose, expr)
    }

    fn check_mask_refs(&self, whose: &str, expr: &MaskExpr) -> std::result::Result<(), String> {
        if expr.is_empty() {
            return Err(crate::t!("config.empty_mask_list", whose = whose));
        }
        match expr.refs().find(|n| self.named_mask(n).is_none()) {
            Some(n) => Err(crate::t!("config.unknown_mask", whose = whose, name = n)),
            None => Ok(()),
        }
    }

    /// Every user bundle expands: known steps, no mask, no cycle.
    pub(crate) fn check_bundles(&self) -> std::result::Result<(), String> {
        for (name, p) in &self.content_presets {
            if matches!(p, ContentPreset::Bundle(_)) {
                self.expand_content(name, &[PipeStep::named(name)])?;
            }
        }
        Ok(())
    }

    /// Named masks: no empty names, known references everywhere a mask is
    /// accepted, and no reference cycle.
    fn check_masks(&self) -> std::result::Result<(), String> {
        if self.masks.keys().any(|n| n.is_empty() || n.contains('@')) {
            return Err(crate::t!("config.bad_mask_name"));
        }
        for (name, e) in &self.masks {
            self.check_mask_refs(&crate::t!("config.who_mask", name = name), e)?;
        }
        // Depth-first walk over `@` references; a name met again while it is
        // still open is a cycle.
        fn walk<'a>(
            c: &'a Config,
            name: &'a str,
            open: &mut Vec<&'a str>,
            done: &mut BTreeSet<&'a str>,
        ) -> std::result::Result<(), String> {
            if let Some(i) = open.iter().position(|n| *n == name) {
                let mut cycle = open[i..].to_vec();
                cycle.push(name);
                return Err(crate::t!("config.mask_cycle", cycle = cycle.join(" -> ")));
            }
            if open.len() + 1 >= MAX_REF_DEPTH {
                return Err(crate::t!(
                    "config.mask_too_deep",
                    name = name,
                    depth = MAX_REF_DEPTH - 1
                ));
            }
            if !done.insert(name) {
                return Ok(());
            }
            open.push(name);
            if let Some(e) = c.named_mask(name) {
                for r in e.refs() {
                    walk(c, r, open, done)?;
                }
            }
            open.pop();
            Ok(())
        }
        let mut done = BTreeSet::new();
        for name in self.masks.keys() {
            walk(self, name, &mut Vec::new(), &mut done)?;
        }
        for (name, p) in &self.name_presets {
            let whose = crate::t!("config.who_name_preset", name = name);
            if let Some(m) = &p.mask {
                self.check_mask_refs(&whose, m)?;
            }
            if let Some(m) = &p.skip_if {
                self.check_veto_mask(&whose, "skip_if", m)?;
            }
        }
        for (name, p) in &self.content_presets {
            let m = match p {
                ContentPreset::Metadata(p) => &p.mask,
                ContentPreset::Photo(p) => &p.mask,
                ContentPreset::Video(p) => &p.mask,
                ContentPreset::Audio(p) => &p.mask,
                ContentPreset::Png(p) => &p.mask,
                ContentPreset::Bundle(p) => {
                    for st in &p.steps {
                        if let Some(m) = &st.mask {
                            self.check_mask_refs(&crate::t!("config.who_bundle", name = name), m)?;
                        }
                    }
                    &p.mask
                }
            };
            if let Some(m) = m {
                self.check_mask_refs(&crate::t!("config.who_content_preset", name = name), m)?;
            }
        }
        for t in &self.targets {
            let whose = crate::t!("config.who_target", name = t.name);
            for f in &t.filters {
                if !f.exclude.is_empty() {
                    self.check_veto_mask(
                        &crate::t!("config.who_filter", whose = whose),
                        "exclude",
                        &f.exclude,
                    )?;
                }
                if let Some(n) = &f.matches {
                    self.check_mask_refs(&whose, n)?;
                }
            }
            let steps = t
                .outputs
                .iter()
                .flat_map(|o| o.content.iter().chain(&o.rename));
            for st in steps {
                if let Some(m) = &st.mask {
                    self.check_mask_refs(&whose, m)?;
                }
            }
        }
        Ok(())
    }

    /// Problems worth a warning but not an error: a name preset that makes
    /// folders and has `skip_if`. A file it skips gets no folders, so in a
    /// recursive source that is its own target an already sorted file would
    /// be moved back to the output folder itself.
    pub fn warnings(&self) -> Vec<String> {
        self.name_presets
            .iter()
            .filter(|(_, p)| p.template.contains('/') && p.skip_if.is_some())
            .map(|(name, _)| crate::t!("config.warn_folders_skip_if", name = name))
            .collect()
    }

    /// Every source id is a plain name (it may be a folder via `{source}`),
    /// and every name preset's template renders to a safe relative path.
    fn check_name_templates(&self) -> std::result::Result<(), String> {
        for s in &self.sources {
            let id = s.id.as_str();
            if id.is_empty() || id == "." || id == ".." || id.contains(['/', '\\']) {
                return Err(crate::t!("config.bad_source_id", id = id));
            }
        }
        self.name_presets
            .iter()
            .try_for_each(|(name, p)| crate::name::check_template(name, p))
    }

    /// Check cross-references: unique target names, known sources and presets,
    /// non-empty AI descriptions, no `expand` with `ai`, no outputs on
    /// `trash`/`keep` targets; and safe name templates.
    pub fn validate(&self) -> Result<()> {
        self.check_builtin_clash().map_err(Error::Config)?;
        self.check_name_templates().map_err(Error::Config)?;
        self.check_masks().map_err(Error::Config)?;
        self.check_bundles().map_err(Error::Config)?;
        let sources: BTreeSet<&str> = self.sources.iter().map(|s| s.id.as_str()).collect();
        let check_sources = |target: &str, ids: &[String]| -> Result<()> {
            match ids.iter().find(|id| !sources.contains(id.as_str())) {
                Some(id) => Err(Error::Config(crate::t!(
                    "config.unknown_source",
                    target = target,
                    id = id
                ))),
                None => Ok(()),
            }
        };
        let check_outputs = |target: &str, outputs: &[Output]| -> Result<()> {
            for o in outputs {
                self.expand_content(target, &o.content)
                    .map_err(Error::Config)?;
                for s in &o.rename {
                    if self.name_preset(&s.preset).is_none() {
                        return Err(Error::Config(unknown_preset(target, "name", &s.preset)));
                    }
                }
            }
            Ok(())
        };

        let mut seen = BTreeSet::new();
        for t in &self.targets {
            let name = t.name.as_str();
            if !seen.insert(name) {
                return Err(Error::Config(crate::t!(
                    "config.duplicate_target",
                    name = name
                )));
            }
            for f in &t.filters {
                check_sources(name, &f.sources)?;
                let named: Vec<String> = crate::filter::groups::named_sources(f).cloned().collect();
                check_sources(name, &named)?;
                if f.ai.as_ref().is_some_and(|d| d.trim().is_empty()) {
                    return Err(Error::Config(crate::t!(
                        "config.empty_description",
                        name = name
                    )));
                }
                f.check_conditions(self, name).map_err(Error::Config)?;
                let device = f
                    .device
                    .iter()
                    .flat_map(|d| [&d.make, &d.model, &d.software]);
                for m in device.flatten().chain(f.path.iter()) {
                    self.check_mask_refs(&crate::t!("config.who_target", name = name), m)
                        .map_err(Error::Config)?;
                }
                if f.ai.is_some() && f.expand.is_some() {
                    return Err(Error::Config(crate::t!(
                        "config.expand_with_ai",
                        name = name
                    )));
                }
            }
            if !t.kind.is_folders() && !t.outputs.is_empty() {
                return Err(Error::Config(crate::t!(
                    "config.kind_has_outputs",
                    name = name,
                    kind = t.kind.as_str()
                )));
            }
            check_outputs(name, &t.outputs)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn example() -> Config {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/staged-example.toml");
        Config::load(&p).unwrap()
    }

    fn load_str(text: &str) -> Result<Config> {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("g.toml");
        std::fs::write(&p, text).unwrap();
        Config::load(&p)
    }

    const USES_BUILTINS: &str = r#"
[[sources]]
id = "s"
path = "/tmp/s"

[[targets.after_ai]]
name = "t"

[[targets.after_ai.filters]]
sources = ["s"]
match = "*"

[[targets.after_ai.outputs]]
dir = "/tmp/o"
content = [{ mask = "*", preset = "avif-high" }]
rename = [{ mask = "*", preset = "capture-date" }]
"#;

    #[test]
    fn builtin_presets_need_no_definition() {
        let c = load_str(USES_BUILTINS).unwrap();
        assert!(c.content_presets.is_empty() && c.name_presets.is_empty());
        c.validate().unwrap();
    }

    #[test]
    fn unknown_preset_still_fails() {
        let c = load_str(&USES_BUILTINS.replace("avif-high", "Nope")).unwrap();
        assert!(c.validate().unwrap_err().to_string().contains("Nope"));
    }

    #[test]
    fn redefining_a_builtin_is_an_error() {
        let content =
            format!("{USES_BUILTINS}\n[content_presets.\"avif-high\"]\nkind = \"photo\"\n");
        let name = format!(
            "{USES_BUILTINS}\n[name_presets.\"capture-date\"]\ntemplate = \"{{stem}}.{{ext}}\"\n"
        );
        for (text, who) in [(content, "avif-high"), (name, "capture-date")] {
            let m = load_str(&text).unwrap_err().to_string();
            assert!(m.contains(who), "{m}");
            assert!(
                m.contains("this is a built-in preset — give yours another name"),
                "{m}"
            );
        }
        // `validate` catches presets added in code, too.
        let mut c = load_str(USES_BUILTINS).unwrap();
        c.name_presets.insert(
            "ext-lower".into(),
            NamePreset {
                mask: None,
                template: "{stem}.{ext}".into(),
                date: DateSource::None,
                skip_if: None,
                normalize_ext: false,
            },
        );
        assert!(c.validate().unwrap_err().to_string().contains("ext-lower"));
    }

    #[test]
    fn audio_presets() {
        assert_eq!(AudioPreset::default(), AudioPreset::opus(128));
        assert_eq!(AudioPreset::opus(64).output_ext(), "opus");
        assert_eq!(AudioPreset::mp3(320).kbps, Some(320));
        assert_eq!(AudioPreset::aac(96).output_ext(), "m4a");
        assert_eq!(AudioPreset::flac().kbps, None);
        assert_eq!(AudioPreset::flac().output_ext(), "flac");
    }

    #[test]
    fn example_config_loads() {
        let c = example();
        assert!(c.sources.len() >= 2);
        // Still the old shape: it doubles as a migration fixture.
        assert!(c.was_staged);
        let names: Vec<_> = c.targets.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Screenshots",
                "Screenshots and screen recordings",
                "TikTok",
                "Documents",
                "Receipts",
                "Timeline"
            ]
        );
        assert!(c.target("Documents").unwrap().has_ai());
        assert!(!c.target("Timeline").unwrap().has_ai());
        let all = c.target("Timeline").unwrap().outputs.iter();
        assert!(all.clone().any(|o| o.content.is_empty()));
        let camera = c.sources.iter().find(|s| s.id == "camera").unwrap();
        assert!(camera.ai_prefix.as_deref().unwrap().contains("phone"));
        assert!(camera.ai_suffix.as_deref().unwrap().contains("Rules"));
        assert_eq!(camera.ai_prefix_file, None);
        c.validate().unwrap();
    }

    #[test]
    fn example_config_round_trips() {
        let c = example();
        // Only the user's own presets are stored; built-ins stay out.
        assert_eq!(c.name_presets.keys().collect::<Vec<_>>(), ["TikTok"]);
        assert!(c.content_presets.is_empty());
        let text = toml::to_string(&c).unwrap();
        assert!(
            !text.contains("avif-high\"]") && !text.contains("[name_presets.\"capture-date"),
            "{text}"
        );
        let again: Config = toml::from_str(&text).unwrap();
        assert_eq!(toml::to_string(&again).unwrap(), text);
    }

    #[test]
    fn example_capture_date_name_is_not_doubled() {
        use crate::datetime::capture_date;
        use crate::media::kind_of;
        use crate::name::{NameCtx, apply_name_pipe};
        let c = example();
        let preset = c.name_preset("capture-date").unwrap();
        let name = |n: &str| {
            let ctx = NameCtx {
                capture: capture_date(n, kind_of(Path::new(n)), &serde_json::Value::Null, None),
                ..NameCtx::default()
            };
            let steps = [(MaskExpr::parse("*").unwrap(), "capture-date", preset)];
            apply_name_pipe(&c, &steps, n, &ctx).unwrap().final_name
        };
        assert_eq!(name("IMG_20260412_103314.jpg"), "20260412_103314.jpg");
        assert_eq!(name("20260412_103314.jpg"), "20260412_103314.jpg");
    }

    #[test]
    fn example_dated_folders_preset() {
        use crate::name::{NameCtx, apply_name_pipe};
        let c = example();
        let ctx = NameCtx {
            capture: chrono::NaiveDateTime::parse_from_str(
                "2026-07-05 00:20:22",
                "%Y-%m-%d %H:%M:%S",
            )
            .ok(),
            ..NameCtx::default()
        };
        let steps = [
            (
                MaskExpr::parse("*").unwrap(),
                "capture-date-dirs",
                c.name_preset("capture-date-dirs").unwrap(),
            ),
            (
                MaskExpr::parse("*").unwrap(),
                "ext-lower",
                c.name_preset("ext-lower").unwrap(),
            ),
        ];
        let t = apply_name_pipe(&c, &steps, "Screenshot_2026-07-05-00-20-22.JPEG", &ctx).unwrap();
        assert_eq!(t.rel_path(), Path::new("2026/07/20260705_002022.jpg"));
    }

    #[test]
    fn example_screenshots_are_renamed_in_place() {
        let c = example();
        let t = c.target("Screenshots").unwrap();
        let src = c.sources.iter().find(|s| s.id == "screenshots").unwrap();
        assert_eq!(t.outputs.len(), 1);
        let o = &t.outputs[0];
        assert_eq!(Path::new(&o.dir), src.path);
        assert!(o.content.is_empty());
        assert_eq!(o.rename[0].preset, "capture-date-dirs");
        let f = &t.filters[0];
        assert_eq!(f.sources, ["screenshots"]);
        // Files already named by date still go into their folders.
        assert!(f.exclude.is_empty());
        // Screenshots among camera photos still get a processed copy and a backup.
        let other = c.target("Screenshots and screen recordings").unwrap();
        assert_eq!(other.filters.len(), 1);
        let f = &other.filters[0];
        let n = f.matches.as_ref().unwrap();
        assert!(n.matches("screen-1.mp4", &c) && n.matches("Screenshot_1.png", &c));
        assert!(!n.matches("screen-1.jpg", &c) && !n.matches("IMG_1.jpg", &c));
        assert_eq!(other.outputs.len(), 2);
        assert_eq!(other.outputs[0].content[0].preset, "meta-full");
        assert_eq!(other.outputs[1].dir, "~/Backup/Screenshots");
    }

    const SRC: &str = "[[sources]]\nid = \"cam\"\npath = \"/x\"\n";

    #[test]
    fn source_ai_texts_inline_and_from_files() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("g.toml");
        std::fs::write(&p, format!("{SRC}ai_prefix = \"camera frames\"\n")).unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!(c.sources[0].ai_prefix.as_deref(), Some("camera frames"));
        assert_eq!(c.sources[0].ai_suffix, None);

        // Relative files are read next to the config, not the working dir.
        std::fs::create_dir(d.path().join("ai")).unwrap();
        std::fs::write(d.path().join("ai/pre.md"), "before the list\n").unwrap();
        std::fs::write(d.path().join("ai/post.md"), "price tag goes by item\n").unwrap();
        std::fs::write(
            &p,
            format!("{SRC}ai_prefix_file = \"ai/pre.md\"\nai_suffix_file = \"ai/post.md\"\n"),
        )
        .unwrap();
        let c = Config::load(&p).unwrap();
        let s = &c.sources[0];
        assert_eq!(s.ai_prefix.as_deref(), Some("before the list\n"));
        assert_eq!(s.ai_suffix.as_deref(), Some("price tag goes by item\n"));
        assert_eq!((&s.ai_prefix_file, &s.ai_suffix_file), (&None, &None));

        // Blank text is the same as none.
        std::fs::write(&p, format!("{SRC}ai_suffix = \"  \\n\"\n")).unwrap();
        assert_eq!(Config::load(&p).unwrap().sources[0].ai_suffix, None);
    }

    #[test]
    fn from_str_at_reads_prompt_files_relative_to_base_dir() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("ai")).unwrap();
        std::fs::write(d.path().join("ai/pre.md"), "before the list\n").unwrap();
        let text = format!("{SRC}ai_prefix_file = \"ai/pre.md\"\n");
        let c = Config::from_str_at(&text, d.path(), Path::new("g.toml")).unwrap();
        assert_eq!(c.sources[0].ai_prefix.as_deref(), Some("before the list\n"));
        // Errors name the given label, like `load` names the file.
        let m = Config::from_str_at("[[sources]]\n", d.path(), Path::new("draft.toml"))
            .unwrap_err()
            .to_string();
        assert!(m.contains("draft.toml"), "{m}");
    }

    #[test]
    fn source_ai_text_and_file_together_is_error() {
        for slot in ["ai_prefix", "ai_suffix"] {
            let m = parse_err(&format!("{SRC}{slot} = \"x\"\n{slot}_file = \"r.md\"\n"));
            assert!(m.contains("cam"), "{m}");
            assert!(m.contains(&format!("{slot}_file")), "{m}");
        }
    }

    #[test]
    fn source_ai_missing_file_is_error() {
        for slot in ["ai_prefix", "ai_suffix"] {
            let m = parse_err(&format!("{SRC}{slot}_file = \"missing.md\"\n"));
            assert!(m.contains("cam"), "{m}");
            assert!(m.contains(&format!("{slot}_file")), "{m}");
            assert!(m.contains("missing.md"), "{m}");
        }
    }

    #[test]
    fn removed_kinds_are_rejected_with_a_hint() {
        let filter = "[[targets.after_ai]]\nname = \"t\"\n[[targets.after_ai.filters]]\nmatch = \"*\"\nkinds = [\"photo\"]\n";
        let prefilter = "[[targets.ai]]\nname = \"t\"\ndescription = \"d\"\n[targets.ai.prefilter]\nkinds = [\"photo\"]\n";
        for text in [filter, prefilter] {
            let m = parse_err(text);
            assert!(m.contains("`kinds`"), "{m}");
            assert!(m.contains("match = \"@photo\""), "{m}");
        }
    }

    #[test]
    fn old_name_key_is_rejected_with_a_hint() {
        let filter =
            "[[targets.after_ai]]\nname = \"t\"\n[[targets.after_ai.filters]]\nname = \"*\"\n";
        let prefilter = "[[targets.ai]]\nname = \"t\"\ndescription = \"d\"\n[targets.ai.prefilter]\nname = \"*\"\n";
        for text in [filter, prefilter] {
            let m = parse_err(text);
            assert!(m.contains("unknown field `name`"), "{m}");
            assert!(m.contains("use `match`"), "{m}");
        }
        let output = "[[targets.after_ai]]\nname = \"t\"\n[[targets.after_ai.outputs]]\ndir = \"/x\"\nname = [\"capture-date\"]\n";
        let m = parse_err(output);
        assert!(m.contains("use `rename`"), "{m}");
        assert!(!m.contains("use `match`"), "{m}");
    }

    #[test]
    fn new_match_and_rename_keys_parse() {
        let c = load_str(&short_form_config("", "[]", r#"["capture-date"]"#)).unwrap();
        let t = &c.targets[0];
        assert!(t.filters[0].matches.is_some());
        assert_eq!(t.outputs[0].rename.len(), 1);
    }

    #[test]
    fn removed_ai_instructions_are_rejected() {
        for field in ["instructions", "instructions_file"] {
            let m = parse_err(&format!("[ai]\n{field} = \"x\"\n"));
            assert!(m.contains(&format!("`{field}`")), "{m}");
            // The error points to what replaced it.
            assert!(m.contains("ai_prefix"), "{m}");
        }
    }

    const TWO_SOURCES: &str = "[[sources]]\nid = \"cam\"\npath = \"/x\"\n";

    #[test]
    fn old_stage_shape_migrates_in_order() {
        // Written in the file out of order: the stages decide the order.
        let text = format!(
            r#"{TWO_SOURCES}
[[targets.after_ai]]
name = "B"
[[targets.after_ai.filters]]
sources = ["cam"]

[[targets.ai]]
name = "D"
sources = ["cam"]
description = "docs"
[targets.ai.prefilter]
match = "@photo"
exclude = "skip*"

[[targets.before_ai]]
name = "A"
[[targets.before_ai.filters]]
sources = ["cam"]
match = "*.png"
"#
        );
        let c = load_str(&text).unwrap();
        let names: Vec<_> = c.targets.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["A", "D", "B"]);
        assert!(c.was_staged);
        let d = c.target("D").unwrap();
        assert_eq!(d.kind, TargetKind::Folders);
        assert_eq!(d.filters.len(), 1);
        let f = &d.filters[0];
        assert_eq!(f.sources, ["cam"]);
        assert_eq!(f.matches, Some(MaskExpr::parse("@photo").unwrap()));
        assert_eq!(f.exclude, MaskExpr::parse("skip*").unwrap());
        assert_eq!(f.ai.as_deref(), Some("docs"));
        assert_eq!(f.expand, None);
        let a = c.target("A").unwrap();
        assert_eq!(a.filters[0].ai, None);
        assert!(c.target("nope").is_none());
        c.validate().unwrap();
        // Saved again it is the new shape, same order.
        let text = toml::to_string(&c).unwrap();
        assert!(
            !text.contains("before_ai") && text.contains("[[targets]]"),
            "{text}"
        );
        let again = load_str(&text).unwrap();
        assert!(!again.was_staged);
        let names: Vec<_> = again.targets.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["A", "D", "B"]);
        assert_eq!(
            again.target("D").unwrap().filters[0].ai.as_deref(),
            Some("docs")
        );
    }

    #[test]
    fn an_empty_targets_table_is_not_the_old_shape() {
        let c = load_str("[targets]\n").unwrap();
        assert!(c.targets.is_empty() && !c.was_staged);
        let c = load_str("[targets]\nbefore_ai = []\n").unwrap();
        assert!(!c.was_staged);
    }

    #[test]
    fn the_shipped_example_is_new_shape_and_valid() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/gallery.toml");
        let c = Config::load(&p).unwrap();
        assert!(!c.was_staged);
        c.validate().unwrap();
        let names: Vec<_> = c.targets.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Duplicates",
                "Blurry re-takes",
                "Screenshots",
                "Screenshots and screen recordings",
                "TikTok",
                "Night shots at home",
                "Together",
                "Documents",
                "Receipts",
                "Timeline"
            ]
        );
        assert_eq!(c.target("Duplicates").unwrap().kind, TargetKind::Trash);
        assert_eq!(c.target("Blurry re-takes").unwrap().kind, TargetKind::Trash);
        assert_eq!(
            c.target("Night shots at home").unwrap().kind,
            TargetKind::Keep
        );
        assert!(c.places.contains_key("home") && c.places.contains_key("dacha"));
        let together = c.target("Together").unwrap();
        assert!(
            together
                .filters
                .iter()
                .all(|f| f.near.is_some() && f.expand.is_some())
        );
        assert!(c.target("Documents").unwrap().has_ai());
        // Saved and read again it is the same config.
        let text = toml::to_string(&c).unwrap();
        let again: Config = toml::from_str(&text).unwrap();
        assert_eq!(toml::to_string(&again).unwrap(), text);
    }

    #[test]
    fn ai_target_without_prefilter_matches_every_name() {
        let c = load_str(&format!(
            "{TWO_SOURCES}[[targets.ai]]\nname = \"D\"\nsources = [\"cam\"]\ndescription = \"docs\"\n"
        ))
        .unwrap();
        let f = &c.targets[0].filters[0];
        assert_eq!((&f.matches, f.exclude.is_empty()), (&None, true));
    }

    #[test]
    fn new_shape_parses_kinds() {
        let text = format!(
            r#"{TWO_SOURCES}
[places]
home = {{ lat = 47.0245, lon = 28.8323, km = 2 }}

[[targets]]
name = "Duplicates"
kind = "trash"
[[targets.filters]]
sources = ["cam"]

[[targets]]
name = "Home"
kind = "keep"
[[targets.filters]]
sources = ["cam"]
match = "*.png"

[[targets]]
name = "Docs"
[[targets.filters]]
sources = ["cam"]
ai = "documents"
[[targets.outputs]]
dir = "/o"
"#
        );
        let c = load_str(&text).unwrap();
        assert!(!c.was_staged);
        let kinds: Vec<_> = c.targets.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            [TargetKind::Trash, TargetKind::Keep, TargetKind::Folders]
        );
        assert_eq!(
            c.places["home"],
            Place {
                lat: 47.0245,
                lon: 28.8323,
                km: 2.0
            }
        );
        c.validate().unwrap();
        assert!(c.target("Docs").unwrap().has_ai());

        let bad = text.replace(
            "kind = \"trash\"\n",
            "kind = \"trash\"\n[[targets.outputs]]\ndir = \"/o\"\n",
        );
        let m = load_str(&bad).unwrap().validate().unwrap_err().to_string();
        assert!(m.contains("outputs"), "{m}");
        assert!(m.contains("Duplicates"), "{m}");
        let m = parse_err(&text.replace("\"keep\"", "\"bin\""));
        assert!(m.contains("bin"), "{m}");
    }

    #[test]
    fn expand_with_ai_is_an_error() {
        let base = format!(
            "{TWO_SOURCES}[[targets]]\nname = \"T\"\n[[targets.filters]]\nsources = [\"cam\"]\nexpand = \"event\"\n"
        );
        let c = load_str(&base).unwrap();
        assert_eq!(c.targets[0].filters[0].expand, Some(Expand::Event));
        c.validate().unwrap();
        let m = load_str(&format!("{base}ai = \"docs\"\n"))
            .unwrap()
            .validate()
            .unwrap_err()
            .to_string();
        assert!(
            m.contains("expand") && m.contains("ai") && m.contains("T"),
            "{m}"
        );
    }

    #[test]
    fn unknown_place_ref_is_an_error() {
        let base = format!(
            "{TWO_SOURCES}[places]\nhome = {{ lat = 1.0, lon = 2.0, km = 1.0 }}\n[[targets]]\nname = \"T\"\n[[targets.filters]]\nsources = [\"cam\"]\n"
        );
        load_str(&format!("{base}geo = [\"@home\", \"!@home\"]\n"))
            .unwrap()
            .validate()
            .unwrap();
        let m = load_str(&format!("{base}geo = \"@nowhere\"\n"))
            .unwrap()
            .validate()
            .unwrap_err()
            .to_string();
        assert!(m.contains("nowhere") && m.contains("T"), "{m}");
        let m = load_str(&format!("{base}date = {{ months = [13] }}\n"))
            .unwrap()
            .validate()
            .unwrap_err()
            .to_string();
        assert!(m.contains("13"), "{m}");
        let m = load_str(&format!("{base}path = \"@nomask\"\n"))
            .unwrap()
            .validate()
            .unwrap_err()
            .to_string();
        assert!(m.contains("nomask"), "{m}");
    }

    #[test]
    fn blank_ai_description_is_an_error() {
        let c = load_str(&format!(
            "{TWO_SOURCES}[[targets]]\nname = \"T\"\n[[targets.filters]]\nsources = [\"cam\"]\nai = \" \"\n"
        ))
        .unwrap();
        assert!(c.validate().unwrap_err().to_string().contains("T"));
    }

    #[test]
    fn mixing_old_and_new_shape_is_an_error() {
        let new = "[[targets]]\nname = \"A\"\n";
        let old = "[[targets.before_ai]]\nname = \"B\"\n";
        // Old after new nests into the last new target; new after old is
        // not even valid TOML. Both refuse to load.
        let m = parse_err(&format!("{new}{old}"));
        assert!(m.contains("before_ai"), "{m}");
        assert!(m.contains("[[targets]]"), "{m}");
        let m = parse_err(&format!("{old}{new}"));
        assert!(m.contains("targets"), "{m}");
    }

    #[test]
    fn unknown_filter_field_is_rejected_in_the_new_shape() {
        let m = parse_err(
            "[[targets]]\nname = \"t\"\n[[targets.filters]]\nmatch = \"*\"\nkinds = [\"photo\"]\n",
        );
        assert!(m.contains("`kinds`"), "{m}");
        assert!(m.contains("match = \"@photo\""), "{m}");
    }

    fn parse_err(text: &str) -> String {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("g.toml");
        std::fs::write(&p, text).unwrap();
        match Config::load(&p) {
            Err(crate::Error::Config(m)) => m,
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[test]
    fn unknown_fields_are_rejected() {
        // A typo must not silently fall back to a default.
        let m = parse_err("[content_presets.P]\nkind = \"photo\"\nqualty = 70\n");
        assert!(m.contains("qualty"), "{m}");
        let m = parse_err("[content_presets.V]\nkind = \"video\"\ncrf_ = 30\n");
        assert!(m.contains("crf_"), "{m}");
        let m = parse_err("[content_presets.M]\nkind = \"metadata\"\nwrite_date = true\n");
        assert!(m.contains("write_date"), "{m}");
        let m = parse_err("[content_presets.A]\nkind = \"audio\"\ncodec = \"opus\"\nkpbs = 9\n");
        assert!(m.contains("kpbs"), "{m}");
        let m = parse_err("[content_presets.N]\nkind = \"png\"\nlevl = 2\n");
        assert!(m.contains("levl"), "{m}");
        let m = parse_err("[name_presets.N]\ntemplate = \"{stem}\"\nskipif = \"x\"\n");
        assert!(m.contains("skipif"), "{m}");
        let m = parse_err("[[sources]]\nid = \"s\"\npath = \"/x\"\nrecursiv = true\n");
        assert!(m.contains("recursiv"), "{m}");
        let m = parse_err("[ai]\nmodle = \"x\"\n");
        assert!(m.contains("modle"), "{m}");
        let m = parse_err("[target]\n");
        assert!(m.contains("target"), "{m}");
        let m = parse_err(
            "[[targets.after_ai]]\nname = \"t\"\n[[targets.after_ai.outputs]]\ndir = \"/x\"\ncontnet = []\n",
        );
        assert!(m.contains("contnet"), "{m}");
        let m = parse_err(
            "[[targets.after_ai]]\nname = \"t\"\n[[targets.after_ai.filters]]\nmatch = \"*\"\nkind = []\n",
        );
        assert!(m.contains("kind"), "{m}");
        let m = parse_err(
            "[[targets.ai]]\nname = \"t\"\ndescription = \"d\"\n[targets.ai.prefilter]\nmatc = \"x\"\n",
        );
        assert!(m.contains("matc"), "{m}");
        let m = parse_err(
            "[[targets.after_ai]]\nname = \"t\"\n[[targets.after_ai.outputs]]\ndir = \"/x\"\nrename = [{ mask = \"*\", preset = \"p\", x = 1 }]\n",
        );
        assert!(m.contains("`x`"), "{m}");
    }

    #[test]
    fn unsafe_name_templates_are_rejected_at_load() {
        for t in [
            "../x.{ext}",
            "/abs/{stem}.{ext}",
            "a//b.{ext}",
            "a\\\\b.{ext}",
        ] {
            let m = parse_err(&format!(
                "[name_presets.\"With folders\"]\ntemplate = \"{t}\"\n"
            ));
            assert!(m.contains("With folders"), "{t}: {m}");
        }
        // Folders below the output directory are fine.
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("g.toml");
        std::fs::write(
            &p,
            "[name_presets.F]\ntemplate = \"{date:YYYY}/{date:MM}/{stem}.{ext}\"\n",
        )
        .unwrap();
        Config::load(&p).unwrap();
    }

    #[test]
    fn bad_source_ids_are_rejected_at_load() {
        for id in ["", "a/b", "a\\\\b", ".", ".."] {
            let m = parse_err(&format!("[[sources]]\nid = \"{id}\"\npath = \"/x\"\n"));
            assert!(m.contains("source id"), "{id}: {m}");
        }
    }

    #[test]
    fn folder_template_with_skip_if_is_warned_about() {
        let mut c = Config::default();
        let p = |t: &str, skip: Option<&str>| NamePreset {
            mask: None,
            template: t.into(),
            date: DateSource::Capture,
            skip_if: skip.map(|s| MaskExpr::parse(s).unwrap()),
            normalize_ext: false,
        };
        c.name_presets
            .insert("Folders".into(), p("{date:YYYY}/{stem}.{ext}", Some("^2")));
        c.name_presets
            .insert("Flat".into(), p("{date}.{ext}", Some("^2")));
        c.name_presets.insert(
            "Folders no skip".into(),
            p("{date:YYYY}/{stem}.{ext}", None),
        );
        let w = c.warnings();
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(
            w[0].contains("Folders") && w[0].contains("skip_if"),
            "{w:?}"
        );
        assert!(example().warnings().is_empty());
    }

    fn err(c: &Config) -> String {
        match c.validate() {
            Err(crate::Error::Config(m)) => m,
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_duplicate_target_name() {
        let mut c = example();
        let dup = c.targets[0].clone();
        c.targets.push(dup.clone());
        assert!(err(&c).contains(&dup.name));
    }

    #[test]
    fn validate_rejects_unknown_source() {
        let mut c = example();
        c.targets[0].filters[0].sources = vec!["nope".into()];
        assert!(err(&c).contains("nope"));
        let mut c = example();
        c.targets[3].filters[0].sources = vec!["nope2".into()];
        assert!(err(&c).contains("nope2"));
    }

    #[test]
    fn validate_rejects_unknown_presets() {
        let mut c = example();
        c.targets[5].outputs[0].content[0].preset = "ghost".into();
        assert!(err(&c).contains("ghost"));
        let mut c = example();
        c.targets[5].outputs[0].rename.push(PipeStep {
            mask: Some(MaskExpr::parse("*").unwrap()),
            preset: "ghost2".into(),
        });
        assert!(err(&c).contains("ghost2"));
    }

    #[test]
    fn validate_rejects_empty_ai_description() {
        let mut c = example();
        c.targets[3].filters[0].ai = Some("  ".into());
        assert!(err(&c).contains(&c.targets[3].name));
    }

    /// The effective mask of a content step, as the plan resolves it.
    fn content_mask(c: &Config, st: &PipeStep) -> MaskExpr {
        let preset = c.content_preset(&st.preset).unwrap();
        st.mask.clone().unwrap_or_else(|| preset.mask_or_default())
    }

    fn short_form_config(extra: &str, content: &str, name: &str) -> String {
        format!(
            r#"{extra}
[[sources]]
id = "s"
path = "/tmp/s"

[[targets.after_ai]]
name = "t"

[[targets.after_ai.filters]]
sources = ["s"]
match = "*"

[[targets.after_ai.outputs]]
dir = "/tmp/o"
content = {content}
rename = {name}
"#
        )
    }

    #[test]
    fn short_step_uses_the_preset_mask() {
        let c = load_str(&short_form_config(
            "",
            r#"["meta-full", "avif-high", "av1-high"]"#,
            r#"["capture-date"]"#,
        ))
        .unwrap();
        c.validate().unwrap();
        let o = &c.targets[0].outputs[0];
        assert!(o.content.iter().all(|s| s.mask.is_none()));
        let photo = content_mask(&c, &o.content[1]);
        assert!(photo.matches("a.HEIC", &c) && photo.matches("b.jpg", &c));
        assert!(!photo.matches("c.mp4", &c));
        let video = content_mask(&c, &o.content[2]);
        assert!(video.matches("c.mp4", &c) && !video.matches("a.jpg", &c));
        assert!(o.rename[0].mask.is_none());
    }

    #[test]
    fn table_step_overrides_the_mask() {
        let c = load_str(&short_form_config(
            "",
            r#"[{ mask = "*.png", preset = "avif-high" }, { preset = "meta-full" }]"#,
            "[]",
        ))
        .unwrap();
        let o = &c.targets[0].outputs[0];
        let m = content_mask(&c, &o.content[0]);
        assert!(m.matches("a.png", &c) && !m.matches("a.jpg", &c));
        assert!(o.content[1].mask.is_none());
    }

    #[test]
    fn user_preset_mask_is_its_default() {
        let c = load_str(&short_form_config(
            "[content_presets.mine]\nkind = \"photo\"\nmask = \"*.tiff\"\n\n[name_presets.nm]\ntemplate = \"{stem}.{ext}\"\nmask = \"*.mov\"\n",
            r#"["mine"]"#,
            r#"["nm"]"#,
        ))
        .unwrap();
        c.validate().unwrap();
        let o = &c.targets[0].outputs[0];
        let m = content_mask(&c, &o.content[0]);
        assert!(m.matches("x.TIFF", &c) && !m.matches("x.jpg", &c));
        let nm = c.name_preset("nm").unwrap().mask_or_default();
        assert!(nm.matches("a.mov", &c) && !nm.matches("a.jpg", &c));
    }

    #[test]
    fn default_masks_per_kind() {
        use crate::presets::{builtin_content_preset, builtin_name_preset};
        let m = |n: &str| builtin_content_preset(n).unwrap().mask_or_default();
        let c = Config::default();
        assert!(
            m("meta-full").matches("anything.xyz", &c) && m("meta-dates").matches("README", &c)
        );
        assert!(m("png-optipng").matches("a.PNG", &c) && !m("png-optipng").matches("a.jpg", &c));
        assert!(m("opus-128").matches("a.flac", &c) && !m("opus-128").matches("a.mp4", &c));
        assert!(m("webp-low").matches("a.heif", &c) && !m("webp-low").matches("a.png", &c));
        assert!(m("av1-high").matches("a.mkv", &c) && m("av1-high").matches("a.m4v", &c));
        assert!(
            builtin_name_preset("capture-date")
                .unwrap()
                .mask_or_default()
                .matches("zzz", &c)
        );
    }

    #[test]
    fn short_form_round_trips_as_a_string() {
        let c = load_str(&short_form_config(
            "",
            r#"["meta-full", { mask = "*.jpg", preset = "avif-high" }]"#,
            r#"["capture-date"]"#,
        ))
        .unwrap();
        let text = toml::to_string(&c).unwrap();
        assert!(text.contains("\"meta-full\""), "{text}");
        assert!(!text.contains("preset = \"meta-full\""), "{text}");
        assert!(text.contains("\"capture-date\""), "{text}");
        let back: Config = toml::from_str(&text).unwrap();
        let o = &back.targets[0].outputs[0];
        assert_eq!(o.content, c.targets[0].outputs[0].content);
        assert!(o.content[0].mask.is_none() && o.content[1].mask.is_some());
    }

    #[test]
    fn unknown_short_preset_is_a_validate_error() {
        let c = load_str(&short_form_config("", r#"["ghost-preset"]"#, "[]")).unwrap();
        assert!(err(&c).contains("ghost-preset"));
        let c = load_str(&short_form_config("", "[]", r#"["ghost-name"]"#)).unwrap();
        assert!(err(&c).contains("ghost-name"));
    }

    /// A one-filter config; `extra` holds top-level tables such as `[masks]`.
    fn masks_config(extra: &str, filter: &str, step: &str) -> String {
        format!(
            r#"{extra}
[[sources]]
id = "s"
path = "/tmp/s"

[[targets.after_ai]]
name = "t"

[[targets.after_ai.filters]]
sources = ["s"]
{filter}

[[targets.after_ai.outputs]]
dir = "/tmp/o"
content = [{step}]
"#
        )
    }

    #[test]
    fn mask_reference_resolves() {
        let c = load_str(&masks_config("", r#"match = "@photo""#, r#""avif-high""#)).unwrap();
        c.validate().unwrap();
        let f = &c.targets[0].filters[0];
        let n = f.matches.as_ref().unwrap();
        assert!(n.matches("a.WEBP", &c) && n.matches("b.tiff", &c));
        assert!(!n.matches("c.mp4", &c));
    }

    #[test]
    fn mask_list_is_or_and_exclude_takes_references() {
        let c = load_str(&masks_config(
            "",
            r#"match = ["@photo", "@video", "*.txt"]
exclude = ["@raw", "*.tmp.*"]"#,
            r#""meta-full""#,
        ))
        .unwrap();
        c.validate().unwrap();
        let f = &c.targets[0].filters[0];
        let n = f.matches.as_ref().unwrap();
        assert!(n.matches("a.jpg", &c) && n.matches("b.MOV", &c) && n.matches("c.txt", &c));
        assert!(!n.matches("d.png", &c));
        assert!(f.exclude.matches("x.CR2", &c) && f.exclude.matches("a.tmp.jpg", &c));
        assert!(!f.exclude.matches("x.jpg", &c));
    }

    #[test]
    fn nested_mask_references() {
        let c = load_str(&masks_config(
            "[masks]\nshots = \"*screenshot*.{png,jpg}\"\nvisual = [\"@shots\", \"@video\"]\n",
            r#"match = "@visual""#,
            r#""meta-full""#,
        ))
        .unwrap();
        c.validate().unwrap();
        let n = c.targets[0].filters[0].matches.clone().unwrap();
        assert!(n.matches("Screenshot_1.PNG", &c) && n.matches("v.mkv", &c));
        assert!(!n.matches("IMG_1.jpg", &c));
        // A built-in one is nested too: media = photo + video + png.
        let media = MaskExpr::parse("@media").unwrap();
        assert!(media.matches("a.png", &c) && media.matches("a.avi", &c));
        assert!(!media.matches("a.cr2", &c));
    }

    #[test]
    fn mask_cycle_is_a_validate_error() {
        let c = load_str(&masks_config(
            "[masks]\na = \"@b\"\nb = [\"*.x\", \"@c\"]\nc = \"@a\"\n",
            r#"match = "@a""#,
            r#""meta-full""#,
        ))
        .unwrap();
        let m = err(&c);
        assert!(m.contains("cycle") && m.contains("a -> b -> c -> a"), "{m}");
        // Matching a cyclic mask on an unvalidated config still ends.
        assert!(!MaskExpr::parse("@a").unwrap().matches("q.y", &c));
    }

    #[test]
    fn unknown_mask_reference_is_a_validate_error() {
        let slots = [
            (r#"match = "@nope1""#, r#""meta-full""#, "nope1"),
            (
                "match = \"*\"\nexclude = [\"@nope2\"]",
                r#""meta-full""#,
                "nope2",
            ),
            (
                r#"match = "*""#,
                r#"{ mask = ["@nope3"], preset = "meta-full" }"#,
                "nope3",
            ),
        ];
        for (filter, step, who) in slots {
            let c = load_str(&masks_config("", filter, step)).unwrap();
            assert!(err(&c).contains(&format!("@{who}")), "{filter} {step}");
        }
        let c = load_str(&masks_config(
            "[content_presets.p]\nkind = \"photo\"\nmask = \"@nope4\"\n",
            r#"match = "*""#,
            r#""p""#,
        ))
        .unwrap();
        assert!(err(&c).contains("@nope4"));
        let c = load_str(&masks_config(
            "[masks]\na = \"@nope5\"\n",
            r#"match = "*""#,
            r#""meta-full""#,
        ))
        .unwrap();
        assert!(err(&c).contains("@nope5"));
    }

    #[test]
    fn negation_in_exclude_or_skip_if_is_a_validate_error() {
        let c = load_str(&masks_config(
            "",
            "match = \"*\"\nexclude = [\"*.raw\", \"!*.jpg\"]",
            r#""meta-full""#,
        ))
        .unwrap();
        let m = err(&c);
        assert!(
            m.contains("exclude") && m.contains("'!*.jpg'") && m.contains("match"),
            "{m}"
        );
        let c = load_str(
            "[[sources]]\nid = \"s\"\npath = \"/tmp/s\"\n\n[[targets.ai]]\nname = \"t\"\n\
             description = \"d\"\n[targets.ai.prefilter]\nexclude = [\"!@raw\"]\n",
        )
        .unwrap();
        let m = err(&c);
        assert!(
            m.contains("filter") && m.contains("exclude") && m.contains("'!@raw'"),
            "{m}"
        );
        let mut c = Config::default();
        c.name_presets.insert(
            "n".into(),
            NamePreset {
                mask: None,
                template: "{date}.{ext}".into(),
                date: DateSource::Capture,
                skip_if: Some(MaskExpr::parse("!*.jpg").unwrap()),
                normalize_ext: false,
            },
        );
        let m = err(&c);
        assert!(
            m.contains("skip_if") && m.contains("'!*.jpg'") && m.contains("'n'"),
            "{m}"
        );
    }

    #[test]
    fn user_mask_named_like_a_builtin_is_an_error() {
        let text = masks_config(
            "[masks]\nphoto = \"*.jpg\"\n",
            r#"match = "*""#,
            r#""meta-full""#,
        );
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("g.toml");
        std::fs::write(&p, &text).unwrap();
        let m = match Config::load(&p) {
            Err(crate::Error::Config(m)) => m,
            other => panic!("expected Config error, got {other:?}"),
        };
        assert!(m.contains("mask 'photo'"), "{m}");
    }

    #[test]
    fn preset_default_mask_is_a_reference() {
        use crate::presets::builtin_content_preset;
        let c = Config::default();
        let m = builtin_content_preset("avif-high")
            .unwrap()
            .mask_or_default();
        assert_eq!(m.source(), "@photo-original");
        assert!(m.matches("a.heic", &c) && !m.matches("a.webp", &c));
        let m = builtin_content_preset("opus-96").unwrap().mask_or_default();
        assert_eq!(m.source(), "@audio & !*.opus");
        assert!(m.matches("a.flac", &c) && !m.matches("a.opus", &c));
        for (preset, own, other) in [
            ("mp3-192", "mp3", "opus"),
            ("aac-128", "m4a", "mp3"),
            ("flac", "flac", "wav"),
        ] {
            let m = builtin_content_preset(preset).unwrap().mask_or_default();
            assert!(!m.matches(&format!("a.{own}"), &c), "{preset}");
            assert!(m.matches(&format!("a.{other}"), &c), "{preset}");
        }
    }

    #[test]
    fn masks_round_trip() {
        let c = load_str(&masks_config(
            "[masks]\nshots = \"*screenshot*\"\nboth = [\"@shots\", \"@raw\"]\n",
            "match = [\"@photo\", \"@video\"]\nexclude = \"@raw\"",
            r#"{ mask = ["@photo", "*.tif"], preset = "avif-high" }, "meta-full""#,
        ))
        .unwrap();
        let text = toml::to_string(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.masks, c.masks);
        let (f, g) = (&back.targets[0].filters[0], &c.targets[0].filters[0]);
        assert_eq!(f.matches, g.matches);
        assert_eq!(f.exclude, g.exclude);
        assert_eq!(f.exclude.source(), "@raw");
        assert_eq!(
            back.targets[0].outputs[0].content,
            c.targets[0].outputs[0].content
        );
        assert!(text.contains("\"@shots\""), "{text}");
    }

    #[test]
    fn skip_if_takes_references_and_lists() {
        use crate::name::{NameCtx, apply_name_pipe};
        let c = load_str(&masks_config(
            "[name_presets.p]\ntemplate = \"x_{stem}.{ext}\"\ndate = \"none\"\nskip_if = [\"@dated\", \"*.tmp\"]\n",
            r#"match = "*""#,
            r#""meta-full""#,
        ))
        .unwrap();
        c.validate().unwrap();
        let p = c.name_preset("p").unwrap();
        let run = |n: &str| {
            let steps = [(MaskExpr::parse("*").unwrap(), "p", p)];
            apply_name_pipe(&c, &steps, n, &NameCtx::default())
                .unwrap()
                .final_name
        };
        assert_eq!(run("20260101_120000.jpg"), "20260101_120000.jpg");
        assert_eq!(run("a.tmp"), "a.tmp");
        assert_eq!(run("IMG_1.jpg"), "x_IMG_1.jpg");
        // The built-in date presets skip by `@dated`.
        let b = c.name_preset("capture-date").unwrap();
        assert_eq!(b.skip_if.as_ref().unwrap().source(), "@dated");
    }

    #[test]
    fn unknown_skip_if_reference_and_empty_lists_are_errors() {
        let c = load_str(&masks_config(
            "[name_presets.p]\ntemplate = \"{stem}.{ext}\"\nskip_if = \"@nope6\"\n",
            r#"match = "*""#,
            r#""meta-full""#,
        ))
        .unwrap();
        assert!(err(&c).contains("@nope6"));
        for (extra, filter) in [
            ("[masks]\nempty = []\n", r#"match = "*""#),
            ("", r#"match = []"#),
        ] {
            let c = load_str(&masks_config(extra, filter, r#""meta-full""#)).unwrap();
            assert!(err(&c).contains("empty mask list"), "{extra}{filter}");
        }
        // An empty exclude is just "exclude nothing".
        let c = load_str(&masks_config(
            "",
            "match = \"*\"\nexclude = []",
            r#""meta-full""#,
        ))
        .unwrap();
        c.validate().unwrap();
    }

    #[test]
    fn too_deep_reference_chain_is_a_validate_error() {
        let mut extra = String::from("[masks]\n");
        for i in 0..20 {
            extra.push_str(&format!("m{i} = \"@m{}\"\n", i + 1));
        }
        extra.push_str("m20 = \"*.x\"\n");
        let c = load_str(&masks_config(&extra, r#"match = "@m0""#, r#""meta-full""#)).unwrap();
        let m = err(&c);
        assert!(m.contains("deeper than"), "{m}");
    }

    #[test]
    fn old_builtin_preset_name_gets_a_rename_hint() {
        let c = load_str(&masks_config("", r#"match = "*""#, r#""AVIF high""#)).unwrap();
        let m = err(&c);
        assert!(
            m.contains("'AVIF high'") && m.contains("'avif-high'"),
            "{m}"
        );
        let c = load_str(&masks_config("", r#"match = "*""#, r#""ghost-x""#)).unwrap();
        assert!(!err(&c).contains("renamed"));
    }

    const BUNDLE_BASE: &str = r#"
[[sources]]
id = "s"
path = "/tmp/s"
"#;

    fn bundle_err(extra: &str) -> String {
        load_str(&format!("{BUNDLE_BASE}{extra}"))
            .and_then(|c| c.validate())
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn bundle_parses_with_both_step_forms() {
        let c = load_str(&format!(
            "{BUNDLE_BASE}\n[content_presets.pack]\nkind = \"bundle\"\nsteps = [\"avif-high\", {{ mask = \"*.png\", preset = \"png-avif\" }}]\n"
        ))
        .unwrap();
        c.validate().unwrap();
        let ContentPreset::Bundle(b) = &c.content_presets["pack"] else {
            panic!("not a bundle")
        };
        assert_eq!(b.steps.len(), 2);
        assert!(b.steps[0].mask.is_none() && b.steps[1].mask.is_some());
    }

    #[test]
    fn bundle_with_a_mask_is_an_error() {
        let e = bundle_err(
            "\n[content_presets.pack]\nkind = \"bundle\"\nmask = \"*.png\"\nsteps = [\"png-avif\"]\n",
        );
        assert!(e.contains("pack") && e.contains("no mask"), "{e}");
    }

    #[test]
    fn bundle_step_with_a_mask_on_a_bundle_is_an_error() {
        let e = bundle_err(
            "\n[[targets.after_ai]]\nname = \"t\"\n[[targets.after_ai.outputs]]\ndir = \"/tmp/o\"\ncontent = [{ mask = \"*.png\", preset = \"compress-all\" }]\n",
        );
        assert!(e.contains("compress-all") && e.contains("no mask"), "{e}");
    }

    #[test]
    fn bundle_cycles_unknown_steps_and_empty_are_errors() {
        let e = bundle_err(
            "\n[content_presets.a]\nkind = \"bundle\"\nsteps = [\"b\"]\n[content_presets.b]\nkind = \"bundle\"\nsteps = [\"a\"]\n",
        );
        assert!(e.contains("bundle cycle: "), "{e}");
        let e = bundle_err("\n[content_presets.a]\nkind = \"bundle\"\nsteps = [\"nope\"]\n");
        assert!(e.contains("unknown content preset 'nope'"), "{e}");
        let e = bundle_err("\n[content_presets.a]\nkind = \"bundle\"\nsteps = []\n");
        assert!(e.contains("no steps"), "{e}");
        let e = bundle_err(
            "\n[content_presets.a]\nkind = \"bundle\"\nsteps = [\"avif-high\"]\nmaks = 1\n",
        );
        assert!(e.contains("maks"), "{e}");
    }

    #[test]
    fn bundle_may_not_reuse_a_builtin_name() {
        let e = bundle_err(
            "\n[content_presets.compress-all]\nkind = \"bundle\"\nsteps = [\"avif-high\"]\n",
        );
        assert!(e.contains("compress-all") && e.contains("built-in"), "{e}");
    }

    #[test]
    fn builtin_bundles_expand_to_known_presets() {
        let c = Config::default();
        for (name, want) in [
            (
                "compress-all",
                ["avif-high", "png-avif", "webp-avif", "av1-high"].as_slice(),
            ),
            ("compress-photos", &["avif-high", "png-avif", "webp-avif"]),
            (
                "compress-fast",
                &["avif-fast", "png-avif", "webp-avif", "av1-fast"],
            ),
        ] {
            let got: Vec<_> = c
                .expand_content("t", &[PipeStep::named(name)])
                .unwrap()
                .into_iter()
                .map(|e| e.step.preset)
                .collect();
            assert_eq!(got, want, "{name}");
        }
    }
}
