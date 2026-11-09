//! Plans the GUI saves (`<data dir>/gallery/plans/YYYYMMDD-HHMMSS.plan.json`)
//! and the view of a plan the Plan screen shows.

use std::path::{Path, PathBuf};

use gallery_core::overrides::{Action, Overrides};
use serde::Serialize;

const SUFFIX: &str = ".plan.json";

/// `<user data dir>/gallery/plans` (`$GALLERY_DATA_DIR/plans` when set).
pub fn default_dir() -> Option<PathBuf> {
    gallery_core::paths::data_dir().map(|d| d.join("plans"))
}

/// Where the one current plan per config lives: `current/` next to the
/// archive of executed plans (`plans/`).
pub fn current_dir() -> Option<PathBuf> {
    crate::settings::data_dir().map(|d| d.join("current"))
}

/// `YYYYMMDD-HHMMSS.plan.json` for the given local time.
pub fn file_name(at: chrono::NaiveDateTime) -> String {
    format!("{}{SUFFIX}", at.format("%Y%m%d-%H%M%S"))
}

/// A path for a new plan in `dir` made at `at`: two scans in one second get
/// `-2`, `-3`… so an earlier plan is never overwritten.
pub fn new_plan_path(dir: &Path, at: chrono::NaiveDateTime) -> PathBuf {
    let first = dir.join(file_name(at));
    if !first.exists() {
        return first;
    }
    let stem = at.format("%Y%m%d-%H%M%S").to_string();
    (2..)
        .map(|n| dir.join(format!("{stem}-{n}{SUFFIX}")))
        .find(|p| !p.exists())
        .expect("a free name")
}

/// A manual decision as the UI sees it.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct OverrideView {
    pub path: String,
    pub action: Action,
    /// The file state the decision was made for; the core ignores it once
    /// the file differs, so the UI compares these with the plan item's.
    pub size: u64,
    pub mtime_ns: i128,
}

pub fn overrides_view(ov: &Overrides) -> Vec<OverrideView> {
    ov.entries()
        .map(|e| OverrideView {
            path: e.path.display().to_string(),
            action: e.action.clone(),
            size: e.size,
            mtime_ns: e.mtime_ns,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(h: u32, m: u32, s: u32) -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 5)
            .unwrap()
            .and_hms_opt(h, m, s)
            .unwrap()
    }

    #[test]
    fn names_sort_by_time_and_never_collide() {
        assert_eq!(file_name(at(9, 5, 7)), "20261105-090507.plan.json");
        let dir = tempfile::tempdir().unwrap();
        let a = new_plan_path(dir.path(), at(9, 5, 7));
        std::fs::write(&a, "{}").unwrap();
        let b = new_plan_path(dir.path(), at(9, 5, 7));
        assert_eq!(b, dir.path().join("20261105-090507-2.plan.json"));
        std::fs::write(&b, "{}").unwrap();
        for n in 3..=10 {
            let p = new_plan_path(dir.path(), at(9, 5, 7));
            assert!(p.ends_with(format!("20261105-090507-{n}.plan.json")));
            std::fs::write(&p, "{}").unwrap();
        }
    }
}
