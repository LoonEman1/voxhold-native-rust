//! Test-only hooks for the voice panel. They live in their own module so the
//! production file stays small and the seam is obviously not part of the UI.

use super::*;
use voxhold_media::publisher::Sharing;
use voxhold_media::screen_capture::ScreenSource;

impl VoicePanel {
    /// Install a handle that has no worker or socket, so `refresh_capture`
    /// queues the snapshot exactly as the live path would.
    pub fn test_attach_detached_handle(&mut self) {
        let handle = VoiceHandle::detached_for_tests();
        self.attempt = handle.attempt();
        self.handle = Some(handle);
        // A new worker starts with no viewport; the next render must send it.
        self.requested_viewport = None;
    }

    /// Take the viewport the UI queued for the detached media worker.
    pub fn test_take_viewport(&mut self) -> Option<(u32, u32)> {
        self.handle.as_ref()?.take_viewport_for_tests()
    }

    /// The newest control snapshot waiting for the media layer, if any. The
    /// request slot holds one value, so `Some` means exactly one pending State.
    pub fn test_take_audio_snapshot(&mut self) -> Option<(Controls, bool)> {
        self.handle
            .as_ref()
            .and_then(|handle| handle.take_controls_for_tests())
    }

    /// The power transition queued for the media worker by the real handler.
    pub fn test_take_power(&mut self) -> Option<bool> {
        self.handle
            .as_ref()
            .and_then(|handle| handle.take_power_for_tests())
    }

    /// Open the picker without the OS capture gate and with a fixed list, so
    /// the keyboard and cancel paths are testable on any host.
    pub fn test_open_screen_picker(&mut self, source: ScreenSource) {
        self.test_share_gate = Some(Ok(()));
        self.screen = ScreenShareUi::default();
        self.screen.open = true;
        self.screen.picker = 7;
        self.screen.kind = voxhold_media::screen_capture::SourceKind::Monitor;
        self.screen.monitors = vec![source.clone()];
        self.screen.monitor_loading = false;
        self.screen.monitor_generation = 1;
        self.screen.window_loading = false;
        self.screen.window_generation = 1;
        self.screen.selected = Some(source.clone());
        self.screen.last_request = Some(voxhold_media::publisher::PublishRequest {
            source,
            quality: self.screen.quality(),
            codec: voxhold_media::publisher::Codec::Vp8,
            audio: false,
        });
        self.screen.audio_availability = Some(Ok(voxhold_media::system_audio::AudioDeviceInfo {
            id: "test".into(),
            label: "Устройство вывода по умолчанию".into(),
            format: "48000 Гц · 2 канала · 16 бит PCM".into(),
        }));
        // Keep the endpoint probe off the real device: it still runs on the
        // production background task, just with a deterministic answer.
        self.test_audio_probe = Some(Arc::new(|| {
            Ok(voxhold_media::system_audio::AudioDeviceInfo {
                id: "test".into(),
                label: "Устройство вывода по умолчанию".into(),
                format: "48000 Гц · 2 канала · 16 бит PCM".into(),
            })
        }));
    }

    /// Production path of the «Экран» click, for the non-blocking probe test.
    pub fn test_open_screen_share(&mut self, cx: &mut Context<Self>) {
        self.open_screen_share(cx);
    }

    /// The publish intent without the picker keyboard path.
    pub fn test_start_screen_share(&mut self, cx: &mut Context<Self>) {
        self.start_screen_share(cx);
    }

    /// Select a source and close the picker surface, so a test can exercise
    /// the publish/stop handoff without the dialog taking keyboard focus.
    pub fn test_prepare_screen_publish(&mut self, source: ScreenSource) {
        self.test_open_screen_picker(source);
        self.screen.open = false;
    }

    /// The stop intent, exactly as the on-air card sends it.
    pub fn test_stop_screen_share(&mut self, cx: &mut Context<Self>) {
        self.stop_screen_share(cx);
    }

    /// Install a probe with controlled timing. It runs on the same background
    /// task as the real `system_audio::probe`, so a slow answer cannot be
    /// mistaken for a synchronous call.
    pub fn test_set_audio_probe(
        &mut self,
        probe: Arc<
            dyn Fn() -> Result<voxhold_media::system_audio::AudioDeviceInfo, String> + Send + Sync,
        >,
    ) {
        self.test_audio_probe = Some(probe);
    }

    /// Whether the picker is still waiting for the background probe.
    pub fn test_audio_pending(&self) -> bool {
        self.screen.audio_availability.is_none()
    }

    /// The availability line the picker paints.
    pub fn test_audio_status(&self) -> String {
        self.screen.audio_status()
    }

    /// The label of the system-audio toggle.
    pub fn test_audio_toggle_label(&self) -> &'static str {
        self.screen.audio_toggle_label()
    }

    /// Whether the picker would offer Start with the current probe answer.
    pub fn test_can_start(&self) -> bool {
        self.screen_can_start()
    }

    /// Select the source after the production «Экран» path opened the picker.
    pub fn test_select_screen_source(&mut self, source: ScreenSource, cx: &mut Context<Self>) {
        self.screen.selected = Some(source);
        cx.notify();
    }

    /// Open the live quality dialog with the publication already Live.
    pub fn test_open_screen_settings(
        &mut self,
        source: ScreenSource,
        sharing: Sharing,
        cx: &mut Context<Self>,
    ) {
        self.test_open_screen_picker(source);
        self.screen.sharing = sharing;
        // Production reaches this dialog while the publication is Live, so the
        // normal gate answers "already live". The restart path must pass that
        // one reason without opening a second publication.
        self.test_share_gate = Some(Err("Трансляция уже идёт."));
        self.open_screen_settings(cx);
    }

    /// The quality/audio fields the picker will send.
    pub fn test_screen_request(&self) -> voxhold_media::publisher::PublishRequest {
        let mut request = self.screen.last_request.clone().unwrap_or_else(|| {
            voxhold_media::publisher::PublishRequest {
                source: self.screen.selected.clone().unwrap_or(
                    voxhold_media::screen_capture::ScreenSource {
                        id: 0,
                        kind: voxhold_media::screen_capture::SourceKind::Monitor,
                        title: String::new(),
                        display_id: -1,
                        width: 0,
                        height: 0,
                    },
                ),
                quality: self.screen.quality(),
                codec: voxhold_media::publisher::Codec::Vp8,
                audio: self.screen.audio_requested(),
            }
        });
        request.quality = self.screen.quality();
        request.audio = self.screen.audio_requested();
        request
    }

    /// The on-air sound line the card paints.
    pub fn test_sound_label(&self) -> String {
        self.screen.sound_label()
    }

    /// The restart warning shown in the live dialog, if any.
    pub fn test_restart_warning(&self) -> Option<&'static str> {
        self.screen.restart_warning()
    }

    pub fn test_screen_restart(&self) -> bool {
        self.screen.restart
    }

    pub fn test_screen_phase(&self) -> voxhold_media::publisher::Phase {
        self.screen.sharing.phase
    }

    pub fn test_apply_preset(
        &mut self,
        preset: voxhold_media::publisher::Preset,
        cx: &mut Context<Self>,
    ) {
        self.apply_preset(preset, cx);
    }

    pub fn test_toggle_system_audio(&mut self, cx: &mut Context<Self>) {
        self.toggle_system_audio(cx);
    }

    /// Replace the gate answer without a capture stack.
    pub fn test_set_share_gate(&mut self, gate: Result<(), &'static str>) {
        self.test_share_gate = Some(gate);
    }

    /// The reason the room would print next to the «Экран» action.
    pub fn test_screen_reason(&self) -> Option<&'static str> {
        self.screen_reason()
    }

    /// Structural check that the picker handles cannot collide with the
    /// toolbar or the on-air card.
    pub fn test_picker_focus_is_separate(&self) -> bool {
        self.picker_focus
            .iter()
            .all(|picker| !self.screen_focus.contains(picker))
    }

    /// Replace the screen-share snapshot as a worker update would.
    pub fn test_set_screen_sharing(&mut self, sharing: Sharing) {
        self.screen.sharing = sharing;
    }

    /// Queued screen actions of the detached handle, as (kind, picker) pairs.
    pub fn test_take_screen_commands(&mut self) -> Vec<(&'static str, u64)> {
        self.handle
            .as_ref()
            .map(|handle| handle.take_screen_commands_for_tests())
            .unwrap_or_default()
    }

    /// Debug label of the focused picker element, e.g. `Source(0)` or `Start`.
    pub fn test_focused_share(&self, window: &Window) -> Option<String> {
        self.screen
            .ring
            .iter()
            .find(|(handle, _)| handle.is_focused(window))
            .map(|(_, key)| format!("{key:?}"))
    }

    /// One roster row by connection id, for a test that needs to change the
    /// server-authoritative fields without an event helper.
    pub fn test_self_participant(
        &self,
        connection: &str,
    ) -> Option<voxhold_media::controller::Participant> {
        self.participants
            .iter()
            .find(|participant| participant.connection_id == connection)
            .cloned()
    }
}
