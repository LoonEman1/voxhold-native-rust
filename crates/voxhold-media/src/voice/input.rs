use super::*;

pub(super) fn normalize_input(input: Option<String>) -> Option<String> {
    input.filter(|id| !id.is_empty())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RecoveryStep {
    /// Reopen the selected microphone once in this failure episode.
    ReopenSelected,
    /// The selected retry is spent; try the bounded system WinMM input.
    System,
    /// The system input budget is spent; go straight to the platform ADM.
    Platform,
}

/// Recovery ladder for a capture that stopped delivering PCM. The selected
/// device is only retried once, and the system WinMM input has its own bounded
/// budget so a device that opens but stays silent cannot loop forever.
pub(super) fn recovery_step(
    has_selected: bool,
    reopen_attempted: bool,
    system_attempts: u8,
) -> RecoveryStep {
    if has_selected && !reopen_attempted {
        RecoveryStep::ReopenSelected
    } else if system_attempts >= MAX_SYSTEM_INPUT_ATTEMPTS {
        RecoveryStep::Platform
    } else {
        RecoveryStep::System
    }
}

impl Session {
    pub(super) fn report_audio(&self, events: &Events) {
        let applied = self.controls.audio_params();
        let (capture, noise_mode, echo, gain) = if self.mic.is_some() {
            (
                "software",
                applied.mode.as_str(),
                ECHO_CANCELLATION_AVAILABLE && applied.echo_cancellation,
                applied.auto_gain_control,
            )
        } else {
            ("platform", NoiseMode::Auto.as_str(), true, true)
        };
        events.push(Event::Audio {
            capture,
            noise_mode,
            echo_cancellation: echo,
            auto_gain_control: gain,
        });
    }

    /// Replace the outgoing track with the software capture. The new device is
    /// opened and the peer sender is switched before the old capture stops, so
    /// a failed selection leaves the working path untouched.
    pub(super) fn install_software_input(
        &mut self,
        input: Option<String>,
        events: &Events,
    ) -> Result<(), &'static str> {
        let input = normalize_input(input);
        self.capture.set_params(self.controls.audio_params());
        let params = self.capture.params();
        let source = libwebrtc::audio_source::native::NativeAudioSource::new(
            capture::source_options(),
            capture::SAMPLE_RATE,
            capture::CHANNELS,
            0,
        );
        let mut mic = MicCapture::new(
            source.clone(),
            params,
            self.capture.permission_epoch(),
            Some(self.capture.clone()),
            self.events.clone(),
        );
        mic.start(input.as_deref())
            .map_err(|_| "Выбранный микрофон недоступен.")?;
        let track = self
            .audio
            .0
            .create_audio_track("voxhold-microphone", source.clone());
        // The sender must never transmit a freshly created track before the
        // gate has applied the current mute/PTT/permission state.
        track.set_enabled(false);
        if let Some(sender) = &self.sender {
            sender
                .set_track(Some(MediaStreamTrack::Audio(track.clone())))
                .map_err(|_| "Не удалось подключить микрофон к WebRTC.")?;
        }
        if let Some(mut old) = self.mic.take() {
            old.stop();
        }
        // The ADM fallback may still hold the microphone; software capture is
        // now the only owner.
        if self.audio.running(true) {
            self.audio.stop(true);
        }
        self.audio.set_recording(false);
        self.capture.attach_source(track, source);
        self.input_progress = mic.progress();
        self.input_progress_at = std::time::Instant::now();
        self.input_started_at = std::time::Instant::now();
        self.input_samples = None;
        self.input_connected = false;
        self.input_stable_since = None;
        self.mic = Some(mic);
        self.controls.input = input.clone();
        self.input_opened = input;
        self.input_index = None;
        self.report_audio(events);
        Ok(())
    }

    /// Switch to the platform ADM capture path (used when WinMM cannot open
    /// any microphone, or when the software driver reports a failure).
    ///
    /// The recording path is started and confirmed before the sender track is
    /// replaced. A failed start or `set_track` rolls the ADM selection back to
    /// the previous device (or releases it when software capture was already
    /// working), so a failed fallback never turns the working input into
    /// silence.
    pub(super) fn install_platform_input(
        &mut self,
        input: Option<String>,
        events: &Events,
    ) -> Result<(), &'static str> {
        let input = normalize_input(input);
        let had_platform = self.mic.is_none();
        let previous_opened = self.input_opened.clone();
        let previous_index = self.input_index;
        let Some(index) = self.audio.resolve(input.as_deref().unwrap_or(""), true) else {
            return Err(if input.is_some() {
                "Выбранный микрофон недоступен."
            } else {
                "Системный микрофон недоступен."
            });
        };
        self.audio.set_recording(true);
        // `apply(..., running = true)` stops the previous ADM station, selects
        // the new device and returns the result of StartRecording. Checking
        // only `recording_is_initialized()` would report a failed start as a
        // working microphone.
        if !self.audio.apply(index, true, true) {
            self.restore_platform_input(had_platform, &previous_opened, previous_index);
            return Err("Не удалось запустить микрофон ADM.");
        }
        let track = self.audio.0.create_device_audio_track("voxhold-microphone");
        // The new track is attached to the sender below, before CaptureGate
        // knows it. Keep it disabled until the gate has applied the current
        // mute/PTT/server permission.
        track.set_enabled(false);
        if let Some(sender) = &self.sender {
            if sender
                .set_track(Some(MediaStreamTrack::Audio(track.clone())))
                .is_err()
            {
                self.restore_platform_input(had_platform, &previous_opened, previous_index);
                return Err("Не удалось подключить микрофон к WebRTC.");
            }
        }
        if let Some(mut old) = self.mic.take() {
            old.stop();
        }
        // Registers the track and enables it only if the live permission
        // allows transmission right now.
        self.capture.attach(track);
        self.mic = None;
        self.input_progress = 0;
        self.input_progress_at = std::time::Instant::now();
        self.input_started_at = std::time::Instant::now();
        self.input_samples = None;
        self.input_connected = false;
        self.input_stable_since = None;
        self.controls.input = input.clone();
        self.input_opened = input;
        self.input_index = Some(index);
        self.report_audio(events);
        Ok(())
    }

    /// Restore the path that was live before a failed platform switch. When
    /// software capture was already working, the ADM must be released and the
    /// old sender track stays untouched. When the ADM was the previous path,
    /// reopen its device and start recording again.
    fn restore_platform_input(
        &mut self,
        had_platform: bool,
        previous_opened: &Option<String>,
        previous_index: Option<u16>,
    ) {
        if !had_platform {
            self.release_platform_capture();
            return;
        }
        let previous = previous_opened.as_deref().unwrap_or("");
        let restored = self
            .audio
            .resolve(previous, true)
            .or_else(|| self.audio.resolve("", true))
            .filter(|index| self.audio.apply(*index, true, true));
        if let Some(index) = restored {
            self.input_index = Some(index);
        } else {
            self.input_index = previous_index;
            self.release_platform_capture();
        }
    }

    fn release_platform_capture(&self) {
        self.audio.set_recording(false);
        if self.audio.running(true) {
            let _ = self.audio.stop(true);
        }
    }

    /// Explicit input selection. It has its own request/generation path so a
    /// later PTT or DSP update cannot displace it, and the typed result tells
    /// the UI which device is actually applied after a fallback.
    pub(super) fn select_input(
        &mut self,
        input: Option<String>,
        generation: u64,
        events: &Events,
    ) -> Result<(), &'static str> {
        let error = self
            .switch_input(normalize_input(input), true, events)
            .err();
        events.push(Event::InputApplied {
            generation,
            input: self.controls.input.clone(),
            error,
        });
        // A failed explicit selection keeps the previous working path and is
        // reported through the typed event, not by ending the call.
        Ok(())
    }

    /// Try the requested software device, then the system WinMM device, then
    /// the platform ADM. This keeps the DSP alive across a temporary device
    /// failure and lets a later explicit selection return to software.
    pub(super) fn switch_input(
        &mut self,
        requested: Option<String>,
        retry: bool,
        events: &Events,
    ) -> Result<(), &'static str> {
        let requested = normalize_input(requested);
        if !retry && self.controls.input == requested {
            return Ok(());
        }
        self.input_recovery_failed = false;
        self.input_reopen_attempted = false;
        self.input_system_attempts = 0;
        self.input_stable_since = None;
        if self
            .install_software_input(requested.clone(), events)
            .is_ok()
        {
            return Ok(());
        }
        if requested.is_some() && self.install_software_input(None, events).is_ok() {
            events.push(Event::DeviceError {
                message: "Выбранный микрофон недоступен, используется системный.",
            });
            return Ok(());
        }
        if self
            .install_platform_input(requested.clone(), events)
            .is_ok()
        {
            return Ok(());
        }
        if self.install_platform_input(None, events).is_ok() {
            events.push(Event::DeviceError {
                message: "Микрофон недоступен, используется системный ADM.",
            });
            return Ok(());
        }
        Err("Не удалось открыть микрофон.")
    }

    /// WinMM stopped delivering or requeueing buffers (USB unplug, driver
    /// reset). First retry the selected id, then the system WinMM input, then
    /// the platform ADM. Keep the call alive.
    pub(super) fn recover_input(&mut self, events: &Events) {
        self.input_progress_at = std::time::Instant::now();
        let step = recovery_step(
            self.controls.input.is_some(),
            self.input_reopen_attempted,
            self.input_system_attempts,
        );
        if step == RecoveryStep::ReopenSelected {
            self.input_reopen_attempted = true;
            if let Some(id) = self.controls.input.clone() {
                if self.install_software_input(Some(id), events).is_ok() {
                    self.report_input_applied(events, None);
                    events.push(Event::DeviceError {
                        message: "Микрофон перезапущен.",
                    });
                    return;
                }
            }
        }
        if step != RecoveryStep::Platform && self.input_system_attempts < MAX_SYSTEM_INPUT_ATTEMPTS
        {
            self.input_system_attempts += 1;
            if self.install_software_input(None, events).is_ok() {
                self.report_input_applied(events, None);
                events.push(Event::DeviceError {
                    message: "Микрофон недоступен, используется системный.",
                });
                return;
            }
        }
        // A system WinMM device that opens but stays silent would otherwise be
        // chosen again on every recovery tick. Once its budget is spent the
        // ladder must fall through to the platform ADM.
        if self.install_platform_input(None, events).is_ok() {
            self.input_system_attempts = 0;
            self.report_input_applied(events, None);
            events.push(Event::DeviceError {
                message: "Микрофон недоступен, звук переведён на системный ADM.",
            });
            return;
        }
        self.input_recovery_failed = true;
        self.report_input_applied(events, Some("Микрофон недоступен, звук не восстановлен."));
        events.push(Event::DeviceError {
            message: "Микрофон недоступен, звук не восстановлен.",
        });
    }

    fn report_input_applied(&self, events: &Events, error: Option<&'static str>) {
        events.push(Event::InputApplied {
            generation: 0,
            input: self.controls.input.clone(),
            error,
        });
    }
}
