//! Bounded last-value request slots shared by `VoiceHandle` and the worker.

use super::*;

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
    /// Explicit output selection from settings. This is separate from generic
    /// control updates so it always reaches the media engine even when the
    /// runtime cache still describes the pre-unplug device.
    pub(super) output: Mutex<Option<Option<String>>>,
    pub(super) viewport: Mutex<Option<(u32, u32)>>,
    pub(super) leave: AtomicBool,
    pub(super) notify: Notify,
}

pub(super) enum Pending {
    Leave,
    Stream(StreamCommand),
    Output(Option<String>),
    Input(InputRequest),
    Controls(Controls, bool),
    StreamMute(u64, bool),
    LocalMute(String, bool),
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
