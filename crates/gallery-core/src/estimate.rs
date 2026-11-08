//! Estimate of a run before it starts: how much space it saves and how long
//! it takes, learned from the reports of past runs (their sizes and the
//! per-item time), with rough built-in defaults when there is no history;
//! plus the cost of an AI scan. Reads only; every figure is a range
//! (p25 / median / p75), never a fake-precise number.
//!
//! Model. A sample is one written output of a finished item: its class (the
//! content presets of the output, e.g. `photo-max`, which fixes kind and
//! level), the source's extension, source bytes, output bytes and the item's
//! wall time. Output size is `source size x ratio`, with the ratio taken
//! from the (class, extension) cell, else from the class, else from the
//! defaults of the kind. Time is `source bytes / throughput` per item (one
//! worker), divided by `jobs`; video is estimated by size too, not by its
//! duration (the plan holds no durations and reading them would need
//! ffprobe on every file).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ai::Usage;
use crate::ai::usage_log::UsageRow;
use crate::apply::{ItemResult, ItemStatus, Report};
use crate::config::ContentPreset;
use crate::pipeline::AiDry;
use crate::plan::{OutputOp, Plan, PlanItem};

/// A cell needs this many samples before its figures are trusted.
const MIN_SAMPLES: usize = 3;
/// Reports read by [`History::load_dir`], newest first.
const MAX_REPORTS: usize = 40;

/// What kind of work an output is: it picks the defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Photo,
    Png,
    Video,
    Audio,
    Metadata,
    /// No content step: a byte copy.
    Copy,
}

/// Low / median / high of a figure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Range<T> {
    pub low: T,
    pub mid: T,
    pub high: T,
}

/// One written output of a finished item.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub class: String,
    pub kind: Kind,
    /// Lower-case extension of the source.
    pub ext: String,
    pub input: u64,
    pub output: u64,
    /// Wall time of the output (the item's time split among its outputs).
    pub elapsed_ms: Option<u64>,
}

/// What past runs taught.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct History {
    pub samples: Vec<Sample>,
}

fn ext_of(p: &Path) -> String {
    p.extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_lowercase()
}

/// Class and kind of a write output with these content steps.
fn class_of(plan: &Plan, steps: &[String]) -> (String, Kind) {
    if steps.is_empty() {
        return ("copy".into(), Kind::Copy);
    }
    let rank = |k: Kind| match k {
        Kind::Video => 5,
        Kind::Photo => 4,
        Kind::Png => 3,
        Kind::Audio => 2,
        Kind::Metadata => 1,
        Kind::Copy => 0,
    };
    let kind = steps
        .iter()
        .filter_map(|s| match plan.content_presets.get(s)? {
            ContentPreset::Photo(_) => Some(Kind::Photo),
            ContentPreset::Png(_) => Some(Kind::Png),
            ContentPreset::Video(_) => Some(Kind::Video),
            ContentPreset::Audio(_) => Some(Kind::Audio),
            ContentPreset::Metadata(_) => Some(Kind::Metadata),
            ContentPreset::Bundle(_) => None,
        })
        .max_by_key(|k| rank(*k))
        .unwrap_or(Kind::Metadata);
    (steps.join("+"), kind)
}

impl History {
    /// Samples of one run: the report says what came out and how long it
    /// took, the plan says which presets made it and how big the source was.
    pub fn add_report(&mut self, plan: &Plan, report: &Report) {
        let by_source: HashMap<&Path, &PlanItem> =
            plan.items.iter().map(|i| (i.source.as_path(), i)).collect();
        for res in &report.items {
            let (ItemStatus::Done { .. }, Some(item)) =
                (&res.status, by_source.get(res.source.as_path()))
            else {
                continue;
            };
            self.add_item(plan, item, res);
        }
    }

    fn add_item(&mut self, plan: &Plan, item: &PlanItem, res: &ItemResult) {
        if res.outputs.len() != item.outputs.len() {
            return;
        }
        let writes = item
            .outputs
            .iter()
            .filter(|o| o.op == OutputOp::Write)
            .count() as u64;
        for (planned, out) in item.outputs.iter().zip(&res.outputs) {
            let usable = planned.op == OutputOp::Write
                && out.planned == planned.dest
                && out.error.is_none()
                && !out.fell_back
                && out.actual.is_some();
            let (true, Some(size)) = (usable, out.size) else {
                continue;
            };
            let (class, kind) = class_of(plan, &planned.steps);
            self.samples.push(Sample {
                class,
                kind,
                ext: ext_of(&item.source),
                input: item.size,
                output: size,
                elapsed_ms: res.elapsed_ms.map(|ms| ms / writes.max(1)),
            });
        }
    }

    /// Where the GUI keeps its plans and reports.
    pub fn gui_dir() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join("gallery").join("plans"))
    }

    /// [`History::load_dir`] of several folders, merged.
    pub fn load_dirs(dirs: &[&Path]) -> History {
        let mut h = History::default();
        for d in dirs {
            h.samples.extend(History::load_dir(d).samples);
        }
        h
    }

    /// The newest reports in `dir` (the GUI plans folder: `*.report.json`
    /// and archived `*.report.<id>.json`), each with the plan it ran. A
    /// report or plan that cannot be read is skipped.
    pub fn load_dir(dir: &Path) -> History {
        let mut reports: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.contains(".report") && n.ends_with(".json"))
            })
            .filter_map(|p| Some((p.metadata().ok()?.modified().ok()?, p)))
            .collect();
        reports.sort_by_key(|a| std::cmp::Reverse(a.0));
        let mut h = History::default();
        for (_, path) in reports.into_iter().take(MAX_REPORTS) {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(report) = serde_json::from_str::<Report>(&text) else {
                continue;
            };
            let sibling = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.split(".report").next())
                .map(|stem| dir.join(format!("{stem}.plan.json")));
            let plan = [Some(report.plan.clone()), sibling]
                .into_iter()
                .flatten()
                .filter(|p| p.is_file())
                .find_map(|p| Plan::load(&p).ok());
            if let Some(plan) = plan {
                h.add_report(&plan, &report);
            }
        }
        h
    }
}

/// Where the figures came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EstimateSource {
    /// Past runs gave `samples` files; `guessed_files` of the plan's files
    /// fell back to the built-in defaults.
    History {
        samples: usize,
        guessed_files: usize,
    },
    /// No usable history: rough built-in defaults.
    Defaults,
}

impl EstimateSource {
    /// The line the UI and CLI show.
    pub fn label(&self) -> String {
        match self {
            EstimateSource::Defaults => "rough estimate".into(),
            EstimateSource::History {
                samples,
                guessed_files: 0,
            } => format!("from past runs ({samples} files)"),
            EstimateSource::History {
                samples,
                guessed_files,
            } => format!(
                "from past runs ({samples} files); for {guessed_files} files — rough estimate"
            ),
        }
    }
}

/// Figures of one target, or of the whole plan.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TargetEstimate {
    pub name: String,
    pub files: usize,
    pub input_bytes: u64,
    /// Bytes that exist afterwards (new files, plus sources that stay).
    pub output_bytes: Range<u64>,
    /// Input minus output; negative when a run adds more than it frees.
    pub saving_bytes: Range<i64>,
    pub seconds: Range<f64>,
    /// Sources that go to the trash.
    pub trash: usize,
    /// Sources renamed or moved in place.
    pub renamed: usize,
    /// Sources an output keeps as they are.
    pub kept: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Estimate {
    pub targets: Vec<TargetEstimate>,
    pub total: TargetEstimate,
    pub jobs: usize,
    pub source: EstimateSource,
    pub source_label: String,
}

/// Rough figures per kind when nothing was learned: output / input ratio
/// and per-item throughput in bytes per second, as (p25, p50, p75).
/// Conservative: little saving, slow work.
fn defaults(kind: Kind) -> ([f64; 3], [f64; 3]) {
    const MB: f64 = 1_000_000.0;
    match kind {
        Kind::Photo => ([0.40, 0.60, 0.85], [0.5 * MB, 1.0 * MB, 2.0 * MB]),
        Kind::Png => ([0.15, 0.30, 0.50], [0.3 * MB, 0.6 * MB, 1.2 * MB]),
        Kind::Video => ([0.30, 0.50, 0.80], [0.2 * MB, 0.5 * MB, 1.5 * MB]),
        Kind::Audio => ([0.30, 0.40, 0.60], [1.0 * MB, 3.0 * MB, 6.0 * MB]),
        Kind::Metadata | Kind::Copy => ([1.0, 1.0, 1.0], [20.0 * MB, 50.0 * MB, 100.0 * MB]),
    }
}

/// Linear-interpolated percentile `q` (0..=1) of an unsorted, non-empty list.
fn percentile(v: &[f64], q: f64) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let pos = q * (s.len() - 1) as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    s[lo] + (s[hi] - s[lo]) * (pos - lo as f64)
}

fn quartiles(v: &[f64]) -> [f64; 3] {
    [percentile(v, 0.25), percentile(v, 0.5), percentile(v, 0.75)]
}

/// Ratios and throughputs of one cell.
#[derive(Default)]
struct Cell {
    ratios: Vec<f64>,
    tputs: Vec<f64>,
}

struct Model {
    by_ext: HashMap<(String, String), Cell>,
    by_class: HashMap<String, Cell>,
}

impl Model {
    fn new(h: &History) -> Self {
        let mut m = Model {
            by_ext: HashMap::new(),
            by_class: HashMap::new(),
        };
        for s in &h.samples {
            let ratio = (s.input > 0).then(|| s.output as f64 / s.input as f64);
            let tput = s
                .elapsed_ms
                .filter(|&ms| ms > 0 && s.input > 0)
                .map(|ms| s.input as f64 / (ms as f64 / 1000.0));
            for cell in [
                m.by_ext
                    .entry((s.class.clone(), s.ext.clone()))
                    .or_default(),
                m.by_class.entry(s.class.clone()).or_default(),
            ] {
                cell.ratios.extend(ratio);
                cell.tputs.extend(tput);
            }
        }
        m
    }

    /// Ratio quartiles and the cell they come from (`None`: defaults).
    fn ratio(&self, class: &str, ext: &str) -> Option<([f64; 3], String, usize)> {
        let by_ext = self.by_ext.get(&(class.to_string(), ext.to_string()));
        let cell = by_ext
            .filter(|c| c.ratios.len() >= MIN_SAMPLES)
            .map(|c| (c, format!("{class}/{ext}")))
            .or_else(|| {
                self.by_class
                    .get(class)
                    .filter(|c| c.ratios.len() >= MIN_SAMPLES)
                    .map(|c| (c, class.to_string()))
            })?;
        Some((quartiles(&cell.0.ratios), cell.1, cell.0.ratios.len()))
    }

    fn tput(&self, class: &str, ext: &str) -> Option<[f64; 3]> {
        let by_ext = self.by_ext.get(&(class.to_string(), ext.to_string()));
        by_ext
            .filter(|c| c.tputs.len() >= MIN_SAMPLES)
            .or_else(|| {
                self.by_class
                    .get(class)
                    .filter(|c| c.tputs.len() >= MIN_SAMPLES)
            })
            .map(|c| quartiles(&c.tputs))
    }
}

/// A duration for people: `45 s`, `12 min`, `2 h 05 min`.
pub fn duration_text(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    match s {
        0..=59 => format!("{s} s"),
        60..=3599 => format!("{} min", (s + 30) / 60),
        _ => format!("{} h {:02} min", s / 3600, (s % 3600) / 60),
    }
}

/// Default parallelism of a run (the same as `ApplyOptions::default`).
pub fn default_jobs() -> usize {
    crate::apply::ApplyOptions::default().jobs
}

/// [`estimate_with`] at the default `jobs`.
pub fn estimate(plan: &Plan, history: &History) -> Estimate {
    estimate_with(plan, history, default_jobs())
}

/// Estimate a run of `plan` with `jobs` items in parallel.
pub fn estimate_with(plan: &Plan, history: &History, jobs: usize) -> Estimate {
    let jobs = jobs.max(1);
    let model = Model::new(history);
    let mut targets: BTreeMap<String, TargetEstimate> = BTreeMap::new();
    let mut cells: BTreeMap<String, usize> = BTreeMap::new();
    let mut guessed = BTreeSet::new();

    for item in &plan.items {
        let t = targets
            .entry(item.target.clone())
            .or_insert_with(|| TargetEstimate {
                name: item.target.clone(),
                ..Default::default()
            });
        let renamed = item.outputs.iter().any(|o| o.op == OutputOp::Rename);
        let kept = !renamed && item.outputs.iter().any(|o| o.op == OutputOp::Keep);
        let consumes = renamed || kept;
        t.files += 1;
        t.input_bytes += item.size;
        t.trash += usize::from(!consumes);
        t.renamed += usize::from(renamed);
        t.kept += usize::from(kept);

        let ext = ext_of(&item.source);
        let size = item.size as f64;
        let (mut out, mut secs) = ([0.0; 3], [0.0; 3]);
        if consumes {
            out = [size; 3];
        }
        for o in item.outputs.iter().filter(|o| o.op == OutputOp::Write) {
            let (class, kind) = class_of(plan, &o.steps);
            let (dr, dt) = defaults(kind);
            let ratio = match model.ratio(&class, &ext) {
                Some((q, cell, n)) => {
                    cells.insert(cell, n);
                    q
                }
                None => {
                    guessed.insert(item.source.clone());
                    dr
                }
            };
            let tput = model.tput(&class, &ext).unwrap_or_else(|| {
                guessed.insert(item.source.clone());
                dt
            });
            for i in 0..3 {
                out[i] += size * ratio[i];
                // The slowest quartile of throughput is the longest time.
                secs[2 - i] += size / tput[i].max(1.0) / jobs as f64;
            }
        }
        let bytes = |v: f64| v.round() as u64;
        t.output_bytes.low += bytes(out[0]);
        t.output_bytes.mid += bytes(out[1]);
        t.output_bytes.high += bytes(out[2]);
        t.seconds.low += secs[0];
        t.seconds.mid += secs[1];
        t.seconds.high += secs[2];
    }

    let mut total = TargetEstimate::default();
    for t in targets.values_mut() {
        let inb = t.input_bytes as i64;
        t.saving_bytes = Range {
            low: inb - t.output_bytes.high as i64,
            mid: inb - t.output_bytes.mid as i64,
            high: inb - t.output_bytes.low as i64,
        };
        total.files += t.files;
        total.input_bytes += t.input_bytes;
        total.trash += t.trash;
        total.renamed += t.renamed;
        total.kept += t.kept;
        for (a, b) in [
            (&mut total.output_bytes.low, t.output_bytes.low),
            (&mut total.output_bytes.mid, t.output_bytes.mid),
            (&mut total.output_bytes.high, t.output_bytes.high),
        ] {
            *a += b;
        }
        for (a, b) in [
            (&mut total.saving_bytes.low, t.saving_bytes.low),
            (&mut total.saving_bytes.mid, t.saving_bytes.mid),
            (&mut total.saving_bytes.high, t.saving_bytes.high),
        ] {
            *a += b;
        }
        for (a, b) in [
            (&mut total.seconds.low, t.seconds.low),
            (&mut total.seconds.mid, t.seconds.mid),
            (&mut total.seconds.high, t.seconds.high),
        ] {
            *a += b;
        }
    }
    let samples: usize = cells.values().sum();
    let source = if samples == 0 {
        EstimateSource::Defaults
    } else {
        EstimateSource::History {
            samples,
            guessed_files: guessed.len(),
        }
    };
    Estimate {
        targets: targets.into_values().collect(),
        total,
        jobs,
        source_label: source.label(),
        source,
    }
}

impl TargetEstimate {
    /// `12 min (8 min – 19 min)`.
    pub fn time_text(&self) -> String {
        format!(
            "{} ({} – {})",
            duration_text(self.seconds.mid),
            duration_text(self.seconds.low),
            duration_text(self.seconds.high)
        )
    }
}

/// Cost of an AI scan before it starts.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AiEstimate {
    pub candidates: usize,
    pub cached: usize,
    pub to_send: usize,
    pub model: String,
    /// USD; `None` for a model without a known price.
    pub usd: Option<Range<f64>>,
    pub source_label: String,
    /// The line the UI and CLI show.
    pub label: String,
    /// The scan costs more than [`AI_CONFIRM_USD`]: ask before starting it.
    pub confirm: bool,
}

/// A scan estimated above this (USD) asks for confirmation.
pub const AI_CONFIRM_USD: f64 = 1.0;

/// Typical request when there is no past usage: one thumbnail and a short
/// answer, the system prompt read from the prompt cache.
const DEFAULT_REQUEST: Usage = Usage {
    requests: 1,
    input: 1500,
    output: 80,
    cache_write: 0,
    cache_read: 700,
};

/// Cost of the files of `dry` that would be sent, at `model`'s price and
/// the average request of the past scans in `rows` (those of `model`).
pub fn ai_estimate(dry: &AiDry, model: &str, rows: &[UsageRow]) -> AiEstimate {
    let past: Vec<&UsageRow> = rows
        .iter()
        .filter(|r| r.model == model && r.requests > 0)
        .collect();
    let per_row: Vec<f64> = past
        .iter()
        .filter_map(|r| Some(r.usage().cost(model)? / r.requests as f64))
        .collect();
    let requests: u64 = past.iter().map(|r| r.requests).sum();
    let (per_request, source_label) = if per_row.is_empty() {
        let mid = DEFAULT_REQUEST.cost(model).map(|c| [c * 0.7, c, c * 1.5]);
        (mid, "rough estimate".to_string())
    } else {
        let total: f64 = past
            .iter()
            .filter_map(|r| r.usage().cost(model))
            .sum::<f64>();
        let mid = total / requests as f64;
        let q = if per_row.len() >= 4 {
            quartiles(&per_row)
        } else {
            [mid * 0.8, mid, mid * 1.25]
        };
        (Some(q), format!("from past scans ({requests} requests)"))
    };
    let n = dry.to_send as f64;
    let usd = per_request.map(|q| Range {
        low: q[0] * n,
        mid: q[1] * n,
        high: q[2] * n,
    });
    let label = match &usd {
        Some(r) => format!(
            "≈ ${:.2} ({} files, {} cached)",
            r.mid, dry.to_send, dry.cached
        ),
        None => format!(
            "model price unknown ({} files, {} cached)",
            dry.to_send, dry.cached
        ),
    };
    AiEstimate {
        confirm: usd.is_some_and(|r| r.mid > AI_CONFIRM_USD),
        candidates: dry.candidates,
        cached: dry.cached,
        to_send: dry.to_send,
        model: model.to_string(),
        usd,
        source_label,
        label,
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod text_tests {
    use super::duration_text;

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration_text(0.4), "0 s");
        assert_eq!(duration_text(45.0), "45 s");
        assert_eq!(duration_text(700.0), "12 min");
        assert_eq!(duration_text(7500.0), "2 h 05 min");
    }
}
