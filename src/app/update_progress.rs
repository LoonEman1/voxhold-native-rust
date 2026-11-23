//! Download progress counters and the staging phase callback.
//!
//! Staging decides when a download starts again after a failed delta; the UI
//! must not guess that from byte counters, because a full package restarts at
//! zero after the delta already reported its total.

use std::sync::{Arc, Mutex};
use voxhold_update::stage::{PhaseFn, StagePhase};

/// Progress and phase shared between staging and the UI poller.
pub(super) struct ProgressCounters {
    state: Mutex<ProgressState>,
}

#[derive(Clone, Copy)]
struct ProgressState {
    downloaded: u64,
    total: u64,
    phase: StagePhase,
}

impl ProgressCounters {
    pub(super) fn new(total: u64) -> Self {
        Self {
            state: Mutex::new(ProgressState {
                downloaded: 0,
                total,
                phase: StagePhase::Downloading { total },
            }),
        }
    }

    /// Record download bytes without changing the phase.
    pub(super) fn set(&self, downloaded: u64, total: Option<u64>) {
        let mut state = self.lock();
        state.downloaded = downloaded;
        if let Some(total) = total.filter(|value| *value > 0) {
            state.total = total;
        }
    }

    /// Start a new download attempt: zero bytes and a new expected total.
    pub(super) fn restart(&self, total: u64) {
        *self.lock() = ProgressState {
            downloaded: 0,
            total,
            phase: StagePhase::Downloading { total },
        };
    }

    pub(super) fn set_phase(&self, phase: StagePhase) {
        self.lock().phase = phase;
    }

    pub(super) fn snapshot(&self) -> (u64, u64) {
        let state = self.lock();
        (state.downloaded, state.total)
    }

    pub(super) fn phase(&self) -> StagePhase {
        self.lock().phase
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ProgressState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Phase callback for staging: it owns the counters the poller reads.
/// Staging announces every transition; no counters are inferred here.
pub(super) fn phase_counter(progress: Arc<ProgressCounters>) -> PhaseFn {
    Arc::new(move |phase| match phase {
        StagePhase::Downloading { total } => progress.restart(total),
        StagePhase::Preparing => progress.set_phase(StagePhase::Preparing),
    })
}
