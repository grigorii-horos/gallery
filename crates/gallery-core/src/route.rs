//! Routing over the ordered target list: mask targets decide at once, AI
//! targets share one batch.

use serde::{Deserialize, Serialize};

use crate::config::{Config, Filter, Target};
use crate::mask::MaskExpr;
use crate::overrides::{Action, Overrides};
use crate::progress::{Cancel, NoProgress, Progress, ProgressEvent, Stage};
use crate::scan::FileEntry;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Target {
        name: String,
        via: Via,
    },
    Review {
        suggested: String,
        reason: String,
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
    let file_name = entry
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if !name.as_ref().is_none_or(|m| m.matches(file_name, cfg)) {
        Fit::No
    } else if exclude.matches_veto(file_name, cfg) {
        Fit::Excluded
    } else {
        Fit::Match
    }
}

/// How a file fares against a filter's sources and masks only (its other
/// conditions, e.g. `ai`, are not looked at).
pub(crate) fn filter_fit(cfg: &Config, f: &Filter, entry: &FileEntry) -> Fit {
    if !f.sources.contains(&entry.source_id) {
        return Fit::No;
    }
    name_fit(cfg, &f.matches, &f.exclude, entry)
}

fn filter_matches(cfg: &Config, f: &Filter, entry: &FileEntry) -> bool {
    filter_fit(cfg, f, entry) == Fit::Match
}

/// Some filter would take `entry` but for an exclude mask.
fn excluded_somewhere(cfg: &Config, entry: &FileEntry) -> bool {
    cfg.targets
        .iter()
        .flat_map(|t| &t.filters)
        .any(|f| filter_fit(cfg, f, entry) == Fit::Excluded)
}

/// The first of `targets` with a non-AI filter that matches `entry`.
fn first_mask_target<'a>(
    cfg: &Config,
    targets: &'a [Target],
    entry: &FileEntry,
) -> Option<&'a str> {
    targets
        .iter()
        .find(|t| {
            t.filters
                .iter()
                .any(|f| f.ai.is_none() && filter_matches(cfg, f, entry))
        })
        .map(|t| t.name.as_str())
}

/// AI categories `entry` can reach: (target name, description) of every AI
/// target with an AI filter whose sources and masks match; descriptions of
/// several such filters of one target are joined with "; ".
fn ai_categories(cfg: &Config, entry: &FileEntry) -> Vec<(String, String)> {
    cfg.targets
        .iter()
        .filter_map(|t| {
            let descs: Vec<&str> = t
                .filters
                .iter()
                .filter(|f| filter_matches(cfg, f, entry))
                .filter_map(|f| f.ai.as_deref())
                .collect();
            (!descs.is_empty()).then(|| (t.name.clone(), descs.join("; ")))
        })
        .collect()
}

/// Position of the first AI target (the list's length when there is none).
/// TEMPORARY, until Task 5: routing decides the targets before it at once
/// and the mask filters from it on after the one AI batch.
pub fn ai_cut(cfg: &Config) -> usize {
    cfg.targets
        .iter()
        .position(Target::has_ai)
        .unwrap_or(cfg.targets.len())
}

fn mask_decision(name: &str) -> Decision {
    Decision::Target {
        name: name.to_string(),
        via: Via::Mask,
    }
}

/// Route every file to a target: mask targets before the first AI target,
/// then one AI batch for files with at least one candidate, then the mask
/// filters of the targets after it; otherwise `Excluded` when an exclude
/// mask turned the file away, else `Unmatched`.
pub fn route(
    cfg: &Config,
    files: &[FileEntry],
    ai: &dyn Classifier,
    accept_low: bool,
) -> Result<Vec<(FileEntry, Decision)>> {
    route_with(cfg, files, ai, accept_low, &NoProgress, &Cancel::new())
}

/// [`route`] with the classifier's [`Stage::Ai`] events; a cancel ends it
/// with `Error::Cancelled` (the classifier stops between requests).
pub fn route_with(
    cfg: &Config,
    files: &[FileEntry],
    ai: &dyn Classifier,
    accept_low: bool,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<Vec<(FileEntry, Decision)>> {
    let empty = Overrides::default();
    route_with_overrides(cfg, files, ai, accept_low, &empty, progress, cancel).map(|r| r.decisions)
}

pub(crate) fn target_known(cfg: &Config, name: &str) -> bool {
    cfg.target(name).is_some()
}

/// [`route_with`] where manual decisions come first: a fresh entry of
/// `overrides` (same size and mtime) decides the file before any mask or
/// the AI. An entry naming a target the config does not have is ignored
/// with a warning; stale entries are ignored silently.
pub fn route_with_overrides(
    cfg: &Config,
    files: &[FileEntry],
    ai: &dyn Classifier,
    accept_low: bool,
    overrides: &Overrides,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<Routed> {
    cancel.check()?;
    let mut decisions: Vec<Option<Decision>> = vec![None; files.len()];
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
            None => {}
        }
    }

    // TEMPORARY (until Task 5's AI pause/resume): the list is cut at its
    // first AI target. Targets before it decide at once, AI targets share
    // one batch, and the non-AI filters of the targets from the cut on come
    // last. For a config migrated from the three stages this is exactly
    // `before_ai`, `ai`, `after_ai`.
    let (before, rest) = cfg.targets.split_at(ai_cut(cfg));

    for (i, entry) in files.iter().enumerate() {
        if decisions[i].is_some() {
            continue;
        }
        if let Some(name) = first_mask_target(cfg, before, entry) {
            decisions[i] = Some(mask_decision(name));
        }
    }

    // Undecided files that have at least one AI candidate, in file order.
    let mut idx = Vec::new();
    let mut batch = Vec::new();
    for (i, entry) in files.iter().enumerate() {
        if decisions[i].is_some() {
            continue;
        }
        let categories = ai_categories(cfg, entry);
        if !categories.is_empty() {
            idx.push(i);
            let source = cfg.sources.iter().find(|s| s.id == entry.source_id);
            let examples = cfg
                .ai_examples
                .get(&entry.source_id)
                .map(|c| crate::learn::render_examples(c, &categories, cfg.ai.examples))
                .unwrap_or_default();
            batch.push(AiRequest {
                entry: entry.clone(),
                categories,
                prompt: SourcePrompt {
                    prefix: source.and_then(|s| s.ai_prefix.clone()),
                    suffix: source.and_then(|s| s.ai_suffix.clone()),
                    examples,
                    language: cfg.ai.language.clone(),
                },
            });
        }
    }

    // Files the AI gets to judge: its "none" is a verdict, so such a file is
    // unmatched rather than excluded.
    let asked: Vec<bool> = {
        let mut a = vec![false; files.len()];
        for &i in &idx {
            a[i] = true;
        }
        a
    };
    if !batch.is_empty() {
        let candidates: Vec<Vec<(String, String)>> =
            batch.iter().map(|r| r.categories.clone()).collect();
        let answers = ai.classify_with(batch, progress, cancel)?;
        if answers.len() != idx.len() {
            return Err(Error::Ai(crate::t!(
                "route.answer_count",
                answers = answers.len(),
                requests = idx.len()
            )));
        }
        for ((i, cands), answer) in idx.into_iter().zip(candidates).zip(answers) {
            if let Some(cat) = &answer.category
                && !cands.iter().any(|(n, _)| n == cat)
            {
                return Err(Error::Ai(crate::t!(
                    "route.unknown_category",
                    cat = cat,
                    path = files[i].path.display()
                )));
            }
            if answer.confidence == Confidence::Low && !accept_low {
                decisions[i] = Some(Decision::Review {
                    suggested: answer.category.unwrap_or_else(|| "—".to_string()),
                    reason: answer.reason,
                });
            } else if let Some(name) = answer.category {
                decisions[i] = Some(Decision::Target {
                    name,
                    via: Via::Ai {
                        reason: answer.reason,
                        low: answer.confidence == Confidence::Low,
                    },
                });
            }
        }
    }

    let decisions = files
        .iter()
        .zip(decisions)
        .zip(asked)
        .map(|((entry, d), asked)| {
            let d = d
                .or_else(|| first_mask_target(cfg, rest, entry).map(mask_decision))
                .unwrap_or_else(|| {
                    if !asked && excluded_somewhere(cfg, entry) {
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
        let r = route(&c, &files, &fake, false).unwrap();
        assert_eq!(r[0].1, Decision::Unmatched);
        assert_eq!(r[1].1, target("rest", Via::Mask));
        assert_eq!(r[2].1, Decision::Excluded);
        assert_eq!(r[3].1, Decision::Unmatched);
        // Without an AI candidate the mask-excluded png is left alone.
        let mut shots = mask_target("shots", "a", "*.png");
        shots.filters[0].exclude = MaskExpr::parse("2026*").unwrap();
        let c = cfg(vec![shots], vec![], vec![]);
        let r = route(&c, &[file("a", "20260412_103314.png")], &Panics, false).unwrap();
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
        let r = route(&c, &[file("a", "x.jpg")], &Panics, false).unwrap();
        assert_eq!(r[0].1, target("one", Via::Mask));
    }

    #[test]
    fn masked_file_not_sent_to_ai() {
        let c = cfg(
            vec![mask_target("shots", "a", "Screenshot*")],
            vec![ai_target("cats", None)],
            vec![],
        );
        let r = route(&c, &[file("a", "Screenshot_1.png")], &Panics, false).unwrap();
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
        let r = route(&c, &files, &fake, false).unwrap();
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
    fn ordered_list_cuts_at_the_first_ai_target() {
        // A target with two AI filters, and a mask filter next to one of
        // them: until Task 5 the mask filter only decides after the batch.
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
        let r = route(&c, &files, &fake, false).unwrap();
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
        route(&c, &[file("a", "1.jpg"), file("b", "2.jpg")], &fake, false).unwrap();
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
        let r = route(&c, &[file("a", "p.jpg")], &fake, false).unwrap();
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
        let r = route(&c, &[file("a", "p.jpg"), file("a", "q.jpg")], &fake, false).unwrap();
        assert_eq!(
            r[0].1,
            Decision::Review {
                suggested: "cats".into(),
                reason: "why".into()
            }
        );
        assert_eq!(
            r[1].1,
            Decision::Review {
                suggested: "—".into(),
                reason: "why".into()
            }
        );
    }

    #[test]
    fn low_accepted_with_flag() {
        let c = cfg(vec![], vec![ai_target("cats", None)], vec![]);
        let fake = Fake::new(vec![ans(Some("cats"), Confidence::Low)]);
        let r = route(&c, &[file("a", "p.jpg")], &fake, true).unwrap();
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
        let r = route(&c, &[file("a", "p.jpg")], &Panics, false).unwrap();
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
        let r = route(&c, &files, &Panics, false).unwrap();
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
        let r = route(&c, &[file("a", "a.gif")], &fake, false).unwrap();
        assert!(fake.seen.borrow().is_empty());
        assert_eq!(r[0].1, Decision::Excluded);
    }

    #[test]
    fn exclude_mask_drops_file_from_ai_prefilter() {
        let mut t = ai_target("cats", None);
        t.filters[0].exclude = MaskExpr::parse("*.gif").unwrap();
        let c = cfg(vec![], vec![t], vec![]);
        let fake = Fake::new(vec![ans(Some("cats"), Confidence::Ok)]);
        let r = route(&c, &[file("a", "a.gif"), file("a", "b.jpg")], &fake, false).unwrap();
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
        let e = route(&c, &[file("a", "p.jpg")], &fake, false).unwrap_err();
        assert!(matches!(e, Error::Ai(_)));
    }

    fn routed_with(c: &Config, files: &[FileEntry], ai: &dyn Classifier, o: &Overrides) -> Routed {
        route_with_overrides(c, files, ai, false, o, &NoProgress, &Cancel::new()).unwrap()
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
        route(&c, &[file("a", "p.jpg")], &fake, false).unwrap();
        assert_eq!(
            fake.seen.borrow()[0].prompt.examples,
            vec!["grey cat — AI chose cats, correct is dogs"]
        );

        // `examples = 0` turns the learning off.
        c.ai.examples = 0;
        let fake = Fake::new(vec![ans(Some("cats"), Confidence::Ok)]);
        route(&c, &[file("a", "p.jpg")], &fake, false).unwrap();
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
        let r = route_with(&c, &files, &fake, false, &rec, &Cancel::new()).unwrap();
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
        let r = route_with(&c, &files, &Panics, false, &NoProgress, &cancel);
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    }
}
