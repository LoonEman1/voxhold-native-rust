use super::*;
use libwebrtc::{MediaType, rtp_sender::VideoEncoderBackend, video_codec::VideoCodec};

#[test]
fn installed_webrtc_build_has_voxhold_web_codecs() {
    let factory = PeerConnectionFactory::default();
    let video = factory.video_encoder_codecs(VideoEncoderBackend::Auto);
    let receive_video = factory.get_rtp_receiver_capabilities(MediaType::Video);
    for (codec, name) in [
        (VideoCodec::VP8, "VP8"),
        (VideoCodec::VP9, "VP9"),
        (VideoCodec::H264, "H264"),
        (VideoCodec::AV1, "AV1"),
    ] {
        assert!(video.contains(&codec), "no {name} encoder: {video:?}");
        let mime = format!("video/{name}");
        assert!(
            receive_video
                .codecs
                .iter()
                .any(|codec| codec.mime_type.eq_ignore_ascii_case(&mime)),
            "no {name} decoder"
        );
    }
    for capabilities in [
        factory.get_rtp_sender_capabilities(MediaType::Audio),
        factory.get_rtp_receiver_capabilities(MediaType::Audio),
    ] {
        assert!(capabilities.codecs.iter().any(|codec| {
            codec.mime_type.eq_ignore_ascii_case("audio/opus") && codec.clock_rate == Some(48_000)
        }));
    }
}

#[test]
fn trickle_ice_is_matched_to_the_offer_generation() {
    let mut candidate = Candidate {
        candidate: "candidate:1 1 udp 1 127.0.0.1 9999 typ host ufrag old".into(),
        sdp_mid: Some("0".into()),
        sdp_mline_index: Some(0),
        username_fragment: None,
    };
    let offer = RemoteSdp::new("v=0\r\na=ice-ufrag:new\r\n");
    assert!(!offer.accepts(&candidate));
    candidate.username_fragment = Some("new".into());
    assert!(offer.accepts(&candidate));
}

#[test]
fn a_failed_rollback_keeps_the_output_request_pending() {
    let mut output = OutputState::new(Some("A".into()));
    assert!(!output.needs_switch(&Some("A".into())));
    output.request(Some("B".into()));
    // B fails and the rollback to A also fails: nothing is applied.
    output.failed();
    assert!(output.needs_switch(&Some("B".into())));
    // Selecting A again must reach the engine even though the selected
    // preference previously said B.
    output.request(Some("A".into()));
    assert!(output.needs_switch(&Some("A".into())));
    output.confirmed(Some("A".into()));
    assert!(!output.needs_switch(&Some("A".into())));
}
