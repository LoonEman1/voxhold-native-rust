//! C07 regressions: the viewer decodes by its own receiver capabilities,
//! never by the local publisher mode. The catalogue is checked before
//! `stream.watch`, the offer profile after it.

use super::*;
use crate::publisher::Codec;

/// Receiver capabilities of a test build: only the named decoders exist, and
/// no encoder at all, so viewing can never be confused with a local encoder
/// mode (CPU or GPU).
fn receiver_capabilities(codecs: &[Codec]) -> CodecSnapshot {
    use crate::publisher::VideoCodecCapability;
    let receiver = codecs
        .iter()
        .map(|codec| {
            let fmtp = match codec {
                Codec::H264 => Some("packetization-mode=1;profile-level-id=42e01f"),
                Codec::Vp8 => None,
                Codec::Vp9 => Some("profile-id=0"),
                Codec::Av1 => Some("profile=0"),
            };
            VideoCodecCapability::new(*codec, 90_000, fmtp)
        })
        .collect();
    CodecSnapshot::receiver_only(receiver)
}

/// A browser/SFU offer for one video codec of an SDR rendition, plus its RTX
/// pair. `fmtp` is the exact profile line the publisher would carry.
fn offer(codec: Codec, fmtp: Option<&str>) -> String {
    let name = match codec {
        Codec::H264 => "H264",
        Codec::Vp8 => "VP8",
        Codec::Vp9 => "VP9",
        Codec::Av1 => "AV1",
    };
    let mut sdp = format!(
        "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96 97\r\na=mid:0\r\na=sendonly\r\n\
a=rtpmap:96 {name}/90000\r\n"
    );
    if let Some(fmtp) = fmtp {
        sdp.push_str(&format!("a=fmtp:96 {fmtp}\r\n"));
    }
    sdp.push_str("a=rtpmap:97 rtx/90000\r\na=fmtp:97 apt=96\r\n");
    sdp
}

/// The profile the SFU puts in the SDR rendition of each codec.
fn sdr_fmtp(codec: Codec) -> Option<&'static str> {
    match codec {
        Codec::H264 => {
            Some("level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f")
        }
        Codec::Vp8 => None,
        Codec::Vp9 => Some("profile-id=0"),
        Codec::Av1 => Some("profile=0"),
    }
}

/// Enter the room and watch one publication: the snapshot is catalogued, the
/// join is confirmed, and the selection is the only path that sends
/// `stream.watch`.
fn watch(
    viewer: &mut Viewer,
    publication: &Publication,
    selection: u64,
    now: Instant,
) -> Vec<Outcome> {
    snapshot(viewer, std::slice::from_ref(publication), now);
    viewer.set_joined(true, now);
    viewer.select(publication, selection, now)
}

/// C07: the catalogue names the codec before `stream.watch`. A build without
/// its decoder refuses the selection without opening a subscription, and the
/// next publication is still selectable.
#[test]
fn every_codec_without_its_decoder_is_refused_before_the_watch() {
    let now = Instant::now();
    for missing in Codec::ALL {
        let supported: Vec<Codec> = Codec::ALL
            .into_iter()
            .filter(|codec| *codec != missing)
            .collect();
        let fallback = supported[0];
        let mut unavailable = publication("missing", 1);
        unavailable.codec = missing.wire().into();
        let mut available = publication("available", 2);
        available.codec = fallback.wire().into();

        let mut viewer = Viewer::with_snapshot(1, 10, receiver_capabilities(&supported));
        snapshot(&mut viewer, &[unavailable.clone(), available.clone()], now);
        viewer.set_joined(true, now);
        let outcomes = viewer.select(&unavailable, 1, now);

        assert!(
            !outcomes
                .iter()
                .any(|outcome| matches!(outcome, Outcome::Start { .. })),
            "no stream.watch for a missing {missing:?} decoder: {outcomes:?}"
        );
        let message = outcomes
            .iter()
            .find_map(|outcome| match outcome {
                Outcome::Failed { message, .. } => Some(message.clone()),
                _ => None,
            })
            .expect("the refusal is visible");
        assert!(
            message.contains(missing.label()),
            "the reason names {missing:?}: {message}"
        );
        assert!(viewer.watching().is_none());
        assert!(
            viewer.intent().is_none(),
            "the refused intent is cleared for {missing:?}"
        );

        // The next, decodable publication starts normally.
        let started = viewer.select(&available, 2, now);
        start_of(&started);
        assert_eq!(
            viewer
                .watching()
                .expect("the next publication is watched")
                .publication
                .publisher_connection_id,
            "available"
        );
    }
}

/// C07: an offer first names the real profile. A rendition this build cannot
/// decode releases the subscription through the ordinary `stream.leave` and
/// reports one clear reason instead of a black picture.
#[test]
fn an_offer_without_a_decoder_releases_the_watch_with_stream_leave() {
    let now = Instant::now();
    for missing in Codec::ALL {
        let supported: Vec<Codec> = Codec::ALL
            .into_iter()
            .filter(|codec| *codec != missing)
            .collect();
        let mut unavailable = publication("a", 1);
        // An older/unknown catalogue value does not decide; the offer does.
        unavailable.codec = String::new();
        let mut other = publication("b", 2);
        other.codec = supported[0].wire().into();

        let mut viewer = Viewer::with_snapshot(1, 10, receiver_capabilities(&supported));
        snapshot(&mut viewer, &[unavailable.clone(), other.clone()], now);
        viewer.set_joined(true, now);
        start_of(&viewer.select(&unavailable, 1, now));

        let outcomes = viewer.on_server_event(
            "stream.webrtc_offer",
            "",
            &json!({"sdp": offer(missing, sdr_fmtp(missing))}),
            now,
        );
        assert!(
            !outcomes
                .iter()
                .any(|outcome| matches!(outcome, Outcome::Offer(_))),
            "an undecodable {missing:?} offer must not reach the engine: {outcomes:?}"
        );
        assert_eq!(
            sends(&outcomes, "stream.leave").len(),
            1,
            "the subscription is released once: {outcomes:?}"
        );
        let message = outcomes
            .iter()
            .find_map(|outcome| match outcome {
                Outcome::Failed { message, .. } => Some(message.clone()),
                _ => None,
            })
            .expect("the failure is visible");
        assert!(
            message.contains(missing.label()),
            "the reason names {missing:?}: {message}"
        );
        assert!(viewer.watching().is_none());
        assert!(viewer.intent().is_none());

        // The released subscription leaves the next publication selectable;
        // it starts once the outstanding `stream.leave` is acknowledged.
        let leave = sends(&outcomes, "stream.leave")
            .into_iter()
            .next()
            .expect("the release is tagged");
        let selecting = viewer.select(&other, 2, now);
        assert!(
            !selecting
                .iter()
                .any(|outcome| matches!(outcome, Outcome::Start { .. })),
            "a second subscription waits for the leave ack: {selecting:?}"
        );
        let started = viewer.on_server_event("stream.left", &leave, &json!({}), now);
        start_of(&started);
        assert_eq!(
            viewer
                .watching()
                .expect("the next publication is watched")
                .publication
                .publisher_connection_id,
            "b"
        );
    }
}

/// C07: the codec may be decodable while the profile in the offer is not.
#[test]
fn an_offer_profile_the_decoder_lacks_releases_the_watch() {
    let now = Instant::now();
    let mut publication = publication("a", 1);
    publication.codec = "vp9".into();
    let mut viewer = Viewer::with_snapshot(1, 10, receiver_capabilities(&[Codec::Vp9]));
    start_of(&watch(&mut viewer, &publication, 1, now));

    let outcomes = viewer.on_server_event(
        "stream.webrtc_offer",
        "",
        &json!({"sdp": offer(Codec::Vp9, Some("profile-id=2"))}),
        now,
    );
    assert!(
        !outcomes
            .iter()
            .any(|outcome| matches!(outcome, Outcome::Offer(_))),
        "an HDR profile is not an SDR stream: {outcomes:?}"
    );
    assert_eq!(sends(&outcomes, "stream.leave").len(), 1);
    let message = outcomes
        .iter()
        .find_map(|outcome| match outcome {
            Outcome::Failed { message, .. } => Some(message.clone()),
            _ => None,
        })
        .expect("the refusal is visible");
    assert!(
        message.contains("профиль") && message.contains("profile-id=2"),
        "the reason names the profile: {message}"
    );
}

/// C07: VP9 and AV1 watch normally without any local encoder, because the
/// viewer asks only the receiver capabilities. A hardware/CPU publishing mode
/// never enters this decision.
#[test]
fn vp9_and_av1_watch_without_any_local_encoder() {
    let now = Instant::now();
    for codec in [Codec::Vp9, Codec::Av1] {
        let mut publication = publication("a", 1);
        publication.codec = codec.wire().into();
        let snapshot_value = receiver_capabilities(&[codec]);
        // The test matrix really has no encoder on either side of the local
        // build; a viewer that asked `CodecRole::Sender` would fail here.
        assert!(
            snapshot_value
                .sender()
                .iter()
                .all(|entry| !entry.available())
        );
        assert!(snapshot_value.supports(crate::publisher::CodecRole::Receiver, codec));

        let mut viewer = Viewer::with_snapshot(1, 10, snapshot_value);
        start_of(&watch(&mut viewer, &publication, 1, now));

        let sdp = offer(codec, sdr_fmtp(codec));
        let outcomes = viewer.on_server_event("stream.webrtc_offer", "", &json!({"sdp": sdp}), now);
        assert!(
            matches!(outcomes.as_slice(), [Outcome::Offer(received)] if received == &sdp),
            "{codec:?} must reach the engine: {outcomes:?}"
        );
        assert!(
            sends(&outcomes, "stream.leave").is_empty(),
            "a decodable {codec:?} keeps its subscription: {outcomes:?}"
        );
        assert!(viewer.watching().is_some());
    }
}

/// C07: an audio m-line and a rejected video m-line belong to other tracks:
/// neither can turn a valid SDR video offer into a refusal.
#[test]
fn a_rejected_video_line_and_the_audio_track_do_not_affect_the_view() {
    let now = Instant::now();
    let mut publication = publication("a", 1);
    publication.codec = "vp8".into();
    let mut viewer = Viewer::with_snapshot(1, 10, receiver_capabilities(&[Codec::Vp8]));
    start_of(&watch(&mut viewer, &publication, 1, now));

    let sdp = format!(
        "v=0\r\nm=video 0 UDP/TLS/RTP/SAVPF 90\r\na=mid:old\r\na=inactive\r\n\
{}m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:audio\r\na=sendonly\r\n\
a=rtpmap:111 opus/48000/2\r\n",
        offer(Codec::Vp8, None)
    );
    let outcomes = viewer.on_server_event("stream.webrtc_offer", "", &json!({"sdp": sdp}), now);
    assert!(
        matches!(outcomes.as_slice(), [Outcome::Offer(_)]),
        "the live video line decides alone: {outcomes:?}"
    );
}
