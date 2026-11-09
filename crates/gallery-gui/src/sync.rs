//! Syncthing pause for the Run screen: whether the plan's folders can be
//! paused, what Gallery has paused (the banner) and the resume. The logic is
//! `gallery_core::syncthing`; here only the GUI's state-file place.

use std::path::{Path, PathBuf};

use gallery_core::plan::Plan;
use gallery_core::syncthing::{
    PauseState, ResumeOutcome, SyncCheck, SyncPause, Syncthing, default_state_path, load_state,
};

/// `<user data dir>/gallery/sync-paused.json` (`$GALLERY_DATA_DIR/` in
/// debug builds when set).
pub fn state_path() -> Option<PathBuf> {
    match crate::data_dir_override() {
        Some(dir) => Some(dir.join("sync-paused.json")),
        None => default_state_path(),
    }
}

/// The local Syncthing with the GUI's state file.
pub fn discover() -> Result<SyncPause, String> {
    let state_path = state_path().ok_or_else(|| gallery_core::t!("lock.no_data_dir"))?;
    Ok(SyncPause {
        client: Syncthing::discover().map_err(|e| e.to_string())?,
        state_path,
    })
}

/// The plan whose folders a run would touch: the whole plan, or with `pilot`
/// (files per target) only the pilot slice.
fn plan_to_run(plan_path: &Path, pilot: Option<usize>) -> Result<Plan, String> {
    let plan = Plan::load(plan_path).map_err(|e| e.to_string())?;
    match pilot {
        Some(n) => crate::pilot::choose(&plan, n).map(|(slice, _)| slice),
        None => Ok(plan),
    }
}

/// What the confirmation shows: reachable or why not, and the folders.
pub fn check(plan_path: &Path, pilot: Option<usize>) -> SyncCheck {
    let unreachable = |reason: String| SyncCheck {
        reachable: false,
        reason: Some(reason),
        folders: vec![],
    };
    let plan = match plan_to_run(plan_path, pilot) {
        Ok(p) => p,
        Err(e) => return unreachable(e),
    };
    match discover() {
        Ok(s) => s.check(&plan),
        Err(e) => unreachable(e),
    }
}

/// What Gallery paused and has not resumed (the state file; no network).
pub fn paused() -> Result<Option<PauseState>, String> {
    let path = state_path().ok_or_else(|| gallery_core::t!("lock.no_data_dir"))?;
    load_state(&path).map_err(|e| e.to_string())
}

pub fn resume() -> Result<ResumeOutcome, String> {
    discover()?.resume().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_state_file_means_nothing_paused() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(load_state(&d.path().join("none.json")).unwrap(), None);
    }

    #[test]
    fn a_missing_plan_is_unreachable_with_a_reason() {
        let c = check(Path::new("/nonexistent/x.plan.json"), None);
        assert!(!c.reachable && c.reason.is_some() && c.folders.is_empty());
    }

    #[test]
    fn a_pilot_check_looks_at_the_pilot_slice_only() {
        use gallery_core::plan::{OutputOp, PLAN_VERSION, PlanItem, PlannedOutput, Side};
        use gallery_core::route::Via;
        let d = tempfile::tempdir().unwrap();
        let item = |n: &str| PlanItem {
            source: PathBuf::from(format!("/phone/{n}/a.jpg")),
            source_root: PathBuf::from(format!("/phone/{n}")),
            size: 1,
            mtime_ns: 1,
            target: "t".into(),
            via: Via::Mask,
            outputs: vec![PlannedOutput {
                dest: PathBuf::from(format!("/out/{n}.jpg")),
                op: OutputOp::Write,
                steps: vec![],
                trace: vec![],
            }],
            kind: Default::default(),
        };
        let plan = Plan {
            version: PLAN_VERSION,
            created: String::new(),
            side: Side::Local,
            tz: None,
            content_presets: Default::default(),
            items: vec![item("one"), item("two"), item("three")],
            review: vec![],
            unmatched: vec![],
            warnings: vec![],
            unchanged: vec![],
            retry_of: None,
            kept: Vec::new(),
        };
        let path = d.path().join("p.plan.json");
        plan.save(&path).unwrap();
        assert_eq!(plan_to_run(&path, None).unwrap().items.len(), 3);
        let slice = plan_to_run(&path, Some(1)).unwrap();
        assert_eq!(slice.items.len(), 1);
        assert!(
            plan_to_run(&path, Some(5)).is_err(),
            "a pilot of everything is refused"
        );
    }
}
