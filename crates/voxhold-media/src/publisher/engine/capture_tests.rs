//! Capture-failure policy of the frame loop: a temporary compositor failure
//! waits for the next frame (or the five-second watchdog), a permanent one
//! ends the attempt immediately.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use super::{EngineEvent, FrameProducer, PreviewStore, PublishRequest, PublisherEngine, Quality};
use crate::publisher::{Codec, Resolution};
use crate::screen_capture::{CaptureFailure, CapturedImage, ScreenSource, SourceKind};
use libwebrtc::video_frame::I420Buffer;

#[derive(Clone, Copy)]
enum Script {
    TemporaryThenFrames,
    Permanent,
}

struct ScriptedProducer {
    script: Script,
    tick: usize,
}

impl FrameProducer for ScriptedProducer {
    fn next_frame(&mut self) -> Option<CapturedImage> {
        let tick = self.tick;
        self.tick += 1;
        match self.script {
            // The first tick reports a temporary failure with no picture; the
            // next tick delivers a good frame.
            Script::TemporaryThenFrames if tick == 0 => None,
            Script::TemporaryThenFrames => Some(CapturedImage {
                width: 64,
                height: 36,
                i420: I420Buffer::new_black(64, 36),
                preview: None,
            }),
            Script::Permanent => None,
        }
    }

    fn failure(&self) -> Option<CaptureFailure> {
        match self.script {
            Script::TemporaryThenFrames if self.tick == 0 => Some(CaptureFailure::Temporary),
            Script::Permanent => Some(CaptureFailure::Permanent),
            _ => None,
        }
    }

    fn dropped(&self) -> u64 {
        0
    }
}

fn request() -> PublishRequest {
    PublishRequest {
        source: ScreenSource {
            id: 0,
            kind: SourceKind::Monitor,
            title: "synthetic".into(),
            display_id: -1,
            width: 64,
            height: 36,
            monitor: None,
        },
        quality: Quality {
            resolution: Resolution::P720,
            fps: 30,
            bitrate_kbps: 500,
            audio_bitrate_kbps: 128,
        },
        codec: Codec::Vp8,
        audio: false,
        audio_scope: crate::system_audio::AudioScope::Device,
    }
}

fn start(script: Script) -> PublisherEngine {
    PublisherEngine::spawn(
        request(),
        Vec::new(),
        "all".into(),
        Box::new(ScriptedProducer { script, tick: 0 }),
        None,
        None,
        Arc::new(PreviewStore::default()),
    )
    .expect("synthetic engine")
}

/// Drain events until `stop` accepts one or the deadline expires.
fn wait_for(
    engine: &PublisherEngine,
    deadline: Duration,
    mut stop: impl FnMut(&EngineEvent) -> bool,
) -> Vec<EngineEvent> {
    let until = Instant::now() + deadline;
    let mut seen = Vec::new();
    while Instant::now() < until {
        while let Some(event) = engine.poll() {
            let done = stop(&event);
            seen.push(event);
            if done {
                return seen;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    seen
}

#[test]
fn a_temporary_capture_failure_recovers_on_the_next_frame() {
    let engine = start(Script::TemporaryThenFrames);
    // Without an offer there is no encoder sink, so the proof is the absence
    // of a terminal error plus a stats sample with the real captured size.
    let seen = wait_for(&engine, Duration::from_millis(2500), |_| false);
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, EngineEvent::Error(_))),
        "a temporary failure is not terminal: {seen:?}"
    );
    assert!(
        seen.iter().any(|event| matches!(
            event,
            EngineEvent::Stats(stats) if stats.width == 64 && stats.height == 36
        )),
        "the next good frame kept the publication alive: {seen:?}"
    );
}

#[test]
fn a_permanent_capture_failure_ends_the_attempt() {
    let engine = start(Script::Permanent);
    let seen = wait_for(&engine, Duration::from_secs(5), |event| {
        matches!(event, EngineEvent::Error(_))
    });
    let error = seen
        .iter()
        .find_map(|event| match event {
            EngineEvent::Error(message) => Some(message.clone()),
            _ => None,
        })
        .expect("a permanent failure is terminal");
    assert!(
        error.contains("закрыт") || error.contains("отключ"),
        "the message names the lost source: {error}"
    );
}
