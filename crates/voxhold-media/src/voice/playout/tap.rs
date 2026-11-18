//! Post-mix block tap for the W07 recorder.

use super::*;

#[allow(dead_code)]
/// One post-mix block tap. W07's recorder subscribes here instead of decoding
/// the same tracks a second time; each subscriber gets its own bounded queue.
///
/// A subscriber that stops reading is **not** removed: its queue fills, the
/// block is counted in `dropped()`, and recording resumes when it drains. Only
/// a receiver that was actually dropped (`Disconnected`) is removed. `try_recv`
/// distinguishes an empty queue from a closed tap, so truncation is never
/// silent.
#[derive(Clone, Default)]
pub(super) struct Tap {
    senders: Arc<Mutex<Vec<TapSender>>>,
}

struct TapSender {
    sender: SyncSender<Vec<i16>>,
    dropped: Arc<AtomicU64>,
}

/// One read attempt from the tap. `Closed` is only returned after the mixer
/// (or its tap) is gone. W07's recorder consumes this API; the media crate has
/// no consumer in this stage.
#[allow(dead_code)]
#[derive(Debug)]
pub(super) enum TapRead {
    Block(Vec<i16>),
    Empty,
    Closed,
}

#[allow(dead_code)]
pub(super) struct TapReceiver {
    receiver: Receiver<Vec<i16>>,
    dropped: Arc<AtomicU64>,
}

#[allow(dead_code)]
impl TapReceiver {
    pub(super) fn try_recv(&self) -> TapRead {
        match self.receiver.try_recv() {
            Ok(block) => TapRead::Block(block),
            Err(std::sync::mpsc::TryRecvError::Empty) => TapRead::Empty,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => TapRead::Closed,
        }
    }

    /// Blocks this subscriber missed because its bounded queue was full.
    pub(super) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Tap {
    pub(super) fn subscribe(&self) -> TapReceiver {
        let (sender, receiver) = sync_channel(16);
        let dropped = Arc::new(AtomicU64::new(0));
        if let Ok(mut senders) = self.senders.lock() {
            senders.push(TapSender {
                sender,
                dropped: dropped.clone(),
            });
        }
        TapReceiver { receiver, dropped }
    }

    #[cfg(test)]
    pub(super) fn subscribers(&self) -> usize {
        self.senders
            .lock()
            .map(|senders| senders.len())
            .unwrap_or(0)
    }

    pub(super) fn emit(&self, block: &[i16]) {
        let Ok(mut senders) = self.senders.lock() else {
            return;
        };
        senders.retain(
            |subscriber| match subscriber.sender.try_send(block.to_vec()) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    // Keep the subscriber and make the loss explicit; a recorder
                    // that catches up must be able to continue.
                    subscriber.dropped.fetch_add(1, Ordering::Relaxed);
                    true
                }
                Err(TrySendError::Disconnected(_)) => false,
            },
        );
    }
}
