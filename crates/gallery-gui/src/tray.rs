//! "Tray and notifications": the tray icon, the background check of how much
//! work waits, and the desktop notice when it grows. Counting is the core's
//! `pending`; the rules for when to notify are `notify`; this module is the
//! wiring. It only counts and tells: nothing is ever applied from here.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gallery_core::pending::Pending;
use gallery_core::progress::{Cancel, NoProgress};
use gallery_core::syncthing::PauseState;
use serde::Serialize;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};
use tauri_plugin_notification::NotificationExt;

use crate::settings::TraySettings;
use crate::{busy, notify, settings};

/// Event with the new [`Snapshot`] after every check.
pub const UPDATED_EVENT: &str = "pending-updated";
/// Event asking the UI to show the Plan screen.
pub const OPEN_PLAN_EVENT: &str = "open-plan";
/// Event: the pause state of Syncthing may have changed.
pub const SYNC_EVENT: &str = "sync-changed";
/// Command line flag of the autostart entry: start with the window hidden.
pub const HIDDEN_FLAG: &str = gallery_core::desktop::HIDDEN_ARG;
const TRAY_ID: &str = "main";

/// The result of the last check, for the tray and the UI.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Snapshot {
    pub to_process: usize,
    pub need_ai: usize,
    pub total: usize,
    pub by_target: std::collections::BTreeMap<String, usize>,
    /// RFC 3339 local time of the check.
    pub checked_at: String,
    /// Why the check failed (bad config, no exiftool…); counts are 0 then.
    pub error: Option<String>,
}

impl Snapshot {
    fn from_pending(p: Pending, at: String) -> Snapshot {
        Snapshot {
            total: p.total(),
            to_process: p.to_process,
            need_ai: p.need_ai,
            by_target: p.by_target,
            checked_at: at,
            error: None,
        }
    }

    fn failed(message: String, at: String) -> Snapshot {
        Snapshot {
            to_process: 0,
            need_ai: 0,
            total: 0,
            by_target: Default::default(),
            checked_at: at,
            error: Some(message),
        }
    }
}

/// Tooltip and first menu line: "Gallery: N to sort".
pub fn tooltip(s: Option<&Snapshot>) -> String {
    match s {
        None => "Gallery".into(),
        Some(s) if s.error.is_some() => "Gallery: check failed".into(),
        Some(s) if s.total == 0 => "Gallery: all sorted".into(),
        Some(s) => format!("Gallery: {} to sort", s.total),
    }
}

/// What the state file says about a pause, as one line for the tray:
/// "Syncthing paused: Photos, Videos". `None`: nothing paused (or no line to
/// show). A state file that cannot be read is told, never hidden.
pub fn sync_line(state: &Result<Option<PauseState>, String>) -> Option<String> {
    match state {
        Ok(None) => None,
        Ok(Some(p)) => {
            let names: Vec<&str> = p.folders.iter().map(|f| f.label.as_str()).collect();
            Some(format!("Syncthing paused: {}", names.join(", ")))
        }
        Err(e) => Some(format!("Syncthing: could not read the pause state ({e})")),
    }
}

/// Folder names of a pause, for the reminder.
pub fn paused_labels(state: &Result<Option<PauseState>, String>) -> Vec<String> {
    match state {
        Ok(Some(p)) => p.folders.iter().map(|f| f.label.clone()).collect(),
        _ => vec![],
    }
}

/// The tray tooltip: the count, and below it the pause line when there is one.
pub fn tooltip_with_sync(s: Option<&Snapshot>, sync: Option<&str>) -> String {
    match sync {
        Some(line) => format!("{}\n{line}", tooltip(s)),
        None => tooltip(s),
    }
}

/// Background state: settings, the config to watch, the last answer.
pub struct Watch {
    settings: Mutex<TraySettings>,
    config: Mutex<Option<PathBuf>>,
    last: Mutex<Option<Snapshot>>,
    running: AtomicBool,
    wake: (Mutex<bool>, Condvar),
    status_item: Mutex<Option<MenuItem<Wry>>>,
    /// The pause line and the resume entry of the menu.
    sync_item: Mutex<Option<MenuItem<Wry>>>,
    resume_item: Mutex<Option<MenuItem<Wry>>>,
    sync_line: Mutex<Option<String>>,
    tray_ready: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Watch {
    pub fn new() -> Watch {
        let s = settings::data_dir()
            .map(|d| settings::load(&d.join("settings.json")))
            .unwrap_or_default();
        Watch {
            settings: Mutex::new(s),
            config: Mutex::new(None),
            last: Mutex::new(None),
            running: AtomicBool::new(false),
            wake: (Mutex::new(false), Condvar::new()),
            status_item: Mutex::new(None),
            sync_item: Mutex::new(None),
            resume_item: Mutex::new(None),
            sync_line: Mutex::new(None),
            tray_ready: AtomicBool::new(false),
        }
    }

    pub fn settings(&self) -> TraySettings {
        lock(&self.settings).clone()
    }

    /// Save and use `s` (in bounds); wakes the background loop.
    pub fn set_settings(&self, s: TraySettings) -> Result<TraySettings, String> {
        let s = s.sanitized();
        let dir = settings::data_dir().ok_or("no user data directory")?;
        settings::save(&dir.join("settings.json"), &s)?;
        *lock(&self.settings) = s.clone();
        *lock(&self.wake.0) = true;
        self.wake.1.notify_all();
        Ok(s)
    }

    pub fn last(&self) -> Option<Snapshot> {
        lock(&self.last).clone()
    }

    /// Watch this config from now on (the one the UI works with).
    pub fn set_config(&self, path: PathBuf) {
        *lock(&self.config) = Some(path);
    }

    pub fn config(&self) -> PathBuf {
        lock(&self.config)
            .clone()
            .unwrap_or_else(crate::default_config_path)
    }

    /// Whether closing the window should only hide it.
    pub fn hide_on_close(&self) -> bool {
        self.tray_ready.load(Ordering::Relaxed) && lock(&self.settings).hide_to_tray
    }
}

impl Default for Watch {
    fn default() -> Self {
        Watch::new()
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Bring the current plan of `config` up to date (nothing to do when it is
/// fresh) and count what waits from it: one scan, one build, no recount.
fn bring_up_to_date(
    config: &std::path::Path,
    on_stale: &dyn Fn(),
    cancel: &Cancel,
) -> Result<Pending, String> {
    let owned = crate::current::Owned::for_config(config, crate::tools().ok())?;
    let env = owned.env();
    crate::current::refresh_notify(&env, false, on_stale, &NoProgress, cancel)?;
    crate::current::load(&env)?
        .map(|v| crate::current::pending_of(&v))
        .ok_or_else(|| "the plan is not built yet".to_string())
}

/// Count now. Skipped (an error text) while a scan, apply or other
/// operation runs, or another check is under way; with `notify` a growth
/// may send a desktop notice. Blocking: call off the UI thread.
pub fn check(app: &AppHandle, notify: bool) -> Result<Snapshot, String> {
    let w = app.state::<Watch>();
    if w.running.swap(true, Ordering::SeqCst) {
        return Err("a check is already running".into());
    }
    struct Done<'a>(&'a AtomicBool);
    impl Drop for Done<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    let _done = Done(&w.running);
    // The check is a rebuild of the current plan: it takes the same slot, so
    // no run starts under it and no second rebuild races it.
    let busy = app.state::<busy::Busy>();
    let cancel = busy
        .begin(busy::Op::Rebuild)
        .map_err(|_| "another operation is running: check postponed".to_string())?;
    let at = chrono::Local::now().to_rfc3339();
    let config = w.config();
    let counted = busy::run_guarded(
        &busy,
        || {
            let on_stale = || {
                let _ = app.emit(crate::current::REBUILDING_EVENT, ());
            };
            bring_up_to_date(&config, &on_stale, &cancel)
        },
        |what| format!("internal check error: {what}"),
    );
    let snap = match counted {
        Ok(p) => Snapshot::from_pending(p, at),
        Err(e) => Snapshot::failed(e, at),
    };
    show(app, &w, &snap);
    let paused = refresh_sync(app);
    if notify {
        remind_pause(app, &paused);
    }
    if notify && snap.error.is_none() {
        notify_growth(app, &w, snap.total);
    }
    if snap.error.is_none() {
        let _ = app.emit(crate::current::UPDATED_EVENT, ());
    }
    Ok(snap)
}

/// Keep `snap` as the last result, show it in the tray and tell the UI.
fn show(app: &AppHandle, w: &Watch, snap: &Snapshot) {
    *lock(&w.last) = Some(snap.clone());
    refresh_tray(app, snap);
    let _ = app.emit(UPDATED_EVENT, snap.clone());
}

/// The count of the current plan after a rebuild: no recount, no notice.
pub fn publish(app: &AppHandle, p: Pending) {
    let at = chrono::Local::now().to_rfc3339();
    show(app, &app.state::<Watch>(), &Snapshot::from_pending(p, at));
}

fn notify_growth(app: &AppHandle, w: &Watch, total: usize) {
    let Some(path) = settings::data_dir().map(|d| d.join("notify-state.json")) else {
        return;
    };
    let mut state = notify::load(&path);
    let threshold = w.settings().notify_threshold as usize;
    let text = notify::decide(&mut state, total, threshold, now_secs());
    notify::save(&path, &state);
    if let Some(text) = text {
        let _ = app
            .notification()
            .builder()
            .title("Gallery")
            .body(text)
            .show();
    }
}

fn refresh_tray(app: &AppHandle, snap: &Snapshot) {
    let w = app.state::<Watch>();
    let line = lock(&w.sync_line).clone();
    let text = tooltip(Some(snap));
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(&tooltip_with_sync(Some(snap), line.as_deref())));
        let _ = tray.set_title((snap.total > 0).then(|| snap.total.to_string()));
    }
    if let Some(item) = &*lock(&w.status_item) {
        let _ = item.set_text(&text);
    }
}

/// Read the pause state file (no network) and show it in the tray: the
/// tooltip, the menu line and the resume entry. Returns what is paused.
pub fn refresh_sync(app: &AppHandle) -> Result<Option<PauseState>, String> {
    let state = crate::sync::paused();
    let w = app.state::<Watch>();
    let line = sync_line(&state);
    let paused = matches!(state, Ok(Some(_)));
    *lock(&w.sync_line) = line.clone();
    if let Some(item) = &*lock(&w.sync_item) {
        let _ = item.set_text(line.as_deref().unwrap_or("Syncthing: not paused"));
    }
    if let Some(item) = &*lock(&w.resume_item) {
        let _ = item.set_enabled(paused);
    }
    let snap = w.last();
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(&tooltip_with_sync(snap.as_ref(), line.as_deref())));
    }
    let _ = app.emit(SYNC_EVENT, ());
    state
}

/// Resume what Gallery paused (from the tray menu) and show the result.
fn resume_from_tray(app: &AppHandle) {
    let out = crate::sync::resume();
    let body = match &out {
        Ok(o) if o.failed.is_empty() => "Gallery: Syncthing sync resumed".to_string(),
        Ok(o) => format!("Gallery: could not resume: {}", o.failed.join(", ")),
        Err(e) => format!("Gallery: could not resume sync: {e}"),
    };
    let _ = refresh_sync(app);
    let _ = app
        .notification()
        .builder()
        .title("Gallery")
        .body(body)
        .show();
}

/// Once per six hours while folders stay paused: a desktop reminder.
fn remind_pause(app: &AppHandle, state: &Result<Option<PauseState>, String>) {
    let Some(path) = settings::data_dir().map(|d| d.join("notify-state.json")) else {
        return;
    };
    let mut st = notify::load(&path);
    let text = notify::decide_pause(&mut st, &paused_labels(state), now_secs());
    notify::save(&path, &st);
    if let Some(text) = text {
        let _ = app
            .notification()
            .builder()
            .title("Gallery")
            .body(text)
            .show();
    }
}

/// Bring the main window up (a second launch of the app lands here).
pub fn focus_main(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

/// Show the main window; with work waiting, ask the UI for the Plan screen.
fn open_main(app: &AppHandle) {
    focus_main(app);
    if app.state::<Watch>().last().is_some_and(|s| s.total > 0) {
        let _ = app.emit(OPEN_PLAN_EVENT, ());
    }
}

/// Create the tray icon with its menu. A desktop without a tray only means
/// the window then closes normally (`hide_on_close` stays false).
pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let status = MenuItem::with_id(app, "status", tooltip(None), false, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open", true, None::<&str>)?;
    let check_now = MenuItem::with_id(app, "check", "Check now", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let sync = MenuItem::with_id(app, "sync", "Syncthing: not paused", false, None::<&str>)?;
    let resume = MenuItem::with_id(app, "resume", "Resume sync", false, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[&status, &sync, &sep, &open, &check_now, &resume, &quit],
    )?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip(tooltip(None))
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => open_main(app),
            "check" => {
                let app = app.clone();
                std::thread::spawn(move || {
                    let _ = check(&app, false);
                });
            }
            "resume" => {
                let app = app.clone();
                std::thread::spawn(move || resume_from_tray(&app));
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                open_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    let w = app.state::<Watch>();
    *lock(&w.status_item) = Some(status);
    *lock(&w.sync_item) = Some(sync);
    *lock(&w.resume_item) = Some(resume);
    w.tray_ready.store(true, Ordering::Relaxed);
    Ok(())
}

/// Background loop: a check at startup, then every interval (re-read each
/// round) or as soon as the settings change. Off the UI thread.
pub fn spawn_loop(app: AppHandle) {
    std::thread::spawn(move || {
        loop {
            // Skipped checks (busy) are retried next round.
            let _ = check(&app, true);
            let w = app.state::<Watch>();
            let wait = Duration::from_secs(u64::from(w.settings().check_interval_min) * 60);
            let (flag, cv) = &w.wake;
            let mut woke = lock(flag);
            if !*woke {
                woke = cv
                    .wait_timeout(woke, wait)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
            *woke = false;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(total: usize, error: Option<&str>) -> Snapshot {
        Snapshot {
            to_process: total,
            need_ai: 0,
            total,
            by_target: Default::default(),
            checked_at: String::new(),
            error: error.map(str::to_string),
        }
    }

    #[test]
    fn tooltip_text() {
        assert_eq!(tooltip(None), "Gallery");
        assert_eq!(tooltip(Some(&snap(0, None))), "Gallery: all sorted");
        assert_eq!(tooltip(Some(&snap(40, None))), "Gallery: 40 to sort");
        assert_eq!(
            tooltip(Some(&snap(0, Some("no config")))),
            "Gallery: check failed"
        );
    }

    fn paused(labels: &[&str]) -> PauseState {
        PauseState {
            folders: labels
                .iter()
                .map(|l| gallery_core::syncthing::PausedFolder {
                    id: l.to_string(),
                    label: l.to_string(),
                })
                .collect(),
            plan: String::new(),
            report: String::new(),
            paused_at: String::new(),
        }
    }

    #[test]
    fn the_pause_shows_in_the_tooltip_and_a_bad_state_file_is_told() {
        assert_eq!(sync_line(&Ok(None)), None);
        let line = sync_line(&Ok(Some(paused(&["Photos", "Videos"])))).unwrap();
        assert_eq!(line, "Syncthing paused: Photos, Videos");
        assert_eq!(
            tooltip_with_sync(Some(&snap(3, None)), Some(&line)),
            "Gallery: 3 to sort\nSyncthing paused: Photos, Videos"
        );
        assert_eq!(
            tooltip_with_sync(Some(&snap(3, None)), None),
            "Gallery: 3 to sort"
        );
        let bad = sync_line(&Err("sync-paused.json: oops".into())).unwrap();
        assert!(
            bad.contains("could not read") && bad.contains("oops"),
            "{bad}"
        );
        assert!(paused_labels(&Err("x".into())).is_empty());
        assert_eq!(paused_labels(&Ok(Some(paused(&["A"])))), ["A"]);
    }

    #[test]
    fn a_missing_config_is_an_error_not_a_panic() {
        let d = tempfile::tempdir().unwrap();
        assert!(bring_up_to_date(&d.path().join("none.toml"), &|| {}, &Cancel::new()).is_err());
    }
}
