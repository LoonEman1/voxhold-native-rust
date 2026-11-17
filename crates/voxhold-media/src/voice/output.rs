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

impl Session {
    /// A hot-unplugged device cannot be reported by WebRTC; poll the Core
    /// Audio list once a second and fall back to the default path.
    pub(super) fn check_devices(&mut self, events: &Events) {
        if self.device_check.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.device_check = std::time::Instant::now();
        for input in [true, false] {
            let selected = if input {
                self.input_opened.as_deref()
            } else {
                self.output.applied.as_deref()
            };
            let Some(id) = selected.filter(|id| !id.is_empty()) else {
                continue;
            };
            if input && self.mic.is_some() {
                // The software WinMM path is not enumerated by Core Audio, so
                // a name-prefix miss would restart a working microphone every
                // second. Hot-unplug recovery for this path belongs to W02c.
                continue;
            }
            if self.audio.resolve(id, input).is_some() {
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
    }
}
