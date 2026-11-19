//! Test-only observation of the real publishing path for the live stand test.
//!
//! The public runtime snapshot (`Sharing`) exposes the phase, the encoded FPS
//! and the resolution, but not the answer mid, the cumulative RTP counters or
//! the exact signaling kinds. The live test needs those real values to report
//! more than "the phase turned Live", so the runtime and the engine record them
//! here. The module is compiled only into test builds, every write is disabled
//! unless a test calls [`enable`], and nothing is exposed to the application.

use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

use serde_json::Value;

/// What the live test reads back. Every field belongs to the current attempt:
/// [`enable`] resets the whole sample.
#[derive(Clone, Debug, Default)]
pub(crate) struct Sample {
    /// First `a=mid:` of the answer SDP the publisher sent to the SFU.
    pub answer_mid: Option<String>,
    /// Cumulative encoded frames reported by the outbound RTP statistics.
    pub frames_encoded: u32,
    /// Cumulative RTP packets reported by the same statistics.
    pub packets_sent: u64,
    pub width: u32,
    pub height: u32,
    /// Signaling kinds the session really sent, in order.
    pub signals: Vec<&'static str>,
    /// Capture engines the runtime created and released; the release happens
    /// after the engine thread joined, so it proves the capture is gone.
    pub engines_started: u32,
    pub engines_stopped: u32,
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static SAMPLE: Mutex<Option<Sample>> = Mutex::new(None);

/// Start recording a fresh sample. A test calls this once and reads it later;
/// no other test runs the live path in the same process.
pub(crate) fn enable() {
    if let Ok(mut sample) = SAMPLE.lock() {
        *sample = Some(Sample::default());
    }
    ENABLED.store(true, Ordering::SeqCst);
}

pub(crate) fn disable() {
    ENABLED.store(false, Ordering::SeqCst);
}

pub(crate) fn sample() -> Option<Sample> {
    SAMPLE.lock().ok()?.clone()
}

/// One outgoing signal of the voice session. The answer mid is derived from the
/// answer SDP; no SDP text is kept.
pub(crate) fn record_signal(kind: &'static str, data: &Value) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let mid = (kind == "stream.webrtc_answer")
        .then(|| data["sdp"].as_str())
        .flatten()
        .and_then(first_mid);
    let Ok(mut sample) = SAMPLE.lock() else {
        return;
    };
    let Some(sample) = sample.as_mut() else {
        return;
    };
    if sample.signals.len() < 64 {
        sample.signals.push(kind);
    }
    if mid.is_some() {
        sample.answer_mid = mid;
    }
}

/// One outbound statistics sample of the publisher engine.
pub(crate) fn record_outbound(frames: u32, packets: u64, width: u32, height: u32) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let Ok(mut sample) = SAMPLE.lock() else {
        return;
    };
    let Some(sample) = sample.as_mut() else {
        return;
    };
    sample.frames_encoded = frames;
    sample.packets_sent = packets;
    if width > 0 {
        sample.width = width;
    }
    if height > 0 {
        sample.height = height;
    }
}

pub(crate) fn record_engine_started() {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    if let Ok(mut sample) = SAMPLE.lock() {
        if let Some(sample) = sample.as_mut() {
            sample.engines_started = sample.engines_started.saturating_add(1);
        }
    }
}

/// Called after the engine was dropped, which stops the capture thread and
/// joins it.
pub(crate) fn record_engine_stopped() {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    if let Ok(mut sample) = SAMPLE.lock() {
        if let Some(sample) = sample.as_mut() {
            sample.engines_stopped = sample.engines_stopped.saturating_add(1);
        }
    }
}

fn first_mid(sdp: &str) -> Option<String> {
    sdp.replace("\r\n", "\n")
        .lines()
        .find_map(|line| line.strip_prefix("a=mid:").map(str::to_owned))
}
