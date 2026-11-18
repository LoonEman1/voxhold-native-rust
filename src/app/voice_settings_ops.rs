//! Voice tab operations: device selection, levels, bitrate, input/noise modes,
//! reset and the WinMM preview/meter/test tone lifecycle.
//!
//! Every applied change goes through `VoicePanel::apply_voice_prefs`, so the
//! engine receives one `Command::State` snapshot instead of three separate
//! writes. Local mute and push-to-talk stay with the panel, per the R07a
//! capture-ownership invariant.

use super::*;

impl Voxhold {
    /// Handle a personal-settings action that belongs to the voice tab.
    /// Returns `false` for everything else so `personal_action` keeps its match.
    pub(super) fn voice_settings_action(
        &mut self,
        action: personal_settings::Action,
        cx: &mut Context<Self>,
    ) -> bool {
        use personal_settings::Action;
        match action {
            Action::RefreshDevices => {
                self.load_voice_devices(cx);
                self.start_voice_preview(cx);
            }
            Action::SelectInput(index) | Action::SelectOutput(index) => {
                let input = matches!(action, Action::SelectInput(_));
                let (_, ids, _) = crate::voice_settings::device_choices(
                    &self.voice_settings.prefs,
                    &self.voice_settings.devices,
                    input,
                );
                let Some(id) = ids.get(index).cloned() else {
                    return true;
                };
                if input {
                    self.voice_settings.prefs.input = id;
                } else {
                    self.voice_settings.prefs.output = id;
                }
                self.voice_settings.persist();
                self.apply_voice_snapshot(cx);
                #[cfg(feature = "voxhold-media")]
                if let Some(voice) = &self.voice {
                    if input {
                        // R07b: input changes during a call use their own
                        // generation, so a later PTT/DSP State cannot displace
                        // the explicit choice.
                        let input = self.voice_settings.prefs.input.clone();
                        voice.update(cx, |panel, _| panel.select_input(Some(input)));
                    } else {
                        // R07a: an explicit output choice is its own request,
                        // even when the cached id looks unchanged after a
                        // hot-unplug.
                        let output = self.voice_settings.prefs.output.clone();
                        voice.update(cx, |panel, _| panel.select_output(Some(output)));
                    }
                }
                self.voice_settings.device_notice.clear();
                if input {
                    self.start_voice_preview(cx);
                }
                cx.notify();
            }
            Action::SetVolume(which, value) => {
                match which {
                    "input" => {
                        self.voice_settings.prefs.input_volume = value.clamp(0, 200);
                    }
                    "output" => {
                        self.voice_settings.prefs.output_volume = value.clamp(0, 200);
                    }
                    "noise" => self.voice_settings.prefs.noise_gate = value.clamp(0, 100),
                    _ => self.voice_settings.prefs.sound_volume = value.clamp(0, 100),
                }
                self.voice_settings.persist();
                self.apply_voice_snapshot(cx);
                cx.notify();
            }
            Action::SetBitrate(bitrate) => {
                if crate::voice_settings::BITRATES.contains(&bitrate) {
                    self.voice_settings.prefs.bitrate = bitrate;
                    self.voice_settings.persist();
                    self.apply_voice_snapshot(cx);
                    cx.notify();
                }
            }
            Action::ToggleSounds => {
                self.voice_settings.prefs.sounds = !self.voice_settings.prefs.sounds;
                self.voice_settings.persist();
                cx.notify();
            }
            Action::SetNoiseMode(index) => {
                self.voice_settings.prefs.noise_mode = if index == 1 {
                    "threshold".into()
                } else {
                    "auto".into()
                };
                self.voice_settings.persist();
                self.apply_voice_snapshot(cx);
                cx.notify();
            }
            Action::SetInputMode(index) => {
                let push_to_talk = index == 1;
                self.voice_settings.prefs.input_mode = if push_to_talk {
                    "push_to_talk".into()
                } else {
                    "voice_activity".into()
                };
                self.voice_settings.persist();
                self.apply_voice_snapshot(cx);
                #[cfg(feature = "voxhold-media")]
                if let Some(voice) = &self.voice {
                    voice.update(cx, |voice, cx| {
                        voice.set_push_to_talk(push_to_talk);
                        cx.notify();
                    });
                }
                cx.notify();
            }
            Action::ToggleAutoGain => {
                self.voice_settings.prefs.auto_gain = !self.voice_settings.prefs.auto_gain;
                self.voice_settings.persist();
                self.apply_voice_snapshot(cx);
                cx.notify();
            }
            Action::ResetVoice => {
                // Go resets to `defaultVoicePreferences()` and therefore also
                // clears both selected devices.
                self.voice_settings.prefs = crate::voice_settings::VoicePrefs::default();
                self.voice_settings.persist();
                self.apply_voice_snapshot(cx);
                #[cfg(feature = "voxhold-media")]
                if let Some(voice) = &self.voice {
                    // The empty selection is the system default, and it must
                    // reach the live software/ADM input, not only the file.
                    voice.update(cx, |panel, _| panel.select_input(None));
                }
                #[cfg(feature = "voxhold-media")]
                if let Some(voice) = &self.voice {
                    voice.update(cx, |voice, cx| {
                        voice.set_push_to_talk(false);
                        cx.notify();
                    });
                }
                self.voice_settings.device_notice.clear();
                self.voice_settings.tone_error.clear();
                self.start_voice_preview(cx);
                cx.notify();
            }
            Action::TestMicrophone => {
                #[cfg(feature = "voxhold-media")]
                {
                    let in_call = self.voice_call_active(cx);
                    if in_call {
                        // The transmission already owns the microphone; a
                        // second WinMM capture would fight it for the device.
                        self.stop_voice_preview();
                        self.voice_settings.device_notice =
                            "В звонке индикатор показывает уровень передачи. Проверка микрофона идёт только вне звонка."
                                .into();
                    } else {
                        self.voice_settings.device_notice.clear();
                        self.start_voice_preview(cx);
                    }
                }
                #[cfg(not(feature = "voxhold-media"))]
                {
                    self.voice_settings.preview_error =
                        "Проверка микрофона доступна только в сборке с медиа.".into();
                }
                cx.notify();
            }
            Action::TestTone => self.play_voice_test_tone(cx),
            _ => return false,
        }
        true
    }

    /// The single audio snapshot for the settings tab.
    fn apply_voice_snapshot(&mut self, cx: &mut Context<Self>) {
        #[cfg(feature = "voxhold-media")]
        {
            let Some(voice) = &self.voice else {
                return;
            };
            let prefs = &self.voice_settings.prefs;
            let input = Some(prefs.input.clone());
            let output = Some(prefs.output.clone());
            let bitrate = prefs.bitrate as u32;
            let input_volume = prefs.input_volume;
            let output_volume = prefs.output_volume;
            let noise_mode = prefs.noise_mode.clone();
            let noise_gate = prefs.noise_gate;
            let echo_cancellation = prefs.echo_cancellation;
            let auto_gain = prefs.auto_gain;
            voice.update(cx, |panel, _| {
                panel.apply_voice_prefs(
                    input,
                    output,
                    bitrate,
                    input_volume,
                    output_volume,
                    &noise_mode,
                    noise_gate,
                    echo_cancellation,
                    auto_gain,
                );
            });
        }
        #[cfg(not(feature = "voxhold-media"))]
        {
            let _ = cx;
        }
    }

    /// Whether tab 1 of the personal dialog is currently visible.
    fn voice_tab_open(&self) -> bool {
        self.active_settings == Some(SettingsKind::Personal)
            && self.personal.as_ref().is_some_and(|panel| panel.tab() == 1)
    }

    fn voice_call_active(&self, cx: &Context<Self>) -> bool {
        #[cfg(feature = "voxhold-media")]
        {
            self.voice
                .as_ref()
                .is_some_and(|panel| panel.read(cx).call_active())
        }
        #[cfg(not(feature = "voxhold-media"))]
        {
            let _ = cx;
            false
        }
    }

    fn voice_local_level(&self, cx: &Context<Self>) -> f64 {
        #[cfg(feature = "voxhold-media")]
        {
            self.voice
                .as_ref()
                .map(|panel| panel.read(cx).local_level())
                .unwrap_or(0.0)
        }
        #[cfg(not(feature = "voxhold-media"))]
        {
            let _ = cx;
            0.0
        }
    }

    fn voice_device_error(&self, cx: &Context<Self>) -> String {
        #[cfg(feature = "voxhold-media")]
        {
            self.voice
                .as_ref()
                .map(|panel| panel.read(cx).device_error().to_owned())
                .unwrap_or_default()
        }
        #[cfg(not(feature = "voxhold-media"))]
        {
            let _ = cx;
            String::new()
        }
    }

    /// Start the WinMM level preview for the visible voice tab outside a call.
    /// The preview lives only as long as the tab.
    pub(super) fn start_voice_preview(&mut self, cx: &mut Context<Self>) {
        self.stop_voice_preview();
        self.voice_settings.preview_error.clear();
        // Tests never open a real microphone: the WinMM math is covered by
        // pure fixtures and the GPUI cases drive an in-call/inactive tab.
        #[cfg(test)]
        {
            let _ = cx;
        }
        #[cfg(all(feature = "voxhold-media", not(test)))]
        {
            if !self.voice_tab_open() {
                return;
            }
            if self.voice_call_active(cx) {
                return;
            }
            let device = self.voice_settings.prefs.input.clone();
            match voxhold_media::preview::start_level(&device) {
                Ok(preview) => self.voice_settings.preview = Some(preview),
                Err(error) => self.voice_settings.preview_error = error,
            }
        }
        #[cfg(all(not(feature = "voxhold-media"), not(test)))]
        {
            let _ = cx;
        }
    }

    /// Signal the preview thread and drop the handle. Never joins the UI
    /// thread; the thread exits within one 50 ms tick.
    pub(super) fn stop_voice_preview(&mut self) {
        #[cfg(feature = "voxhold-media")]
        if let Some(mut preview) = self.voice_settings.preview.take() {
            preview.stop();
        }
        self.voice_settings.preview_level = 0.0;
    }

    /// Stop the current meter task by invalidating its epoch.
    pub(super) fn stop_voice_meter(&mut self) {
        self.voice_settings.meter_epoch = self.voice_settings.meter_epoch.wrapping_add(1);
        self.voice_settings.preview_level = 0.0;
    }

    /// Start the 100 ms meter task for the visible tab.
    pub(super) fn start_voice_meter(&mut self, cx: &mut Context<Self>) {
        self.stop_voice_meter();
        let epoch = self.voice_settings.meter_epoch;
        let session = self.session_generation;
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| loop {
            executor.timer(std::time::Duration::from_millis(100)).await;
            let Some(this) = this.upgrade() else {
                break;
            };
            let keep = this
                .update(cx, |view, cx| view.voice_meter_tick(epoch, session, cx))
                .unwrap_or(false);
            if !keep {
                break;
            }
        })
        .detach();
    }

    /// One meter tick. Returns whether the task should continue.
    fn voice_meter_tick(&mut self, epoch: u64, session: u64, cx: &mut Context<Self>) -> bool {
        if epoch != self.voice_settings.meter_epoch || session != self.session_generation {
            return false;
        }
        if !self.voice_tab_open() {
            // The dialog was closed or moved to another tab without the
            // activate/close hook; never leave the preview microphone open.
            self.stop_voice_preview();
            return false;
        }
        let in_call = self.voice_call_active(cx);
        // A preview that failed to open reports once and then stops.
        #[cfg(feature = "voxhold-media")]
        {
            let error = self
                .voice_settings
                .preview
                .as_ref()
                .and_then(|preview| preview.error());
            if let Some(error) = error {
                self.voice_settings.preview_error = error;
                self.stop_voice_preview();
            }
            if in_call && self.voice_settings.preview.is_some() {
                self.stop_voice_preview();
            }
            // The tab may have been opened while a call was running. Once the
            // call ends, the tab's own preview starts again (Go `enter`).
            if !in_call
                && self.voice_settings.preview.is_none()
                && self.voice_settings.preview_error.is_empty()
            {
                self.start_voice_preview(cx);
            }
        }
        let raw = if in_call {
            self.voice_local_level(cx)
        } else {
            #[cfg(feature = "voxhold-media")]
            {
                self.voice_settings
                    .preview
                    .as_ref()
                    .map(|preview| preview.level() as f64)
                    .unwrap_or(0.0)
            }
            #[cfg(not(feature = "voxhold-media"))]
            {
                0.0
            }
        };
        // The preview is the microphone signal the user is adjusting, so the
        // stored gain scales it exactly like Go's preview meter. In a call the
        // number must be the real transmission level: gain 1–200 is not in
        // that path until R07b.
        let scaled = if in_call {
            raw
        } else {
            raw * self.voice_settings.prefs.input_volume as f64 / 100.0
        }
        .clamp(0.0, 1.0);
        let mut changed = false;
        if (scaled - self.voice_settings.preview_level as f64).abs() > 0.005 {
            self.voice_settings.preview_level = scaled as f32;
            changed = true;
        }
        // A real hot-unplug reported by the engine: show it and re-read the
        // device list so the unavailable id appears as such.
        let device_error = self.voice_device_error(cx);
        if !device_error.is_empty() && self.voice_settings.device_notice != device_error {
            self.voice_settings.device_notice = device_error;
            self.load_voice_devices(cx);
            changed = true;
        }
        if changed {
            cx.notify();
        }
        true
    }

    /// Run the Go test tone off the UI thread. The button only reflects the
    /// pending state; the 350 ms wait never blocks a frame.
    fn play_voice_test_tone(&mut self, cx: &mut Context<Self>) {
        use crate::audio_capabilities::Support;
        if crate::audio_capabilities::audio_capabilities().output_test != Support::Applied {
            self.voice_settings.tone_error = "Проверка сигналов недоступна в этой сборке.".into();
            cx.notify();
            return;
        }
        if self.voice_settings.tone_pending {
            return;
        }
        self.voice_settings.tone_pending = true;
        self.voice_settings.tone_error.clear();
        // Never play audio from a test process.
        #[cfg(test)]
        {
            self.voice_settings.tone_pending = false;
        }
        #[cfg(all(feature = "voxhold-media", not(test)))]
        {
            let output = self.voice_settings.prefs.output.clone();
            let volume = self.voice_settings.prefs.sound_volume;
            let session = self.session_generation;
            let task =
                cx.background_spawn(
                    async move { voxhold_media::preview::play_tone(&output, volume) },
                );
            cx.spawn(async move |this, cx| {
                let result = task.await;
                if let Some(this) = this.upgrade() {
                    this.update(cx, |view, cx| {
                        if view.session_generation != session {
                            return;
                        }
                        view.voice_settings.tone_pending = false;
                        let visible = view.voice_tab_open();
                        if let Err(error) = result {
                            if visible {
                                view.voice_settings.tone_error = error;
                            }
                        }
                        if visible {
                            cx.notify();
                        }
                    })
                    .ok();
                }
            })
            .detach();
        }
        cx.notify();
    }
}
