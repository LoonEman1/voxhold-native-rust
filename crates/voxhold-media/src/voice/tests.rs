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
#[cfg(windows)]
#[ignore = "opens the Windows audio device; run manually"]
fn switching_back_to_software_stops_the_adm_recording() {
    let capture = Arc::new(CaptureGate::default());
    let events = Arc::new(Events::default());
    let mut session = Session::new(
        Vec::new(),
        "all",
        Controls::default(),
        &events,
        capture,
        None,
    )
    .expect("default microphone");
    session
        .install_platform_input(None, &events)
        .expect("platform ADM");
    assert!(session.audio.running(true));
    session
        .install_software_input(None, &events)
        .expect("software capture");
    assert!(
        !session.audio.running(true),
        "software capture must be the only microphone owner"
    );
}

#[test]
#[cfg(windows)]
#[ignore = "opens the Windows audio device; run manually"]
fn a_failed_software_capture_recovers_on_the_system_input() {
    let capture = Arc::new(CaptureGate::default());
    let events = Arc::new(Events::default());
    let session = Session::new(
        Vec::new(),
        "all",
        Controls::default(),
        &events,
        capture,
        None,
    )
    .expect("default microphone");
    let mut session = session;
    session.mic.as_ref().expect("software capture").test_fail();
    session.device_check = std::time::Instant::now() - Duration::from_secs(3);
    session.check_devices(&events);
    assert!(
        session.mic.as_ref().is_some_and(|mic| !mic.failed()),
        "the capture must be restarted instead of staying dead"
    );
    let mut saw = false;
    while let Some(event) = events.poll(Duration::ZERO) {
        if matches!(event, Event::DeviceError { .. }) {
            saw = true;
        }
    }
    assert!(saw, "the recovery must be reported to the UI");
}

#[test]
#[cfg(windows)]
#[ignore = "opens the Windows audio device; run manually"]
fn an_explicit_missing_microphone_retry_keeps_the_system_input() {
    let capture = Arc::new(CaptureGate::default());
    let events = Arc::new(Events::default());
    let controls = Controls {
        input: Some("9999:Definitely Missing".into()),
        ..Controls::default()
    };
    let mut session = Session::new(Vec::new(), "all", controls, &events, capture, None)
        .expect("the call must start on the system input");
    assert!(session.controls.input.is_none());
    session
        .switch_input(Some("9999:Definitely Missing".into()), true, &events)
        .expect("an explicit retry must fall back, not fail the session");
    assert!(session.controls.input.is_none());
    let mut saw = false;
    while let Some(event) = events.poll(Duration::ZERO) {
        if matches!(event, Event::DeviceError { .. }) {
            saw = true;
        }
    }
    assert!(saw, "the repeated selection must report the fallback");
}

#[test]
#[cfg(windows)]
#[ignore = "opens the Windows audio device; run manually"]
fn platform_adm_and_the_software_source_coexist() {
    use libwebrtc::audio_frame::AudioFrame;

    let audio = PlatformAudio::new(false).expect("platform ADM");
    let source = libwebrtc::audio_source::native::NativeAudioSource::new(
        capture::source_options(),
        capture::SAMPLE_RATE,
        1,
        0,
    );
    let track = audio.0.create_audio_track("coexist", source.clone());
    assert!(track.enabled());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let block = vec![0_i16; (capture::SAMPLE_RATE / 100) as usize];
    for _ in 0..10 {
        let frame = AudioFrame {
            data: std::borrow::Cow::Borrowed(&block),
            sample_rate: capture::SAMPLE_RATE,
            num_channels: 1,
            samples_per_channel: block.len() as u32,
        };
        runtime.block_on(source.capture_frame(&frame)).unwrap();
    }
}

#[test]
#[cfg(windows)]
#[ignore = "opens the Windows audio device; run manually"]
fn a_missing_saved_microphone_does_not_fail_the_join() {
    let capture = Arc::new(CaptureGate::default());
    let events = Arc::new(Events::default());
    let controls = Controls {
        input: Some("9999:Definitely Missing".into()),
        ..Controls::default()
    };
    let session = Session::new(Vec::new(), "all", controls, &events, capture, None)
        .expect("a stale saved microphone must not abort the join");
    assert!(
        session.controls.input.is_none(),
        "the dead saved id must be cleared after the fallback"
    );
    drop(session);
    let mut saw_device_error = false;
    let mut applied = None;
    while let Some(event) = events.poll(Duration::ZERO) {
        match event {
            Event::DeviceError { .. } => saw_device_error = true,
            Event::Audio {
                capture,
                noise_mode,
                echo_cancellation,
                auto_gain_control,
                ..
            } => {
                applied = Some((capture, noise_mode, echo_cancellation, auto_gain_control));
            }
            _ => {}
        }
    }
    assert!(saw_device_error, "the fallback must be reported");
    match applied {
        Some(("software", "auto", false, true)) | Some(("platform", "auto", false, true)) => {}
        other => panic!("unexpected fallback audio mode: {other:?}"),
    }
}

#[test]
fn a_software_track_waits_for_its_own_capture_acknowledgement() {
    let factory = PeerConnectionFactory::default();
    let source_a = libwebrtc::audio_source::native::NativeAudioSource::new(
        capture::source_options(),
        capture::SAMPLE_RATE,
        capture::CHANNELS,
        0,
    );
    let source_b = libwebrtc::audio_source::native::NativeAudioSource::new(
        capture::source_options(),
        capture::SAMPLE_RATE,
        capture::CHANNELS,
        0,
    );
    let track_a = factory.create_audio_track("ack-a", source_a.clone());
    let track_b = factory.create_audio_track("ack-b", source_b.clone());
    let gate = CaptureGate::default();
    gate.attach_source(track_a.clone(), source_a, 11);
    gate.set_joined(true);
    gate.set_server_allowed(true);
    gate.set_local(true);
    assert!(
        !track_a.enabled(),
        "an opened software track must stay silent until the capture thread flushes"
    );
    let generation = gate.permission_epoch().load(Ordering::SeqCst);
    // Capture B may start before its track replaces A in the sender.
    gate.acknowledge(12, generation);
    assert!(
        !track_a.enabled(),
        "a different capture instance must not acknowledge A's old queue"
    );
    gate.acknowledge(11, generation);
    assert!(
        track_a.enabled(),
        "the acknowledged generation may transmit"
    );
    // A later mute/unmute edge must close the track again before the next
    // acknowledgement, so an old block stuck in the sender cannot leak.
    gate.set_local(false);
    gate.set_local(true);
    assert!(
        !track_a.enabled(),
        "a new generation must wait for its own flush acknowledgement"
    );
    // Replacing the sender track resets the acknowledgement. A's late ack
    // must not enable B, and B must acknowledge its own generation.
    gate.attach_source(track_b.clone(), source_b, 12);
    let generation = gate.permission_epoch().load(Ordering::SeqCst);
    gate.acknowledge(11, generation);
    assert!(
        !track_b.enabled(),
        "the old capture cannot enable the new track"
    );
    gate.acknowledge(12, generation);
    assert!(track_b.enabled());
}

#[test]
fn platform_watchdog_requires_pcm_not_just_initialization() {
    assert!(!output::platform_input_stalled(
        true,
        true,
        true,
        Duration::from_millis(100),
        Duration::from_secs(30)
    ));
    assert!(
        output::platform_input_stalled(
            true,
            true,
            true,
            Duration::from_secs(3),
            Duration::from_secs(30)
        ),
        "an initialized but silent ADM is stale"
    );
    assert!(
        output::platform_input_stalled(
            true,
            true,
            true,
            Duration::from_secs(3),
            Duration::from_secs(30)
        ),
        "a stopped captured-duration counter must trigger recovery"
    );
    assert!(
        !output::platform_input_stalled(
            true,
            false,
            false,
            Duration::from_secs(30),
            Duration::from_secs(30)
        ),
        "no stats before the transport connects are not a stall"
    );
    assert!(!output::platform_input_stalled(
        true,
        true,
        false,
        Duration::from_secs(30),
        Duration::from_secs(1)
    ));
    assert!(output::platform_input_stalled(
        true,
        true,
        false,
        Duration::from_secs(30),
        INPUT_CAPTURE_GRACE
    ));
    assert!(output::platform_input_stalled(
        false,
        false,
        false,
        Duration::from_secs(30),
        Duration::from_secs(30)
    ));
}

#[test]
fn recovery_ladder_bounds_the_silent_system_input_before_the_adm() {
    assert_eq!(
        input::recovery_step(true, false, 0),
        input::RecoveryStep::ReopenSelected,
        "the selected device gets one reopen"
    );
    assert_eq!(
        input::recovery_step(true, true, 0),
        input::RecoveryStep::System,
        "a silent selected device falls through to the system input"
    );
    assert_eq!(
        input::recovery_step(true, true, MAX_SYSTEM_INPUT_ATTEMPTS),
        input::RecoveryStep::Platform,
        "a system device that opens but never delivers PCM must not loop forever"
    );
    assert_eq!(
        input::recovery_step(false, false, MAX_SYSTEM_INPUT_ATTEMPTS - 1),
        input::RecoveryStep::System,
        "the last system attempt is still used"
    );
    assert_eq!(
        input::recovery_step(false, false, MAX_SYSTEM_INPUT_ATTEMPTS),
        input::RecoveryStep::Platform
    );
}

#[test]
fn platform_device_names_are_matched_only_when_unique() {
    let unique = platform::unique_name_match(
        "USB Headset",
        [(0, "Speakers".into()), (1, "USB Headset".into())].into_iter(),
    );
    assert_eq!(unique, Some(1));
    let truncated = platform::unique_name_match(
        "USB Head",
        [(0, "Speakers".into()), (1, "USB Headset".into())].into_iter(),
    );
    assert_eq!(truncated, Some(1));
    let duplicate = platform::unique_name_match(
        "USB Mic",
        [(0, "USB Mic".into()), (1, "USB Mic".into())].into_iter(),
    );
    assert_eq!(duplicate, None);
}

#[test]
#[cfg(windows)]
#[ignore = "opens the Windows audio device; run manually"]
fn platform_adm_resolves_a_saved_endpoint_id() {
    let device = crate::devices::list_devices()
        .into_iter()
        .find(|device| device.input)
        .expect("an input device");
    let audio = PlatformAudio::new(false).expect("platform ADM");
    assert!(
        audio.resolve(&device.id, true).is_some(),
        "the selected WinMM endpoint must map to a Core Audio input: {}",
        device.id
    );
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
