//! Test-only hooks for the voice panel. They live in their own module so the
//! production file stays small and the seam is obviously not part of the UI.

use super::*;
use voxhold_media::publisher::{CodecIntent, Confirmed, Sharing};
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
        let handle = VoiceHandle::detached_for_tests_with_wake(self.wake.clone());
        self.attempt = handle.attempt();
        self.handle = Some(handle);
        // A new worker starts with no viewport; the next render must send it.
        self.requested_viewport = None;
    }

    /// Join with the real adoption path and the panel's rotation latch, but
    /// with a detached worker so no socket is opened. This is exactly what
    /// `VoicePanel::join` hands to the worker when a refresh is in flight.
    pub fn test_join_detached(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = VoiceHandle::detached_for_tests_with_wake_and_rotation(
            self.wake.clone(),
            self.rotation_in_flight,
        );
        self.call_channel = self.room;
        self.install_handle(handle, window, cx);
    }

    /// The identity of the installed worker, so a test can prove a session
    /// rotation kept the same handle instead of rebuilding it.
    pub fn test_handle_attempt(&self) -> Option<u64> {
        self.handle.as_ref().map(VoiceHandle::attempt)
    }

    /// Whether the handle currently defers re-authorization for a rotation.
    pub fn test_rotation_active(&self) -> bool {
        self.handle
            .as_ref()
            .is_some_and(VoiceHandle::test_rotation_active)
    }

    /// Whether the installed handle may open its first socket now.
    pub fn test_rotation_authorizes(&self) -> bool {
        self.handle
            .as_ref()
            .map(VoiceHandle::test_rotation_authorizes)
            .unwrap_or(true)
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
    pub fn test_open_screen_picker(&mut self, window: &mut Window, source: ScreenSource) {
        let kind = source.kind;
        let id = source.source_id();
        self.test_open_screen_picker_with(window, kind, vec![source.clone()], Some(id));
    }

    /// Open the picker with a controlled list and an optional selection, so
    /// reorder/invalid-refresh cases use the same state as the worker answer.
    pub fn test_open_screen_picker_with(
        &mut self,
        window: &mut Window,
        kind: SourceKind,
        sources: Vec<ScreenSource>,
        selected: Option<SourceId>,
    ) {
        self.test_share_gate = Some(Ok(()));
        // A test picker is a new session too: release the previous picker's
        // pictures through the production path (epoch move + drop_image), then
        // reset the rest of the dialog state. The on-air picture is not part of
        // `previews`, so it needs its own release; otherwise the old card
        // texture would be forgotten without `drop_image`.
        self.screen.clear_previews(window);
        self.screen.clear_preview(window);
        let epoch = self.screen.preview_epoch;
        let released_images = self.screen.released_images;
        self.screen = ScreenShareUi::default();
        self.screen.preview_epoch = epoch;
        self.screen.released_images = released_images;
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
    pub fn test_open_screen_share(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_screen_share(window, cx);
    }

    /// The publish intent without the picker keyboard path.
    pub fn test_start_screen_share(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_screen_share(window, cx);
    }

    /// Select a source and close the picker surface, so a test can exercise
    /// the publish/stop handoff without the dialog taking keyboard focus.
    pub fn test_prepare_screen_publish(&mut self, window: &mut Window, source: ScreenSource) {
        self.test_open_screen_picker(window, source);
        self.screen.open = false;
    }

    /// The stop intent, exactly as the on-air card sends it.
    pub fn test_stop_screen_share(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_screen_share(window, cx);
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
    pub fn test_close_screen_share(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_screen_share(window, cx);
    }

    /// Switch the picker category exactly like the tab action does.
    pub fn test_switch_share_kind(
        &mut self,
        kind: SourceKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_share_kind(kind, window, cx);
    }

    /// Run the picker refresh exactly like the «Обновить» action does.
    pub fn test_refresh_screen_sources(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_screen_sources(window, cx);
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
        window: &mut Window,
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
        self.screen.insert_preview(window, identity, frame)
    }

    /// Adopt a new physical card width exactly like the first picker frame
    /// after a DPI or size change.
    pub fn test_set_preview_width(
        &mut self,
        window: &mut Window,
        width: u32,
        cx: &mut Context<Self>,
    ) {
        self.set_thumbnail_width(width, window, cx);
    }

    /// The request width currently derived from the card and Windows scale.
    pub fn test_screen_preview_width(&self) -> u32 {
        self.screen.preview_width
    }

    /// Put the request width back without a generation change, so a test can
    /// model a picker that started at 100% and then moved to another monitor.
    pub fn test_force_preview_width(&mut self, width: u32) {
        self.screen.preview_width = width.clamp(2, super::screen_share::SCREEN_THUMBNAIL_WIDTH);
    }

    /// Model the visible grid rows of the last layout, so a test can observe
    /// that captures prioritize the cards on screen and that a card returning
    /// to the viewport retries a memory refusal.
    pub fn test_set_preview_viewport(
        &mut self,
        columns: usize,
        top: usize,
        bottom: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_preview_viewport(columns, top, bottom, window, cx);
    }

    /// Vertical scroll range of the source grid. A positive value proves the
    /// separate viewport really scrolls; the painted bar is asserted separately
    /// through its `share-sources-scrollbar` bounds.
    pub fn test_screen_list_scrollable(&self) -> f32 {
        f32::from(self.screen.list_scroll.max_offset().height)
    }

    /// Current vertical scroll offset of the source grid, as a positive
    /// distance from the top.
    pub fn test_screen_list_offset(&self) -> f32 {
        (-f32::from(self.screen.list_scroll.offset().y)).max(0.)
    }

    /// The grid rows the last frame considered visible.
    pub fn test_screen_visible_rows(&self) -> Option<(usize, usize)> {
        self.screen.visible_rows
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
    pub fn test_request_screen_previews(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.request_screen_previews(window, cx);
    }

    /// Whether a preview batch is marked active for the open picker.
    pub fn test_preview_batch_active(&self) -> bool {
        self.screen.preview_batching
    }

    /// Whether a background capture is really running. A new generation waits
    /// for this latch instead of overlapping the old capture.
    pub fn test_preview_capture_in_flight(&self) -> bool {
        self.screen.preview_in_flight
    }

    /// Model the background capture latch while a provider is still working,
    /// without blocking the test dispatcher inside the provider.
    pub fn test_set_preview_capture_in_flight(&mut self, in_flight: bool) {
        self.screen.preview_in_flight = in_flight;
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
        window: &mut Window,
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
        self.screen.finish_preview(window, epoch, identity, result)
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

    /// Whether one card currently owns a cached picture.
    pub fn test_screen_has_preview(&self, id: SourceId) -> bool {
        self.screen
            .source_for(id)
            .and_then(ScreenSource::identity)
            .is_some_and(|identity| self.screen.previews.contains_key(&identity))
    }

    /// Deferred preview requests the dialog made for changed visible rows.
    pub fn test_screen_preview_deferrals(&self) -> u32 {
        self.screen.preview_deferrals
    }

    /// The hard bounds the cache enforces, for a test assertion.
    pub fn test_screen_preview_bounds(&self) -> (usize, usize) {
        (
            super::screen_share::MAX_SCREEN_PREVIEWS,
            super::screen_share::MAX_SCREEN_PREVIEW_BYTES,
        )
    }

    /// The exact card note for a picture that did not fit the export budget.
    pub fn test_preview_memory_reason(&self) -> &'static str {
        super::screen_share::PREVIEW_MEMORY_REASON
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
    pub fn test_apply_screen_sources(
        &mut self,
        window: &mut Window,
        kind: SourceKind,
        sources: Vec<ScreenSource>,
    ) {
        let generation = match kind {
            SourceKind::Monitor => self.screen.monitor_generation + 1,
            SourceKind::Window => self.screen.window_generation + 1,
        };
        self.apply_screen_update(
            VoiceUpdate::ScreenSources {
                generation,
                kind,
                sources,
                error: None,
            },
            window,
        );
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.test_open_screen_picker(window, source.clone());
        let mut sharing = sharing;
        // The media layer fills `confirmed` when the attempt goes Live. Tests
        // that only describe the live snapshot get the same seam here: the
        // confirmed codec is the one the picker's intent resolves to, never a
        // fixed substitute for a codec this build may not publish.
        if sharing.confirmed.is_none() {
            let codec = self
                .codec_resolution()
                .expect("the fixture codec must resolve");
            sharing.confirmed = Some(Confirmed {
                attempt: sharing.attempt,
                quality: sharing.quality.unwrap_or_default(),
                codec,
                audio: sharing.has_audio,
                audio_scope: voxhold_media::system_audio::AudioScope::Device,
                source,
            });
        }
        self.screen.sharing = sharing;
        // Production reaches this dialog while the publication is Live, so the
        // normal gate answers "already live". The restart path must pass that
        // one reason without opening a second publication.
        self.test_share_gate = Some(Err("Трансляция уже идёт."));
        self.open_screen_settings(window, cx);
    }

    /// Open the live quality dialog exactly like the on-air card does, on the
    /// state a test set up itself. The picker snapshot is not touched.
    pub fn test_open_live_screen_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The live card reaches this dialog while the publication is Live, so
        // the normal gate answers "already live".
        self.test_share_gate = Some(Err("Трансляция уже идёт."));
        self.open_screen_settings(window, cx);
    }

    /// Record the accepted attempt exactly as the media layer does when it
    /// turns Live. This is the configuration a restart dialog must bind to.
    pub fn test_set_screen_confirmed_source(&mut self, source: ScreenSource) {
        // Mirror production: the accepted attempt carried the codec the dialog
        // really resolves, never a fixed substitute that this build may not
        // publish.
        let codec = self
            .codec_resolution()
            .expect("the fixture codec must resolve");
        self.screen.sharing.confirmed = Some(Confirmed {
            attempt: self.screen.sharing.attempt,
            source,
            quality: self.screen.quality(),
            codec,
            audio: false,
            audio_scope: voxhold_media::system_audio::AudioScope::Device,
        });
    }

    /// Record a fully described accepted attempt, for a test that needs the
    /// live audio flag or scope to match the dialog.
    pub fn test_set_screen_confirmed_request(
        &mut self,
        request: voxhold_media::publisher::PublishRequest,
    ) {
        self.screen.sharing.confirmed = Some(Confirmed {
            attempt: self.screen.sharing.attempt,
            source: request.source,
            quality: request.quality,
            codec: request.codec,
            audio: request.audio,
            audio_scope: request.audio_scope,
        });
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

    /// Return the gate to the production checks.
    pub fn test_clear_share_gate(&mut self) {
        self.test_share_gate = None;
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
        self.screen.applied_attempt = self.screen.applied_attempt.max(sharing.attempt);
        self.screen.sharing = sharing;
    }

    /// Confirmed `Stream` permissions of the server, as the shell hands them
    /// over after a channel reload.
    pub fn test_set_stream_permissions(
        &mut self,
        permissions: std::collections::HashMap<i64, bool>,
    ) {
        self.set_stream_permissions(permissions);
    }

    pub fn test_pip_active(&self) -> bool {
        self.pip_active()
    }

    pub fn test_screen_card_in_room(&self) -> bool {
        self.screen_card_in_room()
    }

    pub fn test_pip_tabs(&self) -> bool {
        self.pip_tabs()
    }

    pub fn test_screen_pip_active(&self) -> bool {
        self.screen_pip_active()
    }

    pub fn test_viewer_in_room(&self) -> bool {
        self.viewer_in_room()
    }

    pub fn test_pip_role(&self) -> super::PipRole {
        self.active_pip_role()
    }

    pub fn test_selected_pip_role(&self) -> super::PipRole {
        self.pip_role
    }

    pub fn test_select_pip_role(&mut self, role: super::PipRole) {
        self.pip_role = role;
    }

    pub fn test_pip_settings_open(&self) -> bool {
        self.pip_settings
    }

    pub fn test_pip_close_role(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pip_close_role(window, cx);
    }

    pub fn test_sync_pip(&mut self, window: &mut Window) {
        self.sync_pip(window);
    }

    pub fn test_publisher_room(&self) -> i64 {
        self.publisher_room()
    }

    pub fn test_pip_restore_focus(&self) -> bool {
        self.pip_restore_focus
    }

    /// The stream intent commands the UI handed to the detached worker. A
    /// navigation must not add `watch`/`close` entries.
    pub fn test_take_stream_commands(&mut self) -> Vec<(&'static str, u64)> {
        self.handle
            .as_ref()
            .map(|handle| handle.take_stream_commands_for_tests())
            .unwrap_or_default()
    }

    pub fn test_watching_selection(&self) -> Option<u64> {
        self.watching_selection
    }

    pub fn test_pip_status(&self) -> String {
        self.pip_status()
    }

    pub fn test_take_stream_mute(&mut self) -> Option<(u64, bool)> {
        self.handle
            .as_ref()
            .and_then(|handle| handle.take_stream_mute_for_tests())
    }

    /// Apply a screen-share runtime update exactly as the publisher emits it
    /// (including the live stats sample every second).
    pub fn test_apply_screen_sharing(&mut self, window: &mut Window, sharing: Sharing) -> bool {
        self.apply_screen_update(VoiceUpdate::ScreenShare(sharing), window)
    }

    /// Put one screen snapshot into the real coalescing queue, exactly as the
    /// worker does. A following `test_poll_screen_sharing` reads what the panel
    /// would read after a burst.
    pub fn test_push_screen_sharing(&self, sharing: Sharing) {
        if let Some(handle) = &self.handle {
            handle.push_update_for_tests(VoiceUpdate::ScreenShare(sharing));
        }
    }

    /// Read one update from the real queue, as the panel poll does.
    pub fn test_poll_screen_sharing(&self) -> Option<Sharing> {
        match self.handle.as_ref()?.try_update()? {
            VoiceUpdate::ScreenShare(sharing) => Some(sharing),
            _ => None,
        }
    }

    /// The quality-change operation whose answer this dialog waits for.
    pub fn test_pending_quality_op(&self) -> Option<u64> {
        self.screen.pending_quality_op
    }

    /// Queued screen actions of the detached handle, as (kind, picker) pairs.
    pub fn test_take_screen_commands(&mut self) -> Vec<(&'static str, u64)> {
        self.handle
            .as_ref()
            .map(|handle| handle.take_screen_commands_for_tests())
            .unwrap_or_default()
    }

    /// Queued screen actions with the concrete codec of every publish, restart
    /// or live-bitrate request.
    pub fn test_take_screen_requests(
        &mut self,
    ) -> Vec<(&'static str, u64, Option<voxhold_media::publisher::Codec>)> {
        self.handle
            .as_ref()
            .map(|handle| handle.take_screen_requests_for_tests())
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

    /// Pictures handed to `Window::drop_image` from the picker/on-air cache.
    pub fn test_screen_images_released(&self) -> u32 {
        self.screen.released_images
    }

    /// Incoming player pictures handed to `Window::drop_image`.
    pub fn test_player_images_released(&self) -> u32 {
        self.player_images_released
    }

    /// Image id of the on-air card picture, if it holds one.
    pub fn test_screen_preview_image_id(&self) -> Option<usize> {
        self.screen.preview.as_ref().map(|image| image.id.0)
    }

    /// Image id of the incoming player frame, if it holds one.
    pub fn test_player_frame_image_id(&self) -> Option<usize> {
        self.frame.as_ref().map(|image| image.id.0)
    }

    /// Image id that stood in the on-air slot when the picker last released a
    /// picture. `None` there means the slot was already empty.
    pub fn test_screen_release_slot_image_id(&self) -> Option<u64> {
        self.screen.last_release_slot_image
    }

    /// Image id that stood in the player slot when the last frame was
    /// released. `None` there means the slot was already empty.
    pub fn test_player_release_slot_image_id(&self) -> Option<u64> {
        self.last_release_slot_image
    }

    /// Install an on-air picture through the production setter, so a test can
    /// observe the replace/release rule without a capture stack.
    pub fn test_set_screen_preview(&mut self, window: &mut Window, image: Arc<RenderImage>) {
        self.screen.set_preview(window, image);
    }

    /// Put one picture into the detached handle exactly as the publisher worker
    /// does when the capture attaches it to a frame.
    pub fn test_push_screen_preview(&mut self, frame: PreviewFrame) {
        if let Some(handle) = &self.handle {
            handle.push_screen_preview_for_tests(frame);
        }
    }

    /// The production take path, for the inactive-phase discard case.
    pub fn test_take_screen_preview(&mut self, window: &mut Window) -> bool {
        self.take_screen_preview(window)
    }

    /// Run the production poll once, so a test covers the order of update
    /// application, demand sync and frame take rather than just the helpers.
    pub fn test_poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.poll(window, cx);
    }

    /// `(prepared, shown)` of the shared preview demand.
    pub fn test_screen_preview_stats(&self) -> (u64, u64) {
        self.handle
            .as_ref()
            .map(VoiceHandle::screen_preview_stats)
            .unwrap_or((0, 0))
    }

    /// `(enabled, requested width)` of the demand the card last wrote.
    pub fn test_screen_preview_demand(&self) -> Option<(bool, u32)> {
        let demand = self.handle.as_ref()?.screen_preview_demand_for_tests();
        Some((demand.is_enabled(), demand.width()))
    }

    /// Whether the frame slot still holds a picture the card did not take.
    pub fn test_screen_frame_pending(&self) -> bool {
        self.handle
            .as_ref()
            .is_some_and(VoiceHandle::screen_preview_pending_for_tests)
    }

    /// Run the production demand sync for the last painted card.
    pub fn test_sync_screen_preview_demand(&self, window: &Window) {
        self.sync_screen_preview_demand(window);
    }

    /// Scroll the room grid before the next paint, so a card can be moved out
    /// of the clipped viewport without a synthetic wheel event.
    pub fn test_set_room_scroll(&mut self, scroll: f32) {
        self.room_scroll = scroll;
    }

    /// Install an incoming player frame through the production setter.
    pub fn test_set_player_frame(&mut self, window: &mut Window, image: Arc<RenderImage>) {
        self.set_player_frame(window, image);
    }

    /// The production player close path: leave the stream and release its
    /// last frame.
    pub fn test_leave_stream(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_stream(window, cx);
    }

    /// The production reset path shared by leaving the call and a lost call.
    pub fn test_reset_screen(&mut self, window: &mut Window) {
        self.reset_screen(window);
    }

    /// Number of `poll` calls, so a test proves that an idle panel has none.
    pub fn test_poll_count(&self) -> u32 {
        self.polls
    }

    /// Start counting polls from zero after a test finished its setup.
    pub fn test_reset_poll_count(&mut self) {
        self.polls = 0;
    }

    /// Live event tasks. The handle stays readable after the panel entity is
    /// released, so a test can prove that the idle task really ended.
    pub fn test_event_tasks(&self) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
        self.event_tasks.running()
    }

    /// Override the platform minimize state for the event and preview gates.
    pub fn test_set_minimized(&mut self, minimized: Option<bool>) {
        self.test_minimized = minimized;
    }

    /// The production window-activation path with an explicit answer: a GPUI
    /// test window never reports itself as active.
    pub fn test_window_activated(&mut self) {
        self.window_activation(true);
    }

    /// Put one update into the real coalescing queue, exactly as the worker
    /// does; the queue is the production wake path of the panel.
    pub fn test_push_update(&self, update: VoiceUpdate) {
        if let Some(handle) = &self.handle {
            handle.push_update_for_tests(update);
        }
    }

    /// Publish one prepared player picture for `selection` and wake the panel.
    pub fn test_push_player_frame(&self, selection: u64, width: u32, height: u32) {
        if let Some(handle) = &self.handle {
            handle.push_stream_frame_for_tests(voxhold_media::runtime::StreamFrame {
                selection,
                width,
                height,
                bgra: vec![0x24; width as usize * height as usize * 4],
            });
        }
    }

    /// Take the player's one pending conversion request like the worker does.
    pub fn test_take_frame_request(&self) -> bool {
        self.handle
            .as_ref()
            .is_some_and(VoiceHandle::take_frame_request_for_tests)
    }

    /// The live-clock second the last poll recorded, for the 1 s tick test.
    pub fn test_live_clock_second(&self) -> Option<i64> {
        self.live_clock_second
    }
}

mod request;
