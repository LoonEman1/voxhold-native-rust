//! One outbound statistics sample of the publisher.
//!
//! `PublishStats` feeds the on-air card; the raw video part is kept so the
//! engine can confirm the negotiated codec from `OutboundRtpStats.stream
//! .codec_id` and the `RtcStats::Codec` it points at. Both come from the same
//! `get_stats()` call, so the confirmation never compares two different
//! moments. Nothing here waits longer than `STATS_TIMEOUT`.

use std::{collections::BTreeMap, sync::atomic::Ordering};

use libwebrtc::{peer_connection::PeerConnection, stats::RtcStats};

use super::{
    AudioCounters, AudioMeter, FrameCounters, PublishStats, QualityLimitation, STATS_TIMEOUT,
    negotiation::WireCodec, negotiation::WireVideo,
};

/// The codec dictionary entry the outbound stream points at.
struct CodecEntry {
    payload_type: u32,
    mime_type: String,
    fmtp: String,
}

/// The card values and the raw video stream of one poll.
pub(super) struct Sample {
    pub stats: PublishStats,
    pub video: Option<WireVideo>,
}

/// Does this outbound video stream carry the negotiated mid?
///
/// An empty negotiated mid names nothing: it must not match another stream
/// whose mid is also empty, otherwise a missing field would select an
/// unrelated video stream.
fn mid_matches(video_mid: Option<&str>, outbound_mid: &str) -> bool {
    video_mid.is_some_and(|mid| !mid.is_empty() && mid == outbound_mid)
}

/// Read one sample of the live senders. `video_mid` is the negotiated video
/// mid; the matching outbound stream wins over any other video stream of the
/// peer.
pub(super) async fn sample(
    peer: &PeerConnection,
    counters: &FrameCounters,
    audio_counters: &AudioCounters,
    audio_meter: &mut AudioMeter,
    audio_active: bool,
    video_mid: Option<&str>,
) -> Option<Sample> {
    let capture_width = counters.width.load(Ordering::Relaxed);
    let capture_height = counters.height.load(Ordering::Relaxed);
    let capture_fps = f32::from_bits(counters.fps_bits.load(Ordering::Relaxed));
    let dropped = counters.dropped.load(Ordering::Relaxed);
    let mut codecs: BTreeMap<String, CodecEntry> = BTreeMap::new();
    let (mut matched, mut fallback, mut audio) = (None, None, None);
    if let Ok(Ok(stats)) = tokio::time::timeout(STATS_TIMEOUT, peer.get_stats()).await {
        for stat in stats {
            match stat {
                RtcStats::Codec(codec) => {
                    codecs.insert(
                        codec.rtc.id,
                        CodecEntry {
                            payload_type: codec.codec.payload_type,
                            mime_type: codec.codec.mime_type,
                            fmtp: codec.codec.sdp_fmtp_line,
                        },
                    );
                }
                RtcStats::OutboundRtp(outbound) => match outbound.stream.kind.as_str() {
                    "video" => {
                        if mid_matches(video_mid, &outbound.outbound.mid) {
                            matched = Some(outbound);
                        } else {
                            fallback = Some(outbound);
                        }
                    }
                    "audio" => audio = Some(outbound),
                    _ => {}
                },
                _ => {}
            }
        }
    }
    let video = matched.or(fallback);
    let video_wire = video.as_ref().map(|outbound| WireVideo {
        mid: outbound.outbound.mid.clone(),
        frames_encoded: outbound.outbound.frames_encoded,
        codec: codecs
            .get(&outbound.stream.codec_id)
            .map(|codec| WireCodec {
                payload_type: codec.payload_type,
                mime_type: codec.mime_type.clone(),
                fmtp: codec.fmtp.clone(),
            }),
    });
    let mut audio_packets_sent = 0;
    let mut audio_kbps = 0.0;
    if let Some(outbound) = &audio {
        audio_packets_sent = outbound.sent.packets_sent;
        audio_meter.observe(outbound.sent.bytes_sent);
        audio_kbps = audio_meter.kbps();
    }
    let base = PublishStats {
        fps: capture_fps,
        dropped,
        width: capture_width,
        height: capture_height,
        sources: 1,
        limitation: QualityLimitation::None,
        audio_active,
        audio_kbps,
        audio_packets_sent,
        audio_dropped: audio_counters.dropped(),
        confirmed_codec: None,
    };
    let Some(outbound) = video else {
        return Some(Sample {
            stats: base,
            video: None,
        });
    };
    let stream = outbound.outbound;
    #[cfg(test)]
    crate::live_probe::record_outbound(
        stream.frames_encoded,
        outbound.sent.packets_sent,
        stream.frame_width,
        stream.frame_height,
        audio_packets_sent,
        audio_kbps,
    );
    Some(Sample {
        stats: PublishStats {
            fps: stream.frames_per_second as f32,
            width: if stream.frame_width > 0 {
                stream.frame_width
            } else {
                capture_width
            },
            height: if stream.frame_height > 0 {
                stream.frame_height
            } else {
                capture_height
            },
            limitation: QualityLimitation::from_reason(stream.quality_limitation_reason),
            ..base
        },
        video: video_wire,
    })
}

#[cfg(test)]
mod tests {
    use super::mid_matches;

    /// An empty negotiated mid names nothing: it must never match another
    /// stream with an empty mid, while a real mid still selects its stream.
    #[test]
    fn an_empty_negotiated_mid_never_matches_an_empty_stream_mid() {
        assert!(mid_matches(Some("0"), "0"));
        assert!(!mid_matches(Some("0"), "1"));
        assert!(!mid_matches(Some("0"), ""));
        assert!(!mid_matches(Some(""), ""));
        assert!(!mid_matches(Some(""), "0"));
        assert!(!mid_matches(None, ""));
        assert!(!mid_matches(None, "0"));
    }
}
