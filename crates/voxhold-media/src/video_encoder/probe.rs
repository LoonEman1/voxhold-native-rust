//! The boundary between the deterministic encoder model and real hardware.
//!
//! The service owns cancellation and generation; a platform probe only answers
//! one question for one adapter, codec and query. It must check `cancel`
//! between hardware stages and release every COM object before returning, so a
//! cancelled probe leaves no transform or encoded frame behind.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use super::{AdapterInfo, AdapterInventory, Codec, EncodeQuery, EncoderReason, SdrProfile};

/// Cancellation token of one probe generation. The current generation lives in
/// the service; an older probe sees a different value at its next checkpoint.
#[derive(Clone)]
pub struct ProbeCancel {
    current: Arc<AtomicU64>,
    generation: u64,
}

impl ProbeCancel {
    pub(crate) fn new(current: Arc<AtomicU64>, generation: u64) -> Self {
        Self {
            current,
            generation,
        }
    }

    /// A token that never cancels. Only the bounded revalidation uses it, so a
    /// live-publication check cannot be discarded as a stale dialog probe.
    pub(crate) fn never() -> Self {
        let current = Arc::new(AtomicU64::new(0));
        Self {
            current,
            generation: 0,
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.current.load(Ordering::Acquire) != self.generation
    }
}

/// The result of probing one codec on one adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeOutcome {
    Confirmed(SdrProfile),
    Unavailable(EncoderReason),
    /// The generation changed. The service drops the whole attempt.
    Cancelled,
}

/// Hardware probing is deliberately a trait: CI runs the same model against
/// fake adapters, while Windows runs DXGI and Media Foundation.
pub trait HardwareProbe: Send + Sync {
    /// The current suitable adapter list and the Windows primary adapter.
    fn inventory(&self) -> Result<AdapterInventory, String>;

    /// Enumerate a hardware encoder for this output format, activate it on the
    /// adapter LUID, set the NV12/profile/resolution/FPS types and encode one
    /// keyframe. `cancel` must be checked between stages.
    fn probe(
        &self,
        adapter: &AdapterInfo,
        codec: Codec,
        query: EncodeQuery,
        cancel: &ProbeCancel,
    ) -> ProbeOutcome;
}
