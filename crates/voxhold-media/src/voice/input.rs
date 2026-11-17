use super::*;

pub(super) fn normalize_input(input: Option<String>) -> Option<String> {
    input.filter(|id| !id.is_empty())
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
    fn install_software_input(
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
            self.events.clone(),
        );
        mic.start(input.as_deref())
            .map_err(|_| "Выбранный микрофон недоступен.")?;
        let track = self
            .audio
            .0
            .create_audio_track("voxhold-microphone", source.clone());
        if let Some(sender) = &self.sender {
            sender
                .set_track(Some(MediaStreamTrack::Audio(track.clone())))
                .map_err(|_| "Не удалось подключить микрофон к WebRTC.")?;
        }
        if let Some(mut old) = self.mic.take() {
            old.stop();
        }
        self.capture.attach_source(track, source);
        self.input_progress = mic.progress();
        self.input_progress_at = std::time::Instant::now();
        self.mic = Some(mic);
        self.controls.input = input.clone();
        self.input_opened = input;
        self.input_index = None;
        self.report_audio(events);
        Ok(())
    }

    /// Switch to the platform ADM capture path (used when WinMM cannot open
    /// any microphone, or when the software driver reports a failure).
    fn install_platform_input(
        &mut self,
        input: Option<String>,
        events: &Events,
    ) -> Result<(), &'static str> {
        let input = normalize_input(input);
        if let Some(id) = input.as_deref() {
            if !self.audio.select(id, true) {
                return Err("Выбранный микрофон недоступен.");
            }
            self.input_index = self.audio.resolve(id, true);
        } else {
            if !self.audio.select("", true) {
                return Err("Системный микрофон недоступен.");
            }
            self.input_index = None;
        }
        self.audio.set_recording(true);
        let track = self.audio.0.create_device_audio_track("voxhold-microphone");
        if let Some(sender) = &self.sender {
            if sender
                .set_track(Some(MediaStreamTrack::Audio(track.clone())))
                .is_err()
            {
                self.audio.set_recording(false);
                return Err("Не удалось подключить микрофон к WebRTC.");
            }
        }
        if let Some(mut old) = self.mic.take() {
            old.stop();
        }
        self.capture.attach(track);
        self.mic = None;
        self.input_progress = 0;
        self.input_progress_at = std::time::Instant::now();
        self.controls.input = input.clone();
        self.input_opened = input;
        self.report_audio(events);
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
        if let Some(id) = self.controls.input.clone() {
            if self.install_software_input(Some(id), events).is_ok() {
                events.push(Event::DeviceError {
                    message: "Микрофон перезапущен.",
                });
                return;
            }
        }
        if self.install_software_input(None, events).is_ok() {
            events.push(Event::DeviceError {
                message: "Микрофон недоступен, используется системный.",
            });
            return;
        }
        if self.install_platform_input(None, events).is_ok() {
            events.push(Event::DeviceError {
                message: "Микрофон недоступен, звук переведён на системный ADM.",
            });
            return;
        }
        self.input_recovery_failed = true;
        events.push(Event::DeviceError {
            message: "Микрофон недоступен, звук не восстановлен.",
        });
    }
}
