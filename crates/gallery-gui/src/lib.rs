//! Gallery desktop GUI: Tauri commands over `gallery-core`.
//!
//! File-affecting logic stays in the core: commands read, scan into a new
//! plan file, or record a manual decision through the core's overrides.

pub mod apply;
pub mod busy;
pub mod config_edit;
pub mod current;
pub mod effect;
pub mod estimate;
pub mod explain;
pub mod export;
pub mod history;
pub mod learn;
pub mod notify;
pub mod overrides;
pub mod pilot;
pub mod plans;
pub mod presets;
pub mod scan;
pub mod settings;
pub mod status;
pub mod summary;
pub mod sync;
pub mod thumbs;
pub mod tray;
pub mod triage;

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use gallery_core::overrides::Action;
use gallery_core::progress::{NoProgress, ProgressEvent};
use gallery_core::tools::Tools;
use gallery_core::trash::TrashMode;
use tauri::{AppHandle, Emitter, Manager, State};

/// Resolve the config path: the given one (with `~` expanded), else the
/// default `~/.config/gallery/gallery.toml`.
pub fn config_path(path: Option<String>) -> PathBuf {
    match path.filter(|p| !p.trim().is_empty()) {
        Some(p) => gallery_core::config::expand_tilde(std::path::Path::new(&p)),
        None => default_config_path(),
    }
}

/// `~/.config/gallery/gallery.toml` (the platform config dir on other systems).
pub fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("gallery")
        .join("gallery.toml")
}

/// `$GALLERY_DATA_DIR`, when set and not empty: the GUI keeps its plans and
/// manual decisions there instead of the user data dir (tests, screenshots).
/// Debug builds only: the CLI and `apply` read the default overrides file, so
/// in a release build the variable would silently cut the GUI's decisions off
/// from them.
#[cfg(debug_assertions)]
pub fn data_dir_override() -> Option<PathBuf> {
    data_dir_from(std::env::var_os("GALLERY_DATA_DIR"))
}

#[cfg(not(debug_assertions))]
pub fn data_dir_override() -> Option<PathBuf> {
    None
}

#[cfg(debug_assertions)]
fn data_dir_from(var: Option<std::ffi::OsString>) -> Option<PathBuf> {
    var.filter(|v| !v.is_empty())
        .map(|v| gallery_core::config::expand_tilde(Path::new(&v)))
}

/// The machine-wide apply/undo lock (also taken by the CLI); a dry run
/// writes nothing and needs none. The message says who holds it.
fn global_lock(dry_run: bool) -> Result<Option<gallery_core::lock::FileLock>, String> {
    if dry_run {
        return Ok(None);
    }
    gallery_core::lock::acquire_apply()
        .map(Some)
        .map_err(|e| e.to_string())
}

/// Run blocking work off the async runtime; a panic becomes an error string.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("internal error: {e}"))?
}

#[tauri::command]
async fn app_status(path: Option<String>) -> Result<status::AppStatus, String> {
    blocking(move || Ok(status::app_status(&config_path(path)))).await
}

#[tauri::command]
async fn load_config(path: Option<String>) -> Result<summary::ConfigSummary, String> {
    blocking(move || summary::load_summary(&config_path(path))).await
}

#[tauri::command]
fn builtin_presets() -> presets::BuiltinPresets {
    presets::builtin_presets()
}

#[tauri::command]
fn default_config() -> String {
    default_config_path().display().to_string()
}

/// External tools, found once (a failed lookup is retried next time).
fn tools() -> Result<Tools, String> {
    static TOOLS: Mutex<Option<Tools>> = Mutex::new(None);
    let mut t = TOOLS.lock().map_err(|e| e.to_string())?;
    if t.is_none() {
        *t = Some(Tools::discover().map_err(|e| e.to_string())?);
    }
    Ok(t.clone().expect("set above"))
}

/// The explicit AI scan, in a background thread: asks the AI for the files
/// without a cached answer (this is the only place that pays), then brings
/// the current plan up to date from the cache. Progress comes as
/// `scan-progress`, the end as `scan-done` (the cost) or `scan-error`.
#[tauri::command]
fn start_scan(
    app: AppHandle,
    state: State<'_, busy::Busy>,
    path: Option<String>,
) -> Result<(), String> {
    let cancel = state.begin(busy::Op::Scan(scan::ScanRequest::default()))?;
    let config = config_path(path);
    std::thread::spawn(move || {
        // The slot is freed before the end event, even after a panic: the UI
        // may start the next operation on `scan-done`.
        let result = scan::run_guarded(&app.state::<busy::Busy>(), || {
            let fail = |message| scan::ScanError {
                message,
                cancelled: false,
            };
            let tools = tools().map_err(fail)?;
            let owned = current::Owned::for_config(&config, Some(tools.clone())).map_err(fail)?;
            let throttle = scan::Throttle::new(Duration::from_millis(80));
            let sink = |ev: ProgressEvent| {
                if throttle.pass(&ev, Instant::now()) {
                    let _ = app.emit(scan::PROGRESS_EVENT, scan::ScanProgress::from(ev));
                }
            };
            current::ai_scan(&owned.env(), &tools, None, &sink, &cancel)
        });
        let _ = match result {
            Ok(done) => {
                let _ = app.emit(current::UPDATED_EVENT, ());
                app.emit(scan::DONE_EVENT, done)
            }
            Err(err) => app.emit(scan::ERROR_EVENT, err),
        };
    });
    Ok(())
}

/// The current plan of the config at `path`, `None` before the first build.
/// Reads the stored plan only; `refresh_current` brings it up to date.
#[tauri::command]
async fn current_plan(path: Option<String>) -> Result<Option<current::CurrentView>, String> {
    blocking(move || {
        let owned = current::Owned::for_config(&config_path(path), None)?;
        current::load(&owned.env())
    })
    .await
}

/// Bring the current plan up to date with the config, decisions and files,
/// in a background thread and without any AI call; a no-op when nothing
/// changed. Events: `current-rebuilding` when it is stale, `current-updated`
/// when done, `current-error`. `false`: another operation holds the slot, so
/// nothing was started (an apply refreshes the plan itself when it ends).
#[tauri::command]
fn refresh_current(app: AppHandle, path: Option<String>) -> bool {
    spawn_refresh(&app, config_path(path))
}

/// Start a rebuild, or ask the running one to go round once more.
pub fn spawn_refresh(app: &AppHandle, config: PathBuf) -> bool {
    let busy = app.state::<busy::Busy>();
    let cancel = loop {
        match busy.begin(busy::Op::Rebuild) {
            Ok(c) => break c,
            Err(_) if busy.request_rerun() => return true,
            Err(_) if busy.is_busy() => return false,
            // The slot was freed between the two calls: try again.
            Err(_) => {}
        }
    };
    app.state::<tray::Watch>().set_config(config.clone());
    let app = app.clone();
    std::thread::spawn(move || {
        let busy = app.state::<busy::Busy>();
        let outcome = loop {
            let pass = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let owned = current::Owned::for_config(&config, tools().ok())?;
                let env = owned.env();
                let on_stale = || {
                    let _ = app.emit(current::REBUILDING_EVENT, ());
                };
                current::refresh_notify(&env, false, &on_stale, &NoProgress, &cancel)?;
                Ok::<_, String>(current::load(&env)?.map(|v| current::pending_of(&v)))
            }))
            .unwrap_or_else(|_| Err("internal error refreshing the plan".to_string()));
            // Another pass was asked for meanwhile: it supersedes this one, but
            // a failure of this one is still told.
            if !busy.finish_rebuild() {
                break pass;
            }
            if let Err(message) = &pass {
                let _ = app.emit(current::ERROR_EVENT, message.clone());
            }
        };
        match outcome {
            Ok(pending) => {
                if let Some(p) = pending {
                    tray::publish(&app, p);
                }
                let _ = app.emit(current::UPDATED_EVENT, ());
            }
            Err(message) => {
                let _ = app.emit(current::ERROR_EVENT, message);
            }
        }
    });
    true
}

/// The request of the running scan, `None` when none runs: a reloaded
/// webview uses it to show the scan again.
#[tauri::command]
fn scan_status(state: State<'_, busy::Busy>) -> Option<scan::ScanRequest> {
    state.scan()
}

/// Ask the running scan to stop; whether one was running.
#[tauri::command]
fn cancel_scan(state: State<'_, busy::Busy>) -> bool {
    state.cancel(busy::Kind::Scan)
}

/// What a run of the plan at `plan_path` would do, counted from the plan:
/// the confirmation shows it. Reads only.
#[tauri::command]
async fn apply_preview(plan_path: String) -> Result<apply::ApplyPreview, String> {
    blocking(move || apply::preview(Path::new(&plan_path))).await
}

/// What a run of the plan would save and take (ranges, learned from past
/// reports), for the confirmation and the pilot panel. Reads only.
#[tauri::command]
async fn estimate_plan(
    plan_path: String,
    jobs: Option<usize>,
    pilot: Option<usize>,
) -> Result<gallery_core::estimate::Estimate, String> {
    blocking(move || {
        let dir = scan::default_plans_dir()?;
        estimate::plan_estimate(Path::new(&plan_path), &dir, jobs, pilot)
    })
    .await
}

/// What a scan with AI would cost before it starts: files to send, files
/// from the cache, price. No API call, nothing written.
#[tauri::command]
async fn estimate_ai_scan(
    path: Option<String>,
) -> Result<gallery_core::estimate::AiEstimate, String> {
    let config = config_path(path);
    blocking(move || {
        estimate::ai_scan_estimate_default(&config, overrides::default_file().ok().as_deref())
    })
    .await
}

/// How the current plan splits into a pilot of `per_target` files per target
/// and the rest, with what the pilot would do. Nothing is written: the run
/// makes the pilot plan itself, after its final check.
#[tauri::command]
async fn pilot_preview(
    plan_path: String,
    per_target: Option<usize>,
) -> Result<pilot::PilotSplit, String> {
    blocking(move || {
        pilot::preview(
            Path::new(&plan_path),
            per_target.unwrap_or(pilot::DEFAULT_PER_TARGET),
        )
    })
    .await
}

/// Each file of a pilot run's report before and after (size, date, mtime,
/// GPS) with the anomalies the run found. Reads only.
#[tauri::command]
async fn pilot_result(report_path: String) -> Result<pilot::PilotResult, String> {
    blocking(move || pilot::result(Path::new(&report_path), &tools()?)).await
}

/// Run (or dry-run) a local plan file in a background thread (a retry plan
/// from History; the current plan runs with `start_apply_current`); refuses a server
/// plan. Progress comes as `apply-progress`, the end as `apply-done` (the
/// report is next to the plan) or `apply-error`. The trash is the system
/// one unless `GALLERY_TRASH=folder`.
#[tauri::command]
fn start_apply(
    app: AppHandle,
    state: State<'_, busy::Busy>,
    request: apply::ApplyRequest,
) -> Result<(), String> {
    // Refuse a server plan before claiming the slot (the run checks again).
    let plan =
        gallery_core::plan::Plan::load(Path::new(&request.plan_path)).map_err(|e| e.to_string())?;
    apply::ensure_local(&plan)?;
    // Real runs only: the plan already ran, or decisions came after it.
    apply::guard_real_run(&request, overrides::default_file().ok().as_deref())?;
    let cancel = state.begin(busy::Op::Apply {
        request: request.clone(),
        progress: apply::ApplyProgress {
            total: plan.items.len(),
            ..Default::default()
        },
    })?;
    std::thread::spawn(move || {
        let busy = app.state::<busy::Busy>();
        let result = busy::run_guarded(
            &busy,
            || {
                // One apply/undo at a time on the machine (CLI included).
                let _global = global_lock(request.dry_run)?;
                let tools = tools()?;
                let throttle = scan::Throttle::new(Duration::from_millis(80));
                let sink = |p: apply::ApplyProgress| {
                    busy.set_apply_progress(p.clone());
                    let ev = ProgressEvent {
                        stage: gallery_core::progress::Stage::Apply,
                        done: p.done,
                        total: p.total,
                        current: None,
                    };
                    if throttle.pass(&ev, Instant::now()) {
                        let _ = app.emit(apply::PROGRESS_EVENT, p);
                    }
                };
                apply::run(&request, &tools, TrashMode::from_env(), &sink, &cancel)
            },
            |what| apply::ApplyError {
                message: format!("internal run error: {what}"),
            },
        );
        let dry = request.dry_run;
        let _ = match result {
            Ok(done) => {
                busy.set_last_done(done.clone());
                app.emit(apply::DONE_EVENT, done)
            }
            Err(err) => app.emit(apply::ERROR_EVENT, err),
        };
        // The files moved: the current plan follows. The run may have paused
        // or resumed Syncthing.
        let _ = tray::refresh_sync(&app);
        if !dry {
            spawn_refresh(&app, app.state::<tray::Watch>().config());
        }
    });
    Ok(())
}

/// What a run of the current plan asks for.
#[derive(Debug, Clone, serde::Deserialize)]
struct CurrentRun {
    dry_run: bool,
    #[serde(default)]
    jobs: Option<usize>,
    #[serde(default)]
    allow_anomalies: bool,
    #[serde(default)]
    pause_sync: bool,
    /// A pilot of this many files per target instead of the whole plan.
    #[serde(default)]
    pilot: Option<usize>,
    /// Identity of the plan (and pilot slice) the user confirmed.
    #[serde(default)]
    plan_id: Option<String>,
    #[serde(default)]
    pilot_id: Option<String>,
}

/// Run (or dry-run) the current plan of the config at `path` in a background
/// thread. The plan is checked against the files, config and decisions once
/// more first: a stale one is refused (`apply-error`) and rebuilt. A real run
/// works on a copy saved in the plans folder, so its report lands there for
/// History and undo. Progress comes as `apply-progress`, the end as
/// `apply-done` or `apply-error`; the plan is refreshed afterwards.
#[tauri::command]
fn start_apply_current(
    app: AppHandle,
    state: State<'_, busy::Busy>,
    path: Option<String>,
    run: CurrentRun,
) -> Result<(), String> {
    let config = config_path(path);
    let owned = current::Owned::for_config(&config, tools().ok())?;
    let request = apply::ApplyRequest {
        plan_path: owned.env().plan_path().display().to_string(),
        dry_run: run.dry_run,
        jobs: run.jobs,
        allow_anomalies: run.allow_anomalies,
        pause_sync: run.pause_sync,
    };
    let cancel = state.begin(busy::Op::Apply {
        request: request.clone(),
        progress: apply::ApplyProgress::default(),
    })?;
    std::thread::spawn(move || {
        let busy = app.state::<busy::Busy>();
        let result = busy::run_guarded(
            &busy,
            || {
                let _global = global_lock(run.dry_run)?;
                let archive =
                    plans::default_dir().ok_or_else(|| "no user data directory".to_string())?;
                let prepared = current::prepare_run(
                    &owned.env(),
                    &archive,
                    run.pilot,
                    run.dry_run,
                    &current::Confirmed {
                        plan_id: run.plan_id.clone(),
                        pilot_id: run.pilot_id.clone(),
                    },
                    chrono::Local::now().naive_local(),
                    &cancel,
                );
                let prepared = prepared.map_err(|r| {
                    if r == current::Refusal::Stale {
                        let _ = app.emit(current::UPDATED_EVENT, ());
                    }
                    r.message()
                })?;
                let mut request = request;
                request.plan_path = prepared.plan_path.display().to_string();
                busy.set_apply_progress(apply::ApplyProgress {
                    total: prepared.files,
                    ..Default::default()
                });
                let tools = tools()?;
                let throttle = scan::Throttle::new(Duration::from_millis(80));
                let sink = |p: apply::ApplyProgress| {
                    busy.set_apply_progress(p.clone());
                    let ev = ProgressEvent {
                        stage: gallery_core::progress::Stage::Apply,
                        done: p.done,
                        total: p.total,
                        current: None,
                    };
                    if throttle.pass(&ev, Instant::now()) {
                        let _ = app.emit(apply::PROGRESS_EVENT, p);
                    }
                };
                apply::run(&request, &tools, TrashMode::from_env(), &sink, &cancel)
                    .map_err(|e| e.message)
            },
            |what| format!("internal run error: {what}"),
        );
        let dry = run.dry_run;
        let _ = match result {
            Ok(done) => {
                busy.set_last_done(done.clone());
                app.emit(apply::DONE_EVENT, done)
            }
            Err(message) => app.emit(apply::ERROR_EVENT, apply::ApplyError { message }),
        };
        // The run may have paused or resumed Syncthing.
        let _ = tray::refresh_sync(&app);
        // The files moved: the plan is stale. A dry run changes nothing.
        if !dry {
            spawn_refresh(&app, config);
        }
    });
    Ok(())
}

/// Whether the Syncthing folders of the plan (or of its pilot slice, with
/// `pilot` files per target) can be paused, and which.
#[tauri::command]
async fn sync_check(
    plan_path: String,
    pilot: Option<usize>,
) -> Result<gallery_core::syncthing::SyncCheck, String> {
    blocking(move || Ok(sync::check(Path::new(&plan_path), pilot))).await
}

/// What Gallery paused and has not resumed (from the state file).
#[tauri::command]
async fn sync_paused() -> Result<Option<gallery_core::syncthing::PauseState>, String> {
    blocking(sync::paused).await
}

/// Resume the folders Gallery paused.
#[tauri::command]
async fn sync_resume(app: AppHandle) -> Result<gallery_core::syncthing::ResumeOutcome, String> {
    let out = blocking(sync::resume).await;
    let _ = tray::refresh_sync(&app);
    out
}

/// The running apply with its progress and the last finished one: a
/// reloaded webview shows them again.
#[tauri::command]
fn apply_status(state: State<'_, busy::Busy>) -> apply::ApplyState {
    state.apply_state()
}

/// Ask the running apply to stop: no new file starts, files in flight
/// finish safely; whether one was running.
#[tauri::command]
fn cancel_apply(state: State<'_, busy::Busy>) -> bool {
    state.cancel(busy::Kind::Apply)
}

/// Rebuild the plan for the server (same config at `path`, same manual
/// decisions) and save it in `request.dest_dir`. Progress comes as
/// `export-progress`. Only writes the plan file; the server is not touched.
#[tauri::command]
async fn export_server_plan(
    app: AppHandle,
    path: Option<String>,
    request: export::ExportRequest,
) -> Result<export::ExportDone, String> {
    // The export must match the local plan: no overrides, no export.
    let overrides = overrides::default_file()
        .map_err(|e| format!("could not locate the manual decisions file: {e}"))?;
    let cancel = app.state::<busy::Busy>().begin(busy::Op::Export)?;
    let config = config_path(path);
    blocking(move || {
        let busy = app.state::<busy::Busy>();
        let res = scan::run_guarded(&busy, || {
            let err = |message| scan::ScanError {
                message,
                cancelled: false,
            };
            let tools = tools().map_err(err)?;
            let throttle = scan::Throttle::new(Duration::from_millis(80));
            let sink = |ev: ProgressEvent| {
                if throttle.pass(&ev, Instant::now()) {
                    let _ = app.emit(export::PROGRESS_EVENT, scan::ScanProgress::from(ev));
                }
            };
            let cache = gallery_core::ai::AiClassifier::default_cache_path()
                .unwrap_or_else(|| PathBuf::from("none"));
            let examples = learn::default_file().ok();
            export::run_with_examples(
                &config,
                Some(&overrides),
                &cache,
                examples.as_deref(),
                &tools,
                &request,
                None,
                &sink,
                &cancel,
            )
        });
        res.map_err(|e| e.message)
    })
    .await
}

/// Ask the running export to stop; whether one was running.
#[tauri::command]
fn cancel_export(state: State<'_, busy::Busy>) -> bool {
    state.cancel(busy::Kind::Export)
}

/// The export folder the UI remembers, when one is set.
fn export_dir(dir: Option<String>) -> Option<PathBuf> {
    dir.filter(|d| !d.trim().is_empty())
        .map(|d| gallery_core::config::expand_tilde(Path::new(d.trim())))
}

/// Reports of the GUI plans folder (current and archived) and of the export
/// folder `export_dir` (the server's, synced back), newest first. Reads only.
#[tauri::command]
async fn list_reports(export_dir: Option<String>) -> Result<Vec<history::ReportEntry>, String> {
    blocking(move || {
        let plans = plans::default_dir();
        Ok(history::list(
            plans.as_deref(),
            self::export_dir(export_dir).as_deref(),
        ))
    })
    .await
}

/// The report at `path` with its files, what an undo would do, and for a
/// server report the command for the server. Reads only.
#[tauri::command]
async fn load_report(
    path: String,
    export_dir: Option<String>,
) -> Result<history::ReportDetail, String> {
    blocking(move || history::load(Path::new(&path), self::export_dir(export_dir).as_deref())).await
}

/// Undo the run of a local report in a background thread; a server report
/// (or any report not of a local run of a GUI plan) is refused. Progress
/// comes as `undo-progress`, the end as `undo-done` or `undo-error`.
#[tauri::command]
fn start_undo(
    app: AppHandle,
    state: State<'_, busy::Busy>,
    report_path: String,
) -> Result<(), String> {
    let plans = plans::default_dir();
    let path = PathBuf::from(&report_path);
    // Refuse before claiming the slot (the undo checks again).
    let report = history::guard_undo(&path, plans.as_deref())?;
    let cancel = state.begin(busy::Op::Undo {
        report_path: report_path.clone(),
        progress: history::UndoProgress {
            total: report.items.len(),
            ..Default::default()
        },
    })?;
    std::thread::spawn(move || {
        let busy = app.state::<busy::Busy>();
        let result = busy::run_guarded(
            &busy,
            || {
                let _global = global_lock(false)?;
                let throttle = scan::Throttle::new(Duration::from_millis(80));
                let sink = |p: history::UndoProgress| {
                    busy.set_undo_progress(p.clone());
                    let ev = ProgressEvent {
                        stage: gallery_core::progress::Stage::Undo,
                        done: p.done,
                        total: p.total,
                        current: None,
                    };
                    if throttle.pass(&ev, Instant::now()) {
                        let _ = app.emit(history::PROGRESS_EVENT, p);
                    }
                };
                history::undo(&path, plans.as_deref(), &sink, &cancel)
            },
            |what| history::UndoError {
                message: format!("internal undo error: {what}"),
            },
        );
        let _ = match result {
            Ok(done) => app.emit(history::DONE_EVENT, done),
            Err(err) => app.emit(history::ERROR_EVENT, err),
        };
        // The files moved back: the current plan follows.
        spawn_refresh(&app, app.state::<tray::Watch>().config());
    });
    Ok(())
}

/// The running undo with its progress, `None` when none runs.
#[tauri::command]
fn undo_status(state: State<'_, busy::Busy>) -> Option<history::UndoStatus> {
    state.undo()
}

/// Ask the running undo to stop between files; whether one was running.
#[tauri::command]
fn cancel_undo(state: State<'_, busy::Busy>) -> bool {
    state.cancel(busy::Kind::Undo)
}

/// "Retry failed" of a local report: the partial undo of the failed
/// files and their retry plan, saved next to the plans. Nothing runs: the
/// Run screen runs the plan after its confirmation. Refuses server reports.
#[tauri::command]
async fn start_retry(app: AppHandle, report_path: String) -> Result<history::RetryDone, String> {
    let plans = plans::default_dir();
    let path = PathBuf::from(&report_path);
    history::ensure_local_report(&path, plans.as_deref())?;
    app.state::<busy::Busy>().begin(busy::Op::Retry)?;
    let refresh = app.clone();
    let done = blocking(move || {
        let busy = app.state::<busy::Busy>();
        busy::run_guarded(
            &busy,
            || {
                let _global = global_lock(false)?;
                history::retry(&path, plans.as_deref())
            },
            |what| format!("internal retry error: {what}"),
        )
    })
    .await;
    // The failed files went back: the current plan follows.
    spawn_refresh(&refresh, refresh.state::<tray::Watch>().config());
    done
}

/// Asset URL of the thumbnail of `path`, made (and cached by the core) on
/// first request.
#[tauri::command]
async fn thumb(path: String) -> Result<String, String> {
    static GATE: OnceLock<thumbs::Gate> = OnceLock::new();
    blocking(move || {
        let gate = GATE.get_or_init(|| {
            thumbs::Gate::new(std::thread::available_parallelism().map_or(4, |n| n.get()))
        });
        let tools = tools()?;
        let made = gate.run(|| gallery_core::thumbs::thumb_for(&tools, Path::new(&path)));
        made.map(|p| thumbs::asset_url(&p))
            .map_err(|e| e.to_string())
    })
    .await
}

/// Why the scan does what it does with the file at `path`: the core's
/// read-only explanation under the config at `config_path`, with the
/// saved manual decisions and the cached AI answers. Never changes anything
/// and never asks the AI.
#[tauri::command]
async fn explain_file(
    config_path: Option<String>,
    path: String,
) -> Result<gallery_core::explain::Explanation, String> {
    blocking(move || {
        explain::explain_in(
            &self::config_path(config_path),
            &overrides::default_file()?,
            Path::new(&path),
            tools().ok().as_ref(),
            learn::default_file().ok().as_deref(),
        )
    })
    .await
}

/// The learning input of a decision made now: the corrections file and the
/// current plan of the config at `config`, which says what the AI decided.
fn learning_for(config: &Path) -> Option<(PathBuf, PathBuf)> {
    let store = learn::default_file().ok()?;
    let plan = current::Owned::for_config(config, None)
        .ok()?
        .env()
        .plan_path();
    Some((store, plan))
}

fn set_with_learning(
    path: Option<String>,
    items: Vec<overrides::OverrideRequest>,
) -> Result<Vec<overrides::OverrideResult>, String> {
    let config = config_path(path);
    let target = learning_for(&config);
    let learn = target
        .as_ref()
        .map(|(store, plan)| learn::Learn { store, plan });
    learn::set_many_learning_in(&overrides::default_file()?, &config, items, learn.as_ref())
}

/// Record a manual decision for `source_path` (a target, or keep in place),
/// checked against the config at `path` (the UI's config); returns the path
/// the decision is keyed by and the file size and mtime it was stamped with.
/// A decision that overrides an AI answer is also remembered as an example
/// for the AI.
#[tauri::command]
async fn set_override(
    path: Option<String>,
    source_path: String,
    action: Action,
) -> Result<overrides::OverrideResult, String> {
    blocking(move || {
        let req = overrides::OverrideRequest {
            source_path,
            action,
        };
        let res = set_with_learning(path, vec![req])?.remove(0);
        match res.error {
            Some(e) => Err(e),
            None => Ok(res),
        }
    })
    .await
}

/// Record several manual decisions in one write (the Review screen's
/// "Apply to checked"), each checked against the config at `path`;
/// one result per request, in order. Corrections of AI answers are
/// remembered like in [`set_override`].
#[tauri::command]
async fn set_overrides(
    path: Option<String>,
    items: Vec<overrides::OverrideRequest>,
) -> Result<Vec<overrides::OverrideResult>, String> {
    blocking(move || set_with_learning(path, items)).await
}

/// The AI examples (the user's corrections) per source.
#[tauri::command]
async fn list_ai_examples() -> Result<Vec<learn::SourceExamples>, String> {
    blocking(|| Ok(learn::list_in(&learn::default_file()?))).await
}

/// Delete one AI example; whether there was one.
#[tauri::command]
async fn remove_ai_example(id: String) -> Result<bool, String> {
    blocking(move || learn::remove_in(&learn::default_file()?, &id)).await
}

/// Delete the AI examples of one source, or all; how many went.
#[tauri::command]
async fn clear_ai_examples(source_id: Option<String>) -> Result<usize, String> {
    blocking(move || learn::clear_in(&learn::default_file()?, source_id.as_deref())).await
}

/// Drop the manual decision for `source_path`; whether there was one.
#[tauri::command]
async fn remove_override(source_path: String) -> Result<bool, String> {
    blocking(move || overrides::remove_in(&overrides::default_file()?, Path::new(&source_path)))
        .await
}

/// The editable view of the config at `path` with `ops` applied (the
/// editor's unsaved changes), checked by the core; nothing is written.
#[tauri::command]
async fn config_draft(
    path: Option<String>,
    version: Option<String>,
    ops: Vec<config_edit::EditOp>,
) -> Result<config_edit::EditOutcome, config_edit::EditFailure> {
    let config = config_path(path);
    tauri::async_runtime::spawn_blocking(move || {
        let mut out = config_edit::draft(&config, version.as_deref(), &ops)?;
        if let Ok(file) = overrides::default_file() {
            config_edit::with_override_warnings(&mut out, &file);
        }
        Ok(out)
    })
    .await
    .map_err(internal)?
}

/// What the editor's unsaved `ops` do to the files, before saving: both
/// configs are routed over one scanned file list (kept between calls while
/// the sources are the same; `refresh` scans again), AI targets from cached
/// answers only. A newer call cancels the older one (which returns `None`).
/// Progress as `effect-progress`. Read-only.
#[tauri::command]
async fn effect_preview(
    app: AppHandle,
    state: State<'_, effect::EffectState>,
    config_path: Option<String>,
    ops: Vec<config_edit::EditOp>,
    refresh: Option<bool>,
) -> Result<Option<effect::EffectView>, String> {
    let cancel = state.begin();
    let config = self::config_path(config_path);
    let handle = app.clone();
    blocking(move || {
        let state = handle.state::<effect::EffectState>();
        let cache = gallery_core::ai::AiClassifier::default_cache_path()
            .unwrap_or_else(|| PathBuf::from("none"));
        let overrides = overrides::default_file().ok();
        let examples = learn::default_file().ok();
        let tools = tools().ok();
        let env = effect::Env {
            ai_cache: &cache,
            overrides: overrides.as_deref(),
            examples: examples.as_deref(),
            tools: tools.as_ref(),
        };
        let throttle = scan::Throttle::new(Duration::from_millis(80));
        let sink = |ev: ProgressEvent| {
            if throttle.pass(&ev, Instant::now()) {
                let _ = handle.emit(effect::PROGRESS_EVENT, scan::ScanProgress::from(ev));
            }
        };
        effect::preview(
            &state,
            &env,
            &config,
            &ops,
            refresh.unwrap_or(false),
            &sink,
            &cancel,
        )
    })
    .await
}

/// Apply `ops` to the config at `path` and write it in place (comments and
/// order kept, `.bak` of the old text) — only if the file still has
/// `version` and the result is valid.
#[tauri::command]
async fn config_edit(
    path: Option<String>,
    version: String,
    ops: Vec<config_edit::EditOp>,
) -> Result<config_edit::EditOutcome, config_edit::EditFailure> {
    let config = config_path(path);
    tauri::async_runtime::spawn_blocking(move || {
        let mut out = config_edit::save(&config, &version, &ops)?;
        if let Ok(file) = overrides::default_file() {
            config_edit::with_override_warnings(&mut out, &file);
        }
        Ok(out)
    })
    .await
    .map_err(internal)?
}

/// The unmatched files of the current plan in groups with their reasons and
/// suggested config edits, read against the config at `config_path`.
/// Read-only.
#[tauri::command]
async fn triage_view(config_path: Option<String>) -> Result<Option<triage::TriageView>, String> {
    blocking(move || {
        let config = self::config_path(config_path);
        let owned = current::Owned::for_config(&config, None)?;
        let Some(v) = current::load(&owned.env())? else {
            return Ok(None);
        };
        triage::view(&config, &v.plan).map(Some)
    })
    .await
}

/// "Create target…" for an unmatched group with the typed name and folder:
/// the edit operation to preview and save.
#[tauri::command]
async fn triage_create_ops(
    config_path: Option<String>,
    version: String,
    source: String,
    rel_dir: String,
    ext: String,
    name: String,
    dir: String,
) -> Result<triage::SuggestedAction, String> {
    blocking(move || {
        triage::create_ops(
            &self::config_path(config_path),
            &version,
            (&source, &rel_dir, &ext),
            &name,
            &dir,
        )
    })
    .await
}

/// The exact change `ops` would make to the config at `path`, checked;
/// nothing is written. Refused when the file no longer has `version`.
#[tauri::command]
async fn config_preview_change(
    path: Option<String>,
    version: Option<String>,
    ops: Vec<config_edit::EditOp>,
) -> Result<config_edit::ChangePreview, config_edit::EditFailure> {
    let config = config_path(path);
    tauri::async_runtime::spawn_blocking(move || {
        config_edit::preview_change(&config, version.as_deref(), &ops)
    })
    .await
    .map_err(internal)?
}

/// After targets were renamed: move the manual decisions that route to the
/// old names over to the new ones (`renames` is the whole `[from, to]`
/// mapping, applied at once); how many moved. Refused if the config is not
/// at `version` any more or a new name is not in it.
#[tauri::command]
async fn config_retarget_overrides(
    path: Option<String>,
    version: String,
    renames: Vec<(String, String)>,
) -> Result<usize, String> {
    let config = config_path(path);
    blocking(move || {
        overrides::retarget_in(&overrides::default_file()?, &config, &version, &renames)
    })
    .await
}

fn internal(e: impl std::fmt::Display) -> config_edit::EditFailure {
    config_edit::EditFailure {
        kind: config_edit::FailureKind::Io,
        message: format!("internal error: {e}"),
        op: None,
    }
}

/// The AI system prompt for files of `source_id` in the edited config; no
/// request is made.
#[tauri::command]
async fn config_preview_prompt(
    path: Option<String>,
    ops: Vec<config_edit::EditOp>,
    source_id: String,
) -> Result<config_edit::PromptPreview, String> {
    let config = config_path(path);
    blocking(move || {
        let examples = learn::default_file().ok();
        config_edit::preview_prompt(&config, &ops, &source_id, examples.as_deref())
    })
    .await
}

/// Which sample names pass a filter's `match` / `exclude`, with the masks
/// of the edited config.
#[tauri::command]
async fn config_mask_test(
    path: Option<String>,
    ops: Vec<config_edit::EditOp>,
    test: config_edit::MaskTest,
) -> Result<Vec<config_edit::MaskTestResult>, String> {
    let config = config_path(path);
    blocking(move || config_edit::mask_test(&config, &ops, &test)).await
}

/// What a name preset (by name or a form draft) makes of sample names with
/// a fake date, by the core's name pipe and the edited config's masks.
#[tauri::command]
async fn config_preview_name(
    path: Option<String>,
    ops: Vec<config_edit::EditOp>,
    sample: config_edit::NameSample,
) -> Result<Vec<config_edit::NameResult>, String> {
    let config = config_path(path);
    blocking(move || config_edit::preview_name(&config, &ops, &sample)).await
}

/// Bounds of content preset fields, as the encoders take them.
#[tauri::command]
fn preset_limits() -> Vec<config_edit::limits::Limit> {
    config_edit::limits::all().to_vec()
}

/// The tray settings with whether start at login is on.
#[derive(serde::Serialize)]
struct TrayView {
    settings: settings::TraySettings,
    autostart: bool,
}

fn autostart_enabled() -> bool {
    dirs::config_dir().is_some_and(|d| gallery_core::desktop::autostart_enabled(&d))
}

#[tauri::command]
fn tray_settings(state: State<'_, tray::Watch>) -> TrayView {
    TrayView {
        settings: state.settings(),
        autostart: autostart_enabled(),
    }
}

/// Save the tray settings. The XDG autostart entry is written or removed
/// only when `autostart` differs from what is there now (the user's choice).
#[tauri::command]
fn set_tray_settings(
    state: State<'_, tray::Watch>,
    settings: settings::TraySettings,
    autostart: bool,
) -> Result<TrayView, String> {
    if autostart != autostart_enabled() {
        let dir = dirs::config_dir().ok_or("no user settings directory")?;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        gallery_core::desktop::set_autostart(&dir, &exe, autostart).map_err(|e| e.to_string())?;
    }
    let settings = state.set_settings(settings)?;
    Ok(TrayView {
        settings,
        autostart: autostart_enabled(),
    })
}

/// The last background check's result, `None` before the first one.
#[tauri::command]
fn pending_status(state: State<'_, tray::Watch>) -> Option<tray::Snapshot> {
    state.last()
}

/// Count what waits now, for the config the UI works with (which the
/// background check then follows). No notice is sent.
#[tauri::command]
async fn check_pending(app: AppHandle, path: Option<String>) -> Result<tray::Snapshot, String> {
    app.state::<tray::Watch>().set_config(config_path(path));
    blocking(move || tray::check(&app, false)).await
}

pub fn run() {
    tauri::Builder::default()
        // First, so a second launch exits before it sets anything up: the
        // running app shows its window instead.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::focus_main(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .manage(busy::Busy::default())
        .manage(tray::Watch::new())
        .manage(effect::EffectState::default())
        .setup(|app| {
            // The webview may read thumbnails and nothing else from disk.
            if let Some(dir) = gallery_core::thumbs::default_dir() {
                std::fs::create_dir_all(&dir)?;
                app.asset_protocol_scope().allow_directory(&dir, false)?;
            }
            if let Err(e) = tray::setup(app.handle()) {
                eprintln!("gallery: tray unavailable: {e}");
            }
            // Autostart starts in the tray; the window stays hidden.
            let hidden = std::env::args().any(|a| a == tray::HIDDEN_FLAG);
            let w = app.state::<tray::Watch>();
            if let Some(win) = app.get_webview_window("main")
                && !(hidden && w.hide_on_close())
            {
                win.show()?;
            }
            tray::spawn_loop(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
                && window.state::<tray::Watch>().hide_on_close()
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            app_status,
            load_config,
            builtin_presets,
            default_config,
            start_scan,
            cancel_scan,
            scan_status,
            apply_preview,
            estimate_plan,
            estimate_ai_scan,
            list_ai_examples,
            remove_ai_example,
            clear_ai_examples,
            pilot_preview,
            pilot_result,
            start_apply,
            start_apply_current,
            current_plan,
            refresh_current,
            apply_status,
            sync_check,
            sync_paused,
            sync_resume,
            cancel_apply,
            export_server_plan,
            cancel_export,
            list_reports,
            load_report,
            start_undo,
            undo_status,
            cancel_undo,
            start_retry,
            thumb,
            set_override,
            explain_file,
            set_overrides,
            remove_override,
            config_draft,
            effect_preview,
            config_retarget_overrides,
            config_edit,
            config_preview_change,
            triage_view,
            triage_create_ops,
            config_preview_prompt,
            config_mask_test,
            config_preview_name,
            preset_limits,
            tray_settings,
            set_tray_settings,
            pending_status,
            check_pending
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Gallery GUI");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(debug_assertions)]
    #[test]
    fn data_dir_from_env() {
        assert_eq!(data_dir_from(None), None);
        assert_eq!(data_dir_from(Some("".into())), None);
        assert_eq!(
            data_dir_from(Some("/t/d".into())),
            Some(PathBuf::from("/t/d"))
        );
        let home = dirs::home_dir().unwrap();
        assert_eq!(data_dir_from(Some("~/d".into())), Some(home.join("d")));
    }

    #[test]
    fn config_path_defaults_and_expands_tilde() {
        assert_eq!(config_path(None), default_config_path());
        assert_eq!(config_path(Some("  ".into())), default_config_path());
        assert!(default_config_path().ends_with("gallery/gallery.toml"));
        let home = dirs::home_dir().unwrap();
        assert_eq!(config_path(Some("~/x.toml".into())), home.join("x.toml"));
        assert_eq!(
            config_path(Some("/a/b.toml".into())),
            PathBuf::from("/a/b.toml")
        );
    }
}
