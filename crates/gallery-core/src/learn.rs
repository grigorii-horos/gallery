//! "AI learns from manual decisions": when the user overrides what the AI
//! decided (or resolves a doubtful answer), the correction is kept as text
//! and shown to the AI as an example in later requests for the same source.
//!
//! The store is an append-only jsonl file: a line is written with one
//! `write` on an `O_APPEND` file, so concurrent writers never interleave.
//! Removing lines rewrites the file through a temp file and a rename. No
//! images are stored.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{Config, expand_tilde};
use crate::overrides::Action;
use crate::plan::Plan;
use crate::route::Via;
use crate::{Error, Result};

/// Placeholder for "the AI had no category" / "keep in place" in a line.
const NO_CATEGORY: &str = "nothing";
const KEEP: &str = "leave in place";
/// A reason longer than this is cut in the prompt line.
const MAX_REASON_CHARS: usize = 120;

/// One correction: what the AI decided for a file and what the user chose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Correction {
    /// Stable id (for deleting one example).
    pub id: String,
    pub source_id: String,
    /// File name only: examples are text, not a path index.
    pub file: String,
    /// The AI's category; `None` when it had none (or the file was in review
    /// without a suggestion).
    pub ai_suggested: Option<String>,
    /// The AI's reason: the user-visible description of the example.
    pub ai_reason: String,
    pub user_choice: Action,
    /// RFC 3339 local time.
    pub at: String,
}

impl Correction {
    pub fn new(
        source_id: &str,
        file: &str,
        ai_suggested: Option<String>,
        ai_reason: &str,
        user_choice: Action,
        at: &str,
    ) -> Correction {
        let mut h = Sha256::new();
        h.update(
            format!("{source_id}|{file}|{ai_suggested:?}|{ai_reason}|{user_choice:?}|{at}")
                .as_bytes(),
        );
        let id = h
            .finalize()
            .iter()
            .take(6)
            .map(|b| format!("{b:02x}"))
            .collect();
        Correction {
            id,
            source_id: source_id.into(),
            file: file.into(),
            ai_suggested,
            ai_reason: ai_reason.into(),
            user_choice,
            at: at.into(),
        }
    }

    fn same_lesson(&self, o: &Correction) -> bool {
        self.source_id == o.source_id
            && one_line(&self.ai_reason) == one_line(&o.ai_reason)
            && self.ai_suggested == o.ai_suggested
            && self.user_choice == o.user_choice
    }
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The corrections file.
#[derive(Debug, Clone)]
pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn new(path: impl Into<PathBuf>) -> Store {
        Store { path: path.into() }
    }

    /// `<user data dir>/gallery/ai-examples.jsonl`.
    pub fn default_path() -> Option<PathBuf> {
        crate::paths::data_dir().map(|d| d.join("ai-examples.jsonl"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every correction, oldest first; a missing file is empty and broken
    /// lines are skipped.
    pub fn load(&self) -> Vec<Correction> {
        std::fs::read_to_string(&self.path)
            .map(|t| {
                t.lines()
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Append corrections in one write.
    pub fn append(&self, items: &[Correction]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let mut buf = String::new();
        for c in items {
            buf.push_str(&serde_json::to_string(c).expect("correction serializes"));
            buf.push('\n');
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(buf.as_bytes())?;
        f.flush()?;
        Ok(())
    }

    /// Keep only the corrections `keep` accepts; how many were dropped. The
    /// file is rewritten (atomically) only when something goes.
    fn retain(&self, keep: impl Fn(&Correction) -> bool) -> Result<usize> {
        let all = self.load();
        let kept: Vec<_> = all.iter().filter(|c| keep(c)).cloned().collect();
        let dropped = all.len() - kept.len();
        if dropped == 0 {
            return Ok(0);
        }
        let dir = match self.path.parent() {
            Some(d) if !d.as_os_str().is_empty() => d,
            _ => Path::new("."),
        };
        let mut tmp = tempfile::Builder::new()
            .prefix(".ai-examples.")
            .suffix(".tmp")
            .tempfile_in(dir)?;
        for c in &kept {
            writeln!(
                tmp,
                "{}",
                serde_json::to_string(c).expect("correction serializes")
            )?;
        }
        tmp.as_file().sync_all()?;
        tmp.persist(&self.path).map_err(|e| Error::Io(e.error))?;
        Ok(dropped)
    }

    /// Delete one correction by id; whether there was one.
    pub fn remove(&self, id: &str) -> Result<bool> {
        Ok(self.retain(|c| c.id != id)? > 0)
    }

    /// Delete all corrections, or those of one source; how many.
    pub fn clear(&self, source: Option<&str>) -> Result<usize> {
        self.retain(|c| source.is_some_and(|s| c.source_id != s))
    }

    /// Size and mtime of the file, `"none"` when absent: changes whenever
    /// examples change (the current plan's fingerprint uses it).
    pub fn stamp(&self) -> String {
        std::fs::metadata(&self.path)
            .ok()
            .and_then(|m| {
                crate::scan::mtime_ns(&m)
                    .ok()
                    .map(|t| format!("{}|{t}", m.len()))
            })
            .unwrap_or_else(|| "none".into())
    }
}

/// The prompt lines of the examples that apply to a request whose candidate
/// categories are `candidates`: at most `limit` most recent distinct
/// corrections, oldest first (so the prompt changes only when the set does).
/// A correction naming a category that is not a candidate (a removed or
/// renamed target, or one this file is not offered) is skipped.
pub fn render_examples(
    corrections: &[Correction],
    candidates: &[(String, String)],
    limit: usize,
) -> Vec<String> {
    let known = |n: &str| candidates.iter().any(|(c, _)| c == n);
    let mut picked: Vec<&Correction> = Vec::new();
    for c in corrections.iter().rev() {
        if limit == 0 || picked.len() >= limit {
            break;
        }
        let valid = c.ai_suggested.as_deref().is_none_or(known)
            && match &c.user_choice {
                Action::Target { name } => known(name),
                Action::Keep => true,
                // A series choice says nothing about categories.
                Action::SeriesBest => false,
            };
        if valid && !picked.iter().any(|p| p.same_lesson(c)) {
            picked.push(c);
        }
    }
    picked.reverse();
    picked.into_iter().map(line).collect()
}

fn line(c: &Correction) -> String {
    let mut reason = one_line(&c.ai_reason);
    if reason.chars().count() > MAX_REASON_CHARS {
        reason = reason.chars().take(MAX_REASON_CHARS).collect::<String>() + "…";
    }
    let chose = c.ai_suggested.as_deref().unwrap_or(NO_CATEGORY);
    let right = match &c.user_choice {
        Action::Target { name } => name.as_str(),
        // Never picked by `render_examples`.
        Action::Keep | Action::SeriesBest => KEEP,
    };
    format!("{reason} — AI chose {chose}, correct is {right}")
}

/// Corrections made by `decisions` (source path as the scan keys it, what the
/// user chose) against `plan` as it was before: a decision counts when the
/// plan had the file decided by the AI (an item with an AI reason) or held it
/// in review, and the user chose something else than the AI suggested.
pub fn corrections_from_plan(
    cfg: &Config,
    plan: &Plan,
    decisions: &[(PathBuf, Action)],
    at: &str,
) -> Vec<Correction> {
    let source_of = |path: &Path| {
        cfg.sources
            .iter()
            .filter(|s| path.starts_with(expand_tilde(&s.path)))
            .max_by_key(|s| expand_tilde(&s.path).components().count())
            .map(|s| s.id.clone())
    };
    let mut out = Vec::new();
    for (path, choice) in decisions {
        // Confirming a series' best shot corrects no AI answer.
        if *choice == Action::SeriesBest {
            continue;
        }
        let (suggested, reason) = if let Some(it) = plan.items.iter().find(|i| &i.source == path) {
            match &it.via {
                Via::Ai { reason, .. } => (Some(it.target.clone()), reason.clone()),
                _ => continue,
            }
        } else if let Some(r) = plan.review.iter().find(|r| &r.source == path) {
            if r.name_error {
                continue;
            }
            (
                Some(r.suggested.clone()).filter(|s| s != "—"),
                r.reason.clone(),
            )
        } else {
            continue;
        };
        if let Action::Target { name } = choice
            && suggested.as_deref() == Some(name)
        {
            continue;
        }
        let Some(source_id) = source_of(path) else {
            continue;
        };
        let file = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        out.push(Correction::new(
            &source_id,
            &file,
            suggested,
            &reason,
            choice.clone(),
            at,
        ));
    }
    out
}

impl Config {
    /// Load the user's corrections of every source from `store`, so that the
    /// AI requests built from this config carry them (see
    /// [`Config::ai_examples`]). Only callers that send a request need this:
    /// the examples are not part of the cache key, so cached answers are
    /// found (and stay valid) without them.
    pub fn attach_examples(&mut self, store: &Store) {
        self.ai_examples.clear();
        for c in store.load() {
            self.ai_examples
                .entry(c.source_id.clone())
                .or_default()
                .push(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cands() -> Vec<(String, String)> {
        ["Photos", "Receipts", "Memes"]
            .iter()
            .map(|n| (n.to_string(), String::new()))
            .collect()
    }

    fn tgt(n: &str) -> Action {
        Action::Target { name: n.into() }
    }

    fn corr(file: &str, sug: Option<&str>, reason: &str, choice: Action, at: &str) -> Correction {
        Correction::new("cam", file, sug.map(String::from), reason, choice, at)
    }

    #[test]
    fn line_format_and_order_oldest_first() {
        let cs = vec![
            corr(
                "a.jpg",
                Some("Photos"),
                "Lidl receipt",
                tgt("Receipts"),
                "1",
            ),
            corr("b.jpg", None, "funny picture", tgt("Memes"), "2"),
            corr("c.jpg", Some("Memes"), "chat screenshot", Action::Keep, "3"),
        ];
        assert_eq!(
            render_examples(&cs, &cands(), 20),
            vec![
                "Lidl receipt — AI chose Photos, correct is Receipts",
                "funny picture — AI chose nothing, correct is Memes",
                "chat screenshot — AI chose Memes, correct is leave in place",
            ]
        );
    }

    #[test]
    fn limit_keeps_most_recent_and_zero_disables() {
        let cs: Vec<_> = (0..5)
            .map(|i| {
                corr(
                    "f",
                    Some("Photos"),
                    &format!("r{i}"),
                    tgt("Receipts"),
                    &i.to_string(),
                )
            })
            .collect();
        let got = render_examples(&cs, &cands(), 2);
        assert_eq!(got.len(), 2);
        assert!(
            got[0].starts_with("r3") && got[1].starts_with("r4"),
            "{got:?}"
        );
        assert!(render_examples(&cs, &cands(), 0).is_empty());
    }

    #[test]
    fn duplicates_collapse_to_the_latest() {
        let cs = vec![
            corr("a", Some("Photos"), "receipt", tgt("Receipts"), "1"),
            corr("b", Some("Memes"), "meme", tgt("Photos"), "2"),
            corr("c", Some("Photos"), " receipt ", tgt("Receipts"), "3"),
        ];
        let got = render_examples(&cs, &cands(), 20);
        assert_eq!(got.len(), 2);
        assert!(
            got[0].starts_with("meme") && got[1].starts_with("receipt"),
            "{got:?}"
        );
    }

    #[test]
    fn stale_targets_are_skipped() {
        let cs = vec![
            corr("a", Some("Old"), "x", tgt("Receipts"), "1"),
            corr("b", Some("Photos"), "y", tgt("Deleted"), "2"),
            corr("c", Some("Photos"), "z", tgt("Receipts"), "3"),
        ];
        let got = render_examples(&cs, &cands(), 20);
        assert_eq!(got, vec!["z — AI chose Photos, correct is Receipts"]);
    }

    #[test]
    fn long_reason_is_cut() {
        let cs = vec![corr("a", None, &"я".repeat(500), Action::Keep, "1")];
        let l = &render_examples(&cs, &cands(), 20)[0];
        assert!(l.chars().count() < 200, "{l}");
    }

    #[test]
    fn append_load_remove_clear() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path().join("sub").join("ex.jsonl"));
        assert!(s.load().is_empty());
        let a = corr("a", None, "r1", Action::Keep, "1");
        let mut b = corr("b", None, "r2", Action::Keep, "2");
        b.source_id = "other".into();
        s.append(std::slice::from_ref(&a)).unwrap();
        s.append(&[b.clone()]).unwrap();
        assert_eq!(s.load(), vec![a.clone(), b.clone()]);
        assert!(s.remove(&a.id).unwrap());
        assert!(!s.remove(&a.id).unwrap());
        assert_eq!(s.load(), vec![b.clone()]);
        assert_eq!(s.clear(Some("cam")).unwrap(), 0);
        assert_eq!(s.clear(Some("other")).unwrap(), 1);
        assert!(s.load().is_empty());
        assert_eq!(s.clear(None).unwrap(), 0);
    }

    #[test]
    fn concurrent_appends_never_interleave() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path().join("ex.jsonl"));
        std::thread::scope(|sc| {
            for t in 0..8 {
                let s = &s;
                sc.spawn(move || {
                    for i in 0..50 {
                        let c = corr(
                            &format!("f{t}-{i}"),
                            Some("Photos"),
                            &"long reason ".repeat(200),
                            tgt("Receipts"),
                            &format!("{t}-{i}"),
                        );
                        s.append(&[c]).unwrap();
                    }
                });
            }
        });
        let text = std::fs::read_to_string(s.path()).unwrap();
        assert_eq!(text.lines().count(), 400);
        assert_eq!(s.load().len(), 400, "every line parses");
    }

    #[test]
    fn broken_lines_are_skipped_and_stamp_tracks_changes() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path().join("ex.jsonl"));
        assert_eq!(s.stamp(), "none");
        s.append(&[corr("a", None, "r", Action::Keep, "1")])
            .unwrap();
        let before = s.stamp();
        std::fs::write(
            s.path(),
            format!("{}garbage\n", std::fs::read_to_string(s.path()).unwrap()),
        )
        .unwrap();
        assert_eq!(s.load().len(), 1);
        assert_ne!(s.stamp(), before);
    }

    fn plan_with(items: Vec<crate::plan::PlanItem>, review: Vec<crate::plan::ReviewItem>) -> Plan {
        Plan {
            version: 1,
            created: String::new(),
            side: crate::plan::Side::Local,
            tz: None,
            content_presets: Default::default(),
            items,
            review,
            unmatched: vec![],
            warnings: vec![],
            unchanged: vec![],
            retry_of: None,
            kept: Vec::new(),
        }
    }

    fn item(path: &str, target: &str, via: Via) -> crate::plan::PlanItem {
        crate::plan::PlanItem {
            ai_meta: None,
            source: path.into(),
            source_root: "/cam".into(),
            size: 1,
            mtime_ns: 0,
            target: target.into(),
            via,
            outputs: vec![],
            kind: Default::default(),
            captured: None,
        }
    }

    fn review(path: &str, suggested: &str) -> crate::plan::ReviewItem {
        crate::plan::ReviewItem {
            source: path.into(),
            size: 1,
            mtime_ns: 0,
            suggested: suggested.into(),
            reason: "not sure".into(),
            captured: None,
            source_root: None,
            name_error: false,
            series: None,
            ai_meta: None,
        }
    }

    #[test]
    fn records_only_for_ai_decided_or_review_items() {
        let mut cfg = Config::default();
        cfg.sources.push(crate::config::Source {
            id: "cam".into(),
            path: "/cam".into(),
            server_path: None,
            recursive: false,
            skip: vec![],
            ai_prefix: None,
            ai_prefix_file: None,
            ai_suffix: None,
            ai_suffix_file: None,
        });
        let ai = Via::Ai {
            reason: "receipt".into(),
            low: false,
        };
        let plan = plan_with(
            vec![
                item("/cam/ai.jpg", "Photos", ai.clone()),
                item("/cam/same.jpg", "Photos", ai),
                item("/cam/mask.jpg", "Photos", Via::Mask),
                item("/cam/manual.jpg", "Photos", Via::Manual),
            ],
            vec![
                review("/cam/low.jpg", "Memes"),
                review("/cam/none.jpg", "—"),
            ],
        );
        let t = |n: &str| Action::Target { name: n.into() };
        let got = corrections_from_plan(
            &cfg,
            &plan,
            &[
                ("/cam/ai.jpg".into(), t("Receipts")),
                ("/cam/same.jpg".into(), t("Photos")), // confirmed, not corrected
                ("/cam/mask.jpg".into(), t("Receipts")),
                ("/cam/manual.jpg".into(), t("Receipts")),
                ("/cam/low.jpg".into(), Action::Keep),
                ("/cam/none.jpg".into(), t("Receipts")),
                ("/cam/unknown.jpg".into(), t("Receipts")),
                ("/elsewhere/x.jpg".into(), t("Receipts")),
            ],
            "now",
        );
        let view: Vec<_> = got
            .iter()
            .map(|c| {
                (
                    c.file.as_str(),
                    c.ai_suggested.as_deref(),
                    c.ai_reason.as_str(),
                )
            })
            .collect();
        assert_eq!(
            view,
            vec![
                ("ai.jpg", Some("Photos"), "receipt"),
                ("low.jpg", Some("Memes"), "not sure"),
                ("none.jpg", None, "not sure"),
            ]
        );
        assert!(got.iter().all(|c| c.source_id == "cam" && c.at == "now"));
    }
}
