//! "Why this file?": a read-only account of what a scan does with one
//! file. Routing and planning are the real ones (`route_with_overrides`,
//! `build_plan` on this single file), so the account cannot drift from what
//! a scan does; the per-filter and per-step detail is read off the same
//! helpers. The AI is never called: a cached answer is used when present.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::Result;
use crate::ai::AiClassifier;
use crate::ai::cache::{self, Cache};
use crate::ai::client::{system_prompt, to_answer};
use crate::config::{Config, Filter, Output, TargetKind, expand_tilde};
use crate::datetime::{capture_date, file_created};
use crate::mask::{ItemCheck, MaskCheck, MaskExpr};
use crate::media::{kind_of, split_name};
use crate::name::{NameCtx, NameSkip, name_skip};
use crate::overrides::Overrides;
use crate::plan::{OutputOp, Plan, Side, build_plan, local_zone, run_content_pipe};
use crate::progress::{Cancel, NoProgress};
use crate::route::{
    AiAnswer, AiRequest, Classifier, Confidence, Decision, Fit, RouteCtx, Via, filter_fit,
    route_with_overrides,
};
use crate::scan::{FileEntry, Rejection, mtime_ns, rejection};
use crate::tools::Tools;

/// Inputs of [`explain`] besides the config and the file.
#[derive(Default)]
pub struct ExplainOptions<'a> {
    /// Reads the file's EXIF for its capture date; without it the date comes
    /// from the file name and the file times only.
    pub tools: Option<&'a Tools>,
    /// The AI answer cache; `None` means the default one.
    pub ai_cache: Option<PathBuf>,
}

/// Where the file stands against the sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
    /// Under source `source`, and a scan lists it.
    Scanned {
        source: String,
    },
    Missing,
    NotFile,
    NoSource,
    Hidden {
        source: String,
        part: String,
    },
    Skip {
        source: String,
        part: String,
        glob: String,
    },
    NotRecursive {
        source: String,
    },
}

/// One filter (or an AI target's prefilter) against the file.
#[derive(Debug, Clone, Serialize)]
pub struct FilterCheck {
    /// Position among the target's filters; 0 for a prefilter.
    pub index: usize,
    /// The sources the filter covers; empty means none (a filter lists them).
    pub sources: Vec<String>,
    pub source_ok: bool,
    /// `match`; absent means any file.
    pub matches: Option<MaskCheck>,
    pub exclude: Option<MaskCheck>,
    pub fit: Fit,
    /// The filter asks the AI (its masks are the AI's prefilter).
    pub ai: bool,
}

/// One target of the ordered list, in list order.
#[derive(Debug, Clone, Serialize)]
pub struct TargetCheck {
    pub kind: TargetKind,
    /// Some filter of the target asks the AI.
    pub ai: bool,
    pub name: String,
    /// Mask filters; for an AI target the sources and prefilter as one.
    pub filters: Vec<FilterCheck>,
    /// The target's sources include the file's (AI targets) or some filter
    /// fits (mask targets): the target would take or consider the file.
    pub candidate: bool,
    /// The decision names this target.
    pub taken: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Target,
    Review,
    Excluded,
    Kept,
    Unmatched,
}

#[derive(Debug, Clone, Serialize)]
pub struct DecisionInfo {
    pub kind: DecisionKind,
    pub target: Option<String>,
    pub via: Option<Via>,
    /// Review: the AI's reason.
    pub detail: Option<String>,
}

/// What the AI stage knows about the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AiState {
    /// No AI target takes the file.
    NoCandidates,
    /// Decided before the AI stage (an override or a mask target).
    NotReached { candidates: Vec<String> },
    /// The file is a candidate but there is no cached answer.
    NotAsked { candidates: Vec<String> },
    Cached {
        category: Option<String>,
        reason: String,
        low: bool,
    },
}

/// One content or name step of an output.
#[derive(Debug, Clone, Serialize)]
pub struct StepCheck {
    pub index: usize,
    pub preset: String,
    /// The effective mask as written and with `@refs` expanded.
    pub mask: String,
    pub mask_expanded: String,
    pub matched: bool,
    /// Content steps: the bundle this step was expanded from.
    pub bundle: Option<String>,
    /// The name the step saw, and what it left.
    pub before: String,
    pub after: String,
    /// Name steps: why a matching step changed nothing.
    pub skipped: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutputCheck {
    pub content: Vec<StepCheck>,
    pub rename: Vec<StepCheck>,
    pub dest: PathBuf,
    pub op: OutputOp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The plan has an item for the file (something is written or moved).
    Process,
    /// Routed, but every output leaves the file as it is.
    Unchanged,
    Review,
    Unmatched,
    NotScanned,
}

#[derive(Debug, Clone, Serialize)]
pub struct Explanation {
    pub path: PathBuf,
    pub scope: Scope,
    /// Targets that list the file's source (and any that took it).
    pub stages: Vec<TargetCheck>,
    /// Names of the other targets, which do not concern the file's source.
    pub other_targets: Vec<String>,
    pub ai: AiState,
    pub decision: Option<DecisionInfo>,
    pub outcome: Outcome,
    /// Outputs of the target that took the file.
    pub outputs: Vec<OutputCheck>,
    /// One Russian line: what happens and, when nothing does, what to change.
    pub verdict: String,
}

/// Answers from the cache only; a miss answers "none" and is recorded.
struct CachedAi {
    model: String,
    cache: Cache,
    seen: RefCell<Vec<Option<AiAnswer>>>,
}

impl Classifier for CachedAi {
    fn classify(&self, batch: Vec<AiRequest>) -> Result<Vec<AiAnswer>> {
        Ok(batch
            .iter()
            .map(|r| {
                let system = system_prompt(&r.categories, &r.prompt);
                let key = cache::key(&r.entry, &self.model, &cache::prompt_hash(&system));
                let hit = self
                    .cache
                    .get(&key)
                    .and_then(|s| to_answer(s, &r.categories).ok());
                self.seen.borrow_mut().push(hit.clone());
                hit.unwrap_or(AiAnswer {
                    category: None,
                    reason: String::new(),
                    confidence: Confidence::Ok,
                })
            })
            .collect())
    }
}

fn file_name_of(p: &Path) -> &str {
    p.file_name().and_then(|n| n.to_str()).unwrap_or_default()
}

/// `.ext` of a name, for messages.
fn ext_text(name: &str) -> String {
    let (_, ext) = split_name(name);
    if ext.is_empty() {
        crate::t!("explain.no_ext_file")
    } else {
        format!(".{ext}")
    }
}

/// The source a path lies under and its path relative to the root.
fn locate(cfg: &Config, path: &Path) -> Vec<(usize, PathBuf, PathBuf)> {
    let canon = std::fs::canonicalize(path).ok();
    cfg.sources
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let root = expand_tilde(&s.path);
            if let Ok(rel) = path.strip_prefix(&root) {
                return Some((i, root.clone(), rel.to_path_buf()));
            }
            let (c, r) = (canon.as_ref()?, std::fs::canonicalize(&root).ok()?);
            let rel = c.strip_prefix(&r).ok()?.to_path_buf();
            Some((i, root, rel))
        })
        .collect()
}

fn filter_check(cfg: &Config, index: usize, f: &Filter, e: &FileEntry) -> FilterCheck {
    let name = file_name_of(&e.path);
    FilterCheck {
        index,
        sources: f.sources.clone(),
        source_ok: f.sources.contains(&e.source_id),
        matches: f.matches.as_ref().map(|m| m.check(name, cfg, false)),
        exclude: (!f.exclude.is_empty()).then(|| f.exclude.check(name, cfg, true)),
        fit: filter_fit(cfg, f, e),
        ai: f.ai.is_some(),
    }
}

/// Every target in list order, the order routing walks them.
fn stages(cfg: &Config, e: &FileEntry) -> Vec<TargetCheck> {
    cfg.targets
        .iter()
        .map(|t| {
            let filters: Vec<_> = t
                .filters
                .iter()
                .enumerate()
                .map(|(i, f)| filter_check(cfg, i, f, e))
                .collect();
            TargetCheck {
                kind: t.kind,
                ai: t.has_ai(),
                name: t.name.clone(),
                candidate: filters.iter().any(|f| f.fit == Fit::Match),
                filters,
                taken: false,
            }
        })
        .collect()
}

fn outputs_cfg<'a>(cfg: &'a Config, target: &str) -> &'a [Output] {
    cfg.target(target).map_or(&[], |t| t.outputs.as_slice())
}

fn mask_texts(m: &MaskExpr, cfg: &Config) -> (String, String) {
    (m.source(), m.expand(cfg))
}

/// The steps of the output the plan made, matched to the config's output by
/// its pipes (an output whose byte copy already exists is left out of the
/// plan, so positions can shift).
fn output_checks(
    cfg: &Config,
    target: &str,
    plan_outputs: &[crate::plan::PlannedOutput],
    file_name: &str,
    ctx: &NameCtx,
) -> Vec<OutputCheck> {
    use crate::name::Pipe;
    let cfg_outs = outputs_cfg(cfg, target);
    let mut cursor = 0;
    let mut result = Vec::new();
    for po in plan_outputs {
        let content_tr: Vec<_> = po
            .trace
            .iter()
            .filter(|t| t.pipe == Some(Pipe::Content))
            .collect();
        let name_tr: Vec<_> = po
            .trace
            .iter()
            .filter(|t| t.pipe == Some(Pipe::Name))
            .collect();
        let same = |o: &Output| {
            let Ok(ex) = cfg.expand_content(target, &o.content) else {
                return false;
            };
            ex.len() == content_tr.len()
                && o.rename.len() == name_tr.len()
                && ex
                    .iter()
                    .zip(&content_tr)
                    .all(|(s, t)| s.step.preset == t.preset)
                && o.rename
                    .iter()
                    .zip(&name_tr)
                    .all(|(s, t)| s.preset == t.preset)
        };
        let Some(pos) = cfg_outs[cursor.min(cfg_outs.len())..].iter().position(same) else {
            continue;
        };
        let o = &cfg_outs[cursor + pos];
        cursor += pos + 1;

        let mut before = file_name.to_string();
        let mut content = Vec::new();
        let expanded = cfg.expand_content(target, &o.content).unwrap_or_default();
        for (ex, t) in expanded.iter().zip(&content_tr) {
            let st = &ex.step;
            let mask = cfg
                .content_preset(&st.preset)
                .map(|p| st.mask.clone().unwrap_or_else(|| p.mask_or_default()));
            let (m, x) = mask.map_or_else(Default::default, |m| mask_texts(&m, cfg));
            content.push(StepCheck {
                index: t.index,
                preset: t.preset.clone(),
                mask: m,
                mask_expanded: x,
                matched: t.matched,
                bundle: t.bundle.clone(),
                before: before.clone(),
                after: t.after.clone(),
                skipped: None,
            });
            before = t.after.clone();
        }
        let mut rename = Vec::new();
        for (st, t) in o.rename.iter().zip(&name_tr) {
            let preset = cfg.name_preset(&st.preset);
            let mask = preset.map(|p| st.mask.clone().unwrap_or_else(|| p.mask_or_default()));
            let (m, x) = mask.map_or_else(Default::default, |m| mask_texts(&m, cfg));
            let cur = before.rsplit('/').next().unwrap_or(&before).to_string();
            let skipped = preset
                .filter(|_| t.matched)
                .and_then(|p| name_skip(cfg, p, &cur, ctx))
                .map(|s| match s {
                    NameSkip::SkipIf => crate::t!("explain.skip_if"),
                    NameSkip::NoDate => crate::t!("explain.no_date"),
                });
            rename.push(StepCheck {
                index: t.index,
                preset: t.preset.clone(),
                mask: m,
                mask_expanded: x,
                matched: t.matched,
                bundle: None,
                before: before.clone(),
                after: t.after.clone(),
                skipped,
            });
            before = t.after.clone();
        }
        result.push(OutputCheck {
            content,
            rename,
            dest: po.dest.clone(),
            op: po.op,
        });
    }
    result
}

/// "File not found", with a hint when the folder holds the same name under
/// another extension (a converted file).
fn missing_text(path: &Path) -> String {
    let stem = |p: &Path| {
        file_name_of(p)
            .split('.')
            .next()
            .unwrap_or_default()
            .to_string()
    };
    let want = stem(path);
    let own = file_name_of(path).to_string();
    let sibling = path
        .parent()
        .and_then(|d| std::fs::read_dir(d).ok())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !want.is_empty() && *n != own && n.split('.').next() == Some(want.as_str()))
        .min();
    match sibling {
        Some(n) => crate::t!("explain.missing_nearby", path = path.display(), name = n),
        None => crate::t!("explain.missing", path = path.display()),
    }
}

fn base(path: &Path, scope: Scope, verdict: String) -> Explanation {
    Explanation {
        path: path.to_path_buf(),
        scope,
        stages: Vec::new(),
        other_targets: Vec::new(),
        ai: AiState::NoCandidates,
        decision: None,
        outcome: Outcome::NotScanned,
        outputs: Vec::new(),
        verdict,
    }
}

/// Explain what a scan of `cfg` does with the file at `path`. Read-only; no
/// AI call (a cached answer is used). An error only for an unreadable config
/// part (a bad skip glob) or a failed planning of this very file.
pub fn explain(
    cfg: &Config,
    path: &Path,
    overrides: &Overrides,
    opts: &ExplainOptions,
) -> Result<Explanation> {
    let path = expand_tilde(path);
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir().map_or(path.clone(), |d| d.join(&path))
    };
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(_) => {
            return Ok(base(&path, Scope::Missing, missing_text(&path)));
        }
    };
    if !meta.is_file() {
        return Ok(base(&path, Scope::NotFile, crate::t!("explain.not_file")));
    }
    let found = locate(cfg, &path);
    if found.is_empty() {
        return Ok(base(&path, Scope::NoSource, crate::t!("explain.no_source")));
    }
    // The first source that lists the file; else the first rejection.
    let mut first_reject = None;
    let mut chosen = None;
    for (i, root, rel) in found {
        let src = &cfg.sources[i];
        match rejection(src, &rel)? {
            None => {
                chosen = Some((i, root, rel));
                break;
            }
            Some(r) => {
                first_reject.get_or_insert((i, r));
            }
        }
    }
    let Some((si, root, rel)) = chosen else {
        let (i, r) = first_reject.expect("a rejection when nothing is chosen");
        let id = cfg.sources[i].id.clone();
        let (scope, verdict) = match r {
            Rejection::Hidden(part) => {
                let v = crate::t!("explain.hidden", part = part);
                (Scope::Hidden { source: id, part }, v)
            }
            Rejection::Skip { part, glob } => {
                let v = crate::t!("explain.skipped", part = part, glob = glob, id = id);
                (
                    Scope::Skip {
                        source: id,
                        part,
                        glob,
                    },
                    v,
                )
            }
            Rejection::NotRecursive => {
                let v = crate::t!("explain.not_recursive", id = id);
                (Scope::NotRecursive { source: id }, v)
            }
        };
        return Ok(base(&path, scope, verdict));
    };
    let source_id = cfg.sources[si].id.clone();
    let entry = FileEntry {
        source_id: source_id.clone(),
        path: root.join(&rel),
        rel,
        size: meta.len(),
        mtime_ns: mtime_ns(&meta)?,
    };

    // Routing exactly as a scan does, with the AI answered from the cache.
    let model = cfg.ai.model.clone();
    let cache_path = opts
        .ai_cache
        .clone()
        .or_else(AiClassifier::default_cache_path);
    let ai = CachedAi {
        model,
        cache: cache_path.map_or_else(|| Cache::load(Path::new("")), |p| Cache::load(&p)),
        seen: RefCell::new(Vec::new()),
    };
    // TODO(T7): groups (events, neighbours, duplicates, series) are not
    // computed for one file, so their conditions do not hold here.
    let exif: HashMap<PathBuf, Value> = match opts.tools {
        Some(t) => crate::exif::read(t, std::slice::from_ref(&entry.path)).unwrap_or_default(),
        None => HashMap::new(),
    };
    let route_ctx = RouteCtx::without_groups(std::slice::from_ref(&entry), &exif);
    let routed = route_with_overrides(
        cfg,
        std::slice::from_ref(&entry),
        &route_ctx,
        &ai,
        false,
        overrides,
        &NoProgress,
        &Cancel::new(),
    )?;
    let decision = routed.decisions.into_iter().next().map(|(_, d)| d);
    let decision = decision.expect("one file routed gives one decision");

    let mut stages = stages(cfg, &entry);
    let candidates: Vec<String> = stages
        .iter()
        .filter(|t| t.filters.iter().any(|f| f.ai && f.fit == Fit::Match))
        .map(|t| t.name.clone())
        .collect();
    let seen = ai.seen.borrow();
    let ai_state = if candidates.is_empty() {
        AiState::NoCandidates
    } else {
        match seen.first() {
            None => AiState::NotReached { candidates },
            Some(None) => AiState::NotAsked { candidates },
            Some(Some(a)) => AiState::Cached {
                category: a.category.clone(),
                reason: a.reason.clone(),
                low: a.confidence == Confidence::Low,
            },
        }
    };

    let info = match &decision {
        Decision::Target { name, via } => DecisionInfo {
            kind: DecisionKind::Target,
            target: Some(name.clone()),
            via: Some(via.clone()),
            detail: None,
        },
        Decision::Review {
            suggested, reason, ..
        } => DecisionInfo {
            kind: DecisionKind::Review,
            target: Some(suggested.clone()),
            via: None,
            detail: Some(reason.clone()),
        },
        Decision::Excluded => none_decision(DecisionKind::Excluded),
        Decision::Kept => none_decision(DecisionKind::Kept),
        Decision::Unmatched => none_decision(DecisionKind::Unmatched),
    };
    if let Some(t) = &info.target
        && info.kind == DecisionKind::Target
    {
        // The first target of that name in list order is the one routing took.
        if let Some(c) = stages.iter_mut().find(|c| &c.name == t) {
            c.taken = true;
        }
    }

    // Targets for other sources are noise: name them once.
    let mut other_targets = Vec::new();
    let mut stages: Vec<TargetCheck> = stages
        .into_iter()
        .filter_map(|mut t| {
            if t.taken || t.filters.iter().any(|f| f.source_ok) {
                t.filters.retain(|f| f.source_ok);
                Some(t)
            } else {
                other_targets.push(t.name);
                None
            }
        })
        .collect();
    stages.shrink_to_fit();

    // Plan this one file as a scan would.
    let file_name = file_name_of(&entry.path).to_string();
    let plan: Plan = build_plan(
        cfg,
        vec![(entry.clone(), decision.clone())],
        &exif,
        Side::Local,
    )?;
    let zone = local_zone();
    let tz = zone
        .as_deref()
        .and_then(|z| z.parse::<chrono_tz::Tz>().ok());
    let capture = capture_date(
        &file_name,
        kind_of(&entry.path),
        exif.get(&entry.path).unwrap_or(&Value::Null),
        tz,
    );
    let ctx = NameCtx {
        capture,
        created: file_created(&entry.path),
        source_id,
    };
    let item = plan.items.first();
    let (outcome, outputs) = if let Some(it) = item {
        (
            Outcome::Process,
            output_checks(cfg, &it.target, &it.outputs, &file_name, &ctx),
        )
    } else if !plan.review.is_empty() {
        (Outcome::Review, Vec::new())
    } else if !plan.unmatched.is_empty() {
        (Outcome::Unmatched, Vec::new())
    } else {
        (Outcome::Unchanged, Vec::new())
    };
    // An all-Keep target has no plan item; show its outputs from a plan of
    // the same file without dropping the Keep ones.
    let outputs = if outcome == Outcome::Unchanged && info.kind == DecisionKind::Target {
        keep_outputs(cfg, &entry, &decision, &ctx)
    } else {
        outputs
    };

    let mut ex = Explanation {
        path: entry.path.clone(),
        scope: Scope::Scanned {
            source: entry.source_id.clone(),
        },
        stages,
        other_targets,
        ai: ai_state,
        decision: Some(info),
        outcome,
        outputs,
        verdict: String::new(),
    };
    ex.verdict = verdict(cfg, &ex, &entry, &plan, overrides);
    Ok(ex)
}

fn none_decision(kind: DecisionKind) -> DecisionInfo {
    DecisionInfo {
        kind,
        target: None,
        via: None,
        detail: None,
    }
}

/// Step detail for a target whose every output keeps the file: `build_plan`
/// lists such a file as unchanged and drops its item, so the pipes are run
/// again with the same helpers the planner uses.
fn keep_outputs(
    cfg: &Config,
    entry: &FileEntry,
    decision: &Decision,
    ctx: &NameCtx,
) -> Vec<OutputCheck> {
    let Decision::Target { name, .. } = decision else {
        return Vec::new();
    };
    let file_name = file_name_of(&entry.path);
    outputs_cfg(cfg, name)
        .iter()
        .map(|o| replay_output(cfg, name, o, file_name, ctx, &entry.path))
        .collect()
}

/// The content and name pipes of one output on `file_name`, as `build_plan`
/// runs them (same mask, extension and name-pipe code).
fn replay_output(
    cfg: &Config,
    target: &str,
    o: &Output,
    file_name: &str,
    ctx: &NameCtx,
    src: &Path,
) -> OutputCheck {
    let Ok(run) = run_content_pipe(cfg, target, o, file_name) else {
        return OutputCheck {
            content: Vec::new(),
            rename: Vec::new(),
            dest: PathBuf::from(file_name),
            op: OutputOp::Keep,
        };
    };
    let name = run.name;
    let mut before = file_name.to_string();
    let mut content = Vec::new();
    let expanded = cfg.expand_content(target, &o.content).unwrap_or_default();
    for (ex, t) in expanded.iter().zip(&run.trace) {
        let st = &ex.step;
        let mask = cfg
            .content_preset(&st.preset)
            .map(|p| st.mask.clone().unwrap_or_else(|| p.mask_or_default()));
        let (m, x) = mask.map_or_else(Default::default, |m| mask_texts(&m, cfg));
        content.push(StepCheck {
            index: t.index,
            preset: t.preset.clone(),
            mask: m,
            mask_expanded: x,
            matched: t.matched,
            bundle: t.bundle.clone(),
            before: before.clone(),
            after: t.after.clone(),
            skipped: None,
        });
        before = t.after.clone();
    }
    let steps: Vec<_> = o
        .rename
        .iter()
        .filter_map(|st| {
            cfg.name_preset(&st.preset).map(|p| {
                (
                    st.mask.clone().unwrap_or_else(|| p.mask_or_default()),
                    st.preset.as_str(),
                    p,
                )
            })
        })
        .collect();
    let mut rename = Vec::new();
    let mut final_name = name.clone();
    if let Ok(named) = crate::name::apply_name_pipe(cfg, &steps, &name, ctx) {
        let mut before = name.clone();
        for ((mask, preset_name, preset), t) in steps.iter().zip(&named.steps) {
            let cur = before.rsplit('/').next().unwrap_or(&before).to_string();
            let skipped = t
                .matched
                .then(|| name_skip(cfg, preset, &cur, ctx))
                .flatten()
                .map(|s| match s {
                    NameSkip::SkipIf => crate::t!("explain.skip_if"),
                    NameSkip::NoDate => crate::t!("explain.no_date"),
                });
            let (m, x) = mask_texts(mask, cfg);
            rename.push(StepCheck {
                index: t.index,
                preset: (*preset_name).to_string(),
                mask: m,
                mask_expanded: x,
                matched: t.matched,
                bundle: None,
                before: before.clone(),
                after: t.after.clone(),
                skipped,
            });
            before = t.after.clone();
        }
        final_name = named.rel_path().to_string_lossy().into_owned();
    }
    OutputCheck {
        content,
        rename,
        dest: PathBuf::from(final_name),
        op: if src.file_name().and_then(|n| n.to_str()) == Some(name.as_str()) {
            OutputOp::Keep
        } else {
            OutputOp::Rename
        },
    }
}

/// `[a, b]` list of a mask's items, as the user wrote them.
fn items_text(m: &MaskCheck) -> String {
    let parts: Vec<&str> = m.items.iter().map(|i| i.item.as_str()).collect();
    format!("[{}]", parts.join(", "))
}

/// A preset of the same kind as `like` (user's first, then built-in) whose
/// default mask takes `name`, that the output does not already list.
fn suggest_preset(cfg: &Config, like: &str, name: &str, used: &[&str]) -> Option<String> {
    let kind = std::mem::discriminant(cfg.content_preset(like)?);
    let mut cands: Vec<(String, crate::config::ContentPreset)> = cfg
        .content_presets
        .iter()
        .map(|(n, p)| (n.clone(), p.clone()))
        .collect();
    cands.extend(crate::presets::builtin_content_presets());
    cands
        .into_iter()
        .find(|(n, p)| {
            std::mem::discriminant(p) == kind
                && !used.contains(&n.as_str())
                && p.mask_or_default().matches(name, cfg)
        })
        .map(|(n, _)| n)
}

/// The positive item that took the name and the `!` item that then threw it
/// out, searched through references: `(taker, excluder, owner)`.
fn excluded_by(items: &[ItemCheck]) -> Option<(String, String, String)> {
    for i in items {
        if i.negated {
            continue;
        }
        let pos = i.children.iter().find(|c| !c.negated && c.hit);
        let neg = i.children.iter().find(|c| c.negated && c.hit);
        if let (Some(p), Some(n)) = (pos, neg) {
            return Some((p.item.clone(), n.item.clone(), i.item.clone()));
        }
        if let Some(found) = excluded_by(&i.children) {
            return Some(found);
        }
    }
    None
}

/// Why target `t`'s filter does not take `name`; the flag is set when a `!`
/// item cut the name off, the most telling miss.
fn why_filter_misses(t: &TargetCheck, name: &str) -> Option<(bool, String)> {
    let f = t.filters.iter().find(|f| f.source_ok)?;
    let ext = ext_text(name);
    if let Some(m) = &f.matches
        && !m.matched
    {
        if let Some(neg) = m.items.iter().find(|i| i.negated && i.hit) {
            return Some((
                true,
                crate::t!(
                    "explain.cut_off",
                    name = name,
                    item = neg.item,
                    target = t.name,
                    items = items_text(m)
                ),
            ));
        }
        if let Some((taker, neg, owner)) = excluded_by(&m.items) {
            let pattern = neg.trim_start_matches('!');
            let lead = if pattern == "*.av1.mp4" {
                crate::t!("explain.excl_av1", owner = owner)
            } else {
                crate::t!(
                    "explain.excl_other",
                    taker = taker,
                    owner = owner,
                    neg = neg
                )
            };
            return Some((
                false,
                crate::t!(
                    "explain.excl_hint",
                    lead = lead,
                    pattern = pattern,
                    target = t.name
                ),
            ));
        }
        let what = if ext.starts_with('.') {
            crate::t!("explain.extension", ext = ext)
        } else {
            crate::t!("explain.file_no_ext")
        };
        return Some((
            false,
            crate::t!(
                "explain.not_in_match",
                what = what,
                items = items_text(m),
                target = t.name
            ),
        ));
    }
    None
}

fn verdict(
    cfg: &Config,
    ex: &Explanation,
    entry: &FileEntry,
    plan: &Plan,
    overrides: &Overrides,
) -> String {
    let name = file_name_of(&entry.path);
    let d = ex.decision.as_ref().expect("scanned file has a decision");
    let target = d.target.clone().unwrap_or_default();
    match d.kind {
        DecisionKind::Kept => {
            return crate::t!("explain.kept");
        }
        DecisionKind::Review => {
            let r = d.detail.clone().unwrap_or_default();
            if let Some(it) = plan.review.first().filter(|r| r.name_error) {
                return crate::t!("explain.name_error", target = target, reason = it.reason);
            }
            return crate::t!("explain.ai_unsure", target = target, reason = r);
        }
        DecisionKind::Excluded => {
            let hit = ex.stages.iter().find_map(|t| {
                t.filters.iter().find_map(|f| {
                    (f.fit == Fit::Excluded)
                        .then_some(f.exclude.as_ref())
                        .flatten()
                        .map(|x| (t.name.clone(), x.clone()))
                })
            });
            return match hit {
                Some((t, x)) => {
                    let by = x
                        .items
                        .iter()
                        .find(|i| i.hit && !i.negated)
                        .map_or_else(|| x.source.clone(), |i| i.item.clone());
                    crate::t!("explain.excluded_fits", target = t, by = by)
                }
                None => crate::t!("explain.excluded"),
            };
        }
        DecisionKind::Unmatched => {
            match &ex.ai {
                AiState::NotAsked { candidates } => {
                    return crate::t!("explain.ai_not_asked", targets = candidates.join(", "));
                }
                AiState::Cached {
                    category: None,
                    reason,
                    ..
                } => {
                    return crate::t!("explain.ai_none", reason = reason);
                }
                _ => {}
            }
            let mut why: Vec<(bool, String)> = ex
                .stages
                .iter()
                .filter(|t| !t.ai)
                .filter_map(|t| why_filter_misses(t, name))
                .collect();
            if why.is_empty() {
                let srcs = &entry.source_id;
                return crate::t!("explain.no_filter", source = srcs);
            }
            // A name cut by a `!` item is the most telling miss.
            if let Some(i) = why.iter().position(|w| w.0) {
                why.swap(0, i);
            }
            let first = why.remove(0).1;
            return if why.is_empty() {
                first
            } else {
                crate::tn!("explain.and_more", why.len(), first = first)
            };
        }
        DecisionKind::Target => {}
    }

    let via = match &d.via {
        Some(Via::Manual) => crate::t!("explain.via_manual"),
        Some(Via::Ai { .. }) => crate::t!("explain.via_ai"),
        _ => crate::t!("explain.via_mask"),
    };
    if ex.outcome == Outcome::Process {
        let dest = ex
            .outputs
            .first()
            .map_or_else(String::new, |o| o.dest.display().to_string());
        return crate::t!(
            "explain.will_process",
            target = target,
            via = via,
            dest = dest
        );
    }
    // Routed, nothing to do. Did any output's content steps all miss?
    let _ = overrides;
    for o in &ex.outputs {
        if let Some(first) = o.content.first()
            && o.content.iter().all(|s| !s.matched)
        {
            let used: Vec<&str> = o.content.iter().map(|s| s.preset.as_str()).collect();
            let tip = match suggest_preset(cfg, &first.preset, name, &used) {
                Some(p) => crate::t!("explain.tip_add", preset = p),
                None => crate::t!("explain.tip_add_step", ext = ext_text(name)),
            };
            return crate::t!(
                "explain.no_content_step",
                target = target,
                preset = first.preset,
                mask = if first.mask_expanded.is_empty() || first.mask_expanded == first.mask {
                    first.mask.clone()
                } else {
                    format!("{} ({})", first.mask, first.mask_expanded)
                },
                ext = ext_text(name),
                tip = tip
            );
        }
    }
    crate::t!("explain.nothing_to_do", target = target, via = via)
}

impl Explanation {
    /// The explanation as a compact tree, for the terminal.
    pub fn to_text(&self) -> String {
        use std::fmt::Write;
        let mut o = String::new();
        let _ = writeln!(o, "{}", self.path.display());
        let _ = writeln!(
            o,
            "├─ {}",
            crate::t!("explain.tree.source", scope = scope_text(&self.scope))
        );
        if !self.stages.is_empty() {
            let _ = writeln!(o, "├─ {}", crate::t!("explain.tree.targets"));
        }
        for t in &self.stages {
            let mark = if t.taken {
                crate::t!("explain.mark_took")
            } else if t.candidate {
                crate::t!("explain.mark_fits")
            } else {
                crate::t!("explain.mark_no")
            };
            let name = if t.ai {
                format!("{} [{}]", t.name, crate::t!("explain.stage_ai"))
            } else {
                t.name.clone()
            };
            let _ = writeln!(o, "│  ├─ {name} — {mark}");
            for f in &t.filters {
                let _ = writeln!(o, "│  │  ├─ {}", filter_text(f));
                for m in [&f.matches, &f.exclude].into_iter().flatten() {
                    for it in &m.items {
                        item_lines(it, 0, &mut o);
                    }
                }
            }
        }
        if !self.other_targets.is_empty() {
            let _ = writeln!(o, "├─ {}", other_targets_text(self));
        }
        match &self.ai {
            AiState::NoCandidates | AiState::NotReached { .. } => {}
            AiState::NotAsked { candidates } => {
                let _ = writeln!(
                    o,
                    "├─ {}",
                    crate::t!(
                        "explain.tree.ai_not_asked",
                        candidates = candidates.join(", ")
                    )
                );
            }
            AiState::Cached {
                category,
                reason,
                low,
            } => {
                let none = crate::t!("explain.tree.no_category");
                let _ = writeln!(
                    o,
                    "├─ {}",
                    crate::t!(
                        "explain.tree.ai_cached",
                        category = category.as_deref().unwrap_or(&none),
                        unsure = if *low {
                            crate::t!("explain.tree.unsure")
                        } else {
                            String::new()
                        },
                        reason = reason
                    )
                );
            }
        }
        if let Some(d) = &self.decision {
            let text = match d.kind {
                DecisionKind::Target => {
                    crate::t!(
                        "explain.tree.d_target",
                        target = d.target.clone().unwrap_or_default()
                    )
                }
                DecisionKind::Review => crate::t!("explain.tree.d_review"),
                DecisionKind::Excluded => crate::t!("explain.tree.d_excluded"),
                DecisionKind::Kept => crate::t!("explain.tree.d_kept"),
                DecisionKind::Unmatched => crate::t!("explain.tree.d_unmatched"),
            };
            let _ = writeln!(o, "├─ {}", crate::t!("explain.tree.decision", text = text));
        }
        for (i, out) in self.outputs.iter().enumerate() {
            let _ = writeln!(
                o,
                "├─ {}",
                crate::t!(
                    "explain.tree.output",
                    n = i + 1,
                    dest = out.dest.display(),
                    op = op_text(out.op)
                )
            );
            for (kind, steps) in [
                (crate::t!("explain.kind_content"), &out.content),
                (crate::t!("explain.kind_name"), &out.rename),
            ] {
                for s in steps {
                    let state = match (&s.skipped, s.matched) {
                        (Some(why), _) => crate::t!("explain.step_skipped", why = why),
                        (None, true) if s.before == s.after => crate::t!("explain.step_same"),
                        (None, true) => crate::t!("explain.step_applied"),
                        (None, false) => crate::t!("explain.step_no_match"),
                    };
                    let mask = if s.mask_expanded.is_empty() || s.mask_expanded == s.mask {
                        s.mask.clone()
                    } else {
                        format!("{} = {}", s.mask, s.mask_expanded)
                    };
                    let preset = match &s.bundle {
                        Some(b) => format!("{b} › {}", s.preset),
                        None => s.preset.clone(),
                    };
                    let _ = writeln!(
                        o,
                        "│  ├─ {}",
                        crate::t!(
                            "explain.tree.step",
                            kind = kind,
                            preset = preset,
                            state = state,
                            mask = mask,
                            before = s.before,
                            after = s.after
                        )
                    );
                }
            }
        }
        let _ = writeln!(o, "└─ {}", self.verdict);
        o
    }
}

/// `│  │  │  • item = expanded — reason`, children indented.
fn item_lines(it: &ItemCheck, depth: usize, out: &mut String) {
    use std::fmt::Write;
    let ex = if it.expanded.is_empty() {
        String::new()
    } else {
        format!(" = {}", it.expanded)
    };
    let _ = writeln!(
        out,
        "│  │  │  {}• {}{ex} — {}",
        "  ".repeat(depth),
        it.item,
        it.reason
    );
    // The referenced mask's own items only when one of them did something.
    if it.children.iter().any(|c| c.hit) {
        for c in &it.children {
            item_lines(c, depth + 1, out);
        }
    }
}

/// "23 more targets not for source screenshots".
pub fn other_targets_text(ex: &Explanation) -> String {
    let n = ex.other_targets.len();
    let source = match &ex.scope {
        Scope::Scanned { source } => source.as_str(),
        _ => "",
    };
    crate::tn!("explain.other_targets", n, source = source)
}

fn op_text(op: OutputOp) -> String {
    match op {
        OutputOp::Write => crate::t!("explain.op_write"),
        OutputOp::Rename => crate::t!("explain.op_rename"),
        OutputOp::Keep => crate::t!("explain.op_keep"),
    }
}

fn scope_text(s: &Scope) -> String {
    match s {
        Scope::Scanned { source } => crate::t!("explain.scope.scanned", source = source),
        Scope::Missing => crate::t!("explain.scope.missing"),
        Scope::NotFile => crate::t!("explain.scope.not_file"),
        Scope::NoSource => crate::t!("explain.scope.no_source"),
        Scope::Hidden { source, part } => {
            crate::t!("explain.scope.hidden", source = source, part = part)
        }
        Scope::Skip { source, part, glob } => {
            crate::t!(
                "explain.scope.skip",
                source = source,
                part = part,
                glob = glob
            )
        }
        Scope::NotRecursive { source } => crate::t!("explain.scope.not_recursive", source = source),
    }
}

fn filter_text(f: &FilterCheck) -> String {
    let what = if f.ai {
        crate::t!("explain.prefilter")
    } else {
        crate::t!("explain.filter_n", n = f.index + 1)
    };
    if !f.source_ok {
        return crate::t!(
            "explain.filter_source",
            what = what,
            sources = f.sources.join(", ")
        );
    }
    let m = match &f.matches {
        None => crate::t!("explain.match_any"),
        Some(m) => crate::t!(
            if m.matched {
                "explain.match_took"
            } else {
                "explain.match_not_took"
            },
            items = items_text(m)
        ),
    };
    let x = match &f.exclude {
        None => String::new(),
        Some(x) => crate::t!(
            if x.matched {
                "explain.exclude_fired"
            } else {
                "explain.exclude_not_fired"
            },
            items = items_text(x)
        ),
    };
    crate::t!("explain.filter_line", what = what, m = m, x = x)
}

#[cfg(test)]
mod tests;
