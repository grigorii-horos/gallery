//! "AI examples": the user's corrections the core keeps for the AI
//! prompts (`gallery_core::learn`), as the Settings screen shows them.

use std::path::{Path, PathBuf};

use gallery_core::config::Config;
use gallery_core::learn::{Correction, Store, corrections_from_plan};
use gallery_core::overrides::Action;
use gallery_core::plan::Plan;
use serde::Serialize;

use crate::overrides::{OverrideRequest, OverrideResult};

/// The corrections file: `ai-examples.jsonl` in `$GALLERY_DATA_DIR` when set
/// (tests, screenshots), else the core's default.
pub fn default_file() -> Result<PathBuf, String> {
    match crate::data_dir_override() {
        Some(dir) => Ok(dir.join("ai-examples.jsonl")),
        None => Store::default_path().ok_or_else(|| gallery_core::t!("lock.no_data_dir")),
    }
}

/// Where corrections are recorded from: the store, and the plan the user was
/// looking at when deciding (it says which files the AI decided).
pub struct Learn<'a> {
    pub store: &'a Path,
    pub plan: &'a Path,
}

/// [`crate::overrides::set_many_in`] that also records the corrections the
/// decisions make (see [`corrections_from_plan`]). Recording is best effort:
/// a failure is reported on stderr and never refuses the decision.
pub fn set_many_learning_in(
    file: &Path,
    config: &Path,
    requests: Vec<OverrideRequest>,
    learn: Option<&Learn>,
) -> Result<Vec<OverrideResult>, String> {
    let actions: Vec<Action> = requests.iter().map(|r| r.action.clone()).collect();
    let results = crate::overrides::set_many_in(file, config, requests)?;
    if let Some(l) = learn {
        let decided: Vec<(PathBuf, Action)> = results
            .iter()
            .zip(actions)
            .filter_map(|(r, a)| r.key.as_ref().map(|k| (PathBuf::from(k), a)))
            .collect();
        if let Err(e) = record(l, config, &decided) {
            eprintln!("warning: could not save the correction for the AI: {e}");
        }
    }
    Ok(results)
}

fn record(l: &Learn, config: &Path, decided: &[(PathBuf, Action)]) -> Result<(), String> {
    if decided.is_empty() || !l.plan.is_file() {
        return Ok(());
    }
    let cfg = Config::load(config).map_err(|e| e.to_string())?;
    let plan = Plan::load(l.plan).map_err(|e| e.to_string())?;
    let at = chrono::Local::now().to_rfc3339();
    let found = corrections_from_plan(&cfg, &plan, decided, &at);
    Store::new(l.store)
        .append(&found)
        .map_err(|e| e.to_string())
}

/// One source's examples for the settings screen.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SourceExamples {
    pub source_id: String,
    pub count: usize,
    /// Newest first.
    pub items: Vec<ExampleView>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ExampleView {
    pub id: String,
    pub file: String,
    pub description: String,
    /// The AI's category, `None` when it had none.
    pub ai_suggested: Option<String>,
    /// The user's target, `None` for "leave in place".
    pub user_choice: Option<String>,
    pub at: String,
}

/// The stored examples grouped by source (sorted by id), newest first.
pub fn list_in(file: &Path) -> Vec<SourceExamples> {
    let mut by: std::collections::BTreeMap<String, Vec<Correction>> = Default::default();
    for c in Store::new(file).load() {
        by.entry(c.source_id.clone()).or_default().push(c);
    }
    by.into_iter()
        .map(|(source_id, cs)| SourceExamples {
            source_id,
            count: cs.len(),
            items: cs
                .into_iter()
                .rev()
                .map(|c| ExampleView {
                    id: c.id,
                    file: c.file,
                    description: c.ai_reason,
                    ai_suggested: c.ai_suggested,
                    user_choice: match c.user_choice {
                        Action::Target { name } => Some(name),
                        Action::Keep => None,
                    },
                    at: c.at,
                })
                .collect(),
        })
        .collect()
}

pub fn remove_in(file: &Path, id: &str) -> Result<bool, String> {
    Store::new(file).remove(id).map_err(|e| e.to_string())
}

/// Delete all examples, or those of one source; how many.
pub fn clear_in(file: &Path, source: Option<&str>) -> Result<usize, String> {
    Store::new(file).clear(source).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corr(source: &str, file: &str, at: &str) -> Correction {
        Correction::new(
            source,
            file,
            Some("Photos".into()),
            "receipt",
            Action::Target {
                name: "Receipts".into(),
            },
            at,
        )
    }

    #[test]
    fn list_groups_by_source_newest_first_and_delete_works() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("ex.jsonl");
        assert!(list_in(&f).is_empty());
        let s = Store::new(&f);
        let (a, b, c) = (
            corr("cam", "a", "1"),
            corr("phone", "b", "2"),
            corr("cam", "c", "3"),
        );
        s.append(&[a.clone(), b, c.clone()]).unwrap();
        let l = list_in(&f);
        assert_eq!(l.len(), 2);
        assert_eq!((l[0].source_id.as_str(), l[0].count), ("cam", 2));
        assert_eq!(l[0].items[0].file, "c");
        assert_eq!(l[0].items[0].user_choice.as_deref(), Some("Receipts"));
        assert!(remove_in(&f, &c.id).unwrap());
        assert_eq!(list_in(&f)[0].items.len(), 1);
        assert_eq!(clear_in(&f, Some("cam")).unwrap(), 1);
        assert_eq!(clear_in(&f, None).unwrap(), 1);
        assert!(list_in(&f).is_empty());
    }

    #[test]
    fn override_of_an_ai_item_is_recorded_and_of_a_mask_item_is_not() {
        use gallery_core::plan::{PlanItem, Side};
        use gallery_core::route::Via;
        let d = tempfile::tempdir().unwrap();
        let config = crate::scan::tests::fixture(d.path());
        let src = d.path().join("src");
        let item = |f: &str, target: &str, via: Via| PlanItem {
            source: src.join(f),
            source_root: src.clone(),
            size: 1,
            mtime_ns: 0,
            target: target.into(),
            via,
            outputs: vec![],
        };
        let plan = Plan {
            version: 1,
            created: String::new(),
            side: Side::Local,
            tz: None,
            content_presets: Default::default(),
            items: vec![
                item(
                    "cat.jpg",
                    "cats",
                    Via::Ai {
                        reason: "grey cat".into(),
                        low: false,
                    },
                ),
                item("note.txt", "rest", Via::Mask),
            ],
            review: vec![],
            unmatched: vec![],
            warnings: vec![],
            unchanged: vec![],
            retry_of: None,
        };
        let plan_path = d.path().join("current.plan.json");
        plan.save(&plan_path).unwrap();
        let (ovf, store) = (d.path().join("o.json"), d.path().join("ex.jsonl"));
        let learn = Learn {
            store: &store,
            plan: &plan_path,
        };
        let req = |f: &str| OverrideRequest {
            source_path: src.join(f).display().to_string(),
            action: Action::Target {
                name: "rest".into(),
            },
        };
        let res = set_many_learning_in(
            &ovf,
            &config,
            vec![req("cat.jpg"), req("note.txt")],
            Some(&learn),
        )
        .unwrap();
        assert!(res.iter().all(|r| r.error.is_none()), "{res:?}");
        let got = Store::new(&store).load();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(
            (
                got[0].file.as_str(),
                got[0].ai_suggested.as_deref(),
                got[0].ai_reason.as_str()
            ),
            ("cat.jpg", Some("cats"), "grey cat")
        );
        assert_eq!(got[0].source_id, "s");
        // Without a learn target nothing is recorded.
        std::fs::remove_file(&store).unwrap();
        set_many_learning_in(&ovf, &config, vec![req("cat.jpg")], None).unwrap();
        assert!(!store.exists());
    }
}
