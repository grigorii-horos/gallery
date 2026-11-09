//! "Live effect": what an unsaved config change does. Both configs are
//! routed and planned over the SAME scanned file list, without any AI call
//! (AI targets use cached answers only; a file that would need a fresh
//! answer is counted as waiting for the AI), then the fate of every file is
//! compared. Reads only.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::Result;
use crate::ai::cache::Cache;
use crate::ai::client::cached_answer;
use crate::config::Config;
use crate::overrides::Overrides;
use crate::plan::{OutputOp, Plan, Side, build_plan_with};
use crate::progress::{Cancel, NoProgress, Progress};
use crate::route::{AiAnswer, AiRequest, Classifier, Confidence, Decision, route_with_overrides};
use crate::scan::FileEntry;

/// Examples kept per outcome bucket.
pub const EXAMPLES_PER_BUCKET: usize = 5;

/// What becomes of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// A content step re-encodes it (compression, conversion).
    Compressed,
    /// Only moved or renamed (or copied as is).
    Renamed,
    /// Left exactly as it is.
    Unchanged,
    /// Turned away by an exclude mask.
    Excluded,
    /// Held for the user's review.
    Review,
    /// Nothing took it.
    Unmatched,
    /// Needs a fresh AI answer that is not cached.
    WaitingAi,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutputFate {
    pub dest: String,
    pub op: OutputOp,
    pub steps: Vec<String>,
}

/// The fate of one file under one config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fate {
    pub outcome: Outcome,
    pub target: Option<String>,
    pub outputs: Vec<OutputFate>,
    /// Short Russian text, e.g. `png-avif → x.avif`.
    pub label: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub compressed: usize,
    pub renamed: usize,
    pub unchanged: usize,
    pub excluded: usize,
    pub review: usize,
    pub unmatched: usize,
    pub waiting_ai: usize,
}

impl Counts {
    fn add(&mut self, o: Outcome) {
        let slot = match o {
            Outcome::Compressed => &mut self.compressed,
            Outcome::Renamed => &mut self.renamed,
            Outcome::Unchanged => &mut self.unchanged,
            Outcome::Excluded => &mut self.excluded,
            Outcome::Review => &mut self.review,
            Outcome::Unmatched => &mut self.unmatched,
            Outcome::WaitingAi => &mut self.waiting_ai,
        };
        *slot += 1;
    }

    pub fn total(&self) -> usize {
        self.compressed
            + self.renamed
            + self.unchanged
            + self.excluded
            + self.review
            + self.unmatched
            + self.waiting_ai
    }
}

/// A target's files before and after (files it takes, by outcome).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TargetDelta {
    pub name: String,
    pub before: Counts,
    pub after: Counts,
    /// Files taken before or after whose fate differs.
    pub changed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileChange {
    pub path: String,
    pub before: Fate,
    pub after: Fate,
}

/// Files whose fate changed and now end as `outcome`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExampleGroup {
    pub outcome: Outcome,
    pub count: usize,
    pub files: Vec<FileChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectDiff {
    pub files: usize,
    pub before: Counts,
    pub after: Counts,
    /// Files whose fate differs.
    pub changed: usize,
    /// Per target named in either config, in config order (new first).
    pub targets: Vec<TargetDelta>,
    pub examples: Vec<ExampleGroup>,
}

/// Answers from the cache only; a request without a cached answer is
/// remembered and answered "none".
pub struct CachedOnly<'a> {
    pub cache: &'a Cache,
    pub model: &'a str,
    pub pending: RefCell<HashSet<PathBuf>>,
}

impl Classifier for CachedOnly<'_> {
    fn classify(&self, batch: Vec<AiRequest>) -> Result<Vec<AiAnswer>> {
        Ok(batch
            .into_iter()
            .map(|r| {
                cached_answer(self.cache, self.model, &r).unwrap_or_else(|| {
                    self.pending.borrow_mut().insert(r.entry.path.clone());
                    AiAnswer {
                        category: None,
                        reason: String::new(),
                        confidence: Confidence::Ok,
                    }
                })
            })
            .collect())
    }
}

fn file_name(p: &str) -> &str {
    Path::new(p)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(p)
}

fn label(outcome: Outcome, target: &Option<String>, outputs: &[OutputFate]) -> String {
    let t = target.as_deref().unwrap_or("—");
    match outcome {
        Outcome::Unchanged if target.is_none() => crate::t!("effect.unchanged"),
        Outcome::Unchanged => crate::t!("effect.target_unchanged", target = t),
        Outcome::Excluded => crate::t!("effect.excluded"),
        Outcome::Review => crate::t!("effect.review"),
        Outcome::Unmatched => crate::t!("effect.unmatched"),
        Outcome::WaitingAi => crate::t!("effect.waiting_ai"),
        Outcome::Compressed | Outcome::Renamed => {
            let parts: Vec<String> = outputs
                .iter()
                .filter(|o| o.op != OutputOp::Keep)
                .map(|o| {
                    let what = if o.steps.is_empty() {
                        crate::t!("effect.rename")
                    } else {
                        o.steps.join(" + ")
                    };
                    format!("{what} → {}", file_name(&o.dest))
                })
                .collect();
            format!("{t}: {}", parts.join("; "))
        }
    }
}

fn fate(outcome: Outcome, target: Option<String>, outputs: Vec<OutputFate>) -> Fate {
    let label = label(outcome, &target, &outputs);
    Fate {
        outcome,
        target,
        outputs,
        label,
    }
}

/// Fate of every routed file under one config.
fn fates(
    cfg: &Config,
    files: &[FileEntry],
    exif: &HashMap<PathBuf, Value>,
    overrides: &Overrides,
    cache: &Cache,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<(HashMap<PathBuf, Fate>, HashSet<PathBuf>)> {
    let ai = CachedOnly {
        cache,
        model: &cfg.ai.model,
        pending: RefCell::new(HashSet::new()),
    };
    let routed = route_with_overrides(cfg, files, &ai, false, overrides, progress, cancel)?;
    let decisions = routed.decisions;
    let plan: Plan = build_plan_with(cfg, decisions.clone(), exif, Side::Local, progress, cancel)?;
    let by_item: HashMap<&Path, &crate::plan::PlanItem> =
        plan.items.iter().map(|i| (i.source.as_path(), i)).collect();
    let in_review: HashSet<&Path> = plan.review.iter().map(|r| r.source.as_path()).collect();
    let mut out = HashMap::new();
    for (entry, decision) in decisions {
        let target = match &decision {
            Decision::Target { name, .. } => Some(name.clone()),
            _ => None,
        };
        let f = match decision {
            Decision::Excluded => fate(Outcome::Excluded, None, vec![]),
            Decision::Kept => fate(Outcome::Unchanged, None, vec![]),
            Decision::Review { .. } => fate(Outcome::Review, None, vec![]),
            Decision::Unmatched => fate(Outcome::Unmatched, None, vec![]),
            Decision::Target { .. } => {
                if let Some(item) = by_item.get(entry.path.as_path()) {
                    let outputs: Vec<OutputFate> = item
                        .outputs
                        .iter()
                        .map(|o| OutputFate {
                            dest: o.dest.display().to_string(),
                            op: o.op,
                            steps: o.steps.clone(),
                        })
                        .collect();
                    let o = if outputs.iter().any(|o| !o.steps.is_empty()) {
                        Outcome::Compressed
                    } else if outputs.iter().any(|o| o.op != OutputOp::Keep) {
                        Outcome::Renamed
                    } else {
                        Outcome::Unchanged
                    };
                    fate(o, target, outputs)
                } else if in_review.contains(entry.path.as_path()) {
                    fate(Outcome::Review, target, vec![])
                } else {
                    // Planned as unchanged (or dropped): nothing happens.
                    fate(Outcome::Unchanged, target, vec![])
                }
            }
        };
        out.insert(entry.path, f);
    }
    Ok((out, ai.pending.into_inner()))
}

/// Compare what `old` and `new` do to `files` (see the module doc).
#[allow(clippy::too_many_arguments)]
pub fn diff(
    old: &Config,
    new: &Config,
    files: &[FileEntry],
    exif: &HashMap<PathBuf, Value>,
    overrides: &Overrides,
    ai_cache: &Cache,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<EffectDiff> {
    let (fo, po) = fates(old, files, exif, overrides, ai_cache, progress, cancel)?;
    let (fn_, pn) = fates(new, files, exif, overrides, ai_cache, progress, cancel)?;
    let waiting = |p: &PathBuf, f: &Fate, pending: &HashSet<PathBuf>| {
        if pending.contains(p) {
            fate(Outcome::WaitingAi, None, vec![])
        } else {
            f.clone()
        }
    };
    let mut before = Counts::default();
    let mut after = Counts::default();
    let mut changed = 0;
    let mut per: BTreeMap<String, TargetDelta> = BTreeMap::new();
    let mut groups: BTreeMap<Outcome, ExampleGroup> = BTreeMap::new();
    for e in files {
        cancel.check()?;
        let (Some(a), Some(b)) = (fo.get(&e.path), fn_.get(&e.path)) else {
            continue;
        };
        let (a, b) = (waiting(&e.path, a, &po), waiting(&e.path, b, &pn));
        before.add(a.outcome);
        after.add(b.outcome);
        let differs = a != b;
        for (f, is_new) in [(&a, false), (&b, true)] {
            if let Some(t) = &f.target {
                let d = per.entry(t.clone()).or_insert_with(|| TargetDelta {
                    name: t.clone(),
                    before: Counts::default(),
                    after: Counts::default(),
                    changed: 0,
                });
                if is_new {
                    d.after.add(f.outcome);
                } else {
                    d.before.add(f.outcome);
                }
            }
        }
        if differs {
            changed += 1;
            let mut names: Vec<&String> = [&a.target, &b.target].into_iter().flatten().collect();
            names.dedup();
            for n in names {
                if let Some(d) = per.get_mut(n) {
                    d.changed += 1;
                }
            }
            let g = groups.entry(b.outcome).or_insert_with(|| ExampleGroup {
                outcome: b.outcome,
                count: 0,
                files: vec![],
            });
            g.count += 1;
            if g.files.len() < EXAMPLES_PER_BUCKET {
                g.files.push(FileChange {
                    path: e.path.display().to_string(),
                    before: a,
                    after: b,
                });
            }
        }
    }
    let order = |c: &Config| -> Vec<String> {
        let t = &c.targets;
        t.before_ai
            .iter()
            .map(|m| &m.name)
            .chain(t.ai.iter().map(|a| &a.name))
            .chain(t.after_ai.iter().map(|m| &m.name))
            .cloned()
            .collect()
    };
    let mut targets = Vec::new();
    for n in order(new).into_iter().chain(order(old)) {
        if let Some(d) = per.remove(&n) {
            targets.push(d);
        }
    }
    targets.extend(per.into_values());
    Ok(EffectDiff {
        files: files.len(),
        before,
        after,
        changed,
        targets,
        examples: groups.into_values().collect(),
    })
}

/// [`diff`] without progress or cancellation.
pub fn diff_simple(
    old: &Config,
    new: &Config,
    files: &[FileEntry],
    exif: &HashMap<PathBuf, Value>,
    ai_cache: &Cache,
) -> Result<EffectDiff> {
    diff(
        old,
        new,
        files,
        exif,
        &Overrides::default(),
        ai_cache,
        &NoProgress,
        &Cancel::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::scan_sources;

    struct Fx {
        dir: tempfile::TempDir,
        files: Vec<FileEntry>,
    }

    fn fx() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        for n in [
            "20260716_211554.png",
            "20260717_101010.png",
            "shot.jpg",
            "note.txt",
            "keep_out.png",
        ] {
            std::fs::write(src.join(n), b"x").unwrap();
        }
        let cfg = load(&dir, "", "*.png", "", &[], "");
        let files = scan_sources(&cfg.sources).unwrap();
        Fx { dir, files }
    }

    /// One mask target `t` over `*.png`-like `m`, plus optional extras.
    fn load(
        dir: &tempfile::TempDir,
        head: &str,
        m: &str,
        exclude: &str,
        pipe: &[(&str, &str)],
        tail: &str,
    ) -> Config {
        let r = dir.path().to_string_lossy().replace('\\', "/");
        let ex = if exclude.is_empty() {
            String::new()
        } else {
            format!("exclude = \"{exclude}\"\n")
        };
        let content = pipe
            .iter()
            .find(|(k, _)| *k == "content")
            .map(|(_, v)| format!("content = [{v}]\n"))
            .unwrap_or_default();
        let rename = pipe
            .iter()
            .find(|(k, _)| *k == "rename")
            .map(|(_, v)| format!("rename = [{v}]\n"))
            .unwrap_or_default();
        let toml = format!(
            "[[sources]]\nid = \"s\"\npath = \"{r}/src\"\n{head}\n\
             [[targets.after_ai]]\nname = \"pngs\"\n[[targets.after_ai.filters]]\n\
             sources = [\"s\"]\nmatch = \"{m}\"\n{ex}\
             [[targets.after_ai.outputs]]\ndir = \"{r}/out\"\n{content}{rename}{tail}\n"
        );
        let c = Config::from_str_at(&toml, dir.path(), Path::new("t.toml")).unwrap();
        c.validate().unwrap();
        c
    }

    fn run(old: &Config, new: &Config, fx: &Fx) -> EffectDiff {
        diff_simple(
            old,
            new,
            &fx.files,
            &HashMap::new(),
            &Cache::load(&fx.dir.path().join("none")),
        )
        .unwrap()
    }

    fn target<'a>(d: &'a EffectDiff, n: &str) -> &'a TargetDelta {
        d.targets.iter().find(|t| t.name == n).unwrap()
    }

    #[test]
    fn no_change_is_empty() {
        let f = fx();
        let c = load(&f.dir, "", "*.png", "", &[], "");
        let d = run(&c, &c, &f);
        assert_eq!((d.files, d.changed), (5, 0));
        assert!(d.examples.is_empty());
        assert_eq!(d.before, d.after);
    }

    #[test]
    fn match_change_takes_more_files() {
        let f = fx();
        let old = load(&f.dir, "", "*.png", "", &[], "");
        let new = load(&f.dir, "", "*.{png,jpg}", "", &[], "");
        let d = run(&old, &new, &f);
        let t = target(&d, "pngs");
        assert_eq!((t.before.total(), t.after.total()), (3, 4));
        assert_eq!(d.before.unmatched, 2);
        assert_eq!(d.after.unmatched, 1);
        assert_eq!(d.changed, 1);
        assert!(d.examples[0].files[0].path.ends_with("shot.jpg"));
        assert_eq!(d.examples[0].files[0].before.outcome, Outcome::Unmatched);
    }

    #[test]
    fn exclude_change_drops_files() {
        let f = fx();
        let old = load(&f.dir, "", "*.png", "", &[], "");
        let new = load(&f.dir, "", "*.png", "keep_*", &[], "");
        let d = run(&old, &new, &f);
        assert_eq!(target(&d, "pngs").after.total(), 2);
        assert_eq!(d.after.excluded, 1);
        assert_eq!(d.changed, 1);
    }

    #[test]
    fn adding_png_avif_compresses_pngs() {
        let f = fx();
        let old = load(&f.dir, "", "*.png", "", &[], "");
        let new = load(&f.dir, "", "*.png", "", &[("content", "\"png-avif\"")], "");
        let d = run(&old, &new, &f);
        assert_eq!((d.before.compressed, d.after.compressed), (0, 3));
        assert_eq!(d.changed, 3);
        let ex = &d.examples[0];
        assert_eq!(ex.outcome, Outcome::Compressed);
        assert_eq!(ex.count, 3);
        let c = ex
            .files
            .iter()
            .find(|c| c.path.ends_with("20260716_211554.png"))
            .unwrap();
        assert!(
            c.after.label.contains("png-avif → 20260716_211554.avif"),
            "{}",
            c.after.label
        );
    }

    #[test]
    fn rename_preset_change_renames() {
        let f = fx();
        let old = load(&f.dir, "", "*.png", "", &[], "");
        let new = load(
            &f.dir,
            "",
            "*.png",
            "",
            &[("rename", "\"capture-date\"")],
            "",
        );
        let d = run(&old, &new, &f);
        assert!(d.after.renamed > 0 || d.after.unchanged > 0);
        assert!(d.changed > 0 || d.before == d.after);
        assert_eq!(d.after.compressed, 0);
    }

    #[test]
    fn reorder_changes_first_match() {
        let f = fx();
        let two = |a: &str, b: &str| {
            let r = f.dir.path().to_string_lossy().replace('\\', "/");
            let t = |n: &str, m: &str| {
                format!(
                    "[[targets.after_ai]]\nname = \"{n}\"\n[[targets.after_ai.filters]]\n\
                     sources = [\"s\"]\nmatch = \"{m}\"\n[[targets.after_ai.outputs]]\ndir = \"{r}/{n}\"\n"
                )
            };
            let toml = format!(
                "[[sources]]\nid = \"s\"\npath = \"{r}/src\"\n{}{}",
                t(a, if a == "all" { "*" } else { "*.png" }),
                t(b, if b == "all" { "*" } else { "*.png" })
            );
            let c = Config::from_str_at(&toml, f.dir.path(), Path::new("t.toml")).unwrap();
            c.validate().unwrap();
            c
        };
        let old = two("all", "pngs");
        let new = two("pngs", "all");
        let d = run(&old, &new, &f);
        assert_eq!(target(&d, "all").before.total(), 5);
        assert_eq!(target(&d, "all").after.total(), 2);
        assert_eq!(target(&d, "pngs").after.total(), 3);
        assert_eq!(d.changed, 3);
    }

    #[test]
    fn uncached_ai_files_wait_and_cached_ones_route() {
        let f = fx();
        let r = f.dir.path().to_string_lossy().replace('\\', "/");
        let cfg_toml = |extra: &str| {
            format!(
                "[[sources]]\nid = \"s\"\npath = \"{r}/src\"\n\
                 [[targets.ai]]\nname = \"pics\"\nsources = [\"s\"]\ndescription = \"pics\"\n\
                 prefilter = {{ match = \"{extra}\" }}\n[[targets.ai.outputs]]\ndir = \"{r}/pics\"\n"
            )
        };
        let mk = |m: &str| {
            let c = Config::from_str_at(&cfg_toml(m), f.dir.path(), Path::new("t.toml")).unwrap();
            c.validate().unwrap();
            c
        };
        let old = mk("*.png");
        let new = mk("*.{png,jpg}");
        let d = run(&old, &new, &f);
        assert_eq!(d.before.waiting_ai, 3);
        assert_eq!(d.after.waiting_ai, 4);
        assert_eq!(d.after.compressed + d.after.renamed, 0);
        // A cached answer routes the file.
        let mut cache = Cache::load(&f.dir.path().join("cache.jsonl"));
        let shot = f
            .files
            .iter()
            .find(|e| e.path.ends_with("shot.jpg"))
            .unwrap();
        let req = AiRequest {
            entry: shot.clone(),
            categories: vec![("pics".into(), "pics".into())],
            prompt: Default::default(),
        };
        let hash =
            crate::ai::cache::prompt_hash(&crate::ai::system_prompt(&req.categories, &req.prompt));
        cache
            .put(
                crate::ai::cache::key(shot, &new.ai.model, &hash),
                crate::ai::cache::Stored {
                    category: "pics".into(),
                    reason: "r".into(),
                    confidence: "OK".into(),
                },
            )
            .unwrap();
        let d = diff_simple(&old, &new, &f.files, &HashMap::new(), &cache).unwrap();
        assert_eq!(d.after.waiting_ai, 3);
        assert_eq!(d.after.renamed + d.after.unchanged, 1);
    }

    #[test]
    fn examples_are_capped() {
        let f = fx();
        let old = load(&f.dir, "", "*.png", "", &[], "");
        let new = load(&f.dir, "", "*", "", &[], "");
        let d = run(&old, &new, &f);
        assert!(
            d.examples
                .iter()
                .all(|g| g.files.len() <= EXAMPLES_PER_BUCKET)
        );
    }

    #[test]
    fn cancel_stops_the_diff() {
        let f = fx();
        let c = load(&f.dir, "", "*.png", "", &[], "");
        let cancel = Cancel::new();
        cancel.cancel();
        let r = diff(
            &c,
            &c,
            &f.files,
            &HashMap::new(),
            &Overrides::default(),
            &Cache::load(&f.dir.path().join("n")),
            &NoProgress,
            &cancel,
        );
        assert!(matches!(r, Err(crate::Error::Cancelled)));
    }
}
