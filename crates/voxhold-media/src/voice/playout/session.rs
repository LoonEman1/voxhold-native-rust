//! Session-side playout recovery and control.
//!
//! Split from playout.rs so the sink/mixer mechanics and the session state
//! machine stay separately reviewable and each file stays under the size limit.

use super::*;

/// Whether one receiver is audible on the ADM fallback. The room percentage in
/// `controls.output_gain` is deliberately not read: the ADM has no numeric bus
/// gain, so only the real mute/deafen flags and a zeroed participant gain may
/// close the track. The stored value belongs to the software mixer.
pub(super) fn adm_track_enabled(controls: &Controls, source: Source) -> bool {
    let gain = if controls.deafened {
        0.0
    } else {
        source.gain.clamp(0.0, 2.0)
    };
    !source.muted && gain > 0.0
}

impl Session {
    fn adm_calls_allowed(&self) -> bool {
        #[cfg(test)]
        {
            !self.test_no_adm
        }
        #[cfg(not(test))]
        {
            true
        }
    }

    /// Adopt the sink opened by the constructor. Every initial sink enters the
    /// same `Opening → Unstable` proof window as a reopened one; otherwise a
    /// normal call never reaches `Stable` and never clears the budget.
    /// `Session::new` calls this right after it opens the sink, and the media
    /// tests call it on a headless session, so the regression "the first sink
    /// bypasses the recovery machine" is covered by the media tests instead of
    /// a manual field write in the test.
    pub(crate) fn adopt_initial_playout(mut self, playout: Option<Playout>) -> Self {
        if let Some(playout) = playout {
            self.playout = Some(playout);
            self.software_playout = true;
            self.playout_mode = PlayoutMode::Software;
            self.playout_attempts.opened();
            self.last_playout_report = std::time::Instant::now() - Duration::from_secs(1);
        }
        self.apply_audio_controls();
        self
    }

    /// Feed the current bus gain and per-source gains/mutes to the mixer. A
    /// full snapshot is stored as one last-value slot, so a busy frame channel
    /// cannot lose it.
    pub(crate) fn push_playout_state(&self) {
        let Some(playout) = &self.playout else {
            return;
        };
        let bus = if self.controls.deafened {
            0.0
        } else {
            self.controls.output_gain.unwrap_or(1.0).clamp(0.0, 2.0)
        };
        let sources = self
            .sources
            .iter()
            .map(|(id, source)| (id.clone(), *source))
            .collect::<Vec<_>>();
        playout.set_state(MixState { bus, sources });
    }

    /// Switch the software sink. The new device is opened before the old sink
    /// is replaced; a failure leaves the previous audible path in place. A
    /// successful switch is only an open: the new device starts its own
    /// `Unstable` proof window, and the report says so immediately instead of
    /// keeping the previous device's `Stable` phase.
    pub(crate) fn switch_playout(&mut self, id: &str, events: &Events) -> SwitchResult {
        let Some(playout) = &self.playout else {
            return SwitchResult {
                switched: false,
                audible: false,
                applied: self.output.applied.clone(),
            };
        };
        let target = (!id.is_empty()).then(|| id.to_owned());
        let opened = self.output.applied.clone();
        let switched = playout.switch_sink(target.as_deref());
        match switched {
            Ok(()) => {
                // The old sink's age and written blocks must not carry over to
                // the device that was just opened.
                self.playout_attempts.opened();
                self.last_playout_report = std::time::Instant::now();
                if let Some(playout) = &self.playout {
                    let mut stats = playout.stats();
                    stats.phase = PlayoutPhase::Unstable;
                    stats.reconnects = self.playout_reconnects;
                    events.push(Event::Playout(stats));
                }
                SwitchResult {
                    switched: true,
                    audible: true,
                    applied: target,
                }
            }
            Err(_) => SwitchResult {
                switched: false,
                audible: self.playout.as_ref().is_some_and(Playout::running),
                applied: opened,
            },
        }
    }

    /// Release the sink on suspend. The room is silent until a resume; the UI
    /// is told immediately that the mixer no longer runs.
    pub(crate) fn suspend_playout(&mut self, events: &Events) {
        self.playout_suspended = true;
        if self.playout.take().is_some() {
            self.abort_playout_tasks();
        }
        self.report_audio(events);
    }

    /// Reopen the room sink after a resume. A failure is not final: the retry
    /// ladder is driven by `tick_playout`, and after the budget the session
    /// moves to the ADM fallback instead of staying silent.
    pub(crate) fn resume_playout(&mut self, events: &Events) {
        self.playout_suspended = false;
        let now = std::time::Instant::now();
        if !self.software_playout {
            // The platform path owns playout on this session; force the normal
            // device poll instead of opening a second sink.
            self.device_check = now - Duration::from_secs(2);
            self.check_devices(events);
            return;
        }
        if self.playout_mode == PlayoutMode::AdmFallback {
            // The mixer budget was already spent. Reopening it while the ADM
            // plays would duplicate the room; only the ADM path is restored,
            // and its real PCM progress is checked by the watchdog tick.
            self.adm_playout_started_at = now;
            self.adm_playout_duration = None;
            self.adm_playout_progress_at = now;
            if self.adm_calls_allowed() {
                self.audio.set_playout(true);
                if !self.audio.running(false) {
                    let started = self.audio.start(false);
                    self.report_audio(events);
                    if !started {
                        events.push(Event::DeviceError {
                            message: "Системное воспроизведение комнаты не восстановлено после сна.",
                        });
                    }
                }
            }
            return;
        }
        if self.playout.as_ref().is_some_and(Playout::running) {
            self.push_playout_state();
            return;
        }
        self.playout.take();
        self.abort_playout_tasks();
        self.playout_attempts.reset_for_resume(now);
        if !self.attempt_software_playout(events, RecoveryCause::Resume) {
            // The retry ladder owns the next attempt; the UI must stop saying
            // that the mixer applies gain in the meantime.
            self.report_audio(events);
        }
    }

    /// Periodic sink health and retry driver. Called from the media tick, not
    /// from the UI, and never busy-loops: every transition is counted once.
    pub(crate) fn tick_playout(&mut self, events: &Events) {
        let now = std::time::Instant::now();
        self.prune_playout_tasks();
        match self.playout_mode {
            PlayoutMode::Software => {
                if self.playout_suspended {
                    return;
                }
                if let Some(playout) = &self.playout {
                    if playout.running() {
                        if self.playout_attempts.phase() == PlayoutPhase::Unstable
                            && playout.stable(now)
                        {
                            // Only proof of stability clears the shared budget.
                            self.playout_attempts.mark_stable();
                        }
                        if self.last_playout_report.elapsed() >= REPORT_INTERVAL {
                            self.last_playout_report = now;
                            let mut stats = playout.stats();
                            stats.reconnects = self.playout_reconnects;
                            stats.phase = self.playout_attempts.phase();
                            events.push(Event::Playout(stats));
                        }
                        return;
                    }
                    // Final truthful report: the sink is gone, so the settings
                    // line must not keep the last "stable" snapshot.
                    let mut stats = playout.stats();
                    stats.running = false;
                    stats.phase = PlayoutPhase::Opening;
                    stats.reconnects = self.playout_reconnects;
                    events.push(Event::Playout(stats));
                    // The sink opened but died before proving stability. This
                    // is one state transition: one failure, one message, no
                    // reattach and no immediate reopen.
                    self.playout = None;
                    self.abort_playout_tasks();
                    let outcome = self.playout_attempts.sink_died(now);
                    if outcome == FailureOutcome::AdmFallback {
                        self.engage_adm_fallback(events, RecoveryCause::Sink);
                        return;
                    }
                    events.push(Event::DeviceError {
                        message: "Воспроизведение комнаты остановилось; восстанавливаем.",
                    });
                    self.report_audio(events);
                }
                if !self.playout_attempts.due(now) {
                    return;
                }
                self.attempt_software_playout(events, RecoveryCause::Sink);
            }
            PlayoutMode::AdmFallback => {
                if self.playout_suspended || !self.adm_calls_allowed() {
                    return;
                }
                if !self.adm_retry.due(now) {
                    return;
                }
                self.adm_retry.schedule(now, ADM_RETRY);
                if self.adm_playout_healthy(now) {
                    return;
                }
                // Initialized is not proof of PCM: restart a platform path that
                // stopped delivering played samples.
                if self.audio.running(false) {
                    let _ = self.audio.stop(false);
                }
                self.audio.set_playout(true);
                let started = self.audio.start(false);
                self.adm_playout_started_at = now;
                self.adm_playout_duration = None;
                self.adm_playout_progress_at = now;
                self.report_audio(events);
                events.push(Event::DeviceError {
                    message: if started {
                        "Системный ADM не отдаёт PCM; перезапускаем воспроизведение."
                    } else {
                        "Системное воспроизведение комнаты не восстановлено; повторяем."
                    },
                });
            }
        }
    }

    /// The ADM path is healthy only while its cumulative played duration moves.
    /// A startup grace covers the first stats report, exactly as the capture
    /// watchdog does.
    fn adm_playout_healthy(&self, now: std::time::Instant) -> bool {
        adm_playout_ok(
            self.audio.running(false),
            self.has_inbound_audio(),
            now.duration_since(self.adm_playout_started_at),
            self.adm_playout_duration.is_some(),
            self.adm_playout_progress_at.elapsed(),
        )
    }

    fn has_inbound_audio(&self) -> bool {
        self.peer
            .receivers()
            .iter()
            .any(|receiver| matches!(receiver.track(), Some(MediaStreamTrack::Audio(_))))
    }

    /// One bounded open attempt. A successful open is not yet a recovery: it
    /// moves to `Unstable` and the health tick proves it before the budget is
    /// cleared. Failures schedule the next attempt or move to the ADM fallback.
    pub(crate) fn attempt_software_playout(
        &mut self,
        events: &Events,
        cause: RecoveryCause,
    ) -> bool {
        let now = std::time::Instant::now();
        let device = self
            .output
            .selected
            .clone()
            .or_else(|| self.output.applied.clone())
            .or_else(|| self.controls.output.clone());
        match open_sink(
            &self.playout_open,
            device.as_deref(),
            &mut self.playout_attempts,
            now,
        ) {
            Ok(playout) => {
                self.playout = Some(playout);
                self.playout_mode = PlayoutMode::Software;
                self.playout_reconnects = self.playout_reconnects.saturating_add(1);
                self.attach_existing_tracks();
                self.push_playout_state();
                self.report_audio(events);
                if let Some(playout) = &self.playout {
                    let mut stats = playout.stats();
                    stats.reconnects = self.playout_reconnects;
                    stats.phase = PlayoutPhase::Unstable;
                    events.push(Event::Playout(stats));
                }
                events.push(Event::DeviceError {
                    message: match cause {
                        RecoveryCause::Resume => {
                            "Звук комнаты открыт после сна; проверяем стабильность."
                        }
                        RecoveryCause::Sink => "Микшер открыт заново; проверяем стабильность.",
                    },
                });
                true
            }
            Err(FailureOutcome::Retry(_)) => {
                events.push(Event::DeviceError {
                    message: match cause {
                        RecoveryCause::Resume => "Не удалось возобновить звук комнаты; повторяем.",
                        RecoveryCause::Sink => "Не удалось восстановить микшер комнаты; повторяем.",
                    },
                });
                false
            }
            Err(FailureOutcome::AdmFallback) => {
                self.engage_adm_fallback(events, cause);
                false
            }
        }
    }

    /// The software mixer exhausted its budget. Re-enable the platform playout
    /// and tell the UI that only mute is applied from now on.
    fn engage_adm_fallback(&mut self, events: &Events, cause: RecoveryCause) {
        self.playout_mode = PlayoutMode::AdmFallback;
        self.playout_attempts.to_adm();
        self.playout = None;
        self.abort_playout_tasks();
        let started = if self.adm_calls_allowed() {
            self.audio.set_playout(true);
            self.audio.start(false)
        } else {
            false
        };
        let now = std::time::Instant::now();
        self.adm_retry.schedule(now, ADM_RETRY);
        self.adm_playout_started_at = now;
        self.adm_playout_duration = None;
        self.adm_playout_progress_at = now;
        self.apply_audio_controls();
        self.report_audio(events);
        // The UI must drop the stale mixer snapshot: there is no live post-mix
        // meter or gain on the ADM path.
        events.push(Event::Playout(PlayoutStats {
            running: false,
            phase: PlayoutPhase::AdmFallback,
            reconnects: self.playout_reconnects,
            ..PlayoutStats::default()
        }));
        events.push(Event::DeviceError {
            message: match (cause, started) {
                (_, false) => {
                    "Микшер и системное воспроизведение не восстановились; звук комнаты недоступен."
                }
                (RecoveryCause::Resume, true) => {
                    "Микшер не восстановился после сна; звук комнаты идёт через системный ADM без усиления участников."
                }
                (RecoveryCause::Sink, true) => {
                    "Микшер не восстановился; звук комнаты идёт через системный ADM без усиления участников."
                }
            },
        });
    }

    fn abort_playout_tasks(&mut self) {
        if let Ok(mut tasks) = self.playout_tasks.lock() {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
    }

    /// A finished track task is kept in the shared Vec until the call ends
    /// unless it is pruned; a call can add a task per renegotiation.
    fn prune_playout_tasks(&mut self) {
        if let Ok(mut tasks) = self.playout_tasks.lock() {
            prune_finished(&mut tasks);
        }
    }

    /// Every receiver that is still present gets a fresh engine sink on the new
    /// mixer. `on_track` is not repeated for those tracks after a system sleep.
    fn attach_existing_tracks(&mut self) {
        self.prune_playout_tasks();
        let Some(playout) = self.playout.as_ref().map(Playout::clip) else {
            return;
        };
        for receiver in self.peer.receivers() {
            if let Some(MediaStreamTrack::Audio(track)) = receiver.track() {
                let task = playout.attach(track.id(), track, self.runtime.clone());
                if let Ok(mut tasks) = self.playout_tasks.lock() {
                    tasks.push(task);
                }
            }
        }
    }
}

impl Session {
    pub(crate) fn apply_audio_controls(&mut self) {
        if self.playout.is_some() {
            // The software mixer owns the gain and the local mute; the remote
            // track stays enabled so its sink keeps receiving decoded PCM.
            self.push_playout_state();
            return;
        }
        // Without the software mixer the pinned wrapper exposes no numeric
        // gain. The stored room percentage belongs to the mixer and the UI, so
        // only the real mute/deafen flags (and a zeroed participant gain) may
        // close a receiver. Sending `None`/0 % must not silence the ADM.
        for receiver in self.peer.receivers() {
            if let Some(MediaStreamTrack::Audio(track)) = receiver.track() {
                let id = track.id();
                let source = self.sources.get(&id).copied().unwrap_or_default();
                track.set_enabled(adm_track_enabled(&self.controls, source));
            }
        }
    }

    /// Effective gain of one receiver as the mixer sees it.
    #[allow(dead_code)]
    pub fn gain_for_track(&self, track_id: &str) -> f64 {
        let source = self.sources.get(track_id).copied().unwrap_or_default();
        if source.muted { 0.0 } else { source.gain }
    }

    /// True only while the software sink is actually open. The UI must use this
    /// rather than the start-of-session `AppliedAudio` flag.
    pub(crate) fn software_mixer_active(&self) -> bool {
        self.playout_mode == PlayoutMode::Software
            && self.playout.as_ref().is_some_and(Playout::running)
    }

    /// True when the session owns the software path (avoids the ADM device
    /// check racing the retry ladder).
    pub(crate) fn software_playout_mode(&self) -> bool {
        self.playout_mode == PlayoutMode::Software && self.software_playout
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The output percentage is a software-mixer value. On the ADM path only
    /// the real mute/deafen state may close a receiver, and a zero percentage
    /// must not silence the room.
    #[test]
    fn the_adm_path_ignores_the_room_percentage_but_keeps_mute_real() {
        let source = Source {
            gain: 1.0,
            muted: false,
        };
        for output_gain in [None, Some(0.0), Some(0.5), Some(1.5), Some(2.0)] {
            let controls = Controls {
                output_gain,
                ..Controls::default()
            };
            assert!(
                adm_track_enabled(&controls, source),
                "output_gain {output_gain:?} is not applied by the ADM"
            );
        }
        let deafened = Controls {
            deafened: true,
            ..Controls::default()
        };
        assert!(
            !adm_track_enabled(&deafened, source),
            "deafen still closes the ADM room"
        );
        assert!(
            !adm_track_enabled(
                &Controls::default(),
                Source {
                    muted: true,
                    ..source
                }
            ),
            "a local or server mute stays real on the ADM"
        );
        assert!(
            !adm_track_enabled(
                &Controls::default(),
                Source {
                    gain: 0.0,
                    ..source
                }
            ),
            "a participant at 0 % is silence in both paths"
        );
    }
}
