//! The one long operation the GUI runs at a time — a scan, an apply, a
//! server-plan export, an undo or a retry's preparation. They share one
//! slot: a scan must not read sources an apply or undo is moving, and two
//! runs that move files must not race.

use std::sync::{Mutex, MutexGuard};

use gallery_core::progress::Cancel;

use crate::apply::{ApplyDone, ApplyProgress, ApplyRequest, ApplyState, ApplyStatus};
use crate::history::{UndoProgress, UndoStatus};
use crate::scan::ScanRequest;

/// What runs in the slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Scan(ScanRequest),
    Apply {
        request: ApplyRequest,
        progress: ApplyProgress,
    },
    Export,
    Undo {
        report_path: String,
        progress: UndoProgress,
    },
    /// "Retry failed": the partial undo and the retry plan.
    Retry,
    /// The current plan is brought up to date (files, config, decisions;
    /// no AI call).
    Rebuild,
}

/// The kind of an [`Op`], for cancelling only what the button means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Scan,
    Apply,
    Export,
    Undo,
    Retry,
    Rebuild,
}

impl Op {
    pub fn kind(&self) -> Kind {
        match self {
            Op::Scan(_) => Kind::Scan,
            Op::Apply { .. } => Kind::Apply,
            Op::Export => Kind::Export,
            Op::Undo { .. } => Kind::Undo,
            Op::Retry => Kind::Retry,
            Op::Rebuild => Kind::Rebuild,
        }
    }

    /// Why another operation cannot start now.
    fn busy_message(&self) -> String {
        match self {
            Op::Scan(_) => "a scan is running — wait for it to finish or cancel it".into(),
            Op::Apply { request, .. } if request.dry_run => {
                "a plan check (dry-run) is running — wait for it to finish or cancel it".into()
            }
            Op::Apply { .. } => {
                "a plan run is in progress — wait for it to finish or cancel it".into()
            }
            Op::Export => "a server plan export is running — wait for it to finish".into(),
            Op::Undo { .. } => "an undo is running — wait for it to finish or cancel it".into(),
            Op::Retry => {
                "a retry of the failed files is being prepared — wait for it to finish".into()
            }
            Op::Rebuild => "the plan is being refreshed — wait for it to finish".into(),
        }
    }
}

/// The running operation, if any, with its cancel flag.
#[derive(Default)]
pub struct Busy {
    slot: Mutex<Option<(Cancel, Op)>>,
    /// The last finished apply, until the next one starts.
    last_done: Mutex<Option<ApplyDone>>,
    /// A rebuild was asked for while one ran: it runs once more.
    rerun: Mutex<bool>,
}

impl Busy {
    // The slot holds plain data: a panic elsewhere cannot leave it torn.
    fn slot(&self) -> MutexGuard<'_, Option<(Cancel, Op)>> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Claim the slot for `op`; while another operation runs, why not.
    pub fn begin(&self, op: Op) -> Result<Cancel, String> {
        let mut slot = self.slot();
        if let Some((_, running)) = &*slot {
            return Err(running.busy_message());
        }
        let c = Cancel::new();
        if matches!(op, Op::Apply { .. }) {
            *self.last_done.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
        *slot = Some((c.clone(), op));
        Ok(c)
    }

    /// Ask for another rebuild while one runs. `true`: it will run again
    /// after the current pass; `false`: no rebuild runs (start one).
    pub fn request_rerun(&self) -> bool {
        let slot = self.slot();
        if matches!(&*slot, Some((_, Op::Rebuild))) {
            *self.rerun.lock().unwrap_or_else(|e| e.into_inner()) = true;
            return true;
        }
        false
    }

    /// The end of a rebuild pass: `true` keeps the slot (another pass was
    /// asked for meanwhile), `false` frees it. Decided under the slot lock,
    /// so no request is lost and no other operation sneaks in between.
    pub fn finish_rebuild(&self) -> bool {
        let mut slot = self.slot();
        let mut again = self.rerun.lock().unwrap_or_else(|e| e.into_inner());
        if std::mem::take(&mut *again) {
            return true;
        }
        *slot = None;
        false
    }

    /// Whether any operation holds the slot.
    pub fn is_busy(&self) -> bool {
        self.slot().is_some()
    }

    pub fn finish(&self) {
        *self.slot() = None;
    }

    /// The request of the running scan, if a scan runs.
    pub fn scan(&self) -> Option<ScanRequest> {
        match &*self.slot() {
            Some((_, Op::Scan(r))) => Some(*r),
            _ => None,
        }
    }

    /// The running apply and its progress so far, if an apply runs.
    pub fn apply(&self) -> Option<ApplyStatus> {
        match &*self.slot() {
            Some((_, Op::Apply { request, progress })) => Some(ApplyStatus {
                request: request.clone(),
                progress: progress.clone(),
            }),
            _ => None,
        }
    }

    /// Keep the result of the apply that just ended, for a reloaded webview.
    pub fn set_last_done(&self, d: ApplyDone) {
        *self.last_done.lock().unwrap_or_else(|e| e.into_inner()) = Some(d);
    }

    /// The running apply and the last finished one.
    pub fn apply_state(&self) -> ApplyState {
        ApplyState {
            running: self.apply(),
            last_done: self
                .last_done
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        }
    }

    /// Record the running apply's progress (for a reloaded webview).
    pub fn set_apply_progress(&self, p: ApplyProgress) {
        if let Some((_, Op::Apply { progress, .. })) = &mut *self.slot() {
            *progress = p;
        }
    }

    /// The running undo and its progress so far, if an undo runs.
    pub fn undo(&self) -> Option<UndoStatus> {
        match &*self.slot() {
            Some((
                _,
                Op::Undo {
                    report_path,
                    progress,
                },
            )) => Some(UndoStatus {
                report_path: report_path.clone(),
                progress: progress.clone(),
            }),
            _ => None,
        }
    }

    /// Record the running undo's progress (for a reloaded webview).
    pub fn set_undo_progress(&self, p: UndoProgress) {
        if let Some((_, Op::Undo { progress, .. })) = &mut *self.slot() {
            *progress = p;
        }
    }

    /// Ask the running operation to stop if it is of `kind`; whether it was.
    pub fn cancel(&self, kind: Kind) -> bool {
        match &*self.slot() {
            Some((c, op)) if op.kind() == kind => {
                c.cancel();
                true
            }
            _ => false,
        }
    }
}

/// Run `work` in the slot already claimed from `busy` and free the slot
/// however it ends: a panic becomes the error `on_panic` makes of its text,
/// so the UI always gets an end and the next operation can start.
pub fn run_guarded<T, E>(
    busy: &Busy,
    work: impl FnOnce() -> Result<T, E>,
    on_panic: impl FnOnce(String) -> E,
) -> Result<T, E> {
    struct Free<'a>(&'a Busy);
    impl Drop for Free<'_> {
        fn drop(&mut self) {
            self.0.finish();
        }
    }
    let _free = Free(busy);
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or_else(|payload| {
        let what = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown error".into());
        Err(on_panic(what))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply_op(dry_run: bool) -> Op {
        Op::Apply {
            request: ApplyRequest {
                plan_path: "/p/a.plan.json".into(),
                dry_run,
                jobs: None,
                allow_anomalies: false,
                pause_sync: false,
            },
            progress: ApplyProgress::default(),
        }
    }

    #[test]
    fn the_last_apply_result_is_kept_until_the_next_apply_starts() {
        use crate::apply::{ApplyDone, ApplySummary};
        let b = Busy::default();
        assert!(b.apply_state().last_done.is_none());
        b.begin(apply_op(false)).unwrap();
        b.set_last_done(ApplyDone {
            plan_path: "/p/a.plan.json".into(),
            dry_run: false,
            report_path: None,
            summary: ApplySummary::default(),
            failures: vec![],
            warnings: vec![],
        });
        b.finish();
        let st = b.apply_state();
        assert!(st.running.is_none());
        assert_eq!(st.last_done.unwrap().plan_path, "/p/a.plan.json");
        b.begin(apply_op(true)).unwrap();
        assert!(b.apply_state().last_done.is_none());
        assert!(b.apply_state().running.is_some());
    }

    #[test]
    fn a_scan_cannot_start_during_an_apply_and_vice_versa() {
        let b = Busy::default();
        b.begin(apply_op(false)).unwrap();
        let e = b.begin(Op::Scan(ScanRequest::default())).unwrap_err();
        assert!(e.contains("plan run"), "{e}");
        assert!(b.begin(Op::Export).is_err());
        assert!(b.begin(apply_op(true)).is_err());
        assert_eq!(b.scan(), None);
        b.finish();

        b.begin(Op::Scan(ScanRequest::default())).unwrap();
        let e = b.begin(apply_op(false)).unwrap_err();
        assert!(e.contains("scan"), "{e}");
        assert!(b.apply().is_none());
        b.finish();

        b.begin(Op::Export).unwrap();
        assert!(b.begin(apply_op(true)).unwrap_err().contains("export"));
        b.finish();
        assert!(b.begin(apply_op(true)).is_ok());
    }

    #[test]
    fn cancel_only_reaches_the_kind_asked() {
        let b = Busy::default();
        assert!(!b.cancel(Kind::Apply));
        let c = b.begin(apply_op(false)).unwrap();
        // "Cancel" of a scan must not stop an apply.
        assert!(!b.cancel(Kind::Scan));
        assert!(!c.is_cancelled());
        assert!(b.cancel(Kind::Apply));
        assert!(c.is_cancelled());
    }

    #[test]
    fn apply_status_carries_progress_for_reattach() {
        let b = Busy::default();
        b.set_apply_progress(ApplyProgress::default()); // nothing runs: no-op
        b.begin(apply_op(true)).unwrap();
        let p = ApplyProgress {
            done: 2,
            total: 5,
            current: Some("/s/a.jpg".into()),
            failed: 1,
            skipped: 0,
        };
        b.set_apply_progress(p.clone());
        let st = b.apply().unwrap();
        assert!(st.request.dry_run);
        assert_eq!(st.progress, p);
    }

    #[test]
    fn an_undo_or_retry_holds_the_slot_and_reports_progress() {
        let b = Busy::default();
        assert!(b.undo().is_none());
        let c = b
            .begin(Op::Undo {
                report_path: "/p/a.plan.report.json".into(),
                progress: UndoProgress::default(),
            })
            .unwrap();
        let e = b.begin(apply_op(false)).unwrap_err();
        assert!(e.contains("undo"), "{e}");
        assert!(b.begin(Op::Scan(ScanRequest::default())).is_err());
        assert!(b.begin(Op::Retry).is_err());
        let p = UndoProgress {
            done: 1,
            total: 4,
            current: Some("/s/a.jpg".into()),
        };
        b.set_undo_progress(p.clone());
        assert_eq!(b.undo().unwrap().progress, p);
        assert!(!b.cancel(Kind::Apply));
        assert!(b.cancel(Kind::Undo));
        assert!(c.is_cancelled());
        b.finish();

        b.begin(Op::Retry).unwrap();
        assert!(b.begin(apply_op(true)).unwrap_err().contains("retry"));
        assert!(b.undo().is_none());
        b.finish();
    }

    #[test]
    fn a_rebuild_shares_the_slot_and_never_runs_during_an_apply() {
        let b = Busy::default();
        b.begin(apply_op(false)).unwrap();
        let e = b.begin(Op::Rebuild).unwrap_err();
        assert!(e.contains("plan run"), "{e}");
        // Nothing to go round again for: no rebuild runs.
        assert!(!b.request_rerun());
        b.finish();

        let c = b.begin(Op::Rebuild).unwrap();
        // An apply, an AI scan, an export and an undo wait for it.
        let e = b.begin(apply_op(false)).unwrap_err();
        assert!(e.contains("being refreshed"), "{e}");
        assert!(b.begin(Op::Scan(ScanRequest::default())).is_err());
        assert!(b.begin(Op::Export).is_err());
        assert!(b.apply().is_none());
        assert!(!b.cancel(Kind::Apply));
        assert!(b.cancel(Kind::Rebuild));
        assert!(c.is_cancelled());
        b.finish();
        assert!(b.begin(apply_op(false)).is_ok());
    }

    #[test]
    fn a_rebuild_asked_during_a_rebuild_runs_once_more_and_then_frees_the_slot() {
        let b = Busy::default();
        b.begin(Op::Rebuild).unwrap();
        assert!(b.request_rerun());
        assert!(b.request_rerun(), "asked twice, still one more pass");
        // The first pass ends: the slot stays, another pass follows.
        assert!(b.finish_rebuild());
        assert!(b.is_busy());
        assert!(b.begin(apply_op(false)).is_err());
        // The second pass ends with nothing asked: the slot is free.
        assert!(!b.finish_rebuild());
        assert!(!b.is_busy());
        assert!(b.begin(apply_op(false)).is_ok());
    }

    #[test]
    fn a_panic_frees_the_slot() {
        let b = Busy::default();
        b.begin(apply_op(false)).unwrap();
        let out: Result<(), String> = run_guarded(&b, || panic!("boom"), |w| format!("x: {w}"));
        assert_eq!(out, Err("x: boom".into()));
        assert!(b.apply().is_none());
        assert!(b.begin(Op::Export).is_ok());
    }
}
