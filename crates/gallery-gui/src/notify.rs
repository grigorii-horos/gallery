//! When to tell the user about new work: pure rules, no I/O besides the
//! small state file. A notice goes out when the count grew by at least the
//! threshold since the last notice (or since it last fell), and never more
//! than once an hour.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Least time between two notices, seconds.
pub const MIN_GAP_SECS: u64 = 3600;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyState {
    /// The count the growth is measured from: at the last notice, or the
    /// lowest count seen since.
    pub baseline: usize,
    /// Unix seconds of the last notice.
    pub last_notified: Option<u64>,
    /// Unix seconds of the last "Syncthing paused" reminder; cleared when
    /// nothing is paused any more.
    #[serde(default)]
    pub last_pause_reminder: Option<u64>,
}

/// Least time between two reminders that Syncthing is still paused.
pub const PAUSE_REMIND_SECS: u64 = 6 * 3600;

/// Whether to remind now that Syncthing folders are paused by Gallery:
/// at once when the pause is first seen, then once per six hours. `folders`
/// empty clears the memory, so the next pause reminds again.
pub fn decide_pause(state: &mut NotifyState, folders: &[String], now_secs: u64) -> Option<String> {
    if folders.is_empty() {
        state.last_pause_reminder = None;
        return None;
    }
    let due = state
        .last_pause_reminder
        .is_none_or(|t| now_secs.saturating_sub(t) >= PAUSE_REMIND_SECS);
    if !due {
        return None;
    }
    state.last_pause_reminder = Some(now_secs);
    Some(format!(
        "Gallery: Syncthing paused: {}. Resume sync when the run is finished.",
        folders.join(", ")
    ))
}

/// "1 new file", "5 new files".
pub fn new_files(n: usize) -> String {
    let word = if n == 1 { "new file" } else { "new files" };
    format!("{n} {word}")
}

/// Update `state` for a check that found `total` files waiting; the text of
/// the notice to send, if one is due.
pub fn decide(
    state: &mut NotifyState,
    total: usize,
    threshold: usize,
    now_secs: u64,
) -> Option<String> {
    if total < state.baseline {
        state.baseline = total;
    }
    let grown = total - state.baseline;
    let waited = state
        .last_notified
        .is_none_or(|t| now_secs.saturating_sub(t) >= MIN_GAP_SECS);
    if grown == 0 || grown < threshold.max(1) || !waited {
        return None;
    }
    state.baseline = total;
    state.last_notified = Some(now_secs);
    Some(format!("Gallery: {} to sort", new_files(grown)))
}

pub fn load(path: &Path) -> NotifyState {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Best effort: a lost state only repeats one notice.
pub fn save(path: &Path, s: &NotifyState) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(b) = serde_json::to_vec(s) {
        let _ = std::fs::write(path, b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = MIN_GAP_SECS;

    #[test]
    fn russian_plural() {
        assert_eq!(new_files(1), "1 new file");
        assert_eq!(new_files(2), "2 new files");
        assert_eq!(new_files(5), "5 new files");
        assert_eq!(new_files(11), "11 new files");
        assert_eq!(new_files(21), "21 new files");
        assert_eq!(new_files(40), "40 new files");
    }

    #[test]
    fn first_growth_notifies_with_the_growth() {
        let mut s = NotifyState::default();
        assert_eq!(
            decide(&mut s, 40, 1, 1000).as_deref(),
            Some("Gallery: 40 new files to sort")
        );
        assert_eq!(s.baseline, 40);
    }

    #[test]
    fn no_growth_or_below_threshold_is_quiet() {
        let mut s = NotifyState::default();
        assert_eq!(decide(&mut s, 0, 1, 0), None);
        assert_eq!(decide(&mut s, 4, 5, 0), None);
        assert_eq!(s.last_notified, None);
        assert!(decide(&mut s, 5, 5, 0).is_some());
    }

    #[test]
    fn at_most_one_notice_per_hour_and_growth_accumulates() {
        let mut s = NotifyState::default();
        assert!(decide(&mut s, 10, 1, 100).is_some());
        assert_eq!(decide(&mut s, 12, 1, 100 + H - 1), None);
        assert_eq!(decide(&mut s, 15, 1, 100 + H - 1), None);
        assert_eq!(
            decide(&mut s, 15, 1, 100 + H).as_deref(),
            Some("Gallery: 5 new files to sort")
        );
    }

    #[test]
    fn a_falling_count_lowers_the_baseline() {
        let mut s = NotifyState::default();
        assert!(decide(&mut s, 10, 1, 0).is_some());
        assert_eq!(decide(&mut s, 3, 1, 10 * H), None);
        assert_eq!(s.baseline, 3);
        assert_eq!(
            decide(&mut s, 4, 1, 10 * H + 1).as_deref(),
            Some("Gallery: 1 new file to sort")
        );
    }

    #[test]
    fn a_pause_reminds_at_once_then_once_per_six_hours() {
        let mut s = NotifyState::default();
        let f = vec!["Photos".to_string(), "Videos".to_string()];
        let text = decide_pause(&mut s, &f, 100).unwrap();
        assert!(text.contains("Syncthing paused: Photos, Videos"), "{text}");
        assert_eq!(decide_pause(&mut s, &f, 100 + PAUSE_REMIND_SECS - 1), None);
        assert!(decide_pause(&mut s, &f, 100 + PAUSE_REMIND_SECS).is_some());
        // Resumed: quiet, and a later pause reminds at once again.
        assert_eq!(decide_pause(&mut s, &[], 100 + PAUSE_REMIND_SECS + 1), None);
        assert_eq!(s.last_pause_reminder, None);
        assert!(decide_pause(&mut s, &f, 100 + PAUSE_REMIND_SECS + 2).is_some());
    }

    #[test]
    fn an_old_state_file_without_the_reminder_field_still_loads() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.json");
        std::fs::write(&p, r#"{"baseline":3,"last_notified":4}"#).unwrap();
        assert_eq!(load(&p).baseline, 3);
        assert_eq!(load(&p).last_pause_reminder, None);
    }

    #[test]
    fn state_roundtrips_through_its_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("n/state.json");
        assert_eq!(load(&p), NotifyState::default());
        let s = NotifyState {
            baseline: 7,
            last_notified: Some(5),
            last_pause_reminder: Some(9),
        };
        save(&p, &s);
        assert_eq!(load(&p), s);
    }
}
