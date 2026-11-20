//! Test-only hooks for the voice panel. They live in their own module so the
//! production file stays small and the seam is obviously not part of the UI.

use super::*;
use voxhold_media::publisher::Sharing;
use voxhold_media::screen_capture::{
    PreviewFrame, ScreenSource, SourceId, SourceKind, ThumbnailError,
};

impl VoicePanel {
    /// Ids in the current Tab ring, in cycle order. Tests compare these with
    /// painted `debug_selector` bounds so an unmounted handle cannot hide.
    pub fn test_focus_ring_ids(&self) -> Vec<&'static str> {
        self.focus_ring().into_iter().map(|(id, _)| id).collect()
    }

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

    /// The newest screen gate the UI pushed while a picker or an attempt was
    /// open. It is a last-value cell, so `None` after a take proves no repeat.
    pub fn test_take_screen_access(&mut self) -> Option<(bool, bool)> {
        self.handle
            .as_ref()
            .and_then(|handle| handle.take_screen_access_for_tests())
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
        let kind = source.kind;
        let id = source.source_id();
        self.test_open_screen_picker_with(kind, vec![source.clone()], Some(id));
        self.screen.last_request = Some(voxhold_media::publisher::PublishRequest {
            source,
            quality: self.screen.quality(),
            codec: voxhold_media::publisher::Codec::Vp8,
            audio: false,
            audio_scope: voxhold_media::system_audio::AudioScope::Device,
        });
    }

    /// Open the picker with a controlled list and an optional selection, so
    /// reorder/invalid-refresh cases use the same state as the worker answer.
    pub fn test_open_screen_picker_with(
        &mut self,
        kind: SourceKind,
        sources: Vec<ScreenSource>,
        selected: Option<SourceId>,
    ) {
        self.test_share_gate = Some(Ok(()));
        // A test picker is a new session too: the preview epoch moves so a
        // result from the previous picker can never match it again.
        let epoch = self.screen.preview_epoch.wrapping_add(1);
        self.screen = ScreenShareUi::default();
        self.screen.preview_epoch = epoch;
        self.screen.open = true;
        self.screen.picker = 7;
        self.screen.kind = kind;
        match kind {
            SourceKind::Monitor => {
                self.screen.monitors = sources;
                self.screen.monitor_loading = false;
                self.screen.monitor_generation = 1;
                self.screen.window_loading = false;
                self.screen.window_generation = 1;
            }
            SourceKind::Window => {
                self.screen.windows = sources;
                self.screen.window_loading = false;
                self.screen.window_generation = 1;
                self.screen.monitor_loading = false;
                self.screen.monitor_generation = 1;
            }
        }
        self.screen.selected = selected;
        self.screen.selected_identity = selected
            .and_then(|id| self.screen.source_for(id))
            .and_then(ScreenSource::identity);
        self.screen.reset_audio_scope(kind);
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
    /// The row is added to the current snapshot when absent, because the
    /// production handler only selects rows the list already offers.
    pub fn test_select_screen_source(&mut self, source: ScreenSource, cx: &mut Context<Self>) {
        let id = source.source_id();
        match id.kind {
            SourceKind::Monitor if !self.screen.monitors.iter().any(|item| item.id == id.id) => {
                self.screen.monitors.push(source);
            }
            SourceKind::Window if !self.screen.windows.iter().any(|item| item.id == id.id) => {
                self.screen.windows.push(source);
            }
            _ => {}
        }
        self.select_screen_source(id, cx);
    }

    /// Attempt a selection exactly as a row click does, without adding the id
    /// to the snapshot first. Used to prove an absent identity is rejected.
    pub fn test_try_select_screen_source(&mut self, source: ScreenSource, cx: &mut Context<Self>) {
        self.select_screen_source(source.source_id(), cx);
    }

    /// The close intent shared by Cancel, Escape and the backdrop.
    pub fn test_close_screen_share(&mut self, cx: &mut Context<Self>) {
        self.close_screen_share(cx);
    }

    /// Switch the picker category exactly like the tab action does.
    pub fn test_switch_share_kind(&mut self, kind: SourceKind, cx: &mut Context<Self>) {
        self.change_share_kind(kind, cx);
    }

    /// Run the picker refresh exactly like the «Обновить» action does.
    pub fn test_refresh_screen_sources(&mut self, cx: &mut Context<Self>) {
        self.refresh_screen_sources(cx);
    }

    /// The source request generations, used to observe that refresh was inert.
    pub fn test_source_generations(&self) -> (u64, u64) {
        (
            self.screen.monitor_generation,
            self.screen.window_generation,
        )
    }

    /// Replace the background thumbnail capture with a deterministic provider.
    /// Without it the suite never reaches the real desktop capturer.
    pub fn test_set_preview_provider(
        &mut self,
        provider: Arc<dyn Fn(&ScreenSource) -> Result<PreviewFrame, ThumbnailError> + Send + Sync>,
    ) {
        self.test_preview_provider = Some(provider);
    }

    /// Inject one small fake thumbnail, exactly as an accepted background
    /// result would insert it. No capture stack is involved.
    pub fn test_inject_screen_preview(
        &mut self,
        source: &ScreenSource,
        width: u32,
        height: u32,
    ) -> bool {
        let frame = PreviewFrame {
            width,
            height,
            bgra: vec![0x40; width as usize * height as usize * 4],
        };
        let Some(identity) = source.identity() else {
            return false;
        };
        self.screen.insert_preview(identity, frame)
    }

    /// Inject a preview failure for one card without touching the selection.
    pub fn test_inject_screen_preview_error(&mut self, source: &ScreenSource, message: &str) {
        let Some(identity) = source.identity() else {
            return;
        };
        self.screen
            .insert_preview_failure(identity, message.to_owned());
    }

    /// Run the production preview request against the current picker state, so
    /// a test can model a refresh or a source-list update at an exact moment.
    pub fn test_request_screen_previews(&mut self, cx: &mut Context<Self>) {
        self.request_screen_previews(cx);
    }

    /// Whether a preview batch is marked active for the open picker.
    pub fn test_preview_batch_active(&self) -> bool {
        self.screen.preview_batching
    }

    /// Apply the end-of-batch guard rule for an explicit epoch, so a test can
    /// model a batch that finishes after its session was replaced.
    pub fn test_end_preview_batch(&mut self, epoch: u64) -> bool {
        self.screen.end_preview_batch(epoch)
    }

    /// Deliver a result with an explicit epoch/identity, mirroring the
    /// production acceptance check. The id is queued first so a test can model
    /// a capture that was in flight when the snapshot changed.
    pub fn test_deliver_screen_preview(
        &mut self,
        epoch: u64,
        source: &ScreenSource,
        ok: bool,
    ) -> bool {
        let Some(identity) = source.identity() else {
            return false;
        };
        self.screen.preview_pending.insert(identity.clone());
        let result = if ok {
            Ok(PreviewFrame {
                width: 8,
                height: 8,
                bgra: vec![0x20; 8 * 8 * 4],
            })
        } else {
            Err(ThumbnailError::Timeout)
        };
        self.screen.finish_preview(epoch, identity, result)
    }

    /// The current preview session epoch.
    pub fn test_preview_epoch(&self) -> u64 {
        self.screen.preview_epoch
    }

    /// Cached pictures and their summed BGRA bytes.
    pub fn test_screen_preview_counts(&self) -> (usize, usize) {
        (self.screen.previews.len(), self.screen.preview_bytes)
    }

    /// Pictures dropped by the count or byte bound.
    pub fn test_screen_preview_rejected(&self) -> u32 {
        self.screen.preview_rejected
    }

    /// Ids whose capture is still pending.
    pub fn test_screen_preview_pending(&self) -> usize {
        self.screen.preview_pending.len()
    }

    /// Cards painted by the last frame.
    pub fn test_screen_preview_cards(&self) -> usize {
        self.screen.preview_cards
    }

    /// The painted failure note of one card, if its capture failed.
    pub fn test_screen_preview_error(&self, id: SourceId) -> Option<String> {
        let identity = self.screen.source_for(id)?.identity()?;
        self.screen.preview_failures.get(&identity).cloned()
    }

    /// The hard bounds the cache enforces, for a test assertion.
    pub fn test_screen_preview_bounds(&self) -> (usize, usize) {
        (
            super::screen_share::MAX_SCREEN_PREVIEWS,
            super::screen_share::MAX_SCREEN_PREVIEW_BYTES,
        )
    }

    /// Number of rows cached for a category. A restart must not add the
    /// confirmed source here; a fresh picker must start at zero.
    pub fn test_screen_source_count(&self, kind: SourceKind) -> usize {
        match kind {
            SourceKind::Monitor => self.screen.monitors.len(),
            SourceKind::Window => self.screen.windows.len(),
        }
    }

    /// The stable identity the picker would publish, if any.
    pub fn test_selected_source_id(&self) -> Option<SourceId> {
        self.screen.selected
    }

    /// The selected row as the current snapshot describes it, not a copy
    /// remembered at selection time.
    pub fn test_selected_source(&self) -> Option<ScreenSource> {
        self.screen.selected_source().cloned()
    }

    /// Feed a source answer exactly as the media worker does, including the
    /// generation check and the refresh invalidation.
    pub fn test_apply_screen_sources(&mut self, kind: SourceKind, sources: Vec<ScreenSource>) {
        let generation = match kind {
            SourceKind::Monitor => self.screen.monitor_generation + 1,
            SourceKind::Window => self.screen.window_generation + 1,
        };
        self.apply_screen_update(VoiceUpdate::ScreenSources {
            generation,
            kind,
            sources,
            error: None,
        });
    }

    /// Replace the cached list without the refresh invalidation, to model a
    /// Start that races a refresh which has not been applied yet.
    pub fn test_replace_screen_list_without_refresh(
        &mut self,
        kind: SourceKind,
        sources: Vec<ScreenSource>,
    ) {
        match kind {
            SourceKind::Monitor => self.screen.monitors = sources,
            SourceKind::Window => self.screen.windows = sources,
        }
    }

    /// The picker's error line.
    pub fn test_screen_error(&self) -> &str {
        &self.screen.error
    }

    /// The visible refusal for an absent or duplicated source identity.
    pub fn test_source_gone_reason(&self) -> &'static str {
        super::screen_share::SOURCE_GONE_REASON
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

    /// Open the live quality dialog exactly like the on-air card does, on the
    /// state a test set up itself. The picker snapshot is not touched.
    pub fn test_open_live_screen_settings(&mut self, cx: &mut Context<Self>) {
        // The live card reaches this dialog while the publication is Live, so
        // the normal gate answers "already live".
        self.test_share_gate = Some(Err("Трансляция уже идёт."));
        self.open_screen_settings(cx);
    }

    /// Record the accepted attempt exactly as the publish handler does. This
    /// is the request a restart dialog must bind to.
    pub fn test_set_screen_last_request(&mut self, source: ScreenSource) {
        self.screen.last_request = Some(voxhold_media::publisher::PublishRequest {
            source,
            quality: self.screen.quality(),
            codec: voxhold_media::publisher::Codec::Vp8,
            audio: false,
            audio_scope: voxhold_media::system_audio::AudioScope::Device,
        });
    }

    /// Record a fully described accepted attempt, for a test that needs the
    /// live audio flag or scope to match the dialog.
    pub fn test_set_screen_live_request(
        &mut self,
        request: voxhold_media::publisher::PublishRequest,
    ) {
        self.screen.last_request = Some(request);
    }

    /// Step the dialog's video bitrate exactly like the picker button does.
    pub fn test_change_screen_bitrate(&mut self, delta: i64, cx: &mut Context<Self>) {
        self.change_bitrate(delta, cx);
    }

    /// Choose a resolution exactly like the picker button does.
    pub fn test_set_screen_resolution(
        &mut self,
        resolution: voxhold_media::publisher::Resolution,
        cx: &mut Context<Self>,
    ) {
        self.screen.resolution = resolution;
        cx.notify();
    }

    /// The newest screen snapshot the panel holds.
    pub fn test_screen_sharing(&self) -> Sharing {
        self.screen.sharing.clone()
    }

    /// Put the live dialog back into the pending endpoint-probe state, so a
    /// test can observe that Start waits for the repeated probe answer.
    pub fn test_set_screen_audio_pending(
        &mut self,
        probe: Arc<
            dyn Fn() -> Result<voxhold_media::system_audio::AudioDeviceInfo, String> + Send + Sync,
        >,
    ) {
        self.screen.audio_availability = None;
        self.test_audio_probe = Some(probe);
    }

    /// The category the picker dialog shows.
    pub fn test_share_kind(&self) -> SourceKind {
        self.screen.kind
    }

    /// The quality/audio fields the picker will send.
    pub fn test_screen_request(&self) -> voxhold_media::publisher::PublishRequest {
        let mut request = self.screen.last_request.clone().unwrap_or_else(|| {
            let source = self.screen.selected_source().cloned().unwrap_or(
                voxhold_media::screen_capture::ScreenSource {
                    id: 0,
                    kind: voxhold_media::screen_capture::SourceKind::Monitor,
                    title: String::new(),
                    display_id: -1,
                    width: 0,
                    height: 0,
                    monitor: None,
                    topology: None,
                },
            );
            voxhold_media::publisher::PublishRequest {
                source,
                quality: self.screen.quality(),
                codec: voxhold_media::publisher::Codec::Vp8,
                audio: self.screen.audio_requested(),
                audio_scope: self.screen.audio_scope,
            }
        });
        request.quality = self.screen.quality();
        request.audio = self.screen.audio_requested();
        // Mirror the production `commit_screen_share`: the selected identity
        // decides the kind, so a stale window-only choice for a monitor (or the
        // reverse) cannot slip through the test seam. The fallback request uses
        // its own source kind when no row is selected.
        let kind = self
            .screen
            .selected
            .map(|id| id.kind)
            .unwrap_or(request.source.kind);
        request.audio_scope = self.screen.effective_audio_scope(kind);
        request
    }

    /// The on-air sound line the card paints.
    pub fn test_sound_label(&self) -> String {
        self.screen.sound_label()
    }

    /// The exact on-air card line, including the engine's real audio scope.
    /// It is built by the same helper as the painted card.
    pub fn test_screen_on_air_line(&self) -> String {
        let sharing = &self.screen.sharing;
        let quality = self.screen.quality_label();
        let sound = self.screen.sound_label();
        let stats = sharing.stats.map(|stats| stats.describe());
        crate::voice_panel::screen_share::screen_on_air_line(
            sharing,
            &quality,
            &sound,
            stats.as_deref(),
        )
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

    /// The scope the next request carries.
    pub fn test_audio_scope(&self) -> voxhold_media::system_audio::AudioScope {
        self.screen.audio_scope
    }

    /// The sentence the picker prints about the selected scope.
    pub fn test_audio_scope_sentence(&self) -> &'static str {
        self.screen.audio_scope_sentence()
    }

    /// Whether the scope choices are active (audio really requested).
    pub fn test_audio_scope_enabled(&self) -> bool {
        self.screen.audio_requested()
    }

    /// Choose one scope exactly like the picker button does.
    pub fn test_select_audio_scope(
        &mut self,
        scope: voxhold_media::system_audio::AudioScope,
        cx: &mut Context<Self>,
    ) {
        self.select_audio_scope(scope, cx);
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

    /// Apply a screen-share runtime update exactly as the publisher emits it
    /// (including the live stats sample every second).
    pub fn test_apply_screen_sharing(&mut self, sharing: Sharing) -> bool {
        self.apply_screen_update(VoiceUpdate::ScreenShare(sharing))
    }

    /// Queued screen actions of the detached handle, as (kind, picker) pairs.
    pub fn test_take_screen_commands(&mut self) -> Vec<(&'static str, u64)> {
        self.handle
            .as_ref()
            .map(|handle| handle.take_screen_commands_for_tests())
            .unwrap_or_default()
    }

    /// Debug label of the focused picker element, e.g. `Source(monitor:3)` or
    /// `Start`.
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
