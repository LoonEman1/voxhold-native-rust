//! Verified live-bitrate application.
//!
//! The sender queue only proves that a command reached the worker. A live
//! change counts as applied only after every active sender accepted the new
//! encoding limit and the readback (`RtpSender::parameters`) agrees with it.
//! If a stage fails, the previously confirmed limits are restored; a failed
//! restore is reported as a partial change instead of a clean refusal.
//!
//! The [`BitrateSink`] seam keeps those decisions testable without a real
//! network failure: the engine implementation talks to `RtpSender` and
//! `PeerConnection`, while the tests drive a fake with scripted answers.

use libwebrtc::{
    peer_connection::{BitrateSettings, PeerConnection},
    rtp_sender::RtpSender,
};

use crate::publisher::Quality;

/// The limits that were really confirmed on the live senders. `None` means the
/// stage was never confirmed (or has no track), so it cannot be restored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    pub video_bps: Option<u64>,
    pub audio_bps: Option<u64>,
    pub peer_bps: Option<u64>,
}

/// The concrete limits one quality asks for. The audio stage exists only when
/// the publication really carries the system-audio track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub video_bps: u64,
    pub audio_bps: Option<u64>,
    pub peer_bps: u64,
}

impl Target {
    pub fn from_quality(quality: Quality, audio: bool) -> Self {
        let video_bps = u64::from(quality.bitrate_kbps.clamp(500, 16_000)) * 1000;
        let audio_bps = audio.then(|| u64::from(quality.audio_bitrate_kbps.clamp(32, 510)) * 1000);
        Self {
            video_bps,
            audio_bps,
            peer_bps: video_bps + audio_bps.unwrap_or(0),
        }
    }
}

/// One verified application that did not stick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// User-facing reason, including whether the previous limits came back.
    pub message: String,
    /// Every changed stage returned to its previous value.
    pub restored: bool,
    /// At least one changed stage could not be restored.
    pub partial: bool,
}

/// The sender side of one live change.
pub trait BitrateSink {
    /// Apply the video encoding limit and return the readback maximum.
    fn set_video(&mut self, limit: u64) -> Result<Option<u64>, String>;
    /// Apply the published-audio encoding limit and return the readback maximum.
    fn set_audio(&mut self, limit: u64) -> Result<Option<u64>, String>;
    /// Apply the connection-wide limit shared by all RTP streams.
    fn set_peer(&mut self, total: u64) -> Result<(), String>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Video,
    Audio,
    Peer,
}

impl Stage {
    fn label(self) -> &'static str {
        match self {
            Self::Video => "видео",
            Self::Audio => "аудио",
            Self::Peer => "соединения",
        }
    }
}

/// Apply `target`, verify every readback, and restore `previous` on failure.
///
/// The result is the limits the senders really report, never the requested
/// values. A stage that returned a different readback is a failure even when
/// the underlying call reported success.
pub fn apply_verified(
    sink: &mut impl BitrateSink,
    target: Target,
    previous: Limits,
) -> Result<Limits, Failure> {
    match sink.set_video(target.video_bps) {
        Ok(Some(readback)) if readback == target.video_bps => {}
        Ok(readback) => {
            // The call wrote a value but the readback disagrees: the stage
            // changed and must be restored.
            return Err(recover(
                sink,
                Stage::Video,
                mismatch(target.video_bps, readback),
                [true, false, false],
                previous,
            ));
        }
        Err(error) => {
            return Err(recover(
                sink,
                Stage::Video,
                error,
                [false, false, false],
                previous,
            ));
        }
    }

    let audio = match target.audio_bps {
        Some(limit) => match sink.set_audio(limit) {
            Ok(Some(readback)) if readback == limit => true,
            Ok(readback) => {
                return Err(recover(
                    sink,
                    Stage::Audio,
                    mismatch(limit, readback),
                    [true, true, false],
                    previous,
                ));
            }
            Err(error) => {
                return Err(recover(
                    sink,
                    Stage::Audio,
                    error,
                    [true, false, false],
                    previous,
                ));
            }
        },
        None => false,
    };

    if let Err(error) = sink.set_peer(target.peer_bps) {
        return Err(recover(
            sink,
            Stage::Peer,
            error,
            [true, audio, false],
            previous,
        ));
    }

    Ok(Limits {
        video_bps: Some(target.video_bps),
        audio_bps: target.audio_bps,
        peer_bps: Some(target.peer_bps),
    })
}

fn mismatch(expected: u64, readback: Option<u64>) -> String {
    match readback {
        Some(actual) => format!("движок подтвердил {actual} бит/с вместо {expected}"),
        None => "движок не подтвердил новый предел: у отправителя нет активных кодирований".into(),
    }
}

/// Return the changed stages to `previous` in reverse order. A stage whose
/// previous value is unknown cannot be restored, which makes the refusal
/// partial instead of clean.
fn recover(
    sink: &mut impl BitrateSink,
    stage: Stage,
    detail: String,
    applied: [bool; 3],
    previous: Limits,
) -> Failure {
    let [video, audio, peer] = applied;
    let mut restored = true;
    if peer {
        match previous.peer_bps {
            Some(limit) if sink.set_peer(limit).is_ok() => {}
            _ => restored = false,
        }
    }
    if audio {
        match previous.audio_bps {
            Some(limit) => {
                if !matches!(sink.set_audio(limit), Ok(Some(readback)) if readback == limit) {
                    restored = false;
                }
            }
            None => restored = false,
        }
    }
    if video {
        match previous.video_bps {
            Some(limit) => {
                if !matches!(sink.set_video(limit), Ok(Some(readback)) if readback == limit) {
                    restored = false;
                }
            }
            None => restored = false,
        }
    }
    let recovery = if restored {
        "Прежние параметры восстановлены."
    } else {
        "Прежние параметры восстановить не удалось: часть лимитов изменена."
    };
    Failure {
        message: format!(
            "Не удалось применить новый битрейт ({}): {}. {recovery}",
            stage.label(),
            detail.trim_end_matches('.')
        ),
        restored,
        partial: !restored,
    }
}

/// What a re-offer must reapply. Only a confirmed change replaces the live
/// quality; a refused one keeps the previous value so `startRecovery` cannot
/// silently revert or invent a limit.
pub fn confirmed_quality(current: Quality, requested: Quality, applied: bool) -> Quality {
    if applied { requested } else { current }
}

/// The sink of the real engine. It applies one stage at a time and reads the
/// sender parameters back, so a call that reports success but leaves another
/// maximum is still a failure.
pub(super) struct LiveSink<'a> {
    pub(super) video: &'a RtpSender,
    pub(super) audio: Option<&'a RtpSender>,
    pub(super) peer: &'a PeerConnection,
}

impl BitrateSink for LiveSink<'_> {
    fn set_video(&mut self, limit: u64) -> Result<Option<u64>, String> {
        set_sender_limit(self.video, limit)
    }

    fn set_audio(&mut self, limit: u64) -> Result<Option<u64>, String> {
        match self.audio {
            Some(sender) => set_sender_limit(sender, limit),
            None => Err("у публикации нет звуковой дорожки".into()),
        }
    }

    fn set_peer(&mut self, total: u64) -> Result<(), String> {
        self.peer
            .set_bitrate(BitrateSettings {
                max_bitrate_bps: Some(total),
                ..BitrateSettings::default()
            })
            .map_err(|error| error.to_string())
    }
}

/// Apply `quality` to the negotiated senders and the peer, returning only the
/// limits their readback confirmed.
pub(super) fn apply_sender_limits(
    peer: &PeerConnection,
    video: &RtpSender,
    audio: Option<&RtpSender>,
    quality: Quality,
    previous: Limits,
) -> Result<Limits, Failure> {
    let mut sink = LiveSink { video, audio, peer };
    apply_verified(
        &mut sink,
        Target::from_quality(quality, audio.is_some()),
        previous,
    )
}

/// Set `max_bitrate` on every encoding of one sender and return the value the
/// sender reports afterwards. `None` means no active encoding could report a
/// limit, which never confirms a change.
fn set_sender_limit(sender: &RtpSender, limit: u64) -> Result<Option<u64>, String> {
    let mut parameters = sender.parameters();
    for encoding in &mut parameters.encodings {
        encoding.max_bitrate = Some(limit);
    }
    sender
        .set_parameters(parameters)
        .map_err(|error| error.to_string())?;
    Ok(readback(sender))
}

/// The actual `max_bitrate` of one sender, read from `parameters()`.
fn readback(sender: &RtpSender) -> Option<u64> {
    sender
        .parameters()
        .encodings
        .iter()
        .filter_map(|encoding| encoding.max_bitrate)
        .max()
}

/// Read the senders after a refused change. A failed restore can leave a
/// partially changed limit, so the diagnostics must report the real value
/// instead of the previously confirmed one.
pub(super) fn readback_limits(video: &RtpSender, audio: Option<&RtpSender>) -> Limits {
    Limits {
        video_bps: readback(video),
        audio_bps: audio.and_then(readback),
        peer_bps: None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    #[derive(Default)]
    struct FakeSink {
        video: Option<u64>,
        audio: Option<u64>,
        peer: Option<u64>,
        video_answers: VecDeque<Result<Option<u64>, String>>,
        audio_answers: VecDeque<Result<Option<u64>, String>>,
        peer_answers: VecDeque<Result<(), String>>,
    }

    impl FakeSink {
        fn with_video(video: u64, audio: Option<u64>, peer: u64) -> Self {
            Self {
                video: Some(video),
                audio,
                peer: Some(peer),
                ..Self::default()
            }
        }

        fn video_answer(&mut self, answer: Result<Option<u64>, String>) {
            self.video_answers.push_back(answer);
        }

        fn audio_answer(&mut self, answer: Result<Option<u64>, String>) {
            self.audio_answers.push_back(answer);
        }

        fn peer_answer(&mut self, answer: Result<(), String>) {
            self.peer_answers.push_back(answer);
        }
    }

    impl BitrateSink for FakeSink {
        fn set_video(&mut self, limit: u64) -> Result<Option<u64>, String> {
            match self.video_answers.pop_front() {
                Some(Ok(readback)) => {
                    self.video = Some(limit);
                    Ok(readback)
                }
                Some(Err(error)) => Err(error),
                None => {
                    self.video = Some(limit);
                    Ok(Some(limit))
                }
            }
        }

        fn set_audio(&mut self, limit: u64) -> Result<Option<u64>, String> {
            match self.audio_answers.pop_front() {
                Some(Ok(readback)) => {
                    self.audio = Some(limit);
                    Ok(readback)
                }
                Some(Err(error)) => Err(error),
                None => {
                    self.audio = Some(limit);
                    Ok(Some(limit))
                }
            }
        }

        fn set_peer(&mut self, total: u64) -> Result<(), String> {
            match self.peer_answers.pop_front() {
                Some(Ok(())) => {
                    self.peer = Some(total);
                    Ok(())
                }
                Some(Err(error)) => Err(error),
                None => {
                    self.peer = Some(total);
                    Ok(())
                }
            }
        }
    }

    fn target(video: u64, audio: Option<u64>) -> Target {
        Target {
            video_bps: video,
            audio_bps: audio,
            peer_bps: video + audio.unwrap_or(0),
        }
    }

    #[test]
    fn a_verified_change_reports_the_readback_limits() {
        let previous = Limits {
            video_bps: Some(2_500_000),
            audio_bps: Some(128_000),
            peer_bps: Some(2_628_000),
        };
        let mut sink = FakeSink::with_video(2_500_000, Some(128_000), 2_628_000);
        let applied = apply_verified(&mut sink, target(1_200_000, Some(64_000)), previous)
            .expect("both senders and the peer accept the limit");
        assert_eq!(
            applied,
            Limits {
                video_bps: Some(1_200_000),
                audio_bps: Some(64_000),
                peer_bps: Some(1_264_000),
            }
        );
        assert_eq!(sink.video, Some(1_200_000));
        assert_eq!(sink.audio, Some(64_000));
        assert_eq!(sink.peer, Some(1_264_000));
    }

    #[test]
    fn a_readback_that_disagrees_is_not_success_and_the_previous_limit_returns() {
        let previous = Limits {
            video_bps: Some(2_500_000),
            audio_bps: None,
            peer_bps: Some(2_500_000),
        };
        let mut sink = FakeSink::with_video(2_500_000, None, 2_500_000);
        // The apply call succeeds but reports another maximum: the sender did
        // not really take the requested limit.
        sink.video_answer(Ok(Some(2_500_000)));
        // The restore of the previous video limit succeeds.
        sink.video_answer(Ok(Some(2_500_000)));
        let failure = apply_verified(&mut sink, target(1_200_000, None), previous)
            .expect_err("a mismatched readback is a failure");
        assert!(failure.restored, "{failure:?}");
        assert!(!failure.partial, "{failure:?}");
        assert!(
            failure.message.contains("подтвердил 2500000"),
            "the readback is named: {}",
            failure.message
        );
        assert_eq!(sink.video, Some(2_500_000), "the previous limit is back");
    }

    #[test]
    fn an_audio_failure_restores_the_applied_video_and_is_not_partial() {
        let previous = Limits {
            video_bps: Some(2_500_000),
            audio_bps: Some(128_000),
            peer_bps: Some(2_628_000),
        };
        let mut sink = FakeSink::with_video(2_500_000, Some(128_000), 2_628_000);
        // Video accepts the new limit.
        sink.video_answer(Ok(Some(1_200_000)));
        // Audio refuses it.
        sink.audio_answer(Err("аудиокодек отказал".into()));
        // The restore of the video limit succeeds; audio never changed.
        sink.video_answer(Ok(Some(2_500_000)));
        let failure = apply_verified(&mut sink, target(1_200_000, Some(64_000)), previous)
            .expect_err("the audio stage refuses the change");
        assert!(failure.restored, "{failure:?}");
        assert!(!failure.partial, "{failure:?}");
        assert!(
            failure.message.contains("аудио") && !failure.message.contains("часть лимитов"),
            "a clean refusal names the stage: {}",
            failure.message
        );
        assert_eq!(sink.video, Some(2_500_000));
        assert_eq!(sink.audio, Some(128_000));
        assert_eq!(sink.peer, Some(2_628_000));
    }

    #[test]
    fn a_failed_restore_is_reported_as_a_partial_change() {
        let previous = Limits {
            video_bps: Some(2_500_000),
            audio_bps: Some(128_000),
            peer_bps: Some(2_628_000),
        };
        let mut sink = FakeSink::with_video(2_500_000, Some(128_000), 2_628_000);
        sink.video_answer(Ok(Some(1_200_000)));
        sink.audio_answer(Err("аудиокодек отказал".into()));
        // Restoring the video limit also fails.
        sink.video_answer(Err("видеокодек отказал".into()));
        let failure = apply_verified(&mut sink, target(1_200_000, Some(64_000)), previous)
            .expect_err("the audio stage refuses the change");
        assert!(!failure.restored, "{failure:?}");
        assert!(failure.partial, "{failure:?}");
        assert!(
            failure.message.contains("часть лимитов изменена"),
            "a partial change must say so: {}",
            failure.message
        );
    }

    #[test]
    fn a_failed_peer_restores_both_senders() {
        let previous = Limits {
            video_bps: Some(2_500_000),
            audio_bps: Some(128_000),
            peer_bps: Some(2_628_000),
        };
        let mut sink = FakeSink::with_video(2_500_000, Some(128_000), 2_628_000);
        sink.video_answer(Ok(Some(1_200_000)));
        sink.audio_answer(Ok(Some(64_000)));
        sink.peer_answer(Err("соединение отказало".into()));
        sink.peer_answer(Ok(()));
        sink.audio_answer(Ok(Some(128_000)));
        sink.video_answer(Ok(Some(2_500_000)));
        let failure = apply_verified(&mut sink, target(1_200_000, Some(64_000)), previous)
            .expect_err("the peer stage refuses the change");
        assert!(failure.restored, "{failure:?}");
        assert!(!failure.partial, "{failure:?}");
        assert_eq!(sink.video, Some(2_500_000));
        assert_eq!(sink.audio, Some(128_000));
        assert_eq!(sink.peer, Some(2_628_000));
    }

    #[test]
    fn a_reoffer_uses_only_a_confirmed_quality() {
        let current = Quality {
            bitrate_kbps: 2500,
            ..Quality::default()
        };
        let requested = Quality {
            bitrate_kbps: 1200,
            ..current
        };
        assert_eq!(
            confirmed_quality(current, requested, true),
            requested,
            "an applied change becomes the re-offer configuration"
        );
        assert_eq!(
            confirmed_quality(current, requested, false),
            current,
            "a refused change keeps the previous limit for the re-offer"
        );
    }

    #[test]
    fn the_target_clamps_the_web_ranges_and_sums_the_peer_limit() {
        let quality = Quality {
            bitrate_kbps: 40_000,
            audio_bitrate_kbps: 900,
            ..Quality::default()
        };
        assert_eq!(
            Target::from_quality(quality, true),
            Target {
                video_bps: 16_000_000,
                audio_bps: Some(510_000),
                peer_bps: 16_510_000,
            }
        );
        assert_eq!(
            Target::from_quality(quality, false),
            Target {
                video_bps: 16_000_000,
                audio_bps: None,
                peer_bps: 16_000_000,
            }
        );
    }
}
