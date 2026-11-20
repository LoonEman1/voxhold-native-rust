//! Screen-share wiring between the panel, the voice session and the publisher.
//!
//! `VoiceHandle` methods only write bounded request slots; they never create a
//! peer or a capture. The session worker owns `Publisher` and at most one
//! `PublisherEngine`, applies the deterministic decisions and reports the
//! result back as one coalesced `ScreenShare` update. Every engine that stops
//! (accepted or rejected) is dropped on a short-lived release thread, so the
//! loop never joins a capture or a peer worker.

use super::*;
use crate::publisher::engine::{BitrateSend, PublisherEngine};
use crate::publisher::{Access, Outcome as PublisherOutcome, Publisher, Quality, QualityChange};
use crate::runtime::preflight::{PendingRestart, finish_preflight};
use crate::runtime::prep::{
    PREP_BUDGET, PrepBinding, ScreenPrep, access_violation, cancel_prep, finish_prep,
};
use crate::runtime::requests::{
    ScreenAccess, ScreenCommand, SourceRequest, push_screen, set_source_request,
};
use crate::screen_capture::SourceKind;

/// The newest live bitrate change that the engine worker has not accepted yet.
///
/// The worker command queue is bounded and an ICE burst can fill it for a tick,
/// so a full queue defers the value instead of ending the attempt. A newer
/// request replaces the pending one: only the latest quality is ever applied.
#[derive(Default)]
pub(super) struct BitrateQueue {
    pending: Option<(u64, Quality)>,
}

/// What one delivery attempt of [`BitrateQueue`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BitrateDelivery {
    /// Nothing was pending, or it belonged to an older attempt.
    Idle,
    /// The worker accepted the newest quality.
    Sent,
    /// The bounded queue is full; the value stays pending for the next tick.
    Full,
    /// The worker channel is closed; the attempt cannot keep sending.
    Closed,
}

impl BitrateQueue {
    /// Keep the newest request of `attempt`; a newer request replaces an older
    /// pending one.
    pub(super) fn request(&mut self, attempt: u64, quality: Quality) {
        self.pending = Some((attempt, quality));
    }

    /// Forget the pending value: the attempt it belonged to is gone.
    pub(super) fn clear(&mut self) {
        self.pending = None;
    }

    #[cfg(test)]
    pub(super) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Hand the pending quality to the live engine. A request of an older
    /// attempt is dropped without being sent. `Full` keeps the value for the
    /// next tick; `Closed` drops it because only a dead worker answers so.
    pub(super) fn flush(
        &mut self,
        current_attempt: u64,
        send: impl FnOnce(Quality) -> BitrateSend,
    ) -> BitrateDelivery {
        let Some((attempt, quality)) = self.pending else {
            return BitrateDelivery::Idle;
        };
        if attempt != current_attempt {
            self.pending = None;
            return BitrateDelivery::Idle;
        }
        match send(quality) {
            BitrateSend::Sent => {
                self.pending = None;
                BitrateDelivery::Sent
            }
            BitrateSend::Full => BitrateDelivery::Full,
            BitrateSend::Closed => {
                self.pending = None;
                BitrateDelivery::Closed
            }
        }
    }
}

/// One preview picture for the local on-air card.
pub struct ScreenFrame {
    pub width: u32,
    pub height: u32,
    /// BGRA bytes; the panel hands them to `RenderImage` directly.
    pub bgra: Vec<u8>,
}

impl VoiceHandle {
    /// Open the local picker. The returned id is required by
    /// [`Self::publish_screen`] and rejected after a leave or a newer picker.
    pub fn begin_screen_picker(&self) -> u64 {
        let picker = self.screen_picker.fetch_add(1, Ordering::SeqCst);
        push_screen(&self.requests, ScreenCommand::OpenPicker { picker });
        self.requests.wake();
        picker
    }

    /// Ask the session to start publishing the chosen source. Access values
    /// come from the panel's gate; the worker repeats the join check.
    pub fn publish_screen(
        &self,
        picker: u64,
        request: crate::publisher::PublishRequest,
        stream_permission: bool,
        slot_available: bool,
    ) {
        push_screen(
            &self.requests,
            ScreenCommand::Publish {
                picker,
                request,
                stream_permission,
                slot_available,
            },
        );
        self.requests.wake();
    }

    pub fn cancel_screen_picker(&self, picker: u64) {
        push_screen(&self.requests, ScreenCommand::CancelPicker { picker });
        self.requests.wake();
    }

    /// Change the quality of the live publication.
    ///
    /// A bitrate-only change is applied to the live sender. Anything that
    /// needs a new capture is preflighted off the voice loop first; only a
    /// successful preflight commits the confirmed stop+start.
    pub fn restart_screen_share(
        &self,
        request: crate::publisher::PublishRequest,
        stream_permission: bool,
        slot_available: bool,
    ) {
        push_screen(
            &self.requests,
            ScreenCommand::Restart {
                request,
                stream_permission,
                slot_available,
            },
        );
        self.requests.wake();
    }

    /// Apply a bitrate-only change to the live sender. The worker still
    /// classifies the request itself and falls back to the restart path when
    /// anything else changed.
    pub fn apply_screen_quality(
        &self,
        request: crate::publisher::PublishRequest,
        stream_permission: bool,
        slot_available: bool,
    ) {
        push_screen(
            &self.requests,
            ScreenCommand::LiveQuality {
                request,
                stream_permission,
                slot_available,
            },
        );
        self.requests.wake();
    }

    /// The user closed the quality dialog while its restart preflight was
    /// running. The live stream stays untouched.
    pub fn cancel_screen_restart(&self) {
        push_screen(&self.requests, ScreenCommand::CancelRestart);
        self.requests.wake();
    }

    /// One stop intent. The capture stops before the socket answer arrives.
    pub fn stop_screen_share(&self) {
        push_screen(&self.requests, ScreenCommand::Stop);
        self.requests.wake();
    }

    /// The panel's current gate while a picker or an attempt is open. The
    /// session applies the newest value before it accepts a prepared engine, so
    /// a permission or slot loss during the capture start is not missed.
    pub fn screen_access(&self, stream_permission: bool, slot_available: bool) {
        let next = ScreenAccess {
            stream_permission,
            slot_available,
        };
        if let Ok(mut slot) = self.requests.screen_access.lock() {
            if *slot == Some(next) {
                return;
            }
            *slot = Some(next);
        }
        self.requests.wake();
    }

    /// Refresh the picker list of one kind; the answer is tagged with the
    /// returned generation.
    pub fn screen_sources(&self, kind: SourceKind) -> u64 {
        let generation = self.source_generation.fetch_add(1, Ordering::SeqCst);
        set_source_request(&self.requests, kind, generation);
        self.requests.wake();
        generation
    }

    /// The newest local preview; only the on-air card reads it.
    pub fn take_screen_frame(&self) -> Option<ScreenFrame> {
        let frame = self.screen_preview.take()?;
        Some(ScreenFrame {
            width: frame.width,
            height: frame.height,
            bgra: frame.bgra,
        })
    }
}

impl Live {
    /// Apply the publisher that belongs to this socket session.
    pub(super) async fn apply_screen(
        &mut self,
        outcomes: Vec<PublisherOutcome>,
        viewer: &mut Viewer,
    ) -> Result<bool, SessionEnd> {
        let mut work: VecDeque<PublisherOutcome> = outcomes.into();
        while let Some(outcome) = work.pop_front() {
            match outcome {
                PublisherOutcome::StartEngine { attempt, request } => {
                    // A new attempt never reuses the old capture: the previous
                    // engine is handed to the release thread (its close
                    // overlaps the new device start) and a preparation still
                    // opening a device is replaced.
                    self.release_engine();
                    self.bitrate.clear();
                    let _ = self.cancel_screen_prep(None);
                    self.drop_restart_prep();
                    // Bind the capture to the gate the panel has right now.
                    let _ = self.refresh_screen_access();
                    let started = match access_violation(self.access_value()) {
                        Some(message) => Err(message.to_owned()),
                        None => self.start_screen_prep(attempt, request),
                    };
                    let outcomes = match started {
                        Ok(()) => Vec::new(),
                        Err(message) => {
                            self.publisher.on_attempt_rejected(&message, Instant::now())
                        }
                    };
                    for outcome in outcomes.into_iter().rev() {
                        work.push_front(outcome);
                    }
                }
                PublisherOutcome::StopEngine => {
                    // A stop also covers a capture that is still opening, so
                    // the device starts closing even when no engine exists yet.
                    self.bitrate.clear();
                    let _ = self.cancel_screen_prep(None);
                    self.release_engine();
                    self.screen_preview.clear();
                }
                PublisherOutcome::LeaveViewer => {
                    let outcomes = viewer.leave(Instant::now());
                    if !self.apply(outcomes, viewer).await? {
                        return Ok(false);
                    }
                }
                PublisherOutcome::Send(signal) => self.send_signal(signal).await?,
                PublisherOutcome::SetBitrate { attempt, quality } => {
                    // A stale decision of an older attempt must not change the
                    // new sender. The engine applies the limit on its worker.
                    if attempt != self.publisher.sharing().attempt {
                        continue;
                    }
                    // Keep the newest quality even when the bounded worker
                    // queue is full: it is retried on the next tick, and only a
                    // closed channel ends the attempt.
                    self.bitrate.request(attempt, quality);
                    for outcome in self.flush_screen_bitrate().into_iter().rev() {
                        work.push_front(outcome);
                    }
                }
                PublisherOutcome::Offer(sdp) => {
                    let failure = self
                        .screen
                        .as_ref()
                        .and_then(|screen| screen.offer(sdp).err());
                    if let Some(message) = failure {
                        for outcome in self
                            .publisher
                            .on_start_failed(&message, Instant::now())
                            .into_iter()
                            .rev()
                        {
                            work.push_front(outcome);
                        }
                    }
                }
                PublisherOutcome::Ice(candidate) => {
                    // A full command queue drops a candidate and counts the
                    // loss; an ICE burst must not end the publication.
                    if let Some(screen) = self.screen.as_ref() {
                        screen.ice(candidate);
                    }
                }
                PublisherOutcome::State(sharing) => {
                    self.updates.push(VoiceUpdate::ScreenShare(sharing));
                }
            }
        }
        // A server error or a state-machine transition can end the attempt
        // while the capture is still opening. The token is checked at the next
        // stage boundary and no `stream.start` is sent for a dead attempt.
        if self.prep.is_some() && self.publisher.phase() != crate::publisher::Phase::Publishing {
            let _ = self.cancel_screen_prep(None);
        }
        // A preflight only belongs to a live attempt; anything else releases
        // its temporary capture without touching the publisher.
        if self.restart.is_some() && self.publisher.phase() != crate::publisher::Phase::Live {
            self.drop_restart_prep();
        }
        Ok(true)
    }

    /// Deliver the newest pending live bitrate change to the engine.
    ///
    /// `Full` only shows a non-terminal status and keeps the previous limit;
    /// the next tick retries the newest value. `Closed` is the only answer that
    /// means the worker (and its capture) is gone.
    fn flush_screen_bitrate(&mut self) -> Vec<PublisherOutcome> {
        if self.publisher.phase() != crate::publisher::Phase::Live {
            self.bitrate.clear();
            return Vec::new();
        }
        let attempt = self.publisher.sharing().attempt;
        let delivery = self
            .bitrate
            .flush(attempt, |quality| match self.screen.as_ref() {
                Some(screen) => screen.set_bitrate(quality),
                None => BitrateSend::Closed,
            });
        match delivery {
            BitrateDelivery::Sent => self.publisher.bitrate_applied(),
            BitrateDelivery::Full => self.publisher.bitrate_queued(),
            BitrateDelivery::Closed => self.publisher.on_engine(
                crate::publisher::EngineEvent::Error("Медиадвижок трансляции уже закрыт.".into()),
                Instant::now(),
            ),
            BitrateDelivery::Idle => Vec::new(),
        }
    }

    /// Drain the engine of the current attempt, one bounded batch per tick.
    pub(super) async fn drain_screen(&mut self, viewer: &mut Viewer) -> Result<bool, SessionEnd> {
        for _ in 0..FRAMES_PER_TICK {
            let Some(event) = self.screen.as_ref().and_then(|screen| screen.poll()) else {
                return Ok(true);
            };
            let outcomes = self.publisher.on_engine(event, Instant::now());
            if !self.apply_screen(outcomes, viewer).await? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Leave/logout/reconnect: the pending capture is cancelled and the running
    /// one starts closing before the socket is touched (the join itself runs on
    /// the release thread), and a late picker of this session is rejected.
    pub(super) async fn abandon_screen(&mut self, viewer: &mut Viewer) -> Result<(), SessionEnd> {
        self.drop_restart_prep();
        let _ = self.cancel_screen_prep(None);
        let outcomes = self.publisher.abandon();
        let _ = self.apply_screen(outcomes, viewer).await?;
        self.release_engine();
        self.screen_preview.clear();
        Ok(())
    }

    /// The gate of the current room as the session sees it.
    pub(super) fn access_value(&self) -> Access {
        screen_access(
            self.joined,
            self.access.stream_permission,
            self.access.slot_available,
        )
    }

    /// A preparation result must still match the attempt, room and voice
    /// connection that requested it.
    fn prep_binding(&self) -> PrepBinding {
        PrepBinding {
            attempt: self.publisher.sharing().attempt,
            server: self.server,
            channel: self.channel,
            connection: self.connection.clone(),
            access: self.access_value(),
        }
    }

    fn start_screen_prep(
        &mut self,
        attempt: u64,
        request: crate::publisher::PublishRequest,
    ) -> Result<(), String> {
        let binding = PrepBinding {
            attempt,
            server: self.server,
            channel: self.channel,
            connection: self.connection.clone(),
            access: self.access_value(),
        };
        let servers = self.servers.clone();
        let policy = self.policy.clone();
        let preview = self.screen_preview.clone();
        let prep = ScreenPrep::start(binding, Instant::now(), PREP_BUDGET, move |cancelled| {
            let engine = PublisherEngine::start(request, servers, policy, preview, cancelled)?;
            let has_audio = engine.has_audio();
            let audio_scope = engine.audio_scope();
            Ok((engine, has_audio, audio_scope))
        })?;
        self.prep = Some(prep);
        Ok(())
    }

    /// Cancel a pending preparation. The engine of a stage that already
    /// finished is dropped on the preparation thread, not on this one.
    fn cancel_screen_prep(&mut self, message: Option<&'static str>) -> Vec<PublisherOutcome> {
        cancel_prep(&mut self.prep, &mut self.publisher, message, Instant::now())
    }

    /// Start the off-loop preflight of a live quality change. Nothing is
    /// stopped until it confirms the new capture and audio scope.
    fn start_restart_prep(
        &mut self,
        request: crate::publisher::PublishRequest,
    ) -> Result<(), String> {
        let pending = PendingRestart::start(self.prep_binding(), Instant::now(), request)?;
        self.restart = Some(pending);
        Ok(())
    }

    /// Release a preflight without reporting anything: the attempt it belonged
    /// to is leaving, stopping or being replaced.
    fn drop_restart_prep(&mut self) {
        if let Some(mut pending) = self.restart.take() {
            pending.cancel();
        }
    }

    /// Cancel a pending preflight and keep the live attempt. `message` reports
    /// a device/session loss; `None` is the user closing the dialog.
    fn cancel_restart_prep(&mut self, message: Option<&str>) -> Vec<PublisherOutcome> {
        if self.restart.take().is_none() {
            return Vec::new();
        }
        self.publisher.preflight_cancelled(message)
    }

    /// Observe the preflight once per tick. The result is applied only to the
    /// live attempt it was started for; a failure leaves the stream running.
    async fn poll_restart_prep(
        &mut self,
        now: Instant,
        viewer: &mut Viewer,
    ) -> Result<(), SessionEnd> {
        let Some(event) = self.restart.as_mut().and_then(|pending| pending.poll(now)) else {
            return Ok(());
        };
        let Some(pending) = self.restart.take() else {
            return Ok(());
        };
        let binding = pending.binding().clone();
        let request = pending.request().clone();
        drop(pending);
        let current = self.prep_binding();
        let access = self.access_value();
        let outcomes = finish_preflight(
            &mut self.publisher,
            &binding,
            &current,
            access,
            event,
            request,
            now,
        );
        let _ = self.apply_screen(outcomes, viewer).await?;
        Ok(())
    }

    /// Apply the freshest gate written by the panel. While a preparation is
    /// pending a revocation cancels it and fails the attempt before its late
    /// engine can reach `stream.start`. A preflight of a live attempt is
    /// cancelled without touching the stream.
    pub(super) fn apply_screen_access(&mut self, access: ScreenAccess) -> Vec<PublisherOutcome> {
        let gate = screen_access(self.joined, access.stream_permission, access.slot_available);
        let mut outcomes = Vec::new();
        if let Some(message) = access_violation(gate) {
            outcomes.extend(self.cancel_restart_prep(Some(message)));
        }
        outcomes.extend(apply_access(
            self.joined,
            &mut self.access,
            &mut self.prep,
            &mut self.publisher,
            access,
        ));
        outcomes
    }

    /// Pull the newest gate out of the shared last-value slot and apply it.
    fn refresh_screen_access(&mut self) -> Vec<PublisherOutcome> {
        let latest = self
            .screen_access
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        match latest {
            Some(access) => self.apply_screen_access(access),
            None => {
                self.publisher.set_access(self.access_value());
                Vec::new()
            }
        }
    }

    /// `voice.joined` or a lost voice connection. The publisher and a pending
    /// preparation follow the confirmed join, not a queued control snapshot.
    pub(super) fn set_joined(&mut self, joined: bool) -> Vec<PublisherOutcome> {
        if self.joined == joined {
            return Vec::new();
        }
        self.joined = joined;
        self.publisher.set_access(self.access_value());
        if joined {
            Vec::new()
        } else {
            let mut outcomes =
                self.cancel_restart_prep(Some("Сначала подключитесь к голосовому каналу."));
            outcomes
                .extend(self.cancel_screen_prep(Some("Сначала подключитесь к голосовому каналу.")));
            outcomes
        }
    }

    /// A new connection id belongs to a new voice session; a preparation bound
    /// to the old one must not start its capture or send `stream.start`.
    pub(super) fn set_connection(&mut self, connection: Option<String>) -> Vec<PublisherOutcome> {
        if self.connection == connection {
            self.publisher.set_connection(connection);
            return Vec::new();
        }
        self.connection = connection.clone();
        self.publisher.set_connection(connection);
        let mut outcomes = self.cancel_restart_prep(Some(
            "Голосовое соединение изменилось; запустите трансляцию снова.",
        ));
        if self.prep.is_some() {
            outcomes.extend(self.cancel_screen_prep(Some(
                "Голосовое соединение изменилось; запустите трансляцию снова.",
            )));
        }
        outcomes
    }

    /// Observe the preparation worker once per tick. This never waits for the
    /// capture: the result channel is bounded and only probed.
    pub(super) async fn poll_screen_prep(
        &mut self,
        now: Instant,
        viewer: &mut Viewer,
    ) -> Result<(), SessionEnd> {
        // The newest gate is applied before the preparation is observed, so a
        // revocation that arrived during the device start cancels the pending
        // preparation instead of letting its engine become the attempt.
        let stale = self.refresh_screen_access();
        if !stale.is_empty() {
            self.apply_screen(stale, viewer).await?;
        }
        let Some(event) = self.prep.as_mut().and_then(|prep| prep.poll(now)) else {
            return Ok(());
        };
        let Some(prep) = self.prep.take() else {
            return Ok(());
        };
        let current = self.prep_binding();
        // Read immediately before the decision. A revocation that lands between
        // this read and `publisher.engine_ready` can still emit `stream.start`.
        // That narrow residue is not unhandled: the server ends a publication
        // that lost the permission (`stream.stopped` with a permission/access
        // reason) or refuses the start (`forbidden`), and `Publisher` already
        // treats both as the terminal failure of this attempt. Re-reading here
        // narrows the window; it does not claim to close it.
        let access = self.access_value();
        let finish = finish_prep(
            &mut self.publisher,
            prep.binding(),
            &current,
            access,
            event,
            now,
        );
        if let Some(engine) = finish.engine {
            self.install_engine(engine);
        }
        if let Some(engine) = finish.discard {
            // A rejected engine already owns a capture and a peer. Release it
            // away from the session loop; `PublisherEngine::drop` may join the
            // capture thread and must not stall a mute or a socket event. It
            // was never installed, so the live probe does not count it.
            release_off_loop(engine, false);
        }
        self.apply_screen(finish.outcomes, viewer).await?;
        Ok(())
    }

    /// Make the engine of the current attempt the only capture. A replaced
    /// engine is handed to the release thread: installation must never join a
    /// capture that is still closing.
    fn install_engine(&mut self, engine: PublisherEngine) {
        if let Some(previous) = self.screen.replace(engine) {
            release_off_loop(previous, true);
        }
        #[cfg(test)]
        crate::live_probe::record_engine_started();
    }

    /// Hand the current capture to the release thread. A stop, restart or leave
    /// must not wait for `PublisherEngine::drop` to join the publisher worker
    /// (up to `WORKER_JOIN_TIMEOUT`), and the release starts immediately: the
    /// engine is gone from the session before this returns, and its worker
    /// closes just after. A new capture may open while the old one finishes
    /// closing; the desktop capturer supports that overlap.
    fn release_engine(&mut self) {
        if let Some(previous) = self.screen.take() {
            release_off_loop(previous, true);
        }
    }
}

/// Release a capture engine on a short-lived thread. `tracked` marks an engine
/// that was installed as the current attempt: the live probe counts a release
/// only after `PublisherEngine::drop` returned (its bounded join included), so
/// the test never reports a capture as gone while it is still closing.
fn release_off_loop(engine: PublisherEngine, tracked: bool) {
    release_on_worker(engine, move || {
        #[cfg(test)]
        if tracked {
            crate::live_probe::record_engine_stopped();
        }
        #[cfg(not(test))]
        let _ = tracked;
    });
}

/// The off-loop part, split out so it can be tested with a double whose drop is
/// gated: the caller must return before the drop completes. If the release
/// thread cannot be created the closure (and the engine) is dropped inline by
/// `spawn`'s error path, which only happens when the process cannot create
/// threads at all.
pub(super) fn release_on_worker<E: Send + 'static>(
    value: E,
    released: impl FnOnce() + Send + 'static,
) {
    let _ = thread::Builder::new()
        .name("voxhold-screen-release".into())
        .spawn(move || {
            drop(value);
            released();
        });
}

impl Drop for Live {
    /// The session is going away (leave, a lost socket, a reconnect). The
    /// running capture must still be stopped, but joining it here would hold
    /// the voice worker for up to `WORKER_JOIN_TIMEOUT` after the leave
    /// handshake; hand it to the same release thread as every other stop.
    fn drop(&mut self) {
        if let Some(engine) = self.screen.take() {
            release_off_loop(engine, true);
        }
    }
}

/// Set the panel's gate. A revocation while a preparation is pending cancels
/// it and fails the attempt, so the engine of that preparation can never reach
/// `stream.start`. `Live::poll_screen_prep` calls this before it observes the
/// preparation: a gate that changed during the device start outranks a ready
/// engine of the same attempt.
pub(super) fn apply_access<E: Send + 'static>(
    joined: bool,
    access: &mut ScreenAccess,
    prep: &mut Option<ScreenPrep<E>>,
    publisher: &mut Publisher,
    latest: ScreenAccess,
) -> Vec<PublisherOutcome> {
    *access = latest;
    let gate = screen_access(joined, latest.stream_permission, latest.slot_available);
    publisher.set_access(gate);
    if prep.is_none() {
        return Vec::new();
    }
    match access_violation(gate) {
        Some(message) => cancel_prep(prep, publisher, Some(message), Instant::now()),
        None => Vec::new(),
    }
}

/// Access of the room as the worker sees it. `permission` and `slot` are the
/// panel's confirmed gate; the join is re-checked from the voice controller.
pub(super) fn screen_access(joined: bool, permission: bool, slot: bool) -> Access {
    Access {
        joined,
        stream_permission: permission,
        slots_available: slot,
    }
}

/// The viewer is gone (never watched, or its `stream.left` was acknowledged)
/// and the publisher may start.
pub(super) fn viewer_cleared(viewer: &Viewer) -> bool {
    viewer.watching().is_none() && viewer.intent().is_none() && !viewer.leaving()
}

/// A running view, a pending switch (B selected while A is still leaving) and
/// an unacknowledged `stream.leave` all conflict with a publication in the
/// same channel. Treating only `watching()` as a conflict started the capture
/// without `LeaveViewer` and the viewer then began B during the publication.
pub(super) fn viewer_conflicts(viewer: &Viewer) -> bool {
    viewer.watching().is_some() || viewer.intent().is_some() || viewer.leaving()
}

impl Live {
    /// Execute one user action of the picker. The capture belongs to the
    /// session, so every transition is applied through `apply_screen`.
    pub(super) async fn handle_screen_command(
        &mut self,
        command: ScreenCommand,
        controller: &crate::controller::VoiceController,
        viewer: &mut Viewer,
    ) -> Result<(), SessionEnd> {
        let now = Instant::now();
        match command {
            ScreenCommand::OpenPicker { picker } => {
                let outcomes = self.publisher.begin_picker(picker);
                self.apply_screen(outcomes, viewer).await?;
            }
            ScreenCommand::CancelPicker { picker } => {
                let outcomes = self.publisher.cancel_picker(picker);
                self.apply_screen(outcomes, viewer).await?;
            }
            ScreenCommand::Stop => {
                // One stop intent also cancels a capture that is still being
                // prepared; the publisher has not sent `stream.start` yet.
                self.drop_restart_prep();
                let _ = self.cancel_screen_prep(None);
                let outcomes = self.publisher.stop(now);
                self.apply_screen(outcomes, viewer).await?;
            }
            ScreenCommand::CancelRestart => {
                // The dialog that requested the change is gone. The live
                // attempt was never stopped and stays exactly as it was.
                let outcomes = self.cancel_restart_prep(None);
                self.apply_screen(outcomes, viewer).await?;
            }
            ScreenCommand::Publish {
                picker,
                request,
                stream_permission,
                slot_available,
            } => {
                let connection = self.set_connection(controller.resume_connection_id.clone());
                self.apply_screen(connection, viewer).await?;
                let joined = self.set_joined(controller.joined);
                self.apply_screen(joined, viewer).await?;
                self.access = ScreenAccess {
                    stream_permission,
                    slot_available,
                };
                self.publisher.set_access(self.access_value());
                let viewer_active = viewer_conflicts(viewer);
                let outcomes = self.publisher.publish(picker, request, viewer_active, now);
                self.apply_screen(outcomes, viewer).await?;
            }
            ScreenCommand::Restart {
                request,
                stream_permission,
                slot_available,
            }
            | ScreenCommand::LiveQuality {
                request,
                stream_permission,
                slot_available,
            } => {
                // A change is only possible on a live attempt. The publisher
                // itself decides whether the request needs a new capture; the
                // dialog's split is a rendering hint, not the decision.
                if self.publisher.phase() != crate::publisher::Phase::Live {
                    return Ok(());
                }
                let _ = self.cancel_screen_prep(None);
                self.drop_restart_prep();
                let connection = self.set_connection(controller.resume_connection_id.clone());
                self.apply_screen(connection, viewer).await?;
                let joined = self.set_joined(controller.joined);
                self.apply_screen(joined, viewer).await?;
                self.access = ScreenAccess {
                    stream_permission,
                    slot_available,
                };
                self.publisher.set_access(self.access_value());
                let change = self
                    .publisher
                    .request()
                    .map(|current| current.change_to(&request))
                    .unwrap_or(QualityChange::Restart);
                if change == QualityChange::LiveBitrate {
                    let outcomes = self.publisher.apply_quality(request, now);
                    self.apply_screen(outcomes, viewer).await?;
                    return Ok(());
                }
                let outcomes = self.publisher.preflight_started();
                self.apply_screen(outcomes, viewer).await?;
                if self.publisher.phase() != crate::publisher::Phase::Live {
                    return Ok(());
                }
                if let Err(message) = self.start_restart_prep(request) {
                    let outcomes = self.publisher.preflight_failed(&message);
                    self.apply_screen(outcomes, viewer).await?;
                }
            }
        }
        Ok(())
    }

    /// Enumerate the picker list off the UI thread and report one typed answer.
    pub(super) fn list_screen_sources(&self, request: SourceRequest) {
        let (sources, error) = match crate::screen_capture::list_sources(request.kind) {
            Ok(sources) => (sources, None),
            Err(message) => (Vec::new(), Some(message.to_owned())),
        };
        self.updates.push(VoiceUpdate::ScreenSources {
            generation: request.generation,
            kind: request.kind,
            sources,
            error,
        });
    }

    /// Route one `stream.*` event to the publisher and start the attempt that
    /// waited for the viewer's `stream.left`.
    pub(super) async fn on_screen_signal(
        &mut self,
        kind: &str,
        request_id: &str,
        data: &serde_json::Value,
        now: Instant,
        viewer: &mut Viewer,
    ) -> Result<(), SessionEnd> {
        let outcomes = self.publisher.on_server_event(kind, request_id, data, now);
        self.apply_screen(outcomes, viewer).await?;
        if viewer_cleared(viewer) {
            let outcomes = self.publisher.viewer_left(now);
            self.apply_screen(outcomes, viewer).await?;
        }
        Ok(())
    }

    /// Deadlines of the share attempt and the publisher's engine events.
    pub(super) async fn tick_screen(
        &mut self,
        now: Instant,
        viewer: &mut Viewer,
    ) -> Result<(), SessionEnd> {
        // A prepared engine may have become ready since the last tick; the
        // result is probed without waiting for the capture.
        self.poll_screen_prep(now, viewer).await?;
        // A preflight of a live change is observed the same way: one bounded
        // probe per tick, never a wait for a device.
        self.poll_restart_prep(now, viewer).await?;
        // A bitrate change that met a full command queue is retried here, so a
        // burst of ICE/offers cannot lose the newest limit.
        let pending = self.flush_screen_bitrate();
        if !pending.is_empty() {
            self.apply_screen(pending, viewer).await?;
        }
        let outcomes = self.publisher.tick(now);
        self.apply_screen(outcomes, viewer).await?;
        if viewer_cleared(viewer) {
            let outcomes = self.publisher.viewer_left(now);
            self.apply_screen(outcomes, viewer).await?;
        }
        self.drain_screen(viewer).await?;
        Ok(())
    }
}
