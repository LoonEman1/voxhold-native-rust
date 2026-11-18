//! Output-device state kept apart from the confirmed playout path.

use super::*;

#[derive(Clone, Debug)]
pub(super) struct SwitchResult {
    pub(super) switched: bool,
    /// Whether the previous (or default) path was restored after a failure.
    pub(super) audible: bool,
    /// Device id that is actually open after the attempt. `None` means the
    /// Windows default endpoint. This is the applied value, not the request.
    pub(super) applied: Option<String>,
}

/// Selected output, confirmed endpoint and whether playout is actually known
/// to be running. A failed request never becomes an applied value, so the same
/// preference can be retried after a full rollback.
#[derive(Clone, Debug, Default)]
pub(super) struct OutputState {
    pub(super) selected: Option<String>,
    pub(super) applied: Option<String>,
    pub(super) working: bool,
}

impl OutputState {
    pub(super) fn new(selected: Option<String>) -> Self {
        Self {
            applied: selected.clone(),
            selected,
            working: true,
        }
    }

    pub(super) fn needs_switch(&self, requested: &Option<String>) -> bool {
        self.selected != *requested || self.applied != *requested || !self.working
    }

    pub(super) fn request(&mut self, requested: Option<String>) {
        self.selected = requested;
    }

    pub(super) fn confirmed(&mut self, applied: Option<String>) {
        self.applied = applied;
        self.working = true;
    }

    pub(super) fn failed(&mut self) {
        self.applied = None;
        self.working = false;
    }
}

/// The platform ADM is stale when it is no longer initialized, when its
/// captured-sample duration stopped moving, or when it never reported any
/// capture after the transport connected.
pub(super) fn platform_input_stalled(
    running: bool,
    connected: bool,
    samples_seen: bool,
    since_progress: Duration,
    since_start: Duration,
) -> bool {
    if !running {
        return true;
    }
    if !connected {
        return false;
    }
    if samples_seen {
        since_progress >= Duration::from_secs(2)
    } else {
        since_start >= INPUT_CAPTURE_GRACE
    }
}

impl Session {
    /// A hot-unplugged device cannot be reported by WebRTC; poll the Core
    /// Audio list once a second and fall back to the default path.
    pub(super) fn check_devices(&mut self, events: &Events) {
        if self.device_check.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.device_check = std::time::Instant::now();
        for input in [true, false] {
            // A viewer session has no outgoing audio track. Its capture path
            // is intentionally closed and must never be recovered here.
            if input && self.sender.is_none() {
                continue;
            }
            let selected = if input {
                self.input_opened.as_deref()
            } else {
                self.output.applied.as_deref()
            };
            let Some(id) = selected.filter(|id| !id.is_empty()) else {
                continue;
            };
            if input && self.mic.is_some() {
                // The software WinMM path is not enumerated by Core Audio: ask
                // the same resolver the capture path used, so an unplugged
                // selected microphone is recovered instead of staying silent.
                if crate::capture::input_available(id) {
                    continue;
                }
                self.recover_input(events);
                continue;
            }
            let resolved = if !input && self.playout.is_some() {
                // The room sink is a WinMM endpoint, not the ADM index.
                super::winmm_out::resolve_output(id).is_some()
            } else {
                self.audio.resolve(id, input).is_some()
            };
            if resolved {
                continue;
            }
            // The old index may now point at another device. Resolve the
            // system default from scratch instead of reusing it.
            let result = self.switch_device("", input);
            if !input {
                if result.switched || result.audible {
                    self.output.confirmed(result.applied.clone());
                } else {
                    self.output.failed();
                    self.output_index = None;
                }
                let message = if result.switched {
                    "Устройство воспроизведения отключено, используется системное."
                } else if result.audible {
                    "Устройство воспроизведения отключено, прежний путь восстановлен."
                } else {
                    "Устройство воспроизведения отключено, звук не восстановлен."
                };
                // The player needs the same result as a manual switch: the
                // fallback may have changed the actual endpoint while the
                // viewer still remembers the unplugged device.
                events.push(Event::Output {
                    applied: result.applied.clone(),
                    audible: result.audible,
                    working: result.switched || result.audible,
                    error: Some(message),
                });
            }
            if result.switched {
                if input {
                    self.controls.input = None;
                } else {
                    self.controls.output = None;
                    self.output.request(None);
                }
                if input {
                    events.push(Event::InputApplied {
                        generation: 0,
                        input: self.controls.input.clone(),
                        error: None,
                    });
                }
                events.push(Event::DeviceError {
                    message: if input {
                        "Микрофон отключён, используется системный."
                    } else {
                        "Устройство воспроизведения отключено, используется системное."
                    },
                });
            } else {
                events.push(Event::DeviceError {
                    message: if result.audible {
                        if input {
                            "Микрофон отключён, прежний путь восстановлен."
                        } else {
                            "Устройство воспроизведения отключено, прежний путь восстановлен."
                        }
                    } else if input {
                        "Микрофон отключён, звук не восстановлен."
                    } else {
                        "Устройство воспроизведения отключено, звук не восстановлен."
                    },
                });
            }
        }
        // Without an outgoing track there is no capture to watch or recover.
        if self.sender.is_none() {
            return;
        }
        let Some((failed, progress)) = self.mic.as_ref().map(|mic| (mic.failed(), mic.progress()))
        else {
            // The software block counter says nothing about the ADM path.
            // Initialization alone is not proof of recording either: the
            // platform source's captured-sample count must keep moving. Allow
            // a startup grace before the first stats report.
            if self.input_recovery_failed {
                return;
            }
            // Before the transport connects there is no capture stats signal
            // to judge; do not reopen the microphone just because the peer is
            // still negotiating.
            if self.peer.connection_state() != PeerConnectionState::Connected {
                return;
            }
            let stalled = platform_input_stalled(
                self.audio.running(true),
                // The stats probe runs after this check; let it establish the
                // connected baseline first.
                self.input_connected
                    && self.peer.connection_state() == PeerConnectionState::Connected,
                self.input_duration.is_some(),
                self.input_progress_at.elapsed(),
                self.input_started_at.elapsed(),
            );
            if stalled {
                self.recover_input(events);
            }
            return;
        };
        let now = std::time::Instant::now();
        if progress != self.input_progress {
            self.input_progress = progress;
            self.input_progress_at = now;
            match self.input_stable_since {
                Some(since) if now.duration_since(since) >= INPUT_STABLE_CAPTURE => {
                    // A single block was already proven not to be enough:
                    // clear the ladder only after sustained capture.
                    self.input_reopen_attempted = false;
                    self.input_recovery_failed = false;
                    self.input_system_attempts = 0;
                    self.input_stable_since = Some(now);
                }
                None => self.input_stable_since = Some(now),
                Some(_) => {}
            }
            return;
        }
        if failed || self.input_progress_at.elapsed() >= Duration::from_secs(2) {
            if !self.input_recovery_failed {
                self.recover_input(events);
            }
        }
    }
}
