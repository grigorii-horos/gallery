//! Routing over the ordered target list. Every file walks the targets in
//! order and the first target one of whose filters matches takes it. A file
//! that reaches a filter with `ai` (its other conditions holding) without
//! an AI answer pauses there; the paused files go to the AI as one batch,
//! then their walks resume with the answers.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{Config, Expand, Filter, Target};
use crate::filter::facts::check_file_scored;
use crate::filter::groups::check_groups_with;
use crate::filter::{EventKey, Facts, Groups, Miss, facts_of};
use crate::mask::MaskExpr;
use crate::overrides::{Action, Overrides};
use crate::progress::{Cancel, NoProgress, Progress, ProgressEvent, Stage};
use crate::scan::FileEntry;
use crate::tools::Tools;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    Ok,
    Low,
}

/// Texts of a file's source that frame the category list in the AI system
/// prompt (`ai_prefix` / `ai_suffix` of the source).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourcePrompt {
    pub prefix: Option<String>,
    pub suffix: Option<String>,
    /// Prompt lines of the user's corrections (see [`crate::learn`]), shown
    /// after the suffix.
    pub examples: Vec<String>,
    /// Language the model writes `reason` in (`[ai] language`); blank is the interface language.
    pub language: String,
}

/// One file to classify with its candidate categories (target, description)
/// and its source's prompt texts.
#[derive(Debug, Clone)]
pub struct AiRequest {
    pub entry: FileEntry,
    pub categories: Vec<(String, String)>,
    pub prompt: SourcePrompt,
}

#[derive(Debug, Clone)]
pub struct AiAnswer {
    /// `None` means "none of the categories".
    pub category: Option<String>,
    pub reason: String,
    pub confidence: Confidence,
}

pub trait Classifier {
    /// Answer every request, in the same order.
    fn classify(&self, batch: Vec<AiRequest>) -> Result<Vec<AiAnswer>>;

    /// [`Classifier::classify`] with [`Stage::Ai`] events (`done` of the
    /// batch answered) and cancellation (`Error::Cancelled`). By default the
    /// whole batch is one step.
    fn classify_with(
        &self,
        batch: Vec<AiRequest>,
        progress: &dyn Progress,
        cancel: &Cancel,
    ) -> Result<Vec<AiAnswer>> {
        cancel.check()?;
        let total = batch.len();
        let answers = self.classify(batch)?;
        progress.step(ProgressEvent {
            stage: Stage::Ai,
            done: total,
            total,
            current: None,
        });
        Ok(answers)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Via {
    Mask,
    Ai {
        reason: String,
        /// A low-confidence answer accepted with `accept_low`.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        low: bool,
    },
    /// A manual decision (see [`crate::overrides`]).
    Manual,
}

/// The series a file held for review belongs to: every shot, the suggested
/// best one among them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeriesReview {
    /// Source paths in capture order.
    pub members: Vec<PathBuf>,
    pub best: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The target takes the file, whatever its kind (the plan reads the
    /// kind: a `keep` target leaves the file in place, `trash` trashes it).
    Target {
        name: String,
        via: Via,
    },
    Review {
        suggested: String,
        reason: String,
        /// Held because a `series` condition matched: the whole series.
        series: Option<SeriesReview>,
    },
    /// Nothing took the file, the AI gave no verdict on it, and an exclude
    /// mask rejected it from a filter (or AI prefilter) that would otherwise
    /// have matched: left alone on purpose, so it is not reported as unmatched.
    Excluded,
    /// A manual decision leaves the file in its source, untouched.
    Kept,
    Unmatched,
}

/// Routing result with the manual decisions that could not be applied.
#[derive(Debug, Clone)]
pub struct Routed {
    pub decisions: Vec<(FileEntry, Decision)>,
    /// Human-readable notes, e.g. a manual decision naming an unknown target
    /// (such a decision is ignored and the file is routed as usual).
    pub warnings: Vec<String>,
    /// Files decided by a manual decision.
    pub overrides_applied: usize,
}

/// What the conditions know about the routed files: `facts[i]` belongs to
/// `files[i]`, `exif` is keyed by path, `groups` are computed over the same
/// list, and `now` is the plan zone's local time (for `older_than`).
#[derive(Debug, Clone)]
pub struct RouteCtx<'a> {
    pub facts: Cow<'a, [Facts]>,
    pub exif: Cow<'a, HashMap<PathBuf, Value>>,
    pub groups: Cow<'a, Groups>,
    pub now: NaiveDateTime,
}

/// The plan's zone (see [`crate::plan::local_zone`]).
fn plan_tz() -> Option<chrono_tz::Tz> {
    crate::plan::local_zone().and_then(|z| z.parse().ok())
}

fn now_in(tz: Option<chrono_tz::Tz>) -> NaiveDateTime {
    let now = chrono::Utc::now();
    match tz {
        Some(tz) => now.with_timezone(&tz).naive_local(),
        None => now.with_timezone(&chrono::Local).naive_local(),
    }
}

impl RouteCtx<'static> {
    /// For callers without metadata: facts from the file names and times
    /// only, no groups. Conditions on anything else do not hold.
    pub fn empty(files: &[FileEntry]) -> RouteCtx<'static> {
        let tz = plan_tz();
        RouteCtx {
            facts: Cow::Owned(
                files
                    .iter()
                    .map(|e| facts_of(e, &Value::Null, tz))
                    .collect(),
            ),
            exif: Cow::Owned(HashMap::new()),
            groups: Cow::Owned(Groups::default()),
            now: now_in(tz),
        }
    }
}

impl<'a> RouteCtx<'a> {
    /// Facts of every file from `exif` (in the plan zone), no groups:
    /// conditions on events, neighbours, duplicates, series or sharpness do
    /// not hold.
    pub fn without_groups(files: &[FileEntry], exif: &'a HashMap<PathBuf, Value>) -> RouteCtx<'a> {
        let tz = plan_tz();
        RouteCtx {
            facts: Cow::Owned(
                files
                    .iter()
                    .map(|e| facts_of(e, exif.get(&e.path).unwrap_or(&NO_EXIF), tz))
                    .collect(),
            ),
            exif: Cow::Borrowed(exif),
            groups: Cow::Owned(Groups::default()),
            now: now_in(tz),
        }
    }

    /// Facts of every file from `exif` (in the plan zone) and the groups the
    /// config's conditions need, computed over `files`. Thumbnails are made
    /// with `tools` (`None`: none, so no sharpness and no similar series);
    /// scores and hashes are cached in the user cache dir (without one,
    /// nothing is written and no thumbnails are made).
    pub fn build(
        cfg: &Config,
        files: &[FileEntry],
        exif: &'a HashMap<PathBuf, Value>,
        tools: Option<&Tools>,
        progress: &dyn Progress,
        cancel: &Cancel,
    ) -> Result<RouteCtx<'a>> {
        let tz = plan_tz();
        let facts: Vec<Facts> = files
            .iter()
            .map(|e| facts_of(e, exif.get(&e.path).unwrap_or(&Value::Null), tz))
            .collect();
        let needs = crate::filter::needs(cfg);
        // No cache dir: computed without persisting anything.
        let cache = crate::paths::cache_dir();
        let groups = crate::filter::compute(
            cfg,
            files,
            &facts,
            &needs,
            tools,
            cache.as_deref(),
            progress,
            cancel,
        )?;
        Ok(RouteCtx {
            facts: Cow::Owned(facts),
            exif: Cow::Borrowed(exif),
            groups: Cow::Owned(groups),
            now: now_in(tz),
        })
    }
}

/// How a file fares against a name mask and exclude masks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    Match,
    /// The name matches, but an exclude mask rejects the file.
    Excluded,
    No,
}

pub(crate) fn name_fit(
    cfg: &Config,
    name: &Option<MaskExpr>,
    exclude: &MaskExpr,
    entry: &FileEntry,
) -> Fit {
    let file_name = file_name(entry);
    if !name.as_ref().is_none_or(|m| m.matches(file_name, cfg)) {
        Fit::No
    } else if exclude.matches_veto(file_name, cfg) {
        Fit::Excluded
    } else {
        Fit::Match
    }
}

fn file_name(entry: &FileEntry) -> &str {
    entry
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
}

/// How a file fares against a filter's sources and masks only (its other
/// conditions, e.g. `ai`, are not looked at).
pub(crate) fn filter_fit(cfg: &Config, f: &Filter, entry: &FileEntry) -> Fit {
    if !f.sources.contains(&entry.source_id) {
        return Fit::No;
    }
    name_fit(cfg, &f.matches, &f.exclude, entry)
}

/// Some filter would take `entry` but for an exclude mask.
fn excluded_somewhere(cfg: &Config, entry: &FileEntry) -> bool {
    cfg.targets
        .iter()
        .flat_map(|t| &t.filters)
        .any(|f| filter_fit(cfg, f, entry) == Fit::Excluded)
}

static NO_EXIF: Value = Value::Null;

/// The conditions of `f` besides `sources`, `match`, `exclude`, `ai` and
/// `expand`, for file `i` (`entry`). `best`: the user confirmed the file as
/// the best shot of its series.
fn conditions(
    cfg: &Config,
    f: &Filter,
    i: usize,
    entry: &FileEntry,
    ctx: &RouteCtx,
    best: bool,
) -> std::result::Result<(), Miss> {
    let exif = ctx.exif.get(&entry.path).unwrap_or(&NO_EXIF);
    let sharpness = ctx.groups.sharpness(i);
    check_file_scored(cfg, f, entry, &ctx.facts[i], exif, ctx.now, sharpness)?;
    check_groups_with(cfg, f, i, &ctx.facts, &ctx.groups, best)
}

/// Everything of `f` but `ai` and `expand` holds for file `i`.
fn holds(
    cfg: &Config,
    f: &Filter,
    i: usize,
    entry: &FileEntry,
    ctx: &RouteCtx,
    best: bool,
) -> bool {
    filter_fit(cfg, f, entry) == Fit::Match && conditions(cfg, f, i, entry, ctx, best).is_ok()
}

/// Why `f` does not take file `i`: its sources, masks or a condition.
fn filter_miss(
    cfg: &Config,
    f: &Filter,
    entry: &FileEntry,
    i: usize,
    ctx: &RouteCtx,
) -> std::result::Result<(), Miss> {
    if !f.sources.contains(&entry.source_id) {
        return Err(Miss {
            condition: "sources",
            detail: crate::t!("filter.miss.source", source = entry.source_id),
        });
    }
    match name_fit(cfg, &f.matches, &f.exclude, entry) {
        Fit::Match => {}
        Fit::No => {
            return Err(Miss {
                condition: "match",
                detail: crate::t!("filter.miss.name", name = file_name(entry)),
            });
        }
        Fit::Excluded => {
            return Err(Miss {
                condition: "exclude",
                detail: crate::t!("filter.miss.excluded", name = file_name(entry)),
            });
        }
    }
    conditions(cfg, f, i, entry, ctx, false)
}

/// Why `target` does not take file `i` (`entry`), for the explain panel:
/// `None` when one of its filters matches (`ai` and `expand` are not looked
/// at, nor manual series choices); else the first miss of a filter that
/// lists the file's source, or of the first filter when none does.
pub fn first_miss(
    cfg: &Config,
    target: &Target,
    entry: &FileEntry,
    i: usize,
    ctx: &RouteCtx,
) -> Option<Miss> {
    let mut own = None;
    let mut other = None;
    for f in &target.filters {
        match filter_miss(cfg, f, entry, i, ctx) {
            Ok(()) => return None,
            Err(m) if f.sources.contains(&entry.source_id) => {
                own.get_or_insert(m);
            }
            Err(m) => {
                other.get_or_insert(m);
            }
        }
    }
    own.or(other).or_else(|| {
        Some(Miss {
            condition: "sources",
            detail: crate::t!("filter.miss.source", source = entry.source_id),
        })
    })
}

/// Route every file to a target (see [`route_with_overrides`]) without
/// manual decisions, progress or cancel.
pub fn route(
    cfg: &Config,
    files: &[FileEntry],
    ctx: &RouteCtx,
    ai: &dyn Classifier,
    accept_low: bool,
) -> Result<Vec<(FileEntry, Decision)>> {
    let empty = Overrides::default();
    route_with_overrides(
        cfg,
        files,
        ctx,
        ai,
        accept_low,
        &empty,
        &NoProgress,
        &Cancel::new(),
    )
    .map(|r| r.decisions)
}

pub(crate) fn target_known(cfg: &Config, name: &str) -> bool {
    cfg.target(name).is_some()
}

/// Where one file's walk stands.
#[derive(Debug, Default)]
struct Walk {
    /// Index of the target the file is at (`targets.len()`: walked off the end).
    cursor: usize,
    /// Waits for an AI answer at `cursor`.
    paused: bool,
    /// The AI judged the file (its "none" is a verdict too).
    asked: bool,
    /// The AI's answer, once asked (and not sent to review).
    answer: Option<AiAnswer>,
    /// Confirmed best shot of its series (a manual decision).
    best: bool,
}

/// What one target does with one file.
enum Step {
    Take(Decision),
    /// An AI filter of the target could take the file: ask first.
    Pause,
    /// The target expands to events and its seeds are not known yet.
    Wait,
    Next,
}

struct Router<'a> {
    cfg: &'a Config,
    files: &'a [FileEntry],
    ctx: &'a RouteCtx<'a>,
    accept_low: bool,
    /// Targets whose `expand` filters know their files (true for targets
    /// without them).
    resolved: Vec<bool>,
    /// `(target, filter)` → files the event expansion matched.
    expanded: HashMap<(usize, usize), HashSet<usize>>,
}

impl Router<'_> {
    fn holds(&self, f: &Filter, i: usize, w: &Walk) -> bool {
        holds(self.cfg, f, i, &self.files[i], self.ctx, w.best)
    }

    fn step(&self, t: usize, i: usize, w: &Walk) -> Step {
        if !self.resolved[t] {
            return Step::Wait;
        }
        let target = &self.cfg.targets[t];
        let mut need_ai = false;
        let mut series = false;
        for (k, f) in target.filters.iter().enumerate() {
            let hit = self.holds(f, i, w)
                || self.expanded.get(&(t, k)).is_some_and(|s| s.contains(&i))
                || self.suggested_best(f, i, w);
            if !hit {
                continue;
            }
            let via = match (&f.ai, &w.answer) {
                (None, _) => Via::Mask,
                (Some(_), _) if !w.asked => {
                    need_ai = true;
                    continue;
                }
                (Some(_), Some(a)) if a.category.as_deref() == Some(target.name.as_str()) => {
                    Via::Ai {
                        reason: a.reason.clone(),
                        low: a.confidence == Confidence::Low,
                    }
                }
                (Some(_), _) => continue,
            };
            // Shots matched through `series` always go to review first.
            if f.series.as_ref().is_some_and(|c| !c.not) && self.ctx.groups.series_of(i).is_some() {
                series = true;
                continue;
            }
            // A doubtful answer that would take the file: the user decides.
            if let Via::Ai { reason, low: true } = &via
                && !self.accept_low
            {
                return Step::Take(Decision::Review {
                    suggested: target.name.clone(),
                    reason: reason.clone(),
                    series: None,
                });
            }
            return Step::Take(Decision::Target {
                name: target.name.clone(),
                via,
            });
        }
        if need_ai {
            Step::Pause
        } else if series {
            Step::Take(self.series_review(target, i))
        } else {
            Step::Next
        }
    }

    /// File `i` is the suggested best shot of its series and `f` would take
    /// the other shots: the best one goes to review with them, so the user
    /// confirms it (or picks another) for the whole series. A best shot the
    /// user confirmed walks on.
    fn suggested_best(&self, f: &Filter, i: usize, w: &Walk) -> bool {
        if w.best
            || !f.series.as_ref().is_some_and(|c| !c.not)
            || !self.ctx.groups.series_of(i).is_some_and(|s| s.best == i)
        {
            return false;
        }
        let others = Filter {
            series: None,
            ..f.clone()
        };
        self.holds(&others, i, w)
    }

    fn series_review(&self, target: &Target, i: usize) -> Decision {
        let s = self
            .ctx
            .groups
            .series_of(i)
            .expect("a series match has a series");
        Decision::Review {
            suggested: target.name.clone(),
            reason: crate::t!(
                "route.series_review",
                files = crate::tn!("count.files", s.members.len())
            ),
            series: Some(SeriesReview {
                members: s
                    .members
                    .iter()
                    .map(|&m| self.files[m].path.clone())
                    .collect(),
                best: self.files[s.best].path.clone(),
            }),
        }
    }

    /// Limit every series review to the shots held in it (same target):
    /// shots decided otherwise (a manual decision, an earlier target) are
    /// not offered again. The best is kept when held, else the first held.
    fn narrow_series(&self, decisions: &mut [Option<Decision>]) {
        let held_for = |i: usize| match &decisions[i] {
            Some(Decision::Review {
                suggested,
                series: Some(_),
                ..
            }) => Some(suggested.clone()),
            _ => None,
        };
        let mut narrowed = Vec::new();
        for i in 0..decisions.len() {
            let (Some(target), Some(s)) = (held_for(i), self.ctx.groups.series_of(i)) else {
                continue;
            };
            let held: Vec<usize> = s
                .members
                .iter()
                .copied()
                .filter(|&m| held_for(m).as_ref() == Some(&target))
                .collect();
            let best = if held.contains(&s.best) {
                s.best
            } else {
                held[0]
            };
            narrowed.push((
                i,
                SeriesReview {
                    members: held.iter().map(|&m| self.files[m].path.clone()).collect(),
                    best: self.files[best].path.clone(),
                },
            ));
        }
        for (i, sr) in narrowed {
            if let Some(Decision::Review { series, .. }) = &mut decisions[i] {
                *series = Some(sr);
            }
        }
    }

    /// Settle the `expand = "event"` filters of target `t` once every
    /// undecided file stands at it: the seeds are those files for which the
    /// filter's other conditions hold; every undecided file of a seed's
    /// event that is in the filter's sources and passes its masks matches.
    fn resolve_expand(&mut self, t: usize, decided: &[Option<Decision>], walks: &[Walk]) {
        let waiting = |i: usize| decided[i].is_none() && walks[i].cursor == t;
        for (k, f) in self.cfg.targets[t].filters.iter().enumerate() {
            if f.expand != Some(Expand::Event) {
                continue;
            }
            let Some(key) = EventKey::of(f) else {
                continue;
            };
            let mut set = HashSet::new();
            for i in (0..self.files.len()).filter(|&i| waiting(i)) {
                if !self.holds(f, i, &walks[i]) {
                    continue;
                }
                set.insert(i);
                let Some(ev) = self.ctx.groups.event(&key, i) else {
                    continue;
                };
                set.extend(ev.members.iter().copied().filter(|&m| {
                    waiting(m) && filter_fit(self.cfg, f, &self.files[m]) == Fit::Match
                }));
            }
            self.expanded.insert((t, k), set);
        }
        self.resolved[t] = true;
    }

    /// AI categories file `i` can still reach from target `from` on:
    /// (target name, description) of every target with an AI filter whose
    /// other conditions hold; descriptions of several such filters of one
    /// target are joined with "; ".
    fn ai_categories(&self, from: usize, i: usize, w: &Walk) -> Vec<(String, String)> {
        self.cfg.targets[from..]
            .iter()
            .filter_map(|t| {
                let descs: Vec<&str> = t
                    .filters
                    .iter()
                    .filter(|f| f.ai.is_some() && self.holds(f, i, w))
                    .filter_map(|f| f.ai.as_deref())
                    .collect();
                (!descs.is_empty()).then(|| (t.name.clone(), descs.join("; ")))
            })
            .collect()
    }

    fn request(&self, i: usize, categories: Vec<(String, String)>) -> AiRequest {
        let entry = &self.files[i];
        let cfg = self.cfg;
        let source = cfg.sources.iter().find(|s| s.id == entry.source_id);
        let examples = cfg
            .ai_examples
            .get(&entry.source_id)
            .map(|c| crate::learn::render_examples(c, &categories, cfg.ai.examples))
            .unwrap_or_default();
        AiRequest {
            entry: entry.clone(),
            categories,
            prompt: SourcePrompt {
                prefix: source.and_then(|s| s.ai_prefix.clone()),
                suffix: source.and_then(|s| s.ai_suffix.clone()),
                examples,
                language: cfg.ai.language.clone(),
            },
        }
    }
}

/// Route every file: manual decisions first (a fresh entry of `overrides`,
/// same size and mtime, decides the file; an entry naming a target the
/// config does not have is ignored with a warning, stale entries silently;
/// `SeriesBest` only marks the file). Then every file walks the targets in
/// order, and the first target with a matching filter takes it:
///
/// - A filter matches when its sources, masks and conditions hold and, with
///   `ai`, the AI's answer is this target. A file reaching such a filter
///   before it has an answer pauses; when no file can move on, the paused
///   ones go to the AI as one batch, each with every category it could
///   still reach (this and later targets), and their walks resume. A file
///   is asked at most once. Unless `accept_low`, a low-confidence "none"
///   sends it to review at once, and a low-confidence category does when
///   an AI filter would take the file through it (strict order first).
/// - `expand = "event"`: once every undecided file stands at the target,
///   the files its other conditions hold for seed their events, and every
///   undecided file of those events in its sources and masks matches.
/// - A match through `series` goes to review with the whole series, the
///   suggested best shot included (unless the user confirmed it as best).
///
/// A file nothing takes is `Excluded` when an exclude mask turned it away
/// and the AI did not judge it, else `Unmatched`. A cancel ends routing with
/// `Error::Cancelled` (the classifier stops between requests); the AI
/// batches report [`Stage::Ai`] progress.
#[allow(clippy::too_many_arguments)]
pub fn route_with_overrides(
    cfg: &Config,
    files: &[FileEntry],
    ctx: &RouteCtx,
    ai: &dyn Classifier,
    accept_low: bool,
    overrides: &Overrides,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<Routed> {
    cancel.check()?;
    assert_eq!(
        ctx.facts.len(),
        files.len(),
        "routing context built for another file list"
    );
    let mut decisions: Vec<Option<Decision>> = vec![None; files.len()];
    let mut walks: Vec<Walk> = files.iter().map(|_| Walk::default()).collect();
    let mut warnings = Vec::new();
    let mut overrides_applied = 0;

    for (i, entry) in files.iter().enumerate() {
        match overrides.get(entry) {
            Some(Action::Keep) => {
                decisions[i] = Some(Decision::Kept);
                overrides_applied += 1;
            }
            Some(Action::Target { name }) if target_known(cfg, name) => {
                decisions[i] = Some(Decision::Target {
                    name: name.clone(),
                    via: Via::Manual,
                });
                overrides_applied += 1;
            }
            Some(Action::Target { name }) => warnings.push(crate::t!(
                "route.unknown_target",
                path = entry.path.display(),
                name = name
            )),
            Some(Action::SeriesBest) => walks[i].best = true,
            None => {}
        }
    }

    let n = cfg.targets.len();
    let mut router = Router {
        cfg,
        files,
        ctx,
        accept_low,
        resolved: cfg
            .targets
            .iter()
            .map(|t| t.filters.iter().all(|f| f.expand.is_none()))
            .collect(),
        expanded: HashMap::new(),
    };
    loop {
        cancel.check()?;
        for i in 0..files.len() {
            if decisions[i].is_some() || walks[i].paused {
                continue;
            }
            while walks[i].cursor < n {
                match router.step(walks[i].cursor, i, &walks[i]) {
                    Step::Take(d) => {
                        decisions[i] = Some(d);
                        break;
                    }
                    Step::Pause => {
                        walks[i].paused = true;
                        break;
                    }
                    Step::Wait => break,
                    Step::Next => walks[i].cursor += 1,
                }
            }
        }

        // Paused files stand before (or at) every target still waiting to
        // expand, so the AI comes first.
        let paused: Vec<usize> = (0..files.len())
            .filter(|&i| decisions[i].is_none() && walks[i].paused)
            .collect();
        if !paused.is_empty() {
            let batch: Vec<AiRequest> = paused
                .iter()
                .map(|&i| router.request(i, router.ai_categories(walks[i].cursor, i, &walks[i])))
                .collect();
            let candidates: Vec<Vec<(String, String)>> =
                batch.iter().map(|r| r.categories.clone()).collect();
            let answers = ai.classify_with(batch, progress, cancel)?;
            if answers.len() != paused.len() {
                return Err(Error::Ai(crate::t!(
                    "route.answer_count",
                    answers = answers.len(),
                    requests = paused.len()
                )));
            }
            for ((i, cands), answer) in paused.into_iter().zip(candidates).zip(answers) {
                if let Some(cat) = &answer.category
                    && !cands.iter().any(|(n, _)| n == cat)
                {
                    return Err(Error::Ai(crate::t!(
                        "route.unknown_category",
                        cat = cat,
                        path = files[i].path.display()
                    )));
                }
                let w = &mut walks[i];
                w.paused = false;
                w.asked = true;
                // A doubtful "none" goes to review at once; a doubtful
                // category only when the walk reaches a filter it would take
                // the file through (an earlier target may take it first).
                if answer.confidence == Confidence::Low && !accept_low && answer.category.is_none()
                {
                    decisions[i] = Some(Decision::Review {
                        suggested: "—".to_string(),
                        reason: answer.reason,
                        series: None,
                    });
                } else {
                    w.answer = Some(answer);
                }
            }
            continue;
        }

        // Everyone left waits at a target that expands to events.
        let waiting = (0..files.len())
            .filter(|&i| decisions[i].is_none() && walks[i].cursor < n)
            .map(|i| walks[i].cursor)
            .min();
        match waiting {
            Some(t) => router.resolve_expand(t, &decisions, &walks),
            None => break,
        }
    }

    router.narrow_series(&mut decisions);

    let decisions = files
        .iter()
        .zip(decisions)
        .zip(&walks)
        .map(|((entry, d), w)| {
            let d = d.unwrap_or_else(|| {
                if !w.asked && excluded_somewhere(cfg, entry) {
                    Decision::Excluded
                } else {
                    Decision::Unmatched
                }
            });
            (entry.clone(), d)
        })
        .collect();
    Ok(Routed {
        decisions,
        warnings,
        overrides_applied,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mask::MaskItem;
    use crate::overrides::Action;
    use std::cell::RefCell;
    use std::path::PathBuf;

    fn file(src: &str, name: &str) -> FileEntry {
        FileEntry {
            source_id: src.into(),
            path: PathBuf::from("/s").join(name),
            rel: PathBuf::from(name),
            size: 1,
            mtime_ns: 0,
        }
    }

    fn mask_target(name: &str, src: &str, mask: &str) -> Target {
        Target {
            name: name.into(),
            kind: Default::default(),
            filters: vec![Filter {
                sources: vec![src.into()],
                matches: Some(MaskExpr::parse(mask).unwrap()),
                ..Filter::default()
            }],
            outputs: vec![],
        }
    }

    fn ai_target(name: &str, mask: Option<&str>) -> Target {
        Target {
            name: name.into(),
            kind: Default::default(),
            filters: vec![Filter {
                sources: vec!["a".into()],
                matches: mask.map(|m| MaskExpr::parse(m).unwrap()),
                ai: Some(format!("{name} desc")),
                ..Filter::default()
            }],
            outputs: vec![],
        }
    }

    struct Fake {
        answers: Vec<AiAnswer>,
        seen: RefCell<Vec<AiRequest>>,
    }

    impl Fake {
        fn new(answers: Vec<AiAnswer>) -> Self {
            Fake {
                answers,
                seen: RefCell::new(vec![]),
            }
        }
    }

    impl Classifier for Fake {
        fn classify(&self, batch: Vec<AiRequest>) -> Result<Vec<AiAnswer>> {
            *self.seen.borrow_mut() = batch;
            Ok(self.answers.clone())
        }
    }

    struct Panics;
    impl Classifier for Panics {
        fn classify(&self, _: Vec<AiRequest>) -> Result<Vec<AiAnswer>> {
            panic!("classifier must not be called");
        }
    }

    fn ans(cat: Option<&str>, conf: Confidence) -> AiAnswer {
        AiAnswer {
            category: cat.map(String::from),
            reason: "why".into(),
            confidence: conf,
        }
    }

    /// The three old stages, concatenated as a migrated config has them.
    fn cfg(before: Vec<Target>, ai: Vec<Target>, after: Vec<Target>) -> Config {
        Config {
            targets: before.into_iter().chain(ai).chain(after).collect(),
            ..Config::default()
        }
    }

    /// [`route`] without metadata.
    fn run(
        c: &Config,
        files: &[FileEntry],
        ai: &dyn Classifier,
        accept_low: bool,
    ) -> Result<Vec<(FileEntry, Decision)>> {
        route(c, files, &RouteCtx::empty(files), ai, accept_low)
    }

    fn target(name: &str, via: Via) -> Decision {
        Decision::Target {
            name: name.into(),
            via,
        }
    }

    #[test]
    fn excluded_only_by_an_exclude_mask() {
        let mut shots = mask_target("shots", "a", "*.png");
        shots.filters[0].exclude = MaskExpr::parse("2026*").unwrap();
        let mut ai = ai_target("cats", None);
        ai.filters[0].exclude = MaskExpr::parse("skip*").unwrap();
        let c = cfg(
            vec![shots],
            vec![ai],
            vec![mask_target("rest", "a", "2026*.jpg")],
        );
        let files = [
            // Taken by the filter's sources and name, then excluded;
            // AI was asked and said "none": a verdict, so unmatched.
            file("a", "20260412_103314.png"),
            // Excluded before AI, but `after_ai` takes it.
            file("a", "20260412_103314.jpg"),
            // Excluded from the only AI candidate.
            file("a", "skip.jpg"),
            // Other source: no filter matched at all.
            file("b", "20260412_103314.png"),
        ];
        // The two 2026* files are AI candidates; AI takes neither.
        let fake = Fake::new(vec![ans(None, Confidence::Ok), ans(None, Confidence::Ok)]);
        let r = run(&c, &files, &fake, false).unwrap();
        assert_eq!(r[0].1, Decision::Unmatched);
        assert_eq!(r[1].1, target("rest", Via::Mask));
        assert_eq!(r[2].1, Decision::Excluded);
        assert_eq!(r[3].1, Decision::Unmatched);
        // Without an AI candidate the mask-excluded png is left alone.
        let mut shots = mask_target("shots", "a", "*.png");
        shots.filters[0].exclude = MaskExpr::parse("2026*").unwrap();
        let c = cfg(vec![shots], vec![], vec![]);
        let r = run(&c, &[file("a", "20260412_103314.png")], &Panics, false).unwrap();
        assert_eq!(r[0].1, Decision::Excluded);
    }

    #[test]
    fn first_mask_target_wins() {
        let c = cfg(
            vec![
                mask_target("one", "a", "*.jpg"),
                mask_target("two", "a", "*.jpg"),
            ],
            vec![],
            vec![],
        );
        let r = run(&c, &[file("a", "x.jpg")], &Panics, false).unwrap();
        assert_eq!(r[0].1, target("one", Via::Mask));
    }

    #[test]
    fn masked_file_not_sent_to_ai() {
        let c = cfg(
            vec![mask_target("shots", "a", "Screenshot*")],
            vec![ai_target("cats", None)],
            vec![],
        );
        let r = run(&c, &[file("a", "Screenshot_1.png")], &Panics, false).unwrap();
        assert_eq!(r[0].1, target("shots", Via::Mask));
    }

    #[test]
    fn ai_candidates_filtered_by_source_and_prefilter() {
        let c = cfg(vec![], vec![ai_target("cats", Some("*.{jpg,png}"))], vec![]);
        let fake = Fake::new(vec![ans(Some("cats"), Confidence::Ok)]);
        let files = [
            file("a", "v.mp4"),
            file("a", "p.jpg"),
            file("other", "q.jpg"),
        ];
        let r = run(&c, &files, &fake, false).unwrap();
        let seen = fake.seen.borrow();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].entry.rel, PathBuf::from("p.jpg"));
        assert_eq!(
            seen[0].categories,
            vec![("cats".into(), "cats desc".into())]
        );
        assert_eq!(r[0].1, Decision::Unmatched);
        assert_eq!(
            r[1].1,
            target(
                "cats",
                Via::Ai {
                    reason: "why".into(),
                    low: false,
                }
            )
        );
        assert_eq!(r[2].1, Decision::Unmatched);
    }

    #[test]
    fn a_target_mixes_ai_and_mask_filters() {
        // A target with two AI filters and a mask filter: descriptions of
        // the AI filters that hold are joined; the mask filter takes its
        // files without asking.
        let mut docs = ai_target("docs", Some("*.jpg"));
        let mut second = docs.filters[0].clone();
        second.matches = Some(MaskExpr::parse("p*").unwrap());
        second.ai = Some("papers".into());
        docs.filters.push(second);
        docs.filters.push(Filter {
            sources: vec!["a".into()],
            matches: Some(MaskExpr::parse("*.gif").unwrap()),
            ..Filter::default()
        });
        let c = cfg(
            vec![mask_target("pngs", "a", "*.png")],
            vec![docs],
            vec![mask_target("rest", "a", "*")],
        );
        let files = [
            file("a", "p.png"),
            file("a", "p.jpg"),
            file("a", "q.jpg"),
            file("a", "x.gif"),
        ];
        let fake = Fake::new(vec![ans(None, Confidence::Ok), ans(None, Confidence::Ok)]);
        let r = run(&c, &files, &fake, false).unwrap();
        let seen = fake.seen.borrow();
        let cats: Vec<_> = seen.iter().map(|q| q.categories.clone()).collect();
        assert_eq!(
            cats,
            [
                vec![("docs".to_string(), "docs desc; papers".to_string())],
                vec![("docs".to_string(), "docs desc".to_string())],
            ]
        );
        assert_eq!(r[0].1, target("pngs", Via::Mask));
        assert_eq!(r[1].1, target("rest", Via::Mask));
        assert_eq!(r[3].1, target("docs", Via::Mask));
    }

    #[test]
    fn ai_request_carries_its_source_prompt() {
        use crate::config::Source;
        let mut c = cfg(vec![], vec![ai_target("cats", None)], vec![]);
        c.targets[0].filters[0].sources = vec!["a".into(), "b".into()];
        for (id, prefix, suffix) in [("a", Some("before"), Some("after")), ("b", None, None)] {
            c.sources.push(Source {
                id: id.into(),
                path: PathBuf::from("/s"),
                server_path: None,
                recursive: false,
                skip: vec![],
                ai_prefix: prefix.map(String::from),
                ai_prefix_file: None,
                ai_suffix: suffix.map(String::from),
                ai_suffix_file: None,
            });
        }
        let fake = Fake::new(vec![ans(None, Confidence::Ok); 2]);
        run(&c, &[file("a", "1.jpg"), file("b", "2.jpg")], &fake, false).unwrap();
        let seen = fake.seen.borrow();
        assert_eq!(
            seen[0].prompt,
            SourcePrompt {
                prefix: Some("before".into()),
                suffix: Some("after".into()),
                examples: vec![],
                language: String::new(),
            }
        );
        // `[ai] language` reaches every request; nothing else is set here.
        assert_eq!(
            seen[1].prompt,
            SourcePrompt {
                language: String::new(),
                ..Default::default()
            }
        );
    }

    #[test]
    fn ai_none_falls_to_after_ai() {
        let c = cfg(
            vec![],
            vec![ai_target("cats", None)],
            vec![mask_target("rest", "a", "*")],
        );
        let fake = Fake::new(vec![ans(None, Confidence::Ok)]);
        let r = run(&c, &[file("a", "p.jpg")], &fake, false).unwrap();
        assert_eq!(r[0].1, target("rest", Via::Mask));
    }

    #[test]
    fn low_goes_to_review() {
        let c = cfg(
            vec![],
            vec![ai_target("cats", None)],
            vec![mask_target("rest", "a", "*")],
        );
        let fake = Fake::new(vec![
            ans(Some("cats"), Confidence::Low),
            ans(None, Confidence::Low),
        ]);
        let r = run(&c, &[file("a", "p.jpg"), file("a", "q.jpg")], &fake, false).unwrap();
        assert_eq!(
            r[0].1,
            Decision::Review {
                suggested: "cats".into(),
                reason: "why".into(),
                series: None,
            }
        );
        assert_eq!(
            r[1].1,
            Decision::Review {
                suggested: "—".into(),
                reason: "why".into(),
                series: None,
            }
        );
    }

    #[test]
    fn low_accepted_with_flag() {
        let c = cfg(vec![], vec![ai_target("cats", None)], vec![]);
        let fake = Fake::new(vec![ans(Some("cats"), Confidence::Low)]);
        let r = run(&c, &[file("a", "p.jpg")], &fake, true).unwrap();
        assert_eq!(
            r[0].1,
            target(
                "cats",
                Via::Ai {
                    reason: "why".into(),
                    low: true,
                }
            )
        );
    }

    #[test]
    fn ai_via_serde_keeps_old_plans_readable() {
        let ok = Via::Ai {
            reason: "r".into(),
            low: false,
        };
        let v = serde_json::to_value(&ok).unwrap();
        // Not written when false: plans stay as before.
        assert_eq!(v, serde_json::json!({"kind": "ai", "reason": "r"}));
        let back: Via = serde_json::from_value(v).unwrap();
        assert_eq!(back, ok);
        let low = Via::Ai {
            reason: "r".into(),
            low: true,
        };
        let v = serde_json::to_value(&low).unwrap();
        assert_eq!(v["low"], true);
        assert_eq!(serde_json::from_value::<Via>(v).unwrap(), low);
    }

    #[test]
    fn unmatched_stays() {
        let c = cfg(vec![mask_target("x", "a", "*.zip")], vec![], vec![]);
        let r = run(&c, &[file("a", "p.jpg")], &Panics, false).unwrap();
        assert_eq!(r[0].1, Decision::Unmatched);
    }

    #[test]
    fn exclude_mask_drops_file_from_mask_filter() {
        let mut t = mask_target("shots", "a", "*.png");
        t.filters[0].exclude = MaskExpr(vec![
            MaskItem::parse("^[0-9]{8}_[0-9]{6}").unwrap(),
            MaskItem::parse("*.tmp.png").unwrap(),
        ]);
        let c = cfg(vec![t], vec![], vec![]);
        let files = [
            file("a", "Screenshot_1.png"),
            file("a", "20260101_120000.png"),
            file("a", "x.tmp.png"),
        ];
        let r = run(&c, &files, &Panics, false).unwrap();
        assert_eq!(r[0].1, target("shots", Via::Mask));
        assert_eq!(r[1].1, Decision::Excluded);
        assert_eq!(r[2].1, Decision::Excluded);
    }

    #[test]
    fn unresolved_exclude_ref_fails_safe_to_excluded() {
        let mut t = ai_target("cats", None);
        t.filters[0].exclude = MaskExpr::parse("@nope").unwrap();
        let c = cfg(vec![], vec![t], vec![]);
        let fake = Fake::new(vec![]);
        let r = run(&c, &[file("a", "a.gif")], &fake, false).unwrap();
        assert!(fake.seen.borrow().is_empty());
        assert_eq!(r[0].1, Decision::Excluded);
    }

    #[test]
    fn exclude_mask_drops_file_from_ai_prefilter() {
        let mut t = ai_target("cats", None);
        t.filters[0].exclude = MaskExpr::parse("*.gif").unwrap();
        let c = cfg(vec![], vec![t], vec![]);
        let fake = Fake::new(vec![ans(Some("cats"), Confidence::Ok)]);
        let r = run(&c, &[file("a", "a.gif"), file("a", "b.jpg")], &fake, false).unwrap();
        let seen = fake.seen.borrow();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].entry.rel, PathBuf::from("b.jpg"));
        assert_eq!(r[0].1, Decision::Excluded);
        assert_eq!(
            r[1].1,
            target(
                "cats",
                Via::Ai {
                    reason: "why".into(),
                    low: false,
                }
            )
        );
    }

    #[test]
    fn unknown_category_is_error() {
        let c = cfg(vec![], vec![ai_target("cats", None)], vec![]);
        let fake = Fake::new(vec![ans(Some("dogs"), Confidence::Ok)]);
        let e = run(&c, &[file("a", "p.jpg")], &fake, false).unwrap_err();
        assert!(matches!(e, Error::Ai(_)));
    }

    fn routed_with(c: &Config, files: &[FileEntry], ai: &dyn Classifier, o: &Overrides) -> Routed {
        let ctx = RouteCtx::empty(files);
        route_with_overrides(c, files, &ctx, ai, false, o, &NoProgress, &Cancel::new()).unwrap()
    }

    fn corr(sug: &str, reason: &str, choice: &str) -> crate::learn::Correction {
        crate::learn::Correction::new(
            "a",
            "f.jpg",
            Some(sug.into()),
            reason,
            Action::Target {
                name: choice.into(),
            },
            "t",
        )
    }

    #[test]
    fn requests_carry_the_sources_corrections_that_fit_the_candidates() {
        let mut c = cfg(
            vec![],
            vec![ai_target("cats", None), ai_target("dogs", None)],
            vec![],
        );
        c.ai_examples.insert(
            "a".into(),
            vec![
                corr("cats", "grey cat", "dogs"),
                corr("cats", "fox", "gone"),
            ],
        );
        c.ai_examples
            .insert("b".into(), vec![corr("cats", "stranger", "dogs")]);
        let fake = Fake::new(vec![ans(Some("cats"), Confidence::Ok)]);
        run(&c, &[file("a", "p.jpg")], &fake, false).unwrap();
        assert_eq!(
            fake.seen.borrow()[0].prompt.examples,
            vec!["grey cat — AI chose cats, correct is dogs"]
        );

        // `examples = 0` turns the learning off.
        c.ai.examples = 0;
        let fake = Fake::new(vec![ans(Some("cats"), Confidence::Ok)]);
        run(&c, &[file("a", "p.jpg")], &fake, false).unwrap();
        assert!(fake.seen.borrow()[0].prompt.examples.is_empty());
    }

    #[test]
    fn override_comes_before_masks_and_ai() {
        let c = cfg(
            vec![mask_target("shots", "a", "*.jpg")],
            vec![ai_target("cats", None)],
            vec![mask_target("rest", "a", "*")],
        );
        let files = [file("a", "x.jpg"), file("a", "y.png"), file("a", "z.png")];
        let mut o = Overrides::default();
        o.set(
            files[0].path.clone(),
            1,
            0,
            Action::Target {
                name: "rest".into(),
            },
        );
        o.set(files[1].path.clone(), 1, 0, Action::Keep);
        // Only z.png reaches the AI.
        let fake = Fake::new(vec![ans(Some("cats"), Confidence::Ok)]);
        let r = routed_with(&c, &files, &fake, &o);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!(r.overrides_applied, 2);
        assert_eq!(r.decisions[0].1, target("rest", Via::Manual));
        assert_eq!(r.decisions[1].1, Decision::Kept);
        assert_eq!(
            r.decisions[2].1,
            target(
                "cats",
                Via::Ai {
                    reason: "why".into(),
                    low: false,
                }
            )
        );
        let seen = fake.seen.borrow();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].entry.rel, PathBuf::from("z.png"));
    }

    #[test]
    fn override_may_name_any_kind_of_target() {
        let c = cfg(vec![], vec![ai_target("cats", Some("*.png"))], vec![]);
        let mut o = Overrides::default();
        let f = file("a", "x.jpg");
        o.set(
            f.path.clone(),
            1,
            0,
            Action::Target {
                name: "cats".into(),
            },
        );
        let r = routed_with(&c, &[f], &Panics, &o);
        assert_eq!(r.decisions[0].1, target("cats", Via::Manual));
    }

    #[test]
    fn override_of_unknown_target_is_ignored_with_a_warning() {
        let c = cfg(vec![mask_target("shots", "a", "*.jpg")], vec![], vec![]);
        let f = file("a", "x.jpg");
        let mut o = Overrides::default();
        o.set(
            f.path.clone(),
            1,
            0,
            Action::Target {
                name: "gone".into(),
            },
        );
        let r = routed_with(&c, &[f], &Panics, &o);
        assert_eq!(r.decisions[0].1, target("shots", Via::Mask));
        assert_eq!(r.overrides_applied, 0);
        assert_eq!(r.warnings.len(), 1);
        assert!(r.warnings[0].contains("gone"), "{:?}", r.warnings);
        assert!(r.warnings[0].contains("x.jpg"), "{:?}", r.warnings);
    }

    #[test]
    fn stale_override_is_ignored() {
        let c = cfg(vec![mask_target("shots", "a", "*.jpg")], vec![], vec![]);
        let f = file("a", "x.jpg");
        let mut o = Overrides::default();
        // The file had another size when the decision was made.
        o.set(f.path.clone(), 99, 0, Action::Keep);
        let r = routed_with(&c, &[f], &Panics, &o);
        assert_eq!(r.decisions[0].1, target("shots", Via::Mask));
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn route_reports_ai_progress_and_stops_on_cancel() {
        use crate::progress::testing::Recorder;
        use crate::progress::{Cancel, NoProgress, Stage};
        let c = cfg(vec![], vec![ai_target("cats", None)], vec![]);
        let files = [file("a", "x.jpg"), file("a", "y.jpg")];
        let fake = Fake::new(vec![
            ans(Some("cats"), Confidence::Ok),
            ans(None, Confidence::Ok),
        ]);
        let rec = Recorder::default();
        let ctx = RouteCtx::empty(&files);
        let none = Overrides::default();
        let r = route_with_overrides(&c, &files, &ctx, &fake, false, &none, &rec, &Cancel::new())
            .unwrap()
            .decisions;
        assert_eq!(
            r[0].1,
            target(
                "cats",
                Via::Ai {
                    reason: "why".into(),
                    low: false,
                }
            )
        );
        let evs = rec.events(Stage::Ai);
        let last = evs.last().expect("an AI event");
        assert_eq!((last.done, last.total), (2, 2));

        let cancel = Cancel::new();
        cancel.cancel();
        let r = route_with_overrides(
            &c,
            &files,
            &ctx,
            &Panics,
            false,
            &none,
            &NoProgress,
            &cancel,
        );
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    }

    // ------------------------------------------- ordered walk with metadata

    use crate::filter::{Series, compute, needs};
    use serde_json::json;

    /// A config from TOML (the new shape); `validate` is not needed here.
    fn toml_cfg(text: &str) -> Config {
        let c = Config::from_str_at(text, Path::new("/"), Path::new("t.toml")).unwrap();
        c.validate().unwrap();
        c
    }

    use std::path::Path;

    const SOURCES: &str = r#"
[[sources]]
id = "a"
path = "/s"

[[sources]]
id = "camera"
path = "/c"

[[sources]]
id = "olga"
path = "/o"
"#;

    /// Answers by file name; every batch is recorded.
    struct ByName {
        answers: HashMap<&'static str, (Option<&'static str>, Confidence)>,
        batches: RefCell<Vec<Vec<AiRequest>>>,
    }

    impl ByName {
        fn new(answers: &[(&'static str, Option<&'static str>)]) -> Self {
            ByName {
                answers: answers
                    .iter()
                    .map(|&(f, c)| (f, (c, Confidence::Ok)))
                    .collect(),
                batches: RefCell::new(vec![]),
            }
        }

        fn categories(&self, file: &str) -> Vec<String> {
            self.batches
                .borrow()
                .iter()
                .flatten()
                .find(|r| r.entry.rel == Path::new(file))
                .map(|r| r.categories.iter().map(|(n, _)| n.clone()).collect())
                .unwrap_or_default()
        }
    }

    impl Classifier for ByName {
        fn classify(&self, batch: Vec<AiRequest>) -> Result<Vec<AiAnswer>> {
            let out = batch
                .iter()
                .map(|r| {
                    let name = r.entry.rel.to_str().unwrap();
                    let (c, confidence) = self.answers[name];
                    AiAnswer {
                        category: c.map(String::from),
                        reason: "why".into(),
                        confidence,
                    }
                })
                .collect();
            self.batches.borrow_mut().push(batch);
            Ok(out)
        }
    }

    fn ai_ok(name: &str) -> Decision {
        target(
            name,
            Via::Ai {
                reason: "why".into(),
                low: false,
            },
        )
    }

    /// 1 png (mask), 2 AI, 3 jpg (mask), 4 AI.
    fn ordered() -> Config {
        toml_cfg(&format!(
            r#"{SOURCES}
[[targets]]
name = "1"
[[targets.filters]]
sources = ["a"]
match = "*.png"

[[targets]]
name = "2"
[[targets.filters]]
sources = ["a"]
ai = "screens"

[[targets]]
name = "3"
[[targets.filters]]
sources = ["a"]
match = "*.jpg"

[[targets]]
name = "4"
[[targets.filters]]
sources = ["a"]
ai = "docs"
"#
        ))
    }

    #[test]
    fn walk_pauses_at_the_first_ai_filter_and_respects_order() {
        let c = ordered();
        let files = [
            file("a", "x.png"),
            file("a", "y.heic"),
            file("a", "z.heic"),
            file("a", "p.jpg"),
            file("a", "q.jpg"),
        ];
        let ai = ByName::new(&[
            ("y.heic", Some("4")),
            ("z.heic", None),
            ("p.jpg", Some("4")),
            ("q.jpg", Some("2")),
        ]);
        let r = run(&c, &files, &ai, false).unwrap();
        // One batch; the png never reaches the AI.
        assert_eq!(ai.batches.borrow().len(), 1);
        assert_eq!(ai.batches.borrow()[0].len(), 4);
        assert!(ai.categories("x.png").is_empty());
        assert_eq!(r[0].1, target("1", Via::Mask));
        // Every AI category the file can reach; a mask target between them
        // never prunes the list.
        for f in ["y.heic", "z.heic", "p.jpg", "q.jpg"] {
            assert_eq!(ai.categories(f), ["2", "4"], "{f}");
        }
        assert_eq!(r[1].1, ai_ok("4"));
        assert_eq!(r[2].1, Decision::Unmatched);
        // "4" comes after the jpg target, which takes the file first.
        assert_eq!(r[3].1, target("3", Via::Mask));
        assert_eq!(r[4].1, ai_ok("2"));
    }

    #[test]
    fn a_low_answer_goes_to_review_only_where_it_would_take_the_file() {
        let c = ordered();
        let files = [
            file("a", "y.heic"),
            file("a", "p.jpg"),
            file("a", "q.jpg"),
            file("a", "n.heic"),
        ];
        let mut ai = ByName::new(&[]);
        for (f, cat) in [
            ("y.heic", Some("4")),
            ("p.jpg", Some("4")),
            ("q.jpg", Some("2")),
            ("n.heic", None),
        ] {
            ai.answers.insert(f, (cat, Confidence::Low));
        }
        let r = run(&c, &files, &ai, false).unwrap();
        let review = |s: &str| Decision::Review {
            suggested: s.into(),
            reason: "why".into(),
            series: None,
        };
        assert_eq!(r[0].1, review("4"));
        // Strict order: the jpg target comes before "4" and takes the file,
        // whatever the doubtful answer.
        assert_eq!(r[1].1, target("3", Via::Mask));
        assert_eq!(r[2].1, review("2"));
        // A doubtful "none" is reviewed at once.
        assert_eq!(r[3].1, review("—"));
        // Accepted, the answers decide like confident ones.
        let r = run(&c, &files, &ai, true).unwrap();
        let low = |n: &str| {
            target(
                n,
                Via::Ai {
                    reason: "why".into(),
                    low: true,
                },
            )
        };
        assert_eq!(r[0].1, low("4"));
        assert_eq!(r[1].1, target("3", Via::Mask));
        assert_eq!(r[2].1, low("2"));
        assert_eq!(r[3].1, Decision::Unmatched);
    }

    #[test]
    fn cancel_during_the_ai_batch_stops_routing() {
        struct CancelsIn<'a>(&'a Cancel);
        impl Classifier for CancelsIn<'_> {
            fn classify(&self, batch: Vec<AiRequest>) -> Result<Vec<AiAnswer>> {
                self.0.cancel();
                Ok(batch
                    .iter()
                    .map(|_| ans(Some("2"), Confidence::Ok))
                    .collect())
            }
        }
        let c = ordered();
        let files = [file("a", "y.heic")];
        let ctx = RouteCtx::empty(&files);
        let cancel = Cancel::new();
        let r = route_with_overrides(
            &c,
            &files,
            &ctx,
            &CancelsIn(&cancel),
            false,
            &Overrides::default(),
            &NoProgress,
            &cancel,
        );
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    }

    fn photo(src: &str, name: &str, when: &str, gps: Option<(f64, f64)>) -> (FileEntry, Value) {
        let mut exif = json!({ "ExifIFD:DateTimeOriginal": when });
        if let Some((lat, lon)) = gps {
            exif["GPS:GPSLatitude"] = json!(lat);
            exif["GPS:GPSLongitude"] = json!(lon);
        }
        let mut e = file(src, name);
        e.path = PathBuf::from(format!("/{src}/{name}"));
        (e, exif)
    }

    /// Facts from `exif`, groups computed as a scan does (no tools).
    fn with_groups<'a>(
        c: &Config,
        files: &[FileEntry],
        exif: &'a HashMap<PathBuf, Value>,
        cache: &Path,
    ) -> RouteCtx<'a> {
        let facts: Vec<Facts> = files
            .iter()
            .map(|e| facts_of(e, &exif[&e.path], None))
            .collect();
        let groups = compute(
            c,
            files,
            &facts,
            &needs(c),
            None,
            Some(cache),
            &NoProgress,
            &Cancel::new(),
        )
        .unwrap();
        RouteCtx {
            facts: Cow::Owned(facts),
            exif: Cow::Borrowed(exif),
            groups: Cow::Owned(groups),
            now: now_in(None),
        }
    }

    const HOME: (f64, f64) = (47.0245, 28.8323);
    /// About 5.5 km north of `HOME`.
    const AWAY: (f64, f64) = (47.0745, 28.8323);

    #[test]
    fn expand_takes_the_whole_event_of_a_seed() {
        let together = |before: &str| {
            toml_cfg(&format!(
                r#"{SOURCES}
{before}
[[targets]]
name = "Together"
[[targets.filters]]
sources = ["camera"]
near = {{ sources = ["olga"], time = "2h", km = 1 }}
expand = "event"
"#
            ))
        };
        let shots = [
            photo("camera", "c1.jpg", "2026:08:01 10:00:00", Some(HOME)),
            // Alone: Olga is 5 km away, but it is the same outing.
            photo("camera", "c2.jpg", "2026:08:01 10:30:00", Some(AWAY)),
            photo("camera", "c3.jpg", "2026:08:05 10:00:00", Some(HOME)),
            photo("olga", "o1.jpg", "2026:08:01 10:05:00", Some(HOME)),
        ];
        let files: Vec<FileEntry> = shots.iter().map(|(e, _)| e.clone()).collect();
        let exif: HashMap<PathBuf, Value> = shots.into_iter().map(|(e, x)| (e.path, x)).collect();
        let dir = tempfile::tempdir().unwrap();

        let c = together("");
        let ctx = with_groups(&c, &files, &exif, dir.path());
        let r = route(&c, &files, &ctx, &Panics, false).unwrap();
        assert_eq!(r[0].1, target("Together", Via::Mask));
        assert_eq!(r[1].1, target("Together", Via::Mask));
        assert_eq!(r[2].1, Decision::Unmatched);
        assert_eq!(r[3].1, Decision::Unmatched);

        // The seed is taken by an earlier target: nothing seeds the event.
        let c = together(
            r#"
[[targets]]
name = "Early"
[[targets.filters]]
sources = ["camera"]
match = "c1*"
"#,
        );
        let ctx = with_groups(&c, &files, &exif, dir.path());
        let r = route(&c, &files, &ctx, &Panics, false).unwrap();
        assert_eq!(r[0].1, target("Early", Via::Mask));
        assert_eq!(r[1].1, Decision::Unmatched);
    }

    #[test]
    fn expand_waits_for_files_paused_before_it() {
        // `p.jpg` waits for the AI at "docs"; it says none, so the file
        // reaches "Together" and seeds its event there (`q.jpg` is out of
        // the time window, but in the same event).
        let c = toml_cfg(&format!(
            r#"{SOURCES}
[[targets]]
name = "docs"
[[targets.filters]]
sources = ["camera"]
match = "p*"
ai = "papers"

[[targets]]
name = "Together"
[[targets.filters]]
sources = ["camera"]
date = {{ time = "09:30..10:30" }}
expand = "event"
"#
        ));
        let shots = [
            photo("camera", "p.jpg", "2026:08:01 10:00:00", None),
            photo("camera", "q.jpg", "2026:08:01 11:00:00", None),
        ];
        let files: Vec<FileEntry> = shots.iter().map(|(e, _)| e.clone()).collect();
        let exif: HashMap<PathBuf, Value> = shots.into_iter().map(|(e, x)| (e.path, x)).collect();
        let dir = tempfile::tempdir().unwrap();
        let ctx = with_groups(&c, &files, &exif, dir.path());
        let ai = ByName::new(&[("p.jpg", None)]);
        let r = route(&c, &files, &ctx, &ai, false).unwrap();
        assert_eq!(r[0].1, target("Together", Via::Mask));
        assert_eq!(r[1].1, target("Together", Via::Mask));
    }

    #[test]
    fn keep_target_takes_files_before_later_targets() {
        let c = toml_cfg(&format!(
            r#"{SOURCES}
[places]
home = {{ lat = 47.0245, lon = 28.8323, km = 2 }}

[[targets]]
name = "At home"
kind = "keep"
[[targets.filters]]
sources = ["camera"]
geo = "@home"

[[targets]]
name = "All"
[[targets.filters]]
sources = ["camera"]
"#
        ));
        let shots = [
            photo("camera", "h.jpg", "2026:08:01 10:00:00", Some(HOME)),
            photo("camera", "f.jpg", "2026:08:01 10:00:00", Some(AWAY)),
            // No GPS: `geo` does not hold.
            photo("camera", "n.jpg", "2026:08:01 10:00:00", None),
        ];
        let files: Vec<FileEntry> = shots.iter().map(|(e, _)| e.clone()).collect();
        let exif: HashMap<PathBuf, Value> = shots.into_iter().map(|(e, x)| (e.path, x)).collect();
        let dir = tempfile::tempdir().unwrap();
        let ctx = with_groups(&c, &files, &exif, dir.path());
        let r = route(&c, &files, &ctx, &Panics, false).unwrap();
        assert_eq!(r[0].1, target("At home", Via::Mask));
        assert_eq!(r[1].1, target("All", Via::Mask));
        assert_eq!(r[2].1, target("All", Via::Mask));
        // The explain helper names the miss.
        let miss = first_miss(&c, &c.targets[0], &files[2], 2, &ctx).unwrap();
        assert_eq!(miss.condition, "geo");
        assert_eq!(first_miss(&c, &c.targets[0], &files[0], 0, &ctx), None);
        let miss = first_miss(&c, &c.targets[0], &file("a", "x.jpg"), 0, &ctx).unwrap();
        assert_eq!(miss.condition, "sources");
    }

    /// Three shots in one series, the middle one best.
    fn series_ctx(files: &[FileEntry]) -> RouteCtx<'static> {
        let mut ctx = RouteCtx::empty(files);
        let g = ctx.groups.to_mut();
        g.series = vec![Series {
            members: vec![0, 1, 2],
            best: 1,
        }];
        g.series_of = (0..3).map(|i| (i, 0)).collect();
        ctx
    }

    #[test]
    fn series_go_to_review_until_the_best_is_confirmed() {
        let c = toml_cfg(&format!(
            r#"{SOURCES}
[[targets]]
name = "Retakes"
kind = "trash"
[[targets.filters]]
sources = ["a"]
series = {{ keep = "best" }}

[[targets]]
name = "All"
[[targets.filters]]
sources = ["a"]
"#
        ));
        let files = [
            file("a", "s1.jpg"),
            file("a", "s2.jpg"),
            file("a", "s3.jpg"),
            file("a", "single.jpg"),
        ];
        let ctx = series_ctx(&files);
        let review = Decision::Review {
            suggested: "Retakes".into(),
            reason: crate::t!("route.series_review", files = crate::tn!("count.files", 3)),
            series: Some(SeriesReview {
                members: files[..3].iter().map(|f| f.path.clone()).collect(),
                best: files[1].path.clone(),
            }),
        };
        let r = route(&c, &files, &ctx, &Panics, false).unwrap();
        for d in &r[..3] {
            assert_eq!(d.1, review);
        }
        assert_eq!(r[3].1, target("All", Via::Mask));

        // The user confirmed s2 as best and sent the others to the trash.
        let mut o = Overrides::default();
        for (i, a) in [
            Action::Target {
                name: "Retakes".into(),
            },
            Action::SeriesBest,
            Action::Target {
                name: "Retakes".into(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            o.set(files[i].path.clone(), 1, 0, a);
        }
        let r = route_with_overrides(
            &c,
            &files,
            &ctx,
            &Panics,
            false,
            &o,
            &NoProgress,
            &Cancel::new(),
        )
        .unwrap();
        assert_eq!(r.overrides_applied, 2);
        assert_eq!(r.decisions[0].1, target("Retakes", Via::Manual));
        assert_eq!(r.decisions[1].1, target("All", Via::Mask));
        assert_eq!(r.decisions[2].1, target("Retakes", Via::Manual));
    }

    #[test]
    fn a_series_review_offers_only_the_shots_it_holds() {
        let c = toml_cfg(&format!(
            r#"{SOURCES}
[[targets]]
name = "Retakes"
kind = "trash"
[[targets.filters]]
sources = ["a"]
series = {{ keep = "best" }}

[[targets]]
name = "All"
[[targets.filters]]
sources = ["a"]
"#
        ));
        let files = [
            file("a", "s1.jpg"),
            file("a", "s2.jpg"),
            file("a", "s3.jpg"),
        ];
        let ctx = series_ctx(&files);
        let held = |o: &Overrides| {
            route_with_overrides(
                &c,
                &files,
                &ctx,
                &Panics,
                false,
                o,
                &NoProgress,
                &Cancel::new(),
            )
            .unwrap()
            .decisions
            .into_iter()
            .map(|(_, d)| match d {
                Decision::Review { series, .. } => series,
                _ => None,
            })
            .collect::<Vec<_>>()
        };
        let all = Action::Target { name: "All".into() };
        let review = |m: &[usize], best: usize| {
            Some(SeriesReview {
                members: m.iter().map(|&i| files[i].path.clone()).collect(),
                best: files[best].path.clone(),
            })
        };
        // s1 was routed elsewhere by hand: only s2 and s3 are offered.
        let mut o = Overrides::default();
        o.set(files[0].path.clone(), 1, 0, all.clone());
        assert_eq!(held(&o), [None, review(&[1, 2], 1), review(&[1, 2], 1)]);
        // The suggested best was routed elsewhere: the first held one leads.
        let mut o = Overrides::default();
        o.set(files[1].path.clone(), 1, 0, all);
        assert_eq!(held(&o), [review(&[0, 2], 0), None, review(&[0, 2], 0)]);
    }

    #[test]
    fn props_not_negates_sharpness_with_the_rest() {
        let c = toml_cfg(&format!(
            r#"{SOURCES}
[[targets]]
name = "Not blurry"
[[targets.filters]]
sources = ["a"]
props = {{ sharpness = "<40", not = true }}
"#
        ));
        let files = [
            file("a", "sharp.jpg"),
            file("a", "blurry.jpg"),
            file("a", "clip.mp4"),
        ];
        let mut ctx = RouteCtx::empty(&files);
        let g = ctx.groups.to_mut();
        g.sharpness.insert(0, 80.0);
        g.sharpness.insert(1, 20.0);
        let r = route(&c, &files, &ctx, &Panics, false).unwrap();
        assert_eq!(r[0].1, target("Not blurry", Via::Mask));
        assert_eq!(r[1].1, Decision::Unmatched);
        // No score: the sub-option is false, so `not` makes it hold.
        assert_eq!(r[2].1, target("Not blurry", Via::Mask));
    }
}
