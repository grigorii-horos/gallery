//! Progress reporting and cooperative cancellation of long operations.
//!
//! A [`Progress`] sink gets one [`ProgressEvent`] per finished unit of work
//! (a scanned file, an EXIF batch, a classified file, a planned file, an
//! applied or undone item). Sinks are called from worker threads and must be
//! cheap: throttle any rendering on the receiving side.
//!
//! A [`Cancel`] flag is checked between units of work. Read-only stages
//! (scan, EXIF, AI, plan) stop with [`crate::Error::Cancelled`]; apply and
//! undo stop starting new items, let the ones in flight finish and return a
//! valid, shorter report.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;

/// The stage an event belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Scan,
    Exif,
    Ai,
    Plan,
    Apply,
    Undo,
}

/// `done` of `total` units of `stage` are finished; `total` is 0 when not
/// known yet (scan). `current` is the unit just finished, if it has a path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProgressEvent {
    pub stage: Stage,
    pub done: usize,
    pub total: usize,
    pub current: Option<PathBuf>,
}

/// Receiver of progress events; called from worker threads.
pub trait Progress: Send + Sync {
    fn step(&self, ev: ProgressEvent);
}

/// Discards every event.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoProgress;

impl Progress for NoProgress {
    fn step(&self, _: ProgressEvent) {}
}

impl<F: Fn(ProgressEvent) + Send + Sync> Progress for F {
    fn step(&self, ev: ProgressEvent) {
        self(ev)
    }
}

/// A shared cancellation flag; clones share it. Once set it stays set.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// `Err(Cancelled)` once cancelled.
    pub fn check(&self) -> crate::Result<()> {
        if self.is_cancelled() {
            Err(crate::Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::Mutex;

    /// Records every event.
    #[derive(Default)]
    pub struct Recorder(pub Mutex<Vec<ProgressEvent>>);

    impl Progress for Recorder {
        fn step(&self, ev: ProgressEvent) {
            self.0.lock().unwrap().push(ev);
        }
    }

    impl Recorder {
        pub fn events(&self, stage: Stage) -> Vec<ProgressEvent> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.stage == stage)
                .cloned()
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_is_shared_by_clones() {
        let c = Cancel::new();
        let c2 = c.clone();
        assert!(!c2.is_cancelled());
        assert!(c2.check().is_ok());
        c.cancel();
        assert!(c2.is_cancelled());
        assert!(matches!(c2.check(), Err(crate::Error::Cancelled)));
    }

    #[test]
    fn closures_are_progress_sinks() {
        let n = std::sync::atomic::AtomicUsize::new(0);
        let p = |_: ProgressEvent| {
            n.fetch_add(1, Ordering::SeqCst);
        };
        let ev = ProgressEvent {
            stage: Stage::Scan,
            done: 1,
            total: 0,
            current: None,
        };
        p.step(ev.clone());
        NoProgress.step(ev);
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }
}
