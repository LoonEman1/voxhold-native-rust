//! C01/C02: the fake sender/receiver capability matrix and the FMTP fixtures
//! of the four SDR codecs. These tests never open a peer connection.

use super::*;

const H264_SDR: &str = "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f";
const H264_LEGACY: &str = "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42001f";
const H264_MAIN: &str = "packetization-mode=1;profile-level-id=4d001f";

fn capability(codec: Codec) -> VideoCodecCapability {
    match codec {
        Codec::H264 => VideoCodecCapability::new(codec, 90_000, Some(H264_SDR)),
        Codec::Vp8 => VideoCodecCapability::new(codec, 90_000, None),
        Codec::Vp9 => VideoCodecCapability::new(codec, 90_000, Some("profile-id=0")),
        Codec::Av1 => VideoCodecCapability::new(codec, 90_000, None),
    }
}

fn all_capabilities() -> Vec<VideoCodecCapability> {
    Codec::ALL.into_iter().map(capability).collect()
}

fn all_probe() -> CodecProbe {
    CodecProbe {
        encoders: Codec::ALL.to_vec(),
        sender: all_capabilities(),
        receiver: all_capabilities(),
    }
}

fn snapshot_of(probe: &CodecProbe) -> CodecSnapshot {
    CodecSnapshot::from_probe(42, probe)
}

fn reason(snapshot: &CodecSnapshot, role: CodecRole, codec: Codec) -> String {
    snapshot
        .resolve(CodecIntent::Explicit(codec), role)
        .expect_err("the codec must be refused")
        .reason()
        .to_owned()
}

#[test]
fn wire_values_follow_the_website_auto_order() {
    assert_eq!(
        Codec::ALL.map(Codec::wire),
        ["h264", "vp8", "vp9", "av1"],
        "the automatic order is H.264, VP8, VP9, AV1"
    );
    assert_eq!(Codec::ALL.map(Codec::label), ["H.264", "VP8", "VP9", "AV1"]);
    for codec in Codec::ALL {
        assert_eq!(Codec::parse(codec.wire()), Some(codec));
        assert_eq!(Codec::parse(&codec.wire().to_uppercase()), Some(codec));
        assert_eq!(
            Codec::parse(&format!("video/{}", codec.wire().to_uppercase())),
            Some(codec)
        );
    }
    assert_eq!(Codec::parse("h265"), None);
    assert_eq!(Codec::parse("video/rtx"), None);
    assert_eq!(CodecIntent::default(), CodecIntent::Auto);
    assert_eq!(CodecIntent::Auto.label(), "Автоматически");
    assert_eq!(CodecIntent::Explicit(Codec::Av1).label(), "AV1");
}

#[test]
fn all_four_codecs_are_available_and_auto_picks_h264_first() {
    let snapshot = snapshot_of(&all_probe());
    assert_eq!(snapshot.generation(), 42);
    assert_eq!(snapshot.sender().len(), 4);
    assert_eq!(snapshot.receiver().len(), 4);
    for codec in Codec::ALL {
        assert!(snapshot.supports(CodecRole::Sender, codec), "{codec:?}");
        assert!(snapshot.supports(CodecRole::Receiver, codec), "{codec:?}");
        assert_eq!(
            snapshot.resolve(CodecIntent::Explicit(codec), CodecRole::Sender),
            Ok(codec)
        );
        assert_eq!(
            snapshot.resolve(CodecIntent::Explicit(codec), CodecRole::Receiver),
            Ok(codec)
        );
    }
    assert_eq!(
        snapshot.resolve(CodecIntent::Auto, CodecRole::Sender),
        Ok(Codec::H264)
    );
    assert_eq!(
        snapshot.resolve(CodecIntent::Auto, CodecRole::Receiver),
        Ok(Codec::H264)
    );
}

#[test]
fn auto_skips_unavailable_codecs_in_web_order() {
    // Only VP8 is present on either side.
    let vp8_only = CodecProbe {
        encoders: vec![Codec::Vp8],
        sender: vec![capability(Codec::Vp8)],
        receiver: vec![capability(Codec::Vp8)],
    };
    let snapshot = snapshot_of(&vp8_only);
    assert_eq!(snapshot.auto(CodecRole::Sender), Some(Codec::Vp8));
    assert_eq!(snapshot.auto(CodecRole::Receiver), Some(Codec::Vp8));
    assert_eq!(
        snapshot.resolve(CodecIntent::Auto, CodecRole::Sender),
        Ok(Codec::Vp8)
    );
    assert_eq!(
        snapshot.resolve(CodecIntent::Auto, CodecRole::Receiver),
        Ok(Codec::Vp8)
    );
    let message = reason(&snapshot, CodecRole::Sender, Codec::H264);
    assert!(message.contains("H.264"), "{message}");

    // VP9 before AV1 when H.264 is absent.
    let vp9_av1 = CodecProbe {
        encoders: vec![Codec::Vp9, Codec::Av1],
        sender: vec![capability(Codec::Vp9), capability(Codec::Av1)],
        receiver: vec![capability(Codec::Vp9), capability(Codec::Av1)],
    };
    assert_eq!(
        snapshot_of(&vp9_av1).resolve(CodecIntent::Auto, CodecRole::Sender),
        Ok(Codec::Vp9)
    );
}

#[test]
fn an_empty_sender_refuses_auto_and_every_explicit_codec() {
    let empty = CodecProbe {
        encoders: Vec::new(),
        sender: Vec::new(),
        receiver: all_capabilities(),
    };
    let snapshot = snapshot_of(&empty);
    assert!(!snapshot.sender_available());
    let error = snapshot
        .resolve(CodecIntent::Auto, CodecRole::Sender)
        .expect_err("an empty sender cannot publish");
    assert_eq!(error.codec(), None);
    assert!(
        error.reason().contains("нет доступного"),
        "{}",
        error.reason()
    );
    for codec in Codec::ALL {
        let error = snapshot
            .resolve(CodecIntent::Explicit(codec), CodecRole::Sender)
            .expect_err("no encoder exists");
        assert_eq!(error.codec(), Some(codec));
        assert!(error.reason().contains(codec.label()), "{}", error.reason());
    }
    // Watching is independent: the same snapshot keeps its decoder side.
    assert_eq!(
        snapshot.resolve(CodecIntent::Auto, CodecRole::Receiver),
        Ok(Codec::H264)
    );
}

#[test]
fn publication_depends_on_the_encoder_and_viewing_on_the_decoder() {
    let receive_only_av1 = CodecProbe {
        encoders: vec![Codec::Vp8],
        sender: vec![capability(Codec::Vp8)],
        receiver: vec![capability(Codec::Vp8), capability(Codec::Av1)],
    };
    let snapshot = snapshot_of(&receive_only_av1);
    // AV1 is advertised by the decoder but no encoder exists.
    assert!(!snapshot.supports(CodecRole::Sender, Codec::Av1));
    assert!(snapshot.supports(CodecRole::Receiver, Codec::Av1));
    assert_eq!(
        snapshot.resolve(CodecIntent::Explicit(Codec::Av1), CodecRole::Receiver),
        Ok(Codec::Av1)
    );
    let message = reason(&snapshot, CodecRole::Sender, Codec::Av1);
    assert!(message.contains("Кодировщик AV1"), "{message}");

    // The reverse: an encoder without a matching decoder is publication only.
    let send_only_h264 = CodecProbe {
        encoders: Codec::ALL.to_vec(),
        sender: all_capabilities(),
        receiver: vec![capability(Codec::Vp8)],
    };
    let snapshot = snapshot_of(&send_only_h264);
    assert!(snapshot.supports(CodecRole::Sender, Codec::H264));
    assert!(!snapshot.supports(CodecRole::Receiver, Codec::H264));
    assert_eq!(
        snapshot.resolve(CodecIntent::Explicit(Codec::H264), CodecRole::Sender),
        Ok(Codec::H264)
    );
    let message = reason(&snapshot, CodecRole::Receiver, Codec::H264);
    assert!(message.contains("Декодер H.264"), "{message}");
}

#[test]
fn an_incompatible_sender_profile_blocks_only_that_codec() {
    let probe = CodecProbe {
        encoders: Codec::ALL.to_vec(),
        sender: vec![
            // H.264 Main is not SDR; legacy Baseline is still acceptable.
            VideoCodecCapability::new(Codec::H264, 90_000, Some(H264_MAIN)),
            VideoCodecCapability::new(Codec::H264, 90_000, Some(H264_LEGACY)),
            capability(Codec::Vp8),
            // VP9 profile 2 is HDR, not SDR.
            VideoCodecCapability::new(Codec::Vp9, 90_000, Some("profile-id=2")),
            capability(Codec::Av1),
        ],
        receiver: all_capabilities(),
    };
    let snapshot = snapshot_of(&probe);
    assert!(snapshot.supports(CodecRole::Sender, Codec::H264));
    assert_eq!(
        snapshot
            .availability(CodecRole::Sender, Codec::H264)
            .profile(),
        Some(SdrProfile::H264 {
            baseline: H264Baseline::Legacy,
            level: 31
        })
    );
    assert!(!snapshot.supports(CodecRole::Sender, Codec::Vp9));
    let message = reason(&snapshot, CodecRole::Sender, Codec::Vp9);
    assert!(message.contains("profile-id=2"), "{message}");
    // H.264 still wins Auto even though VP9 is unavailable.
    assert_eq!(
        snapshot.resolve(CodecIntent::Auto, CodecRole::Sender),
        Ok(Codec::H264)
    );
}

#[test]
fn constrained_baseline_wins_over_the_legacy_h264_payload() {
    let probe = CodecProbe {
        encoders: vec![Codec::H264],
        sender: vec![
            VideoCodecCapability::new(Codec::H264, 90_000, Some(H264_LEGACY)),
            VideoCodecCapability::new(Codec::H264, 90_000, Some(H264_SDR)),
        ],
        receiver: vec![
            VideoCodecCapability::new(Codec::H264, 90_000, Some(H264_LEGACY)),
            VideoCodecCapability::new(Codec::H264, 90_000, Some(H264_SDR)),
        ],
    };
    let snapshot = snapshot_of(&probe);
    for role in [CodecRole::Sender, CodecRole::Receiver] {
        assert_eq!(
            snapshot.availability(role, Codec::H264).profile(),
            Some(SdrProfile::H264 {
                baseline: H264Baseline::Constrained,
                level: 31
            })
        );
    }
}

#[test]
fn a_nonstandard_video_clock_is_not_the_sdr_payload() {
    let probe = CodecProbe {
        encoders: vec![Codec::Vp8],
        sender: vec![VideoCodecCapability::new(Codec::Vp8, 48_000, None)],
        receiver: vec![capability(Codec::Vp8)],
    };
    let snapshot = snapshot_of(&probe);
    assert!(!snapshot.supports(CodecRole::Sender, Codec::Vp8));
    let message = reason(&snapshot, CodecRole::Sender, Codec::Vp8);
    assert!(message.contains("48000"), "{message}");
}

#[test]
fn h264_fmtp_ignores_case_order_spaces_and_the_level_byte() {
    let fixtures = [
        (
            "profile-level-id=42e01f;packetization-mode=1",
            "PACKETIZATION-MODE = 1 ; PROFILE-LEVEL-ID = 42E033",
            true,
            "the level byte may differ on the receiving side",
        ),
        (
            "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f",
            "profile-level-id=42e01f;level-asymmetry-allowed=1;packetization-mode=1",
            true,
            "parameter order does not matter",
        ),
        (
            "profile-level-id=42001f;packetization-mode=1",
            "profile-level-id=420033;packetization-mode=1",
            true,
            "legacy Baseline is a separate but compatible SDR variant",
        ),
        (
            "profile-level-id=42e01f;packetization-mode=1",
            "profile-level-id=42001f;packetization-mode=1",
            false,
            "Constrained Baseline and legacy Baseline stay separate profiles",
        ),
        (
            "profile-level-id=42e01f",
            "profile-level-id=42e01f;packetization-mode=1",
            false,
            "a missing packetization mode means 0 and is not SDR",
        ),
        (
            "profile-level-id=42e01f;packetization-mode=0",
            "profile-level-id=42e01f;packetization-mode=1",
            false,
            "mode 0 is not packetization-mode=1",
        ),
        (
            "profile-level-id=42e0zz;packetization-mode=1",
            "profile-level-id=42e01f;packetization-mode=1",
            false,
            "a damaged level byte must be refused",
        ),
        (
            H264_MAIN,
            "profile-level-id=4d0033;packetization-mode=1",
            false,
            "Main is not an SDR web profile",
        ),
        (
            "packetization-mode=1",
            "profile-level-id=42e01f;packetization-mode=1",
            true,
            "the backend default profile-level-id is 42e01f",
        ),
    ];
    for (first, second, expected, why) in fixtures {
        assert_eq!(
            sdr_compatible(Codec::H264, Some(first), Some(second)),
            expected,
            "{why}: {first} vs {second}"
        );
    }
    // The SFU-negotiated fmtp never turns into the other H.264 profile.
    assert_eq!(
        sdr_profile(
            Codec::H264,
            Some("packetization-mode=1;profile-level-id=42001f")
        ),
        Ok(SdrProfile::H264 {
            baseline: H264Baseline::Legacy,
            level: 31
        })
    );
    assert_eq!(
        sdr_profile(
            Codec::H264,
            Some("profile-level-id=42e01f;packetization-mode=1")
        ),
        Ok(SdrProfile::H264 {
            baseline: H264Baseline::Constrained,
            level: 31
        })
    );
    assert!(matches!(
        sdr_profile(
            Codec::H264,
            Some("packetization-mode=1;profile-level-id=42e0")
        ),
        Err(ProfileRejection::H264Malformed(_))
    ));
}

#[test]
fn vp8_accepts_receiver_limits_and_requires_the_standard_clock() {
    assert_eq!(sdr_profile(Codec::Vp8, None), Ok(SdrProfile::Vp8));
    assert!(sdr_compatible(
        Codec::Vp8,
        None,
        Some("max-fs=12288;max-fr=60")
    ));
    assert!(sdr_compatible(
        Codec::Vp8,
        Some("max-fs=3600; max-fr=30"),
        Some("MAX-FS = 12288; MAX-FR = 60")
    ));
}

#[test]
fn vp9_defaults_to_profile_zero_and_rejects_profile_two() {
    assert_eq!(sdr_profile(Codec::Vp9, None), Ok(SdrProfile::Vp9));
    assert_eq!(sdr_profile(Codec::Vp9, Some("")), Ok(SdrProfile::Vp9));
    assert_eq!(
        sdr_profile(Codec::Vp9, Some("PROFILE-ID = 0")),
        Ok(SdrProfile::Vp9)
    );
    assert!(sdr_compatible(
        Codec::Vp9,
        Some("max-fs=12288;max-fr=60"),
        Some("profile-id=0")
    ));
    assert!(matches!(
        sdr_profile(Codec::Vp9, Some("profile-id=2")),
        Err(ProfileRejection::Vp9Profile(profile)) if profile == "2"
    ));
    assert!(!sdr_compatible(Codec::Vp9, None, Some("profile-id=2")));
    assert!(!sdr_compatible(
        Codec::Vp9,
        Some("profile-id=2"),
        Some("profile-id=2")
    ));
}

#[test]
fn av1_main_sdr_ignores_the_level_index() {
    assert_eq!(sdr_profile(Codec::Av1, None), Ok(SdrProfile::Av1));
    assert_eq!(sdr_profile(Codec::Av1, Some("")), Ok(SdrProfile::Av1));
    assert!(sdr_compatible(
        Codec::Av1,
        None,
        Some("level-idx=5;profile=0;tier=0")
    ));
    assert!(sdr_compatible(
        Codec::Av1,
        Some("tier=0; PROFILE = 0 ; level-idx=5"),
        Some("profile=0;level-idx=9")
    ));
    assert!(matches!(
        sdr_profile(Codec::Av1, Some("profile=1")),
        Err(ProfileRejection::Av1Profile(profile)) if profile == "1"
    ));
    assert!(!sdr_compatible(Codec::Av1, None, Some("profile=1")));
}

#[test]
fn the_probe_of_this_build_reports_the_pinned_web_codecs() {
    // `installed_webrtc_build_has_voxhold_web_codecs` already proves the names
    // in the build. This test proves the model maps that build into the four
    // SDR entries without weakening the existing check: the real probe is used
    // and every codec must be usable on both sides.
    let probe = CodecProbe::probe();
    let snapshot = CodecSnapshot::from_probe(1, &probe);
    for codec in Codec::ALL {
        assert!(
            snapshot.supports(CodecRole::Sender, codec),
            "no SDR sender entry for {codec:?}: {:?}",
            snapshot.availability(CodecRole::Sender, codec)
        );
        assert!(
            snapshot.supports(CodecRole::Receiver, codec),
            "no SDR receiver entry for {codec:?}: {:?}",
            snapshot.availability(CodecRole::Receiver, codec)
        );
    }
    assert_eq!(
        snapshot.resolve(CodecIntent::Auto, CodecRole::Sender),
        Ok(Codec::H264)
    );
    assert!(CodecSnapshot::current().generation() >= 1);
}
