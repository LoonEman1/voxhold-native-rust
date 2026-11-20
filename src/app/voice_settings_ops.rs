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
            Action::SelectInput(id) => {
                self.choose_voice_device(true, id, cx);
            }
            Action::SelectOutput(id) => {
                self.choose_voice_device(false, id, cx);
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
                self.persist_voice(cx);
                self.apply_voice_snapshot(cx);
                cx.notify();
            }
            Action::SetBitrate(bitrate) => {
                if crate::voice_settings::BITRATES.contains(&bitrate) {
                    self.voice_settings.prefs.bitrate = bitrate;
                    self.persist_voice(cx);
                    self.apply_voice_snapshot(cx);
                    cx.notify();
                }
            }
            Action::ToggleSounds => {
                self.voice_settings.prefs.sounds = !self.voice_settings.prefs.sounds;
                self.persist_voice(cx);
                cx.notify();
            }
            Action::SetNoiseMode(index) => {
                self.voice_settings.prefs.set_noise_mode(index);
                self.persist_voice(cx);
                self.apply_voice_snapshot(cx);
                cx.notify();
            }
            Action::SetInputMode(index) => {
                let push_to_talk = index == 1;
                self.voice_settings.prefs.set_input_mode(index);
                self.persist_voice(cx);
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
                self.persist_voice(cx);
                self.apply_voice_snapshot(cx);
                cx.notify();
            }
            Action::ToggleVoiceDiagnostics => {
                // View-only state: the section must not touch the saved file.
                self.voice_settings.diagnostics_open = !self.voice_settings.diagnostics_open;
                cx.notify();
            }
            Action::ResetVoice => {
                // Go resets to `defaultVoicePreferences()` and therefore also
                // clears both selected devices.
                self.voice_settings.prefs = crate::voice_settings::VoicePrefs::default();
                self.voice_settings.recording_shortcut = None;
                self.voice_settings.shortcut_conflict.clear();
                self.persist_voice(cx);
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
            Action::RecordShortcut(field) => {
                #[cfg(feature = "voxhold-media")]
                if self.voice_settings.recording_shortcut == Some(field) {
                    self.voice_settings.recording_shortcut = None;
                    self.voice_settings.shortcut_conflict.clear();
                    cx.notify();
                } else {
                    self.start_shortcut_recording(field, cx);
                }
                #[cfg(not(feature = "voxhold-media"))]
                {
                    let _ = field;
                }
            }
            Action::SetVoiceUserVolume(user_id, volume) => {
                if user_id > 0 {
                    let value = volume.clamp(0, 200);
                    if value == 100 {
                        self.voice_settings.prefs.user_volumes.remove(&user_id);
                    } else {
                        self.voice_settings
                            .prefs
                            .user_volumes
                            .insert(user_id, value);
                    }
                    self.persist_voice(cx);
                    self.apply_voice_snapshot(cx);
                    cx.notify();
                }
            }
            _ => return false,
        }
        true
    }

    /// One device choice, identified by the endpoint id that was captured when
    /// the row was drawn. A list that changed since then (hot-unplug, refresh)
    /// makes the id stale: it is rejected with a visible notice instead of
    /// silently selecting whatever now sits at that position.
    fn choose_voice_device(&mut self, input: bool, id: String, cx: &mut Context<Self>) {
        let (_, ids, _) = crate::voice_settings::device_choices(
            &self.voice_settings.prefs,
            &self.voice_settings.devices,
            input,
        );
        if !id.is_empty() && !ids.iter().any(|known| *known == id) {
            self.voice_settings.device_notice =
                "Это устройство больше не подключено — обновите список и выберите снова.".into();
            self.load_voice_devices(cx);
            cx.notify();
            return;
        }
        if input {
            self.voice_settings.prefs.input = id;
        } else {
            self.voice_settings.prefs.output = id;
        }
        self.persist_voice(cx);
        self.apply_voice_snapshot(cx);
        #[cfg(feature = "voxhold-media")]
        if let Some(voice) = &self.voice {
            if input {
                // R07b: input changes during a call use their own generation,
                // so a later PTT/DSP State cannot displace the explicit choice.
                let input = self.voice_settings.prefs.input.clone();
                voice.update(cx, |panel, _| panel.select_input(Some(input)));
            } else {
                // R07a: an explicit output choice is its own request, even when
                // the cached id looks unchanged after a hot-unplug.
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

    /// Persist the voice preferences off the UI thread. Rapid changes (a slider
    /// drag) collapse into one atomic write: every request takes a newer token
    /// and only the token that is still the newest reaches the disk, so a late
    /// writer can never resurrect an older value.
    pub(super) fn persist_voice(&mut self, cx: &mut Context<Self>) {
        let store = self.voice_settings.store.clone();
        let generation = store.next();
        let prefs = self.voice_settings.prefs.clone();
        let path = self.voice_settings.path.clone();
        let session = self.session_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(150))
                .await;
            let Some(view) = this.upgrade() else {
                return;
            };
            let newest = view
                .update(cx, |view, _| {
                    view.session_generation == session
                        && view.voice_settings.store.is_newest(generation)
                })
                .unwrap_or(false);
            if !newest {
                return;
            }
            let writer = store.clone();
            let result = cx
                .background_spawn(async move { writer.save(generation, &prefs, path.as_deref()) })
                .await;
            let Some(saved) = result else { return };
            let Some(view) = this.upgrade() else {
                return;
            };
            view.update(cx, |view, cx| {
                if view.session_generation != session
                    || !view.voice_settings.store.is_newest(generation)
                {
                    return;
                }
                if saved {
                    view.voice_settings.writes = view.voice_settings.writes.wrapping_add(1);
                    view.voice_settings.saved_generation = generation;
                    view.voice_settings.saved_error.clear();
                } else {
                    view.voice_settings.saved_error =
                        "Не удалось сохранить настройки звука.".into();
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The single audio snapshot for the settings tab.
    pub(super) fn apply_voice_snapshot(&mut self, cx: &mut Context<Self>) {
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
            // AEC needs a measured render reference, which no chain has, so the
            // request never asks for it even though the stored preference is
            // kept. The applied report is the only place that could say
            // otherwise, and it never does in this build.
            let echo_cancellation = prefs.echo_cancellation
                && crate::audio_capabilities::audio_capabilities().echo_cancellation
                    == crate::audio_capabilities::Support::Applied;
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
                    prefs.user_volumes.clone(),
                );
            });
        }
        #[cfg(not(feature = "voxhold-media"))]
        {
            let _ = cx;
        }
    }

    /// Whether tab 1 of the personal dialog is currently visible.
    pub(super) fn voice_tab_open(&self) -> bool {
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

    /// The microphone is closed in the live call: local mute, server mute,
    /// deafen, push-to-talk release or a blocking modal all fold into
    /// `Controls::muted`, so the in-call meter can show the same silence the
    /// transmission sends.
    fn voice_input_muted(&self, cx: &Context<Self>) -> bool {
        #[cfg(feature = "voxhold-media")]
        {
            self.voice
                .as_ref()
                .is_some_and(|panel| panel.read(cx).input_muted())
        }
        #[cfg(not(feature = "voxhold-media"))]
        {
            let _ = cx;
            false
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
    /// The preview lives only as long as the tab. Endpoint resolution and WinMM
    /// enumeration run on the background executor: a slow device stack must not
    /// delay the frame, and a stale answer is dropped by `preview_epoch`.
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
            if !self.voice_tab_open() || self.voice_call_active(cx) {
                return;
            }
            let device = self.voice_settings.prefs.input.clone();
            let epoch = self.voice_settings.preview_epoch;
            self.voice_settings.preview_pending = true;
            cx.spawn(async move |this, cx| {
                let started = cx
                    .background_spawn(async move { voxhold_media::preview::start_level(&device) })
                    .await;
                let Some(this) = this.upgrade() else {
                    return;
                };
                this.update(cx, |view, cx| {
                    view.install_voice_preview(epoch, started, cx)
                })
                .ok();
            })
            .detach();
        }
        #[cfg(all(not(feature = "voxhold-media"), not(test)))]
        {
            let _ = cx;
        }
    }

    /// Apply a background preview start. A result for an older epoch, or one
    /// that arrives after the tab closed or a call started, is stopped instead
    /// of installed.
    #[cfg(all(feature = "voxhold-media", not(test)))]
    fn install_voice_preview(
        &mut self,
        epoch: u64,
        started: Result<voxhold_media::preview::LevelPreview, String>,
        cx: &mut Context<Self>,
    ) {
        self.voice_settings.preview_pending = false;
        if epoch != self.voice_settings.preview_epoch
            || !self.voice_tab_open()
            || self.voice_call_active(cx)
        {
            if let Ok(mut preview) = started {
                preview.stop();
            }
            return;
        }
        match started {
            Ok(preview) => self.voice_settings.preview = Some(preview),
            Err(error) => self.voice_settings.preview_error = error,
        }
        cx.notify();
    }

    /// Signal the preview thread and drop the handle. Never joins the UI
    /// thread; the thread exits within one 50 ms tick.
    pub(super) fn stop_voice_preview(&mut self) {
        self.voice_settings.preview_epoch = self.voice_settings.preview_epoch.wrapping_add(1);
        self.voice_settings.preview_pending = false;
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
    pub(super) fn voice_meter_tick(
        &mut self,
        epoch: u64,
        session: u64,
        cx: &mut Context<Self>,
    ) -> bool {
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
                && !self.voice_settings.preview_pending
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
        // The preview is the device signal the user is adjusting, so the stored
        // gain scales it exactly like Go's `levelMeter` (`level*InputVolume/100`).
        // In a call the engine reports the pre-gate level of the software chain,
        // i.e. after the APM but before the gate and the applied gain
        // (`capture.rs::BlockProcessor::process`), so the meter applies the same
        // gain here and shows silence while the microphone is closed. The gate
        // itself stays out of the meter: it is a non-linear on/off, and the
        // coral riser already shows its threshold.
        let gain = self.voice_settings.prefs.input_volume as f64 / 100.0;
        let muted = in_call && self.voice_input_muted(cx);
        let scaled = if muted { 0.0 } else { raw * gain }.clamp(0.0, 1.0);
        let mut changed = false;
        if (scaled - self.voice_settings.preview_level as f64).abs() > 0.005 {
            self.voice_settings.preview_level = scaled as f32;
            changed = true;
        }
        // A real hot-unplug reported by the engine: show it and re-read the
        // device list so the unavailable id appears as such. The reverse is
        // also handled: once the engine reports a working input again, the
        // stale notice is cleared and the list is re-read, without touching
        // the saved choice.
        let device_error = self.voice_device_error(cx);
        if !device_error.is_empty() && self.voice_settings.device_notice != device_error {
            self.voice_settings.device_notice = device_error.clone();
            self.voice_settings.device_error_shown = device_error;
            self.load_voice_devices(cx);
            changed = true;
        } else if device_error.is_empty() && !self.voice_settings.device_error_shown.is_empty() {
            if self.voice_settings.device_notice == self.voice_settings.device_error_shown {
                self.voice_settings.device_notice.clear();
            }
            self.voice_settings.device_error_shown.clear();
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
