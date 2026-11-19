//! Live evidence through the real stand backend.
//!
//! `live_stand_publishes_a_monitor_and_a_viewer_decodes_frames` is `#[ignore]`d:
//! it needs the local Docker stand, credentials and a real desktop. Without the
//! credentials it prints a skip line instead of failing or pretending success.
//! Every join, recv and wait below has a deadline, so a broken stand fails with
//! the step that did not happen instead of hanging the test run.

use crate::{
    Controls,
    controller::Publication,
    publisher::{Codec, Phase, PublishRequest, Quality, Resolution, Sharing},
    runtime::{VoiceHandle, VoiceUpdate},
    screen_capture::{self, SourceKind},
};
use std::{
    env, thread,
    time::{Duration, Instant},
};
use voxhold_api::Client;

/// Each step has its own budget; nothing here may wait forever.
const JOIN_BUDGET: Duration = Duration::from_secs(20);
const LIVE_BUDGET: Duration = Duration::from_secs(20);
const STATS_BUDGET: Duration = Duration::from_secs(12);
const STOP_BUDGET: Duration = Duration::from_secs(10);
const WATCH_BUDGET: Duration = Duration::from_secs(20);
const FRAMES_BUDGET: Duration = Duration::from_secs(20);
const FRAME_WINDOW: Duration = Duration::from_secs(2);
const LOGIN_BUDGET: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(50);

struct Stand {
    url: String,
    username: String,
    password: String,
    server: i64,
    channel: i64,
    /// The publisher and the viewer must be different accounts: the backend
    /// moves a voice session to its newest connection.
    viewer: Option<(String, String)>,
}

impl Stand {
    /// `None` means the operator did not provide the publisher credentials.
    fn load() -> Option<Self> {
        let username = env::var("VOXHOLD_STAND_USERNAME")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let password = env::var("VOXHOLD_STAND_PASSWORD")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let (Some(username), Some(password)) = (username, password) else {
            println!(
                "live: SKIP — set VOXHOLD_STAND_USERNAME/VOXHOLD_STAND_PASSWORD to run the stand proof"
            );
            return None;
        };
        let url =
            env::var("VOXHOLD_STAND_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_owned());
        let server = env::var("VOXHOLD_STAND_SERVER_ID")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1);
        let channel = env::var("VOXHOLD_STAND_CHANNEL_ID")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1);
        let viewer = match (
            env::var("VOXHOLD_STAND_VIEWER_USERNAME").ok(),
            env::var("VOXHOLD_STAND_VIEWER_PASSWORD").ok(),
        ) {
            (Some(user), Some(pass)) if !user.trim().is_empty() && !pass.trim().is_empty() => {
                Some((user, pass))
            }
            _ => None,
        };
        Some(Self {
            url,
            username,
            password,
            server,
            channel,
            viewer,
        })
    }
}

fn login(stand: &Stand, username: &str, password: &str) -> Client {
    let mut client = Client::new(&stand.url)
        .unwrap_or_else(|error| panic!("live: invalid stand URL {:?}: {error}", stand.url));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("live: tokio runtime for login");
    runtime.block_on(async {
        match tokio::time::timeout(LOGIN_BUDGET, client.login(username, password)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => panic!("live: login failed: {error}"),
            Err(_) => panic!("live: login did not answer within {LOGIN_BUDGET:?}"),
        }
    });
    client
}

/// Drain updates until the server confirmed `voice.joined`. The latest catalog
/// is returned because the snapshot may arrive before the join confirmation and
/// a coalesced update slot would otherwise lose it.
fn wait_joined(handle: &VoiceHandle, label: &str) -> (Duration, Vec<Publication>) {
    let started = Instant::now();
    let deadline = started + JOIN_BUDGET;
    let mut streams = Vec::new();
    loop {
        while let Some(update) = handle.try_update() {
            match update {
                VoiceUpdate::SelfConnection(Some(_)) => return (started.elapsed(), streams),
                VoiceUpdate::Streams(next) => streams = next,
                VoiceUpdate::Ended(Some(error)) => {
                    panic!("live: {label} voice session ended: {error}")
                }
                VoiceUpdate::Ended(None) => {
                    panic!("live: {label} voice session ended before the join")
                }
                VoiceUpdate::DeviceError(error) => println!("live: {label} device notice: {error}"),
                _ => {}
            }
        }
        assert!(
            Instant::now() < deadline,
            "live: {label} did not confirm voice.joined within {JOIN_BUDGET:?}"
        );
        thread::sleep(POLL);
    }
}

/// Publish the first monitor through the public runtime path and wait for Live.
fn publish_monitor(handle: &VoiceHandle) -> (Duration, Sharing) {
    let sources = match screen_capture::list_sources(SourceKind::Monitor) {
        Ok(sources) => sources,
        Err(error) => panic!("live: monitor listing failed: {error}"),
    };
    let source = match sources.into_iter().next() {
        Some(source) => source,
        None => panic!("live: the desktop has no monitor source to publish"),
    };
    let picker = handle.begin_screen_picker();
    let request = PublishRequest {
        source,
        quality: Quality {
            resolution: Resolution::P720,
            fps: 30,
            bitrate_kbps: 4000,
        },
        codec: Codec::Vp8,
    };
    handle.publish_screen(picker, request, true, true);

    let started = Instant::now();
    let deadline = started + LIVE_BUDGET;
    loop {
        while let Some(update) = handle.try_update() {
            match update {
                VoiceUpdate::ScreenShare(sharing) => match sharing.phase {
                    Phase::Live => return (started.elapsed(), sharing),
                    Phase::Failed => panic!(
                        "live: publication failed before Live: {}",
                        sharing.error.unwrap_or_default()
                    ),
                    _ => {}
                },
                VoiceUpdate::Ended(Some(error)) => {
                    panic!("live: voice session ended while publishing: {error}")
                }
                _ => {}
            }
        }
        assert!(
            Instant::now() < deadline,
            "live: ScreenShare did not reach Live within {LIVE_BUDGET:?}"
        );
        thread::sleep(POLL);
    }
}

/// Wait until both the public stats and the cumulative RTP counters are real,
/// and then for a second sample whose packet counter grew, so the numbers prove
/// continuous sending instead of a single burst at connect time.
fn wait_stats(handle: &VoiceHandle, mut sharing: Sharing) -> (Sharing, crate::live_probe::Sample) {
    let deadline = Instant::now() + STATS_BUDGET;
    let mut first: Option<crate::live_probe::Sample> = None;
    loop {
        while let Some(update) = handle.try_update() {
            match update {
                VoiceUpdate::ScreenShare(next) => {
                    if next.phase == Phase::Failed {
                        panic!(
                            "live: publication failed while waiting for stats: {}",
                            next.error.unwrap_or_default()
                        );
                    }
                    sharing = next;
                }
                VoiceUpdate::Ended(Some(error)) => {
                    panic!("live: voice session ended while waiting for stats: {error}")
                }
                _ => {}
            }
        }
        if let Some(stats) = sharing.stats {
            if stats.fps > 0.0 {
                if let Some(sample) = crate::live_probe::sample() {
                    if sample.frames_encoded > 0 && sample.packets_sent > 0 {
                        match &first {
                            None => first = Some(sample),
                            Some(previous) if sample.packets_sent > previous.packets_sent => {
                                return (sharing, sample);
                            }
                            Some(_) => {}
                        }
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "live: no growing outbound packet counter within {STATS_BUDGET:?}"
        );
        thread::sleep(POLL);
    }
}

/// Stop the publication and wait for the worker to close the attempt.
fn stop_and_wait(handle: &VoiceHandle) -> Duration {
    let started = Instant::now();
    handle.stop_screen_share();
    let deadline = started + STOP_BUDGET;
    loop {
        while let Some(update) = handle.try_update() {
            match update {
                VoiceUpdate::ScreenShare(sharing) => match sharing.phase {
                    Phase::Idle => return started.elapsed(),
                    Phase::Failed => {
                        panic!("live: stop failed: {}", sharing.error.unwrap_or_default())
                    }
                    _ => {}
                },
                VoiceUpdate::Ended(Some(error)) => {
                    panic!("live: voice session ended during the stop: {error}")
                }
                _ => {}
            }
        }
        assert!(
            Instant::now() < deadline,
            "live: the publication did not reach Idle within {STOP_BUDGET:?}"
        );
        thread::sleep(POLL);
    }
}

/// Wait for the publisher's publication in the viewer's catalog.
fn wait_publication(handle: &VoiceHandle, known: Vec<Publication>) -> Publication {
    let deadline = Instant::now() + WATCH_BUDGET;
    if let Some(publication) = known.first().cloned() {
        return publication;
    }
    loop {
        while let Some(update) = handle.try_update() {
            match update {
                VoiceUpdate::Streams(streams) => {
                    if let Some(publication) = streams.into_iter().next() {
                        return publication;
                    }
                }
                VoiceUpdate::Ended(Some(error)) => {
                    panic!("live: viewer voice session ended: {error}")
                }
                _ => {}
            }
        }
        assert!(
            Instant::now() < deadline,
            "live: the viewer saw no publication within {WATCH_BUDGET:?}"
        );
        thread::sleep(POLL);
    }
}

/// Watch the owner's publication through the real SFU and count decoded frames
/// for a short window. `publisher` is drained too, so a failed publication is
/// reported instead of being mistaken for a silent viewer.
fn watch_frames(
    viewer: &VoiceHandle,
    publisher: &VoiceHandle,
    publication: Publication,
) -> (usize, u32, u32, Duration) {
    let selection = 1;
    viewer.watch_stream(selection, publication);
    let started = Instant::now();
    let first_deadline = started + FRAMES_BUDGET;
    let mut window: Option<Instant> = None;
    let mut frames = 0_usize;
    let (mut width, mut height) = (0, 0);
    loop {
        while let Some(update) = viewer.try_update() {
            match update {
                VoiceUpdate::StreamError { selection, error } => {
                    panic!("live: viewer failed (selection {selection}): {error}")
                }
                VoiceUpdate::Ended(Some(error)) => {
                    panic!("live: viewer voice session ended: {error}")
                }
                _ => {}
            }
        }
        while let Some(update) = publisher.try_update() {
            if let VoiceUpdate::ScreenShare(sharing) = update {
                if sharing.phase == Phase::Failed {
                    panic!(
                        "live: publication failed while the viewer watched: {}",
                        sharing.error.unwrap_or_default()
                    );
                }
            }
        }
        if let Some(frame) = viewer.take_frame() {
            frames += 1;
            width = frame.width;
            height = frame.height;
            window.get_or_insert_with(Instant::now);
        }
        if let Some(window) = window {
            if window.elapsed() >= FRAME_WINDOW {
                return (frames, width, height, window.elapsed());
            }
        } else {
            assert!(
                Instant::now() < first_deadline,
                "live: the viewer decoded no frame within {FRAMES_BUDGET:?}"
            );
        }
        thread::sleep(POLL);
    }
}

#[test]
#[ignore = "live stand: needs the Docker backend, credentials and a real desktop"]
fn live_stand_publishes_a_monitor_and_a_viewer_decodes_frames() {
    let Some(stand) = Stand::load() else {
        return;
    };
    crate::live_probe::enable();
    // The probe is process-wide; a panic must not leave it recording for the
    // rest of the test binary.
    struct ProbeGuard;
    impl Drop for ProbeGuard {
        fn drop(&mut self) {
            crate::live_probe::disable();
        }
    }
    let _guard = ProbeGuard;

    let owner = login(&stand, &stand.username, &stand.password);
    let publisher = VoiceHandle::join(
        owner,
        stand.server,
        stand.channel,
        Controls::default(),
        false,
    )
    .expect("live: voice join");
    let (joined, _catalog) = wait_joined(&publisher, "publisher");
    println!("live: publisher joined in {joined:?}");

    let (live_after, sharing) = publish_monitor(&publisher);
    let quality = sharing.quality.unwrap_or_default();
    println!(
        "live: ScreenShare Live after {live_after:?}; requested {} · {} FPS · {} kbit/s · codec {}",
        quality.resolution.wire(),
        quality.fps,
        quality.bitrate_kbps,
        sharing.codec.map(Codec::wire).unwrap_or("none"),
    );

    let (sharing, sample) = wait_stats(&publisher, sharing);
    let stats = sharing.stats.expect("live stats");
    println!(
        "live: answer_mid={:?} · encoded {}×{} · {:.1} FPS · frames_encoded={} · packets_sent={}",
        sample.answer_mid,
        sample.width,
        sample.height,
        stats.fps,
        sample.frames_encoded,
        sample.packets_sent,
    );
    assert_eq!(
        (sample.width, sample.height),
        (1280, 720),
        "the SDR 720p publication must encode 1280×720"
    );
    assert_eq!(
        (stats.width, stats.height),
        (1280, 720),
        "the public stats must report the real encoded size"
    );

    // The second scenario is optional: without a second account the publisher
    // proof above is still complete.
    if let Some((user, pass)) = &stand.viewer {
        let viewer_client = login(&stand, user, pass);
        let viewer = VoiceHandle::join(
            viewer_client,
            stand.server,
            stand.channel,
            Controls::default(),
            false,
        )
        .expect("live: viewer voice join");
        let (viewer_joined, catalog) = wait_joined(&viewer, "viewer");
        let publication = wait_publication(&viewer, catalog);
        println!(
            "live: viewer catalog has server={} channel={} mode={} codec={}",
            publication.server_id, publication.channel_id, publication.mode, publication.codec
        );
        let (frames, width, height, window) =
            watch_frames(&viewer, &publisher, publication.clone());
        println!(
            "live: viewer joined in {viewer_joined:?}; decoded {frames} frames in {window:?} at {width}×{height} ({}/{})",
            publication.mode, publication.codec,
        );
        assert!(frames > 0, "live: the viewer decoded no frame");
        assert!(
            width > 0 && height > 0,
            "live: the viewer frame has no size"
        );
        viewer.leave_stream(1);
        drop(viewer);
    } else {
        println!(
            "live: viewer SKIP — set VOXHOLD_STAND_VIEWER_USERNAME/VOXHOLD_STAND_VIEWER_PASSWORD"
        );
    }

    let stopped = stop_and_wait(&publisher);
    let final_sample = crate::live_probe::sample().expect("live probe sample");
    let starts = final_sample
        .signals
        .iter()
        .filter(|kind| **kind == "stream.start")
        .count();
    let stops = final_sample
        .signals
        .iter()
        .filter(|kind| **kind == "stream.stop")
        .count();
    let answers = final_sample
        .signals
        .iter()
        .filter(|kind| **kind == "stream.webrtc_answer")
        .count();
    println!(
        "live: stop reached Idle in {stopped:?}; signals start={starts} answer={answers} stop={stops}; engines started={} stopped={}",
        final_sample.engines_started, final_sample.engines_stopped
    );
    assert_eq!(starts, 1, "exactly one stream.start must be sent");
    assert!(
        answers >= 1,
        "the publisher must send its answer (with a viewer the viewer also answers)"
    );
    assert_eq!(stops, 1, "exactly one stream.stop must be sent");
    assert_eq!(
        final_sample.engines_started, 1,
        "the runtime must create exactly one capture engine"
    );
    assert_eq!(
        final_sample.engines_stopped, 1,
        "the runtime must release the capture engine"
    );
    assert!(
        final_sample
            .answer_mid
            .as_deref()
            .is_some_and(|mid| !mid.is_empty()),
        "the publisher answer has no mid"
    );
    assert!(
        publisher.take_screen_frame().is_none(),
        "the capture must not publish a preview after the stop"
    );
    println!("live: OK — publisher reached Live, sent frames/packets and stopped cleanly");
}
