//! Periodic WebRTC statistics of one media session.
//!
//! Audio levels, capture/playout liveness and the viewer's cumulative inbound
//! video counters share one sample, so the viewer sees RTP and decoded frames
//! from the same instant.

use super::*;
use libwebrtc::{peer_connection::PeerConnectionState, stats::RtcStats};

impl Session {
    pub(super) async fn sample_levels(&mut self, events: &Events) {
        if self.peer.connection_state() != PeerConnectionState::Connected {
            return;
        }
        if self.mic.is_none() && !self.input_connected {
            self.input_connected = true;
            self.input_started_at = std::time::Instant::now();
        }
        let Ok(Ok(report)) = time::timeout(Duration::from_millis(125), self.peer.get_stats()).await
        else {
            return;
        };
        let mut local_level = 0.0_f64;
        let mut video_stats = None;
        for stat in report {
            match stat {
                RtcStats::MediaSource(source) => {
                    local_level = local_level.max(source.audio.audio_level);
                    if self.mic.is_none() {
                        // The platform ADM has no software block counter. Its
                        // cumulative captured-sample count is the real proof
                        // that recording is still delivering PCM.
                        let total = source.audio.total_samples_duration;
                        if self.input_duration != Some(total) {
                            self.input_duration = Some(total);
                            self.input_progress_at = std::time::Instant::now();
                        }
                    }
                }
                RtcStats::InboundRtp(inbound) if inbound.stream.kind == "video" => {
                    // Cumulative counters only: the viewer state machine needs
                    // to know whether RTP advances even when no frame decodes.
                    video_stats = Some((
                        inbound.inbound.bytes_received,
                        inbound.inbound.frames_decoded,
                    ));
                }
                RtcStats::InboundRtp(inbound) if inbound.stream.kind == "audio" => {
                    // Only the dedicated stream-viewer peer carries the
                    // publication's audio; the room voice peer must not be
                    // counted as screen audio in the live proof.
                    #[cfg(test)]
                    if self.viewer.is_some() {
                        crate::live_probe::record_viewer_audio(inbound.received.packets_received);
                    }
                    let track_id = inbound.inbound.track_identifier;
                    if track_id.is_empty() {
                        continue;
                    }
                    let level = inbound.inbound.audio_level;
                    let speaking = level >= SPEAKING_THRESHOLD;
                    if let Some(previous) = self.speaking.get_mut(&track_id) {
                        if *previous == speaking {
                            continue;
                        }
                        *previous = speaking;
                    } else {
                        if !speaking || self.speaking.len() >= 64 {
                            continue;
                        }
                        self.speaking.insert(track_id.clone(), speaking);
                    }
                    events.push(Event::Level {
                        track_id: Some(track_id),
                        level,
                    });
                }
                RtcStats::MediaPlayout(playout) => {
                    // The ADM's cumulative played duration is the only proof
                    // that the platform fallback is delivering PCM; initialized
                    // alone is not enough (same rule as the capture watchdog).
                    let total = playout.audio_playout.total_samples_duration;
                    if self.adm_playout_duration != Some(total) {
                        self.adm_playout_duration = Some(total);
                        self.adm_playout_progress_at = std::time::Instant::now();
                    }
                }
                _ => {}
            }
        }
        let speaking =
            local_level >= SPEAKING_THRESHOLD && !self.controls.muted && !self.controls.deafened;
        if self.local_speaking != speaking {
            self.local_speaking = speaking;
            events.push(Event::Level {
                track_id: None,
                level: if speaking { local_level } else { 0.0 },
            });
        }
        if let Some((bytes_received, frames_decoded)) = video_stats {
            // A missing report is simply not pushed; the viewer counts it
            // separately from repeated identical counters.
            events.push(Event::VideoStats {
                bytes_received,
                frames_decoded,
            });
        }
    }
}
