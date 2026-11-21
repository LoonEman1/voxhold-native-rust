//! Coalesced updates from the voice worker to the panel.
//!
//! High-frequency values (levels, speaking, rosters, playout counters) keep
//! only the latest value so a burst of media events cannot grow the queue.
//! Terminal updates are never merged.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use tokio::sync::Notify;

use crate::{
    controller::{AppliedAudio, Participant, Publication},
    mixer::PlayoutStats,
    publisher::Sharing,
    screen_capture::{ScreenSource, SourceKind},
};

pub(super) const UPDATE_CAPACITY: usize = 64;
#[derive(Clone, Debug)]
pub enum VoiceUpdate {
    Connecting,
    Connected,
    Reconnecting,
    Participants(Vec<Participant>),
    Speaking(HashMap<String, bool>),
    LocalLevel {
        level: f64,
        speaking: bool,
    },
    Tracks(std::collections::HashSet<String>),
    Streams(Vec<Publication>),
    StreamStatus {
        selection: u64,
        status: String,
    },
    StreamError {
        selection: u64,
        error: String,
    },
    StreamConnected {
        selection: u64,
        connected: bool,
    },
    /// A stream-audio device notice. It must not close the viewer: on a
    /// successful rollback the video and the previous output stay alive.
    StreamAudio {
        selection: u64,
        error: Option<String>,
    },
    DeviceError(String),
    /// Applied input device for an explicit selection or an engine fallback.
    /// `generation` is zero for engine-initiated changes.
    InputApplied {
        generation: u64,
        input: Option<String>,
        error: Option<String>,
    },
    /// The connection id this client was given on join. The roster can contain
    /// two connections of the same user, so the server-mute row must be found
    /// by this id, not by user id.
    SelfConnection(Option<String>),
    /// The capture chain actually applied by the media engine, including the
    /// platform-ADM fallback and its forced NS/AGC.
    AudioMode(AppliedAudio),
    /// Measured post-mix playout level and counters (software mixer).
    Playout(PlayoutStats),
    /// The screen-sharing attempt state, including its measured counters.
    ScreenShare(Sharing),
    /// The answer of one source listing request. `generation` lets the picker
    /// discard an answer that was already replaced by a refresh.
    ScreenSources {
        generation: u64,
        kind: SourceKind,
        sources: Vec<ScreenSource>,
        error: Option<String>,
    },
    /// The attempt finished. An error reason is carried together with the end
    /// so the UI can never lose it to a coalesced terminal `Left`.
    Ended(Option<String>),
}

impl VoiceUpdate {
    pub(crate) fn same_slot(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::StreamStatus { selection: a, .. }, Self::StreamStatus { selection: b, .. })
            | (Self::StreamError { selection: a, .. }, Self::StreamError { selection: b, .. })
            | (Self::StreamAudio { selection: a, .. }, Self::StreamAudio { selection: b, .. })
            | (
                Self::StreamConnected { selection: a, .. },
                Self::StreamConnected { selection: b, .. },
            ) => a == b,
            (Self::ScreenSources { kind: a, .. }, Self::ScreenSources { kind: b, .. }) => a == b,
            _ => matches!(
                (self, other),
                (Self::Connecting, Self::Connecting)
                    | (Self::Connected, Self::Connected)
                    | (Self::Reconnecting, Self::Reconnecting)
                    | (Self::Participants(_), Self::Participants(_))
                    | (Self::Speaking(_), Self::Speaking(_))
                    | (Self::LocalLevel { .. }, Self::LocalLevel { .. })
                    | (Self::Tracks(_), Self::Tracks(_))
                    | (Self::Streams(_), Self::Streams(_))
                    | (Self::InputApplied { .. }, Self::InputApplied { .. })
                    | (Self::SelfConnection(_), Self::SelfConnection(_))
                    | (Self::AudioMode(_), Self::AudioMode(_))
                    | (Self::Playout(_), Self::Playout(_))
                    | (Self::ScreenShare(_), Self::ScreenShare(_))
            ),
        }
    }

    pub(crate) fn terminal(&self) -> bool {
        matches!(self, Self::Ended(_))
    }
}

/// Coalescing update queue. Terminal updates are never merged into a snapshot.
pub(crate) struct Updates {
    queue: Mutex<VecDeque<VoiceUpdate>>,
    /// Wake of the panel that owns this queue. `None` for a test queue.
    wake: Option<Arc<Notify>>,
}

impl Updates {
    pub(crate) fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            wake: None,
        }
    }

    /// A queue that wakes `wake` when an update turns it from empty into
    /// non-empty. A burst into a queue the panel has not drained yet needs no
    /// extra wake: the one pending wake already covers it.
    pub(crate) fn with_wake(wake: Arc<Notify>) -> Self {
        Self {
            wake: Some(wake),
            ..Self::new()
        }
    }

    pub(crate) fn push(&self, update: VoiceUpdate) {
        let Ok(mut queue) = self.queue.lock() else {
            return;
        };
        let was_empty = queue.is_empty();
        if update.terminal() {
            queue.retain(|old| !old.terminal());
            queue.push_back(update);
        } else if let Some(slot) = queue.iter_mut().find(|old| old.same_slot(&update)) {
            *slot = update;
        } else {
            if queue.len() >= UPDATE_CAPACITY {
                queue.pop_front();
            }
            queue.push_back(update);
        }
        drop(queue);
        // `notify_one` keeps a permit when the panel is between its poll and
        // its wait, so an update that lands exactly in that window is not lost.
        if was_empty {
            if let Some(wake) = &self.wake {
                wake.notify_one();
            }
        }
    }

    pub(crate) fn poll(&self) -> Option<VoiceUpdate> {
        self.queue.lock().ok()?.pop_front()
    }
}
