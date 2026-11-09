//! Conditions on groups of files, computed once over the whole frozen input
//! list (independent of routing): events, neighbours (`near`), duplicates and
//! series, plus the sharpness score. [`needs`] tells from the config what is
//! needed at all; [`compute`] does only that — with none of these conditions
//! configured it hashes, decodes and thumbnails nothing.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::{Duration, NaiveDateTime};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::facts::{Facts, Miss, Spec, Verdict, haversine_km, is_false, miss, with_not, within};
use super::image_metrics::{self, MetricsCache};
use crate::Result;
use crate::config::{Config, Expand, Filter, expand_tilde};
use crate::media::{MediaKind, kind_of};
use crate::progress::{Cancel, Progress, ProgressEvent, Stage};
use crate::scan::FileEntry;
use crate::thumbs;
use crate::tools::Tools;

/// Default `event.gap`.
pub const EVENT_GAP: &str = "4h";
/// Default `near.time`.
pub const NEAR_TIME: &str = "2h";
/// Default `series.gap`.
pub const SERIES_GAP: &str = "5s";
/// dHash bits two shots of one series may differ in.
const SIMILAR_BITS: u32 = 10;

fn default_duration(s: &str) -> Duration {
    super::range::parse_duration(s).expect("built-in duration")
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

fn one() -> usize {
    1
}

fn is_one(n: &usize) -> bool {
    *n == 1
}

// ------------------------------------------------------------ conditions

/// `event = { ... }`: conditions on the event a file belongs to.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventCond {
    /// Which files form events; empty: the filter's sources.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    /// A longer pause between two files starts a new event (default 4h).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<Spec<Duration>>,
    /// Also split on a jump in place longer than this many km.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub km: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_files: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_duration: Option<Spec<Duration>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_duration: Option<Spec<Duration>>,
    /// `"@place"`: at least one file of the event lies outside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub away_from: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub not: bool,
}

/// `near = { ... }`: the file has neighbours among files of other sources.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NearCond {
    #[serde(default)]
    pub sources: Vec<String>,
    /// Largest time difference (default 2h).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<Spec<Duration>>,
    /// Largest distance, checked when both files have GPS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub km: Option<f64>,
    /// Neighbours needed.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub min: usize,
    /// A pair where either file lacks GPS does not count.
    #[serde(default, skip_serializing_if = "is_false")]
    pub require_gps: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub not: bool,
}

/// Where a `duplicate` condition looks for identical copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DupScope {
    /// Other files of the scan; the original itself never matches.
    Sources,
    /// Files already in an output folder of a `folders` target.
    Outputs,
}

fn default_of() -> Vec<DupScope> {
    vec![DupScope::Sources]
}

fn is_default_of(v: &[DupScope]) -> bool {
    v == [DupScope::Sources]
}

/// `duplicate = { of = [...] }`: byte-identical content exists elsewhere.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DuplicateCond {
    #[serde(default = "default_of", skip_serializing_if = "is_default_of")]
    pub of: Vec<DupScope>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub not: bool,
}

/// Which shots of a series a `series` condition holds for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Keep {
    /// Every shot except the best one.
    #[default]
    Best,
}

impl Keep {
    fn is_default(&self) -> bool {
        *self == Keep::default()
    }
}

/// How the best shot of a series is chosen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum By {
    #[default]
    Sharpness,
    Size,
    Resolution,
}

impl By {
    fn is_default(&self) -> bool {
        *self == By::default()
    }
}

/// `series = { ... }`: near-identical shots taken seconds apart.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeriesCond {
    /// Consecutive shots closer than this (default 5s) may form a series.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<Spec<Duration>>,
    /// Shots must also look alike (perceptual hash of the thumbnails).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub similar: bool,
    #[serde(default, skip_serializing_if = "Keep::is_default")]
    pub keep: Keep,
    #[serde(default, skip_serializing_if = "By::is_default")]
    pub by: By,
    #[serde(default, skip_serializing_if = "is_false")]
    pub not: bool,
}

impl SeriesCond {
    fn params(&self) -> SeriesParams {
        SeriesParams {
            gap_ms: self
                .gap
                .as_ref()
                .map_or_else(|| default_duration(SERIES_GAP), |g| g.val)
                .num_milliseconds(),
            similar: self.similar,
            by: self.by,
        }
    }
}

/// What makes up a series; all `series` conditions of a config share it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SeriesParams {
    gap_ms: i64,
    similar: bool,
    by: By,
}

fn series_params(cfg: &Config) -> Option<SeriesParams> {
    filters(cfg).find_map(|f| f.series.as_ref().map(SeriesCond::params))
}

fn filters(cfg: &Config) -> impl Iterator<Item = &Filter> {
    cfg.targets.iter().flat_map(|t| t.filters.iter())
}

/// Event parameters: which sources form the events, the gap, the jump.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EventKey {
    /// Sorted, without repeats.
    pub sources: Vec<String>,
    pub gap_s: i64,
    /// `km` in metres.
    pub km_milli: Option<u64>,
}

impl EventKey {
    /// The events a filter looks at: from its `event` table (sources
    /// default to the filter's), else defaults when it has
    /// `expand = "event"`; `None` when it uses no events.
    pub fn of(f: &Filter) -> Option<EventKey> {
        if f.event.is_none() && f.expand != Some(Expand::Event) {
            return None;
        }
        let c = f.event.clone().unwrap_or_default();
        let mut sources = if c.sources.is_empty() {
            f.sources.clone()
        } else {
            c.sources
        };
        sources.sort();
        sources.dedup();
        let gap = c.gap.map_or_else(|| default_duration(EVENT_GAP), |g| g.val);
        Some(EventKey {
            sources,
            gap_s: gap.num_seconds(),
            km_milli: c.km.map(|k| (k * 1000.0).round().max(0.0) as u64),
        })
    }
}

// ----------------------------------------------------------------- needs

/// What the configured conditions need computed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Needs {
    /// Some condition reads file metadata (date, place, device, properties,
    /// events, neighbours, series, the capture date ranking duplicates), so
    /// every file's metadata is needed.
    pub exif_all: bool,
    /// Event parameter sets in use (each computed once).
    pub events: Vec<EventKey>,
    pub near: bool,
    pub duplicates: Vec<DupScope>,
    pub series: bool,
    /// `props.sharpness` is used (series compute their own scores).
    pub sharpness: bool,
}

impl Needs {
    /// Nothing to compute.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
            && !self.near
            && self.duplicates.is_empty()
            && !self.series
            && !self.sharpness
    }
}

/// What `cfg`'s filters need computed.
pub fn needs(cfg: &Config) -> Needs {
    let mut events = BTreeSet::new();
    let mut dups = BTreeSet::new();
    let mut n = Needs::default();
    for f in filters(cfg) {
        if let Some(k) = EventKey::of(f) {
            events.insert(k);
        }
        if let Some(d) = &f.duplicate {
            dups.extend(d.of.iter().copied());
        }
        n.near |= f.near.is_some();
        n.series |= f.series.is_some();
        n.sharpness |= f.props.as_ref().is_some_and(|p| p.sharpness.is_some());
        n.exif_all |= f.date.is_some()
            || f.geo.is_some()
            || f.device.is_some()
            || f.props.is_some()
            || f.event.is_some()
            || f.expand.is_some()
            || f.near.is_some()
            || f.series.is_some()
            // The original of a duplicate group is the oldest capture.
            || f
                .duplicate
                .as_ref()
                .is_some_and(|d| d.of.contains(&DupScope::Sources));
    }
    n.events = events.into_iter().collect();
    n.duplicates = dups.into_iter().collect();
    n
}

/// Everything [`compute`] reads from a config besides the files.
#[derive(PartialEq)]
struct ComputeInputs<'a> {
    needs: Needs,
    /// In config order: it ranks the original of a duplicate group.
    sources: Vec<(&'a str, &'a Path)>,
    output_roots: BTreeSet<PathBuf>,
    series: Option<SeriesParams>,
    series_sources: BTreeSet<&'a str>,
    sharpness_sources: BTreeSet<&'a str>,
}

impl<'a> ComputeInputs<'a> {
    fn of(cfg: &'a Config) -> Self {
        let sources_of = |pick: fn(&Filter) -> bool| {
            filters(cfg)
                .filter(|f| pick(f))
                .flat_map(|f| f.sources.iter().map(String::as_str))
                .collect()
        };
        ComputeInputs {
            needs: needs(cfg),
            sources: cfg
                .sources
                .iter()
                .map(|s| (s.id.as_str(), s.path.as_path()))
                .collect(),
            output_roots: cfg
                .targets
                .iter()
                .filter(|t| t.kind.is_folders())
                .flat_map(|t| t.outputs.iter())
                .filter_map(|o| output_root(&o.dir))
                .collect(),
            series: series_params(cfg),
            series_sources: sources_of(|f| f.series.is_some()),
            sharpness_sources: sources_of(|f| {
                f.props.as_ref().is_some_and(|p| p.sharpness.is_some())
            }),
        }
    }
}

/// [`compute`] gives `a` and `b` the same groups over the same files.
pub fn same_groups(a: &Config, b: &Config) -> bool {
    ComputeInputs::of(a) == ComputeInputs::of(b)
}

// ---------------------------------------------------------------- groups

/// Files of one event, in capture order.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub members: Vec<usize>,
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
}

/// Shots of one series, in capture order; `best` is one of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Series {
    pub members: Vec<usize>,
    pub best: usize,
}

/// When and (if known) where a file was taken.
type Moment = (NaiveDateTime, Option<(f64, f64)>);

/// Capture times per source, for `near`.
#[derive(Debug, Clone, Default)]
pub struct NearIndex {
    /// Time-sorted `(capture, file index)` per source id.
    by_source: HashMap<String, Vec<(NaiveDateTime, usize)>>,
    /// Capture time and GPS per file index; `None` without a capture date.
    at: Vec<Option<Moment>>,
}

impl NearIndex {
    pub fn new(files: &[FileEntry], facts: &[Facts]) -> NearIndex {
        let mut by_source: HashMap<String, Vec<(NaiveDateTime, usize)>> = HashMap::new();
        let at = facts
            .iter()
            .map(|f| f.capture.map(|t| (t, f.gps)))
            .collect();
        for (i, (e, f)) in files.iter().zip(facts).enumerate() {
            if let Some(t) = f.capture {
                by_source
                    .entry(e.source_id.clone())
                    .or_default()
                    .push((t, i));
            }
        }
        for v in by_source.values_mut() {
            v.sort();
        }
        NearIndex { by_source, at }
    }
}

/// Everything computed over the frozen list; indices are into `files`.
#[derive(Debug, Clone, Default)]
pub struct Groups {
    /// Event parameters → file → index into `events[parameters]`.
    pub event_of: HashMap<EventKey, HashMap<usize, usize>>,
    pub events: HashMap<EventKey, Vec<Event>>,
    /// Duplicate → its original (the copy that never matches).
    pub dup_original: HashMap<usize, usize>,
    /// Files whose content already exists in an output folder.
    pub dup_in_outputs: HashSet<usize>,
    /// Every group of scanned files with the same content (whatever the
    /// `duplicate` scope), with the one to keep should all go to the trash
    /// (ranked like the original of `dup_original`). Only the last-copy guard
    /// reads it: it never makes a condition hold.
    pub same_content: Vec<(Vec<usize>, usize)>,
    /// File → index into `series`.
    pub series_of: HashMap<usize, usize>,
    pub series: Vec<Series>,
    /// Sharpness score 0..100 of photos that needed one.
    pub sharpness: HashMap<usize, f32>,
    pub near: NearIndex,
}

impl Groups {
    /// The event file `i` belongs to under `key`.
    pub fn event(&self, key: &EventKey, i: usize) -> Option<&Event> {
        let e = *self.event_of.get(key)?.get(&i)?;
        self.events.get(key)?.get(e)
    }

    /// The series file `i` belongs to.
    pub fn series_of(&self, i: usize) -> Option<&Series> {
        self.series.get(*self.series_of.get(&i)?)
    }

    pub fn sharpness(&self, i: usize) -> Option<f32> {
        self.sharpness.get(&i).copied()
    }
}

/// How many neighbours file `i` has under `c`, counted up to `c.min` (enough
/// to hold; `0` without a capture date): files of `c.sources` within
/// `c.time`, and within `c.km` when both have GPS; a pair lacking GPS counts
/// unless `require_gps`.
pub fn near_count(groups_index: &NearIndex, i: usize, c: &NearCond) -> usize {
    let Some(Some((t, gps))) = groups_index.at.get(i).copied() else {
        return 0;
    };
    let w = c
        .time
        .as_ref()
        .map_or_else(|| default_duration(NEAR_TIME), |s| s.val);
    let lo = t.checked_sub_signed(w).unwrap_or(NaiveDateTime::MIN);
    let hi = t.checked_add_signed(w).unwrap_or(NaiveDateTime::MAX);
    let sources: BTreeSet<&String> = c.sources.iter().collect();
    let mut n = 0;
    for s in sources {
        let Some(list) = groups_index.by_source.get(s) else {
            continue;
        };
        let from = list.partition_point(|(x, _)| *x < lo);
        for &(x, j) in &list[from..] {
            if x > hi {
                break;
            }
            if j == i {
                continue;
            }
            let other = groups_index.at[j].and_then(|(_, g)| g);
            let ok = match (gps, other) {
                (Some(a), Some(b)) => c.km.is_none_or(|km| haversine_km(a, b) <= km),
                _ => !c.require_gps,
            };
            n += usize::from(ok);
            if n >= c.min {
                return n;
            }
        }
    }
    n
}

// --------------------------------------------------------------- compute

/// Makes the thumbnail of a file (a JPEG path).
type ThumbFn<'a> = dyn Fn(&Path) -> Result<PathBuf> + Sync + 'a;

/// Compute what `needs` asks for over `files` (with `facts[i]` of
/// `files[i]`). Thumbnails (via `tools`, `None`: none can be made) go to
/// `cache_dir/thumbs`, scores and content hashes to `cache_dir/metrics.json`.
/// Without a `cache_dir` nothing is written: no thumbnails are made (so no
/// sharpness and no similar series) and hashes live in memory only.
#[allow(clippy::too_many_arguments)]
pub fn compute(
    cfg: &Config,
    files: &[FileEntry],
    facts: &[Facts],
    needs: &Needs,
    tools: Option<&Tools>,
    cache_dir: Option<&Path>,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<Groups> {
    let thumb_dir = cache_dir.map(|d| d.join("thumbs"));
    let thumb = |p: &Path| match (tools, &thumb_dir) {
        (Some(t), Some(dir)) => thumbs::thumb_for_in(t, p, dir),
        // Never shown: a file without a thumbnail just has no measure.
        _ => Err(crate::Error::Tool(p.display().to_string())),
    };
    compute_with(
        cfg, files, facts, needs, &thumb, cache_dir, progress, cancel,
    )
}

/// [`compute`] with the thumbnail maker given.
#[allow(clippy::too_many_arguments)]
fn compute_with(
    cfg: &Config,
    files: &[FileEntry],
    facts: &[Facts],
    needs: &Needs,
    thumb: &ThumbFn,
    cache_dir: Option<&Path>,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<Groups> {
    cancel.check()?;
    let mut g = Groups::default();
    if needs.near {
        g.near = NearIndex::new(files, facts);
    }
    for key in &needs.events {
        let evs = events_for(key, files, facts);
        let of = evs
            .iter()
            .enumerate()
            .flat_map(|(e, ev)| ev.members.iter().map(move |&i| (i, e)))
            .collect();
        g.event_of.insert(key.clone(), of);
        g.events.insert(key.clone(), evs);
    }
    if needs.duplicates.is_empty() && !needs.series && !needs.sharpness {
        return Ok(g);
    }

    let metrics = cache_dir.map_or_else(MetricsCache::in_memory, MetricsCache::load);
    let work = Work {
        files,
        metrics: &metrics,
        thumb,
        progress,
        cancel,
    };
    let result = (|| -> Result<()> {
        if !needs.duplicates.is_empty() {
            let (orig, outs, same) = duplicates(cfg, files, facts, &needs.duplicates, &work)?;
            g.same_content = same;
            g.dup_original = orig;
            g.dup_in_outputs = outs;
        }
        if needs.series
            && let Some(p) = series_params(cfg)
        {
            series(cfg, files, facts, p, &work, &mut g)?;
        }
        if needs.sharpness {
            let srcs: HashSet<&str> = filters(cfg)
                .filter(|f| f.props.as_ref().is_some_and(|p| p.sharpness.is_some()))
                .flat_map(|f| f.sources.iter().map(String::as_str))
                .collect();
            let photos: Vec<usize> = (0..files.len())
                .filter(|&i| {
                    srcs.contains(files[i].source_id.as_str())
                        && kind_of(&files[i].path) == MediaKind::Photo
                        && !g.sharpness.contains_key(&i)
                })
                .collect();
            for (i, (s, _)) in work.measure_all(&photos)? {
                g.sharpness.insert(i, s);
            }
        }
        Ok(())
    })();
    // Keep what was measured even when cancelled.
    let saved = metrics.save();
    result?;
    saved?;
    Ok(g)
}

/// Shared state of the expensive passes.
struct Work<'a> {
    files: &'a [FileEntry],
    metrics: &'a MetricsCache,
    thumb: &'a ThumbFn<'a>,
    progress: &'a dyn Progress,
    cancel: &'a Cancel,
}

impl Work<'_> {
    /// Sharpness and dHash of photo `i`; `None` when it cannot be measured.
    fn measure(&self, i: usize) -> Option<(f32, u64)> {
        let f = &self.files[i];
        if let Some(m) = self.metrics.get(&f.path, f.size, f.mtime_ns)
            && let (Some(s), Some(d)) = (m.sharpness, m.dhash)
        {
            return Some((s, d));
        }
        let jpeg = (self.thumb)(&f.path).ok()?;
        let (s, d) = image_metrics::measure(&jpeg).ok()?;
        self.metrics.update(&f.path, f.size, f.mtime_ns, |m| {
            m.sharpness = Some(s);
            m.dhash = Some(d);
        });
        Some((s, d))
    }

    fn measure_all(&self, idx: &[usize]) -> Result<HashMap<usize, (f32, u64)>> {
        let done = AtomicUsize::new(0);
        let v = idx
            .par_iter()
            .map(|&i| {
                self.cancel.check()?;
                let m = self.measure(i);
                self.step(&done, idx.len(), &self.files[i].path);
                Ok(m.map(|m| (i, m)))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(v.into_iter().flatten().collect())
    }

    /// blake3 of a file's content (cached by path, size, mtime).
    fn hash(&self, path: &Path, size: u64, mtime_ns: i128) -> Option<String> {
        if let Some(h) = self
            .metrics
            .get(path, size, mtime_ns)
            .and_then(|m| m.blake3)
        {
            return Some(h);
        }
        let mut file = std::fs::File::open(path).ok()?;
        let mut hasher = blake3::Hasher::new();
        hasher.update_reader(&mut file).ok()?;
        let h = hasher.finalize().to_hex().to_string();
        self.metrics
            .update(path, size, mtime_ns, |m| m.blake3 = Some(h.clone()));
        Some(h)
    }

    fn step(&self, done: &AtomicUsize, total: usize, path: &Path) {
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        self.progress.step(ProgressEvent {
            stage: Stage::Plan,
            done: n,
            total,
            current: Some(path.to_path_buf()),
        });
    }
}

fn events_for(key: &EventKey, files: &[FileEntry], facts: &[Facts]) -> Vec<Event> {
    let mut idx: Vec<(NaiveDateTime, usize)> = files
        .iter()
        .zip(facts)
        .enumerate()
        .filter(|(_, (e, _))| key.sources.contains(&e.source_id))
        .filter_map(|(i, (_, f))| f.capture.map(|t| (t, i)))
        .collect();
    idx.sort();
    let gap = Duration::seconds(key.gap_s);
    let km = key.km_milli.map(|m| m as f64 / 1000.0);
    let mut out = Vec::new();
    let mut cur: Option<Event> = None;
    let mut last_gps: Option<(f64, f64)> = None;
    for (t, i) in idx {
        let gps = facts[i].gps;
        let split = match &cur {
            None => true,
            Some(e) => {
                let jump = match (km, last_gps, gps) {
                    (Some(km), Some(a), Some(b)) => haversine_km(a, b) > km,
                    _ => false,
                };
                t - e.end > gap || jump
            }
        };
        if split {
            out.extend(cur.take());
            cur = Some(Event {
                members: vec![i],
                start: t,
                end: t,
            });
            last_gps = None;
        } else if let Some(e) = cur.as_mut() {
            e.members.push(i);
            e.end = t;
        }
        if gps.is_some() {
            last_gps = gps;
        }
    }
    out.extend(cur);
    out
}

/// The directory an output template certainly writes under: the part
/// before the first `{` (its parent when the cut is inside a name),
/// `~` expanded; `None` when that is nothing.
fn output_root(template: &str) -> Option<PathBuf> {
    let (prefix, cut) = match template.find('{') {
        Some(p) => (&template[..p], true),
        None => (template, false),
    };
    if prefix.trim().is_empty() {
        return None;
    }
    let p = Path::new(prefix);
    let p = if cut && !prefix.ends_with(['/', '\\']) {
        p.parent()?
    } else {
        p
    };
    (!p.as_os_str().is_empty()).then(|| expand_tilde(p))
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// A file already in an output folder.
struct OutFile {
    path: PathBuf,
    /// Canonical path, to tell it from a scanned file at the same place.
    real: PathBuf,
    size: u64,
    mtime_ns: i128,
    /// Index into the output roots.
    root: usize,
}

/// The output roots of `folders` targets (canonical) and the files under
/// them whose size is in `sizes`. Hidden entries (`.stversions`,
/// `.gallery-trash`, ...) are skipped; a file under two roots is listed once.
fn output_files(
    cfg: &Config,
    sizes: &HashMap<u64, Vec<usize>>,
    cancel: &Cancel,
) -> Result<(Vec<PathBuf>, Vec<OutFile>)> {
    let roots: BTreeSet<PathBuf> = cfg
        .targets
        .iter()
        .filter(|t| t.kind.is_folders())
        .flat_map(|t| t.outputs.iter())
        .filter_map(|o| output_root(&o.dir))
        .map(|r| canon(&r))
        .collect();
    let roots: Vec<PathBuf> = roots.into_iter().collect();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (root, dir) in roots.iter().enumerate() {
        let walk = walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'));
        for e in walk.flatten() {
            cancel.check()?;
            if !e.file_type().is_file() {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            if !sizes.contains_key(&meta.len()) {
                continue;
            }
            let real = canon(e.path());
            let Ok(mtime_ns) = crate::scan::mtime_ns(&meta) else {
                continue;
            };
            if seen.insert(real.clone()) {
                out.push(OutFile {
                    path: e.path().to_path_buf(),
                    real,
                    size: meta.len(),
                    mtime_ns,
                    root,
                });
            }
        }
    }
    Ok((roots, out))
}

/// Original per duplicate, files already in an output, same-content groups.
type DupResult = (
    HashMap<usize, usize>,
    HashSet<usize>,
    Vec<(Vec<usize>, usize)>,
);

fn duplicates(
    cfg: &Config,
    files: &[FileEntry],
    facts: &[Facts],
    scopes: &[DupScope],
    work: &Work,
) -> Result<DupResult> {
    let in_sources = scopes.contains(&DupScope::Sources);
    let mut by_size: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        // Empty files carry no content to keep or lose.
        if f.size > 0 {
            by_size.entry(f.size).or_default().push(i);
        }
    }
    let (roots, outputs) = if scopes.contains(&DupScope::Outputs) {
        output_files(cfg, &by_size, work.cancel)?
    } else {
        Default::default()
    };
    let out_sizes: HashSet<u64> = outputs.iter().map(|o| o.size).collect();
    let mut to_hash: Vec<usize> = by_size
        .iter()
        .filter(|(size, v)| (in_sources && v.len() > 1) || out_sizes.contains(size))
        .flat_map(|(_, v)| v.iter().copied())
        .collect();
    to_hash.sort_unstable();

    let total = to_hash.len() + outputs.len();
    let done = AtomicUsize::new(0);
    let hashed = to_hash
        .par_iter()
        .map(|&i| {
            work.cancel.check()?;
            let f = &files[i];
            let h = work.hash(&f.path, f.size, f.mtime_ns);
            work.step(&done, total, &f.path);
            Ok(h.map(|h| (i, h)))
        })
        .collect::<Result<Vec<_>>>()?;
    // A scanned file that also lies under an output root (spelled another
    // way: a source reached through a link) keeps the hash it already has.
    let by_real: HashMap<PathBuf, &String> = if outputs.is_empty() {
        HashMap::new()
    } else {
        hashed
            .iter()
            .flatten()
            .map(|(i, h)| (canon(&files[*i].path), h))
            .collect()
    };
    let out_hashed = outputs
        .par_iter()
        .map(|o| {
            work.cancel.check()?;
            let h = match by_real.get(&o.real) {
                Some(h) => Some((*h).clone()),
                None => work.hash(&o.path, o.size, o.mtime_ns),
            };
            work.step(&done, total, &o.path);
            Ok(h.map(|h| ((o.size, h), o)))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut copies: HashMap<(u64, String), Vec<&OutFile>> = HashMap::new();
    for (k, o) in out_hashed.into_iter().flatten() {
        copies.entry(k).or_default().push(o);
    }

    // A file is never its own duplicate: its own path does not count, nor
    // does an output root that overlaps its source folder.
    let source_root: HashMap<&str, PathBuf> = cfg
        .sources
        .iter()
        .map(|s| (s.id.as_str(), canon(&expand_tilde(&s.path))))
        .collect();
    let overlaps = |i: usize, root: usize| {
        source_root
            .get(files[i].source_id.as_str())
            .is_some_and(|s| s.starts_with(&roots[root]) || roots[root].starts_with(s))
    };

    // Equal hashes are confirmed byte for byte: a cached hash is keyed by
    // path, size and mtime, so a file rewritten with the same size and
    // mtime keeps a stale one. A file that differs is no duplicate.
    let mut groups: HashMap<(u64, &str), Vec<usize>> = HashMap::new();
    let mut out_candidates = Vec::new();
    for (i, h) in hashed.iter().flatten() {
        let size = files[*i].size;
        if let Some(found) = copies.get(&(size, h.clone())) {
            let real = canon(&files[*i].path);
            let others: Vec<&OutFile> = found
                .iter()
                .filter(|o| o.real != real && !overlaps(*i, o.root))
                .copied()
                .collect();
            if !others.is_empty() {
                out_candidates.push((*i, others));
            }
        }
        groups.entry((size, h.as_str())).or_default().push(*i);
    }
    let in_outputs: HashSet<usize> = out_candidates
        .par_iter()
        .map(|(i, others)| {
            work.cancel.check()?;
            let p = &files[*i].path;
            Ok(others.iter().any(|o| same_bytes(p, &o.path)).then_some(*i))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    let candidates: Vec<Vec<usize>> = groups.into_values().filter(|m| m.len() > 1).collect();
    let verified: Vec<Vec<usize>> = candidates
        .par_iter()
        .map(|m| {
            work.cancel.check()?;
            Ok(split_by_bytes(m, files))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();

    let rank = |i: usize| {
        let src = cfg
            .sources
            .iter()
            .position(|s| s.id == files[i].source_id)
            .unwrap_or(usize::MAX);
        (
            src,
            facts[i].capture.is_none(),
            facts[i].capture,
            &files[i].path,
        )
    };
    // Kept for the last-copy guard whatever the scope: with `outputs` only,
    // a file and its copy in an output folder may both be scanned files.
    let mut same_content: Vec<(Vec<usize>, usize)> = verified
        .into_iter()
        .map(|mut m| {
            m.sort_unstable();
            let keep = *m.iter().min_by_key(|&&i| rank(i)).unwrap();
            (m, keep)
        })
        .collect();
    same_content.sort_unstable();
    let mut original_of = HashMap::new();
    if in_sources {
        for (members, orig) in &same_content {
            for &i in members.iter().filter(|&&i| i != *orig) {
                original_of.insert(i, *orig);
            }
        }
    }
    Ok((original_of, in_outputs, same_content))
}

/// `a` and `b` have the same bytes; unreadable counts as different.
fn same_bytes(a: &Path, b: &Path) -> bool {
    use std::io::Read;
    let (Ok(mut fa), Ok(mut fb)) = (std::fs::File::open(a), std::fs::File::open(b)) else {
        return false;
    };
    let (mut ba, mut bb) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let Ok(n) = read_full(&mut fa, &mut ba) else {
            return false;
        };
        let Ok(m) = read_full(&mut fb, &mut bb) else {
            return false;
        };
        if n != m || ba[..n] != bb[..m] {
            return false;
        }
        if n == 0 {
            return true;
        }
    }

    fn read_full(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut got = 0;
        while got < buf.len() {
            match f.read(&mut buf[got..])? {
                0 => break,
                k => got += k,
            }
        }
        Ok(got)
    }
}

/// Files with one hash, split into groups of byte-equal files; only
/// groups of two or more are kept.
fn split_by_bytes(members: &[usize], files: &[FileEntry]) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for &i in members {
        match groups
            .iter_mut()
            .find(|g| same_bytes(&files[g[0]].path, &files[i].path))
        {
            Some(g) => g.push(i),
            None => groups.push(vec![i]),
        }
    }
    groups.retain(|g| g.len() > 1);
    groups
}

fn series(
    cfg: &Config,
    files: &[FileEntry],
    facts: &[Facts],
    p: SeriesParams,
    work: &Work,
    g: &mut Groups,
) -> Result<()> {
    let srcs: BTreeSet<&str> = filters(cfg)
        .filter(|f| f.series.is_some())
        .flat_map(|f| f.sources.iter().map(String::as_str))
        .collect();
    let gap = Duration::milliseconds(p.gap_ms);
    // Per source, photos with a capture date in capture order; a chain of
    // consecutive shots closer than `gap` is a candidate run.
    let mut runs: Vec<Vec<usize>> = Vec::new();
    for src in srcs {
        let mut shots: Vec<(NaiveDateTime, usize)> = (0..files.len())
            .filter(|&i| files[i].source_id == src && kind_of(&files[i].path) == MediaKind::Photo)
            .filter_map(|i| facts[i].capture.map(|t| (t, i)))
            .collect();
        shots.sort();
        let mut run: Vec<usize> = Vec::new();
        let mut prev: Option<NaiveDateTime> = None;
        for (t, i) in shots {
            if prev.is_some_and(|p| t - p < gap) {
                run.push(i);
            } else {
                if run.len() > 1 {
                    runs.push(std::mem::take(&mut run));
                }
                run = vec![i];
            }
            prev = Some(t);
        }
        if run.len() > 1 {
            runs.push(run);
        }
    }

    let need: Vec<usize> = if p.similar || p.by == By::Sharpness {
        runs.iter().flatten().copied().collect()
    } else {
        Vec::new()
    };
    let measured = work.measure_all(&need)?;

    // Split runs where consecutive shots do not look alike.
    let mut found: Vec<Vec<usize>> = Vec::new();
    for run in runs {
        let mut cur = vec![run[0]];
        for w in run.windows(2) {
            let alike = !p.similar
                || matches!(
                    (measured.get(&w[0]), measured.get(&w[1])),
                    (Some(a), Some(b)) if image_metrics::hamming(a.1, b.1) <= SIMILAR_BITS
                );
            if !alike {
                if cur.len() > 1 {
                    found.push(std::mem::take(&mut cur));
                } else {
                    cur.clear();
                }
            }
            cur.push(w[1]);
        }
        if cur.len() > 1 {
            found.push(cur);
        }
    }

    for members in found {
        let score = |i: usize| -> f64 {
            match p.by {
                By::Sharpness => measured.get(&i).map_or(-1.0, |m| f64::from(m.0)),
                By::Size => files[i].size as f64,
                By::Resolution => facts[i]
                    .width
                    .zip(facts[i].height)
                    .map_or(0.0, |(w, h)| f64::from(w) * f64::from(h)),
            }
        };
        // The first of equally good shots wins.
        let mut best = members[0];
        for &i in &members[1..] {
            if score(i) > score(best) {
                best = i;
            }
        }
        let s = g.series.len();
        for &i in &members {
            g.series_of.insert(i, s);
            if let Some(m) = measured.get(&i) {
                g.sharpness.insert(i, m.0);
            }
        }
        g.series.push(Series { members, best });
    }
    Ok(())
}

// -------------------------------------------------------------- checking

fn event_verdict(
    cfg: &Config,
    c: &EventCond,
    key: &EventKey,
    i: usize,
    facts: &[Facts],
    groups: &Groups,
) -> Verdict {
    let Some(ev) = groups.event(key, i) else {
        return Err(if facts[i].capture.is_none() {
            crate::t!("filter.miss.no_capture_date")
        } else {
            crate::t!("filter.miss.no_event")
        });
    };
    if let Some(min) = c.min_files
        && ev.members.len() < min
    {
        return Err(crate::t!(
            "filter.miss.event_small",
            files = crate::tn!("count.files", ev.members.len()),
            min = min
        ));
    }
    let len = ev.end - ev.start;
    let text = || crate::estimate::duration_text(len.num_seconds() as f64);
    if let Some(d) = &c.min_duration
        && len < d.val
    {
        return Err(crate::t!(
            "filter.miss.event_short",
            length = text(),
            wanted = d.raw()
        ));
    }
    if let Some(d) = &c.max_duration
        && len > d.val
    {
        return Err(crate::t!(
            "filter.miss.event_long",
            length = text(),
            wanted = d.raw()
        ));
    }
    if let Some(name) = &c.away_from {
        let name = name.trim().trim_start_matches('@');
        // Validated on load; an unknown place has no outside.
        let away = cfg.places.get(name).is_some_and(|p| {
            ev.members
                .iter()
                .any(|&m| facts[m].gps.is_some_and(|g| !within(p, g)))
        });
        if !away {
            return Err(crate::t!(
                "filter.miss.event_at_place",
                place = format!("@{name}")
            ));
        }
    }
    Ok(())
}

fn near_verdict(c: &NearCond, i: usize, facts: &[Facts], groups: &Groups) -> Verdict {
    if facts[i].capture.is_none() {
        return Err(crate::t!("filter.miss.no_capture_date"));
    }
    let n = near_count(&groups.near, i, c);
    if n >= c.min {
        return Ok(());
    }
    let time = c.time.as_ref().map_or(NEAR_TIME, |t| t.raw());
    let wanted = match c.km {
        Some(km) => format!("{time} / {} km", crate::i18n::decimal(km, 1)),
        None => time.to_string(),
    };
    Err(crate::t!(
        "filter.miss.near",
        found = crate::tn!("count.files", n),
        sources = c.sources.join(", "),
        wanted = wanted,
        min = c.min
    ))
}

fn duplicate_verdict(c: &DuplicateCond, i: usize, groups: &Groups) -> Verdict {
    let hit = c.of.iter().any(|s| match s {
        DupScope::Sources => groups.dup_original.contains_key(&i),
        DupScope::Outputs => groups.dup_in_outputs.contains(&i),
    });
    if hit {
        Ok(())
    } else {
        Err(crate::t!("filter.miss.no_duplicate"))
    }
}

fn series_verdict(c: &SeriesCond, i: usize, groups: &Groups, confirmed_best: bool) -> Verdict {
    if confirmed_best {
        return Err(crate::t!("filter.miss.series_best"));
    }
    let s = groups
        .series_of(i)
        .ok_or_else(|| crate::t!("filter.miss.not_in_series"))?;
    match c.keep {
        Keep::Best if s.best == i => Err(crate::t!("filter.miss.series_best")),
        Keep::Best => Ok(()),
    }
}

/// Check the group conditions of `f` (`event`, `near`, `duplicate`,
/// `series`) for file `i`; the first that fails is the `Miss`. `expand` and
/// `props.sharpness` are left to the routing.
pub fn check_groups(
    cfg: &Config,
    f: &Filter,
    i: usize,
    facts: &[Facts],
    groups: &Groups,
) -> std::result::Result<(), Miss> {
    check_groups_with(cfg, f, i, facts, groups, false)
}

/// [`check_groups`] where `confirmed_best` says the user confirmed file `i`
/// as the best shot of its series: a `series` condition never holds for it.
pub(crate) fn check_groups_with(
    cfg: &Config,
    f: &Filter,
    i: usize,
    facts: &[Facts],
    groups: &Groups,
    confirmed_best: bool,
) -> std::result::Result<(), Miss> {
    if let Some(c) = &f.event
        && let Some(key) = EventKey::of(f)
    {
        miss(
            "event",
            with_not(c.not, event_verdict(cfg, c, &key, i, facts, groups)),
        )?;
    }
    if let Some(c) = &f.near {
        miss("near", with_not(c.not, near_verdict(c, i, facts, groups)))?;
    }
    if let Some(c) = &f.duplicate {
        miss(
            "duplicate",
            with_not(c.not, duplicate_verdict(c, i, groups)),
        )?;
    }
    if let Some(c) = &f.series {
        miss(
            "series",
            with_not(c.not, series_verdict(c, i, groups, confirmed_best)),
        )?;
    }
    Ok(())
}

/// Load-time checks of one filter's group conditions; the error is a
/// translated message for `target`.
pub(crate) fn check_conditions(
    f: &Filter,
    cfg: &Config,
    target: &str,
) -> std::result::Result<(), String> {
    if let Some(n) = &f.near
        && n.sources.is_empty()
    {
        return Err(crate::t!("config.near_no_sources", target = target));
    }
    if let Some(a) = f.event.as_ref().and_then(|e| e.away_from.as_ref()) {
        let known = a
            .trim()
            .strip_prefix('@')
            .is_some_and(|n| cfg.places.contains_key(n));
        if !known {
            return Err(crate::t!(
                "config.bad_away_from",
                target = target,
                value = a
            ));
        }
    }
    if let (Some(s), Some(first)) = (&f.series, series_params(cfg))
        && s.params() != first
    {
        return Err(crate::t!("config.series_mismatch", target = target));
    }
    Ok(())
}

/// Source ids a filter's group conditions name (checked on load).
pub(crate) fn named_sources(f: &Filter) -> impl Iterator<Item = &String> {
    let event = f.event.iter().flat_map(|e| e.sources.iter());
    let near = f.near.iter().flat_map(|n| n.sources.iter());
    event.chain(near)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::image_metrics::testing::{blurred, other, save, sharp};
    use crate::progress::NoProgress;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    fn fact(capture: Option<&str>, gps: Option<(f64, f64)>) -> Facts {
        Facts {
            capture: capture.map(dt),
            mtime: dt("2026-08-01 00:00:00"),
            name_date: None,
            gps,
            make: None,
            model: None,
            software: None,
            width: None,
            height: None,
            duration_s: None,
            fps: None,
            video_codec: None,
            audio_codec: None,
            has_audio: None,
            container: "jpg".into(),
            size: 1,
        }
    }

    fn entry(source: &str, path: &Path) -> FileEntry {
        let (size, mtime_ns) = match std::fs::metadata(path) {
            Ok(m) => (m.len(), crate::scan::mtime_ns(&m).unwrap()),
            Err(_) => (1, 0),
        };
        FileEntry {
            source_id: source.into(),
            path: path.to_path_buf(),
            rel: PathBuf::from(path.file_name().unwrap()),
            size,
            mtime_ns,
        }
    }

    fn virt(source: &str, name: &str) -> FileEntry {
        entry(source, &Path::new("/nonexistent").join(source).join(name))
    }

    fn config(targets: &str) -> Config {
        let text = format!(
            r#"
[places]
home = {{ lat = 47.0245, lon = 28.8323, km = 2 }}

[[sources]]
id = "camera"
path = "/nonexistent/camera"

[[sources]]
id = "olga"
path = "/nonexistent/olga"

{targets}
"#
        );
        let c: Config = toml::from_str(&text).unwrap();
        c.validate().unwrap();
        c
    }

    fn no_thumbs(p: &Path) -> Result<PathBuf> {
        panic!("no thumbnail may be made here: {}", p.display())
    }

    fn run(cfg: &Config, files: &[FileEntry], facts: &[Facts], thumb: &ThumbFn) -> Groups {
        let cache = tempfile::tempdir().unwrap();
        compute_with(
            cfg,
            files,
            facts,
            &needs(cfg),
            thumb,
            Some(cache.path()),
            &NoProgress,
            &Cancel::new(),
        )
        .unwrap()
    }

    fn holds(cfg: &Config, i: usize, facts: &[Facts], g: &Groups) -> std::result::Result<(), Miss> {
        check_groups(cfg, &cfg.targets[0].filters[0], i, facts, g)
    }

    const EVENT: &str = r#"
[[targets]]
name = "Trips"
[[targets.filters]]
sources = ["camera"]
event = { min_files = 2 }
"#;

    #[test]
    fn events_split_on_gap_and_jump() {
        let files: Vec<FileEntry> = ["a", "b", "c", "d"]
            .iter()
            .map(|n| virt("camera", n))
            .collect();
        let home = Some((47.02, 28.83));
        let far = Some((47.92, 28.83)); // ~100 km north
        let facts = vec![
            fact(Some("2026-08-01 10:00:00"), home),
            fact(Some("2026-08-01 10:30:00"), far),
            fact(Some("2026-08-01 16:00:00"), None),
            fact(None, home),
        ];

        let cfg = config(EVENT);
        let g = run(&cfg, &files, &facts, &no_thumbs);
        let key = EventKey::of(&cfg.targets[0].filters[0]).unwrap();
        assert_eq!(key.gap_s, 4 * 3600);
        let evs = &g.events[&key];
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].members, vec![0, 1]);
        assert_eq!(evs[0].end, dt("2026-08-01 10:30:00"));
        assert!(holds(&cfg, 0, &facts, &g).is_ok());
        // Alone in its event: fewer than min_files.
        assert_eq!(holds(&cfg, 2, &facts, &g).unwrap_err().condition, "event");
        // No capture date: in no event, the condition misses (and `not` hits).
        assert!(g.event(&key, 3).is_none());
        let m = holds(&cfg, 3, &facts, &g).unwrap_err();
        assert_eq!(m.detail, crate::t!("filter.miss.no_capture_date"));
        let negated = config(&EVENT.replace("min_files = 2", "min_files = 2, not = true"));
        assert!(holds(&negated, 3, &facts, &g).is_ok());

        let cfg = config(&EVENT.replace("min_files = 2", "km = 30"));
        let g = run(&cfg, &files, &facts, &no_thumbs);
        let key = EventKey::of(&cfg.targets[0].filters[0]).unwrap();
        assert_eq!(g.events[&key].len(), 3);
    }

    #[test]
    fn event_duration_and_away_from() {
        let files: Vec<FileEntry> = ["a", "b"].iter().map(|n| virt("camera", n)).collect();
        let at_home = vec![
            fact(Some("2026-08-01 10:00:00"), Some((47.02, 28.83))),
            fact(Some("2026-08-01 12:00:00"), None),
        ];
        let away = vec![
            at_home[0].clone(),
            fact(Some("2026-08-01 12:00:00"), Some((46.5, 30.7))),
        ];
        let cfg = config(&EVENT.replace(
            "min_files = 2",
            r#"min_duration = "1h", max_duration = "3h", away_from = "@home""#,
        ));
        let g = run(&cfg, &files, &at_home, &no_thumbs);
        assert!(holds(&cfg, 0, &at_home, &g).is_err());
        let g = run(&cfg, &files, &away, &no_thumbs);
        assert!(holds(&cfg, 0, &away, &g).is_ok());
        let long = config(&EVENT.replace("min_files = 2", r#"min_duration = "1d""#));
        let g = run(&long, &files, &away, &no_thumbs);
        assert!(holds(&long, 1, &away, &g).is_err());
    }

    const NEAR: &str = r#"
[[targets]]
name = "Together"
[[targets.filters]]
sources = ["camera"]
near = { sources = ["olga"], time = "2h", km = 1 }
"#;

    fn near_case(cfg: &Config, olga_gps: Option<(f64, f64)>) -> bool {
        let files = vec![virt("camera", "c.jpg"), virt("olga", "o.jpg")];
        let facts = vec![
            fact(Some("2026-08-01 12:00:00"), Some((47.02, 28.83))),
            fact(Some("2026-08-01 13:30:00"), olga_gps),
        ];
        let g = run(cfg, &files, &facts, &no_thumbs);
        holds(cfg, 0, &facts, &g).is_ok()
    }

    #[test]
    fn near_by_time_and_place() {
        let cfg = config(NEAR);
        assert!(near_case(&cfg, Some((47.0245, 28.83)))); // ~0.5 km
        assert!(!near_case(&cfg, Some((47.065, 28.83)))); // ~5 km
        assert!(near_case(&cfg, None));
        let strict = config(&NEAR.replace("km = 1", "km = 1, require_gps = true"));
        assert!(!near_case(&strict, None));
        let short = config(&NEAR.replace("\"2h\"", "\"1h\""));
        assert!(!near_case(&short, Some((47.0245, 28.83))));
    }

    #[test]
    fn mixed_zones_quicktime_utc_video_and_other_phone_photo_are_close() {
        // The plan zone is Chisinau (+03:00 in August). The camera's video
        // stores its QuickTime date in UTC (09:20Z = 12:20 local); Olga's
        // phone writes its local wall clock, 12:50, in EXIF (its
        // OffsetTimeOriginal is not read). Compared as plan-zone wall clock
        // they are 30 minutes apart, not 3 hours 30.
        let zone = "Europe/Chisinau";
        let tz: chrono_tz::Tz = zone.parse().unwrap();
        let unused = PathBuf::from("unused");
        let tools = crate::tools::Tools {
            ffmpeg: unused.clone(),
            ffprobe: unused.clone(),
            exiftool: unused.clone(),
            avifenc: unused.clone(),
            magick: unused,
            optipng: None,
            tz: Some(zone.into()),
        };
        let mut video = serde_json::json!({
            "QuickTime:CreateDate": "2026:08:01 09:20:00",
            "Track1:ImageWidth": 1920, "Track1:ImageHeight": 1080,
        });
        crate::exif::quicktime_to_local(&tools, &mut video);
        let photo = serde_json::json!({
            "ExifIFD:DateTimeOriginal": "2026:08:01 12:50:00",
            "ExifIFD:OffsetTimeOriginal": "+03:00",
        });
        let files = vec![virt("camera", "clip.mp4"), virt("olga", "o.jpg")];
        let facts: Vec<Facts> = vec![
            crate::filter::facts::facts_of(&files[0], &video, Some(tz)),
            crate::filter::facts::facts_of(&files[1], &photo, Some(tz)),
        ];
        assert_eq!(facts[0].capture, Some(dt("2026-08-01 12:20:00")));
        assert_eq!(facts[1].capture, Some(dt("2026-08-01 12:50:00")));

        let near = config(&NEAR.replace("\"2h\"", "\"45m\""));
        let g = run(&near, &files, &facts, &no_thumbs);
        assert!(holds(&near, 0, &facts, &g).is_ok());

        let event = config(
            r#"
[[targets]]
name = "Trips"
[[targets.filters]]
sources = ["camera", "olga"]
event = { gap = "45m", min_files = 2 }
"#,
        );
        let g = run(&event, &files, &facts, &no_thumbs);
        let key = EventKey::of(&event.targets[0].filters[0]).unwrap();
        assert_eq!(g.events[&key].len(), 1);
        assert_eq!(g.events[&key][0].members, vec![0, 1]);
        assert!(holds(&event, 0, &facts, &g).is_ok());
        assert!(holds(&event, 1, &facts, &g).is_ok());
    }

    #[test]
    fn near_count_stops_at_min() {
        let cfg = config(NEAR);
        let files = vec![
            virt("camera", "c.jpg"),
            virt("olga", "o1.jpg"),
            virt("olga", "o2.jpg"),
            virt("olga", "o3.jpg"),
        ];
        let facts = vec![fact(Some("2026-08-01 12:00:00"), None); 4];
        let g = run(&cfg, &files, &facts, &no_thumbs);
        let mut c = cfg.targets[0].filters[0].near.clone().unwrap();
        assert_eq!(near_count(&g.near, 0, &c), 1);
        c.min = 2;
        assert_eq!(near_count(&g.near, 0, &c), 2);
        c.min = 5;
        assert_eq!(near_count(&g.near, 0, &c), 3, "fewer than min: all of them");
    }

    #[test]
    fn near_without_capture_date_misses() {
        let cfg = config(NEAR);
        let files = vec![virt("camera", "c.jpg"), virt("olga", "o.jpg")];
        let facts = vec![fact(None, None), fact(Some("2026-08-01 13:30:00"), None)];
        let g = run(&cfg, &files, &facts, &no_thumbs);
        assert_eq!(
            near_count(&g.near, 0, cfg.targets[0].filters[0].near.as_ref().unwrap()),
            0
        );
        let m = holds(&cfg, 0, &facts, &g).unwrap_err();
        assert_eq!(m.detail, crate::t!("filter.miss.no_capture_date"));
    }

    #[test]
    fn duplicates_of_sources_and_outputs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let write = |rel: &str, body: &[u8]| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
            p
        };
        let cam_a = write("camera/a.jpg", b"same picture");
        let olga_a = write("olga/a.jpg", b"same picture");
        let cam_b = write("camera/b.jpg", b"placed before");
        let out_b = write("out/2026/b.jpg", b"placed before");
        write("out/2026/.stversions/a.jpg", b"same picture");
        let out = root.join("out").display().to_string().replace('\\', "/");
        let text = format!(
            r#"
[[sources]]
id = "camera"
path = "{r}/camera"

[[sources]]
id = "olga"
path = "{r}/olga"

[[sources]]
id = "outsrc"
path = "{out}"

[[targets]]
name = "Duplicates"
kind = "trash"
[[targets.filters]]
sources = ["camera", "olga", "outsrc"]
duplicate = {{ of = ["sources", "outputs"] }}

[[targets]]
name = "Album"
[[targets.outputs]]
dir = "{out}/{{YYYY}}"
"#,
            r = root.display().to_string().replace('\\', "/"),
        );
        let cfg: Config = toml::from_str(&text).unwrap();
        let files = vec![
            entry("olga", &olga_a),
            entry("camera", &cam_a),
            entry("camera", &cam_b),
            entry("outsrc", &out_b),
        ];
        let facts = vec![fact(None, None); 4];
        let cache = tempfile::tempdir().unwrap();
        let n = needs(&cfg);
        assert_eq!(n.duplicates, vec![DupScope::Sources, DupScope::Outputs]);
        let g = compute_with(
            &cfg,
            &files,
            &facts,
            &n,
            &no_thumbs,
            Some(cache.path()),
            &NoProgress,
            &Cancel::new(),
        )
        .unwrap();
        // camera is listed first: its copy is the original.
        assert_eq!(g.dup_original.get(&0), Some(&1));
        assert!(!g.dup_original.contains_key(&1));
        assert!(holds(&cfg, 0, &facts, &g).is_ok());
        assert!(holds(&cfg, 1, &facts, &g).is_err());
        // camera/b.jpg is already in the album; the album's own copy (a
        // scanned file of a source inside the album) is not its own duplicate.
        assert!(g.dup_in_outputs.contains(&2));
        assert!(!g.dup_in_outputs.contains(&3));
        // Hidden folders of an output are not looked at.
        assert!(!g.dup_in_outputs.contains(&1));
        // Every same-content group, with the copy to keep (camera first).
        assert_eq!(g.same_content, vec![(vec![0, 1], 1), (vec![2, 3], 2)]);
        // Hashes are cached.
        let cached = std::fs::read_to_string(cache.path().join("metrics.json")).unwrap();
        assert!(cached.contains("blake3"));

        // Without a cache dir: the same groups, nothing written anywhere
        // (the thumbnail maker is never even asked for a folder).
        let tools = crate::tools::Tools::discover().ok();
        let before: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        let g2 = compute(
            &cfg,
            &files,
            &facts,
            &n,
            tools.as_ref(),
            None,
            &NoProgress,
            &Cancel::new(),
        )
        .unwrap();
        assert_eq!(g2.dup_original, g.dup_original);
        assert_eq!(g2.dup_in_outputs, g.dup_in_outputs);
        let after: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        assert_eq!(before, after);
    }

    /// A scanned file under an output root, its source spelled through a
    /// link: hashed once, not again as an output file.
    #[cfg(unix)]
    #[test]
    fn a_file_under_a_source_and_an_output_root_is_hashed_once() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let write = |rel: &str, body: &[u8]| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
            p
        };
        let cam = write("camera/b.jpg", b"placed before");
        write("out/2026/b.jpg", b"placed before");
        std::os::unix::fs::symlink(root.join("out"), root.join("link")).unwrap();
        let r = root.display();
        let text = format!(
            r#"
[[sources]]
id = "camera"
path = "{r}/camera"

[[sources]]
id = "outsrc"
path = "{r}/link"

[[targets]]
name = "Duplicates"
kind = "trash"
[[targets.filters]]
sources = ["camera", "outsrc"]
duplicate = {{ of = ["sources", "outputs"] }}

[[targets]]
name = "Album"
[[targets.outputs]]
dir = "{r}/out/{{YYYY}}"
"#
        );
        let cfg: Config = toml::from_str(&text).unwrap();
        let files = vec![
            entry("camera", &cam),
            entry("outsrc", &root.join("link/2026/b.jpg")),
        ];
        let facts = vec![fact(None, None); 2];
        let cache = tempfile::tempdir().unwrap();
        let g = compute_with(
            &cfg,
            &files,
            &facts,
            &needs(&cfg),
            &no_thumbs,
            Some(cache.path()),
            &NoProgress,
            &Cancel::new(),
        )
        .unwrap();
        assert!(g.dup_in_outputs.contains(&0));
        assert!(!g.dup_in_outputs.contains(&1));
        let cached: HashMap<String, image_metrics::Metric> =
            serde_json::from_slice(&std::fs::read(cache.path().join("metrics.json")).unwrap())
                .unwrap();
        let mut hashed: Vec<_> = cached
            .values()
            .filter(|m| m.blake3.is_some())
            .filter_map(|m| m.path.clone())
            .collect();
        hashed.sort();
        assert_eq!(hashed, vec![cam, root.join("link/2026/b.jpg")]);
    }

    #[test]
    fn stale_cached_hash_is_not_a_duplicate() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let write = |rel: &str, body: &[u8]| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
            p
        };
        // Same size, different bytes; the cache claims they are equal.
        let cam = write("camera/a.jpg", b"picture one");
        let olga = write("olga/a.jpg", b"picture two");
        let out = write("out/2026/a.jpg", b"picture 3!!");
        let outdir = root.join("out").display().to_string().replace('\\', "/");
        let text = format!(
            r#"
[[sources]]
id = "camera"
path = "{r}/camera"

[[sources]]
id = "olga"
path = "{r}/olga"

[[targets]]
name = "Duplicates"
kind = "trash"
[[targets.filters]]
sources = ["camera", "olga"]
duplicate = {{ of = ["sources", "outputs"] }}

[[targets]]
name = "Album"
[[targets.outputs]]
dir = "{outdir}/{{YYYY}}"
"#,
            r = root.display().to_string().replace('\\', "/"),
        );
        let cfg: Config = toml::from_str(&text).unwrap();
        let files = vec![entry("camera", &cam), entry("olga", &olga)];
        let facts = vec![fact(None, None); 2];
        let cache = tempfile::tempdir().unwrap();
        let forge = |paths: &[&PathBuf]| {
            let forged = MetricsCache::load(cache.path());
            for p in paths {
                let m = std::fs::metadata(p).unwrap();
                let mtime_ns = crate::scan::mtime_ns(&m).unwrap();
                forged.update(p, m.len(), mtime_ns, |m| m.blake3 = Some("stale".into()));
            }
            forged.save().unwrap();
        };
        let groups = |files: &[FileEntry]| {
            compute_with(
                &cfg,
                files,
                &facts,
                &needs(&cfg),
                &no_thumbs,
                Some(cache.path()),
                &NoProgress,
                &Cancel::new(),
            )
            .unwrap()
        };
        forge(&[&cam, &olga, &out]);
        let g = groups(&files);
        assert!(g.dup_original.is_empty());
        assert!(g.dup_in_outputs.is_empty());
        assert!(g.same_content.is_empty());
        let m = holds(&cfg, 1, &facts, &g).unwrap_err();
        assert_eq!(m.condition, "duplicate");

        // Real copies under the same forged hash still count.
        std::fs::write(&olga, b"picture one").unwrap();
        std::fs::write(&out, b"picture one").unwrap();
        forge(&[&olga, &out]);
        let files = vec![entry("camera", &cam), entry("olga", &olga)];
        let g = groups(&files);
        assert_eq!(g.dup_original.get(&1), Some(&0));
        assert!(g.dup_in_outputs.contains(&0));
        assert!(holds(&cfg, 1, &facts, &g).is_ok());
    }

    #[test]
    fn oldest_capture_is_the_original_within_one_source() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.jpg");
        let b = tmp.path().join("b.jpg");
        std::fs::write(&a, b"same picture").unwrap();
        std::fs::write(&b, b"same picture").unwrap();
        let cfg = config(
            r#"
[[targets]]
name = "Duplicates"
kind = "trash"
[[targets.filters]]
sources = ["camera"]
duplicate = {}
"#,
        );
        let n = needs(&cfg);
        assert!(n.exif_all, "the capture date ranks the original");
        assert_eq!(n.duplicates, vec![DupScope::Sources]);
        let files = vec![entry("camera", &a), entry("camera", &b)];
        // b.jpg is later by path but older by capture: it is the original.
        let facts = vec![
            fact(Some("2026-08-02 10:00:00"), None),
            fact(Some("2026-08-01 10:00:00"), None),
        ];
        let g = run(&cfg, &files, &facts, &no_thumbs);
        assert_eq!(g.dup_original.get(&0), Some(&1));
        assert!(!g.dup_original.contains_key(&1));
        assert!(holds(&cfg, 0, &facts, &g).is_ok());
        assert!(holds(&cfg, 1, &facts, &g).is_err());
        // Without capture dates the path decides.
        let g = run(
            &cfg,
            &files,
            &[fact(None, None), fact(None, None)],
            &no_thumbs,
        );
        assert_eq!(g.dup_original.get(&1), Some(&0));
    }

    #[test]
    fn output_root_cuts_before_the_template() {
        let p = |s: &str| output_root(s).map(|p| p.display().to_string().replace('\\', "/"));
        assert_eq!(p("/a/b/{YYYY}/x").as_deref(), Some("/a/b/"));
        assert_eq!(p("/a/b/IMG_{YYYY}").as_deref(), Some("/a/b"));
        assert_eq!(p("/a/b").as_deref(), Some("/a/b"));
        assert_eq!(p("{source}/x"), None);
    }

    const SERIES: &str = r#"
[[targets]]
name = "Blurry"
kind = "trash"
[[targets.filters]]
sources = ["camera"]
series = {}
"#;

    #[test]
    fn series_of_similar_shots_keeps_the_sharpest() {
        let tmp = tempfile::tempdir().unwrap();
        let shot = |n: &str, img: &image::GrayImage| {
            let p = tmp.path().join(n);
            save(img, &p);
            entry("camera", &p)
        };
        let files = vec![
            shot("1.jpg", &blurred(512)),
            shot("2.jpg", &sharp(512)),
            shot("3.jpg", &blurred(512)),
            shot("4.jpg", &other(512)),
            shot("5.jpg", &sharp(512)),
            shot("6.jpg", &sharp(512)),
        ];
        let mut facts = vec![
            fact(Some("2026-08-01 12:00:00"), None),
            fact(Some("2026-08-01 12:00:02"), None),
            fact(Some("2026-08-01 12:00:04"), None),
            fact(Some("2026-08-01 12:00:06"), None), // different picture
            fact(Some("2026-08-01 12:00:36"), None), // 30 s later
            fact(Some("2026-08-01 12:00:37"), None),
        ];
        facts[5].capture = None; // no date: never in a series
        let cfg = config(SERIES);
        let identity = |p: &Path| Ok(p.to_path_buf());
        let g = run(&cfg, &files, &facts, &identity);
        assert_eq!(g.series.len(), 1);
        assert_eq!(g.series[0].members, vec![0, 1, 2]);
        assert_eq!(g.series[0].best, 1);
        assert!(g.sharpness(1).unwrap() > g.sharpness(0).unwrap() + 20.0);
        for i in [3, 4, 5] {
            assert!(g.series_of(i).is_none(), "{i}");
        }
        assert!(holds(&cfg, 0, &facts, &g).is_ok());
        assert!(holds(&cfg, 2, &facts, &g).is_ok());
        let best = holds(&cfg, 1, &facts, &g).unwrap_err();
        assert_eq!(best.detail, crate::t!("filter.miss.series_best"));
        assert!(holds(&cfg, 4, &facts, &g).is_err());

        // Without `similar` the different picture joins the series.
        let loose = config(&SERIES.replace("series = {}", "series = { similar = false }"));
        let g = run(&loose, &files, &facts, &identity);
        assert_eq!(g.series[0].members, vec![0, 1, 2, 3]);
        // By size: no thumbnails needed at all.
        let by_size = config(&SERIES.replace(
            "series = {}",
            r#"series = { similar = false, by = "size" }"#,
        ));
        let g = run(&by_size, &files, &facts, &no_thumbs);
        assert_eq!(g.series[0].members, vec![0, 1, 2, 3]);
        assert!(g.sharpness.is_empty());
    }

    #[test]
    fn series_without_thumbnails_is_no_series() {
        let files = vec![virt("camera", "1.jpg"), virt("camera", "2.jpg")];
        let facts = vec![
            fact(Some("2026-08-01 12:00:00"), None),
            fact(Some("2026-08-01 12:00:01"), None),
        ];
        let cfg = config(SERIES);
        let fail = |p: &Path| Err(crate::Error::Tool(p.display().to_string()));
        let g = run(&cfg, &files, &facts, &fail);
        assert!(g.series.is_empty());
    }

    #[test]
    fn props_sharpness_is_measured_for_photos_only() {
        let tmp = tempfile::tempdir().unwrap();
        let photo = tmp.path().join("a.jpg");
        save(&sharp(256), &photo);
        let files = vec![
            entry("camera", &photo),
            virt("camera", "clip.mp4"),
            virt("olga", "b.jpg"),
        ];
        let facts = vec![fact(None, None); 3];
        let cfg = config(
            r#"
[[targets]]
name = "Blurry"
[[targets.filters]]
sources = ["camera"]
props = { sharpness = "<40" }
"#,
        );
        let n = needs(&cfg);
        assert!(n.sharpness && n.exif_all && !n.series);
        let identity = |p: &Path| {
            assert!(p.extension().is_some_and(|e| e == "jpg"), "{}", p.display());
            assert!(!p.starts_with("/nonexistent/olga"));
            Ok(p.to_path_buf())
        };
        let g = run(&cfg, &files, &facts, &identity);
        assert!(g.sharpness(0).is_some());
        assert_eq!(g.sharpness.len(), 1);
    }

    #[test]
    fn nothing_configured_computes_nothing() {
        let cfg = config(
            r#"
[[targets]]
name = "Pictures"
[[targets.filters]]
sources = ["camera"]
match = "*.jpg"
"#,
        );
        let n = needs(&cfg);
        assert!(n.is_empty() && !n.exif_all);
        let files = vec![virt("camera", "a.jpg"), virt("camera", "b.jpg")];
        let facts = vec![fact(Some("2026-08-01 12:00:00"), None); 2];
        let cache = tempfile::tempdir().unwrap();
        let g = compute_with(
            &cfg,
            &files,
            &facts,
            &n,
            &no_thumbs,
            Some(cache.path()),
            &NoProgress,
            &Cancel::new(),
        )
        .unwrap();
        assert!(g.events.is_empty() && g.series.is_empty() && g.dup_original.is_empty());
        assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 0);
    }

    #[test]
    fn cancelled_before_start() {
        let cfg = config(SERIES);
        let c = Cancel::new();
        c.cancel();
        let r = compute_with(
            &cfg,
            &[],
            &[],
            &needs(&cfg),
            &no_thumbs,
            Some(Path::new("/nonexistent")),
            &NoProgress,
            &c,
        );
        assert!(matches!(r, Err(crate::Error::Cancelled)));
    }

    #[test]
    fn conditions_round_trip() {
        let text = r#"sources = ["camera"]
expand = "event"

[event]
sources = ["camera", "olga"]
gap = "6h"
km = 30.0
min_files = 10
min_duration = "1d"
away_from = "@home"

[near]
sources = ["olga"]
time = "2h"
km = 1.0
min = 2
require_gps = true
not = true

[duplicate]
of = ["outputs"]

[series]
gap = "3s"
similar = false
by = "resolution"
"#;
        let f: Filter = toml::from_str(text).unwrap();
        assert_eq!(toml::to_string(&f).unwrap(), text);
        let key = EventKey::of(&f).unwrap();
        assert_eq!(key.sources, vec!["camera", "olga"]);
        assert_eq!((key.gap_s, key.km_milli), (6 * 3600, Some(30_000)));
        let plain: Filter = toml::from_str("sources = [\"b\", \"a\"]\nexpand = \"event\"").unwrap();
        let key = EventKey::of(&plain).unwrap();
        assert_eq!(
            (key.sources, key.gap_s),
            (vec!["a".into(), "b".into()], 4 * 3600)
        );
        let d: Filter = toml::from_str("duplicate = {}").unwrap();
        assert_eq!(d.duplicate.unwrap().of, vec![DupScope::Sources]);
        assert!(toml::from_str::<Filter>("near = { sources = [\"o\"], tme = \"2h\" }").is_err());
    }

    #[test]
    fn load_checks() {
        let base = |targets: &str| {
            let text = format!(
                r#"
[places]
home = {{ lat = 47.0, lon = 28.8, km = 2 }}
[[sources]]
id = "camera"
path = "/nonexistent/camera"
{targets}
"#
            );
            toml::from_str::<Config>(&text).unwrap().validate()
        };
        let err = |t: &str| base(t).unwrap_err().to_string();
        let one = |cond: &str| {
            format!(
                "[[targets]]\nname = \"T\"\n[[targets.filters]]\nsources = [\"camera\"]\n{cond}\n"
            )
        };
        assert!(base(&one(r#"event = { away_from = "@home" }"#)).is_ok());
        assert!(err(&one(r#"event = { away_from = "@nowhere" }"#)).contains("nowhere"));
        assert!(err(&one(r#"event = { sources = ["ghost"] }"#)).contains("ghost"));
        assert!(err(&one("near = { time = \"1h\" }")).contains("near"));
        let two = format!(
            "{}[[targets]]\nname = \"U\"\n[[targets.filters]]\nsources = [\"camera\"]\nseries = {{ gap = \"9s\" }}\n",
            one("series = {}")
        );
        assert!(err(&two).contains("'U'"));
    }

    #[test]
    fn series_through_real_thumbnails() {
        let Some(magick) = crate::tools::find("magick") else {
            eprintln!("skip: magick not found");
            return;
        };
        let bogus = PathBuf::from("/nonexistent/tool");
        let tools = Tools {
            ffmpeg: bogus.clone(),
            ffprobe: bogus.clone(),
            exiftool: bogus.clone(),
            avifenc: bogus,
            magick,
            optipng: None,
            tz: None,
        };
        let tmp = tempfile::tempdir().unwrap();
        let shot = |n: &str, img: &image::GrayImage| {
            let p = tmp.path().join(n);
            save(img, &p);
            entry("camera", &p)
        };
        let files = vec![shot("1.jpg", &sharp(1024)), shot("2.jpg", &blurred(1024))];
        let facts = vec![
            fact(Some("2026-08-01 12:00:00"), None),
            fact(Some("2026-08-01 12:00:02"), None),
        ];
        let cfg = config(SERIES);
        let cache = tempfile::tempdir().unwrap();
        let g = compute(
            &cfg,
            &files,
            &facts,
            &needs(&cfg),
            Some(&tools),
            Some(cache.path()),
            &NoProgress,
            &Cancel::new(),
        )
        .unwrap();
        assert_eq!(g.series.len(), 1);
        assert_eq!(g.series[0].best, 0);
        assert!(cache.path().join("thumbs").is_dir());
    }
}
