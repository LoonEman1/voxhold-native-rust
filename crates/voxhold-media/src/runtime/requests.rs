//! Bounded last-value request slots shared by `VoiceHandle` and the worker.

use super::*;
use crate::publisher::PublishRequest;
use crate::screen_capture::SourceKind;

/// Selecting and closing a stream share one last-value slot. A stale close can
/// therefore never overtake a newer watch intent.
#[derive(Clone, Debug)]
pub(super) enum StreamCommand {
    Watch {
        selection: u64,
        publication: Publication,
    },
    Close {
        selection: u64,
    },
}

/// Screen-share user actions. They are queued in order: opening the picker must
/// reach the worker before the chosen source, and a cancel must be able to
/// clear a pending publish of the same picker.
#[derive(Clone, Debug)]
pub(super) enum ScreenCommand {
    OpenPicker {
        picker: u64,
    },
    Publish {
        picker: u64,
        request: PublishRequest,
        stream_permission: bool,
        slot_available: bool,
    },
    CancelPicker {
        picker: u64,
    },
    /// New settings of a live attempt: one stop followed by one new start.
    Restart {
        request: PublishRequest,
        stream_permission: bool,
        slot_available: bool,
    },
    /// Only the bitrates changed: the live sender applies them without a stop
    /// or a new start.
    LiveQuality {
        request: PublishRequest,
        stream_permission: bool,
        slot_available: bool,
    },
    /// The user closed the quality dialog while the restart preflight was
    /// running. The live stream stays untouched.
    CancelRestart,
    Stop,
}

/// One source listing request. The kind and generation identify the answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SourceRequest {
    pub(super) kind: SourceKind,
    pub(super) generation: u64,
}

/// The panel's confirmed gate for screen sharing. It is a last-value cell: the
/// worker applies the newest value before a preparation may complete.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ScreenAccess {
    pub(super) stream_permission: bool,
    pub(super) slot_available: bool,
}

impl StreamCommand {
    fn selection(&self) -> u64 {
        match self {
            Self::Watch { selection, .. } | Self::Close { selection } => *selection,
        }
    }
}

/// Install a stream intent only if it is not older than the queued or already
/// applied command. Selection ids are monotonic, so a late close for A cannot
/// erase a newer watch for B (and vice versa).
pub(super) fn set_stream_command(requests: &Requests, command: StreamCommand) -> bool {
    let selection = command.selection();
    let Ok(mut queue) = requests.stream.lock() else {
        return false;
    };
    if queue.last_applied > selection
        || queue
            .pending
            .as_ref()
            .is_some_and(|current| current.selection() > selection)
    {
        return false;
    }
    queue.pending = Some(command);
    true
}

/// The last-value slot plus the newest command the worker has already taken.
/// Keeping the applied number after the slot is empty is what lets a newer
/// Close(B) close the still-running A when the intermediate Watch(B) was
/// displaced by the close.
#[derive(Default)]
pub(super) struct StreamRequests {
    pub(super) pending: Option<StreamCommand>,
    pub(super) last_applied: u64,
}

impl StreamRequests {
    pub(super) fn take(&mut self) -> Option<StreamCommand> {
        let command = self.pending.take()?;
        self.last_applied = self.last_applied.max(command.selection());
        Some(command)
    }

    pub(super) fn clear(&mut self) {
        self.pending = None;
    }
}

#[derive(Clone, Debug)]
pub(super) struct InputRequest {
    pub(super) input: Option<String>,
    pub(super) generation: u64,
}

/// Keep the newest explicit input request. Generations are assigned atomically,
/// so a caller that was preempted between `fetch_add` and the lock cannot
/// replace a newer pending selection.
pub(super) fn set_input_request(slot: &mut Option<InputRequest>, request: InputRequest) {
    if slot
        .as_ref()
        .is_none_or(|current| current.generation < request.generation)
    {
        *slot = Some(request);
    }
}

const SCREEN_CAPACITY: usize = 8;
const SOURCE_REQUEST_CAPACITY: usize = 4;

/// Queue one user action. The queue is bounded; only a burst of clicks can
/// overflow it and the oldest action of that burst is the one to drop.
pub(super) fn push_screen(requests: &Requests, command: ScreenCommand) {
    if let Ok(mut queue) = requests.screen.lock() {
        if queue.len() >= SCREEN_CAPACITY {
            queue.pop_front();
        }
        queue.push_back(command);
    }
}

/// Remember the newest listing request of this kind. A late older request
/// never replaces a newer one, and the picker shows one answer per kind.
pub(super) fn set_source_request(requests: &Requests, kind: SourceKind, generation: u64) {
    let Ok(mut queue) = requests.screen_sources.lock() else {
        return;
    };
    if let Some(slot) = queue.iter_mut().find(|request| request.kind == kind) {
        if generation >= slot.generation {
            slot.generation = generation;
        }
        return;
    }
    if queue.len() >= SOURCE_REQUEST_CAPACITY {
        queue.remove(0);
    }
    queue.push(SourceRequest { kind, generation });
}

/// Latest-value request slots shared by the handle and the worker.
#[derive(Default)]
pub(super) struct Requests {
    pub(super) controls: Mutex<Option<(Controls, bool)>>,
    /// Explicit input selection. It has its own slot because a following PTT
    /// or DSP update to `controls` would otherwise overwrite the retry.
    pub(super) input: Mutex<Option<InputRequest>>,
    pub(super) local_mutes: Mutex<VecDeque<(String, bool)>>,
    pub(super) stream: Mutex<StreamRequests>,
    pub(super) stream_mute: Mutex<Option<(u64, bool)>>,
    /// Screen-share actions in order. Low frequency (a few per minute), so a
    /// small bounded queue keeps open→publish→cancel ordering.
    pub(super) screen: Mutex<VecDeque<ScreenCommand>>,
    /// Picker listings. Only the newest request of each kind matters.
    pub(super) screen_sources: Mutex<Vec<SourceRequest>>,
    /// Freshest screen gate from the panel. Shared with the session so the
    /// completion check reads the value directly, not a queued command.
    pub(super) screen_access: Arc<Mutex<Option<ScreenAccess>>>,
    /// Explicit output selection from settings. This is separate from generic
    /// control updates so it always reaches the media engine even when the
    /// runtime cache still describes the pre-unplug device.
    pub(super) output: Mutex<Option<Option<String>>>,
    pub(super) viewport: Mutex<Option<(u32, u32)>>,
    /// System resume/suspend. Last-value: only the newest transition matters.
    pub(super) power: Mutex<Option<bool>>,
    pub(super) leave: AtomicBool,
    pub(super) notify: Notify,
}

pub(super) enum Pending {
    Leave,
    Stream(StreamCommand),
    Screen(ScreenCommand),
    ScreenSources(SourceRequest),
    ScreenAccess(ScreenAccess),
    Output(Option<String>),
    Input(InputRequest),
    Controls(Controls, bool),
    StreamMute(u64, bool),
    LocalMute(String, bool),
    /// `true` = resumed, `false` = suspended.
    Power(bool),
}

impl Requests {
    pub(super) fn wake(&self) {
        self.notify.notify_waiters();
    }

    /// Drain one action by priority. Called in a loop until it returns `None`.
    pub(super) fn take(&self) -> Option<Pending> {
        if self.leave.swap(false, Ordering::SeqCst) {
            return Some(Pending::Leave);
        }
        if let Ok(mut stream) = self.stream.lock() {
            if let Some(command) = stream.take() {
                return Some(Pending::Stream(command));
            }
        }
        if let Ok(mut screen) = self.screen.lock() {
            if let Some(command) = screen.pop_front() {
                return Some(Pending::Screen(command));
            }
        }
        if let Ok(mut sources) = self.screen_sources.lock() {
            if !sources.is_empty() {
                return Some(Pending::ScreenSources(sources.remove(0)));
            }
        }
        if let Ok(mut access) = self.screen_access.lock() {
            if let Some(access) = access.take() {
                return Some(Pending::ScreenAccess(access));
            }
        }
        if let Ok(mut output) = self.output.lock() {
            if let Some(output) = output.take() {
                return Some(Pending::Output(output));
            }
        }
        if let Ok(mut input) = self.input.lock() {
            if let Some(input) = input.take() {
                return Some(Pending::Input(input));
            }
        }
        if let Ok(mut controls) = self.controls.lock() {
            if let Some((controls, self_muted)) = controls.take() {
                return Some(Pending::Controls(controls, self_muted));
            }
        }
        if let Ok(mut power) = self.power.lock() {
            if let Some(resumed) = power.take() {
                return Some(Pending::Power(resumed));
            }
        }
        if let Ok(mut mute) = self.stream_mute.lock() {
            if let Some((selection, muted)) = mute.take() {
                return Some(Pending::StreamMute(selection, muted));
            }
        }
        let item = self.local_mutes.lock().ok()?.pop_front();
        item.map(|(connection, muted)| Pending::LocalMute(connection, muted))
    }

    pub(super) fn viewport(&self) -> (u32, u32) {
        self.viewport
            .lock()
            .ok()
            .and_then(|viewport| *viewport)
            .filter(|(width, height)| *width > 0 && *height > 0)
            .unwrap_or(DEFAULT_VIEWPORT)
    }
}
