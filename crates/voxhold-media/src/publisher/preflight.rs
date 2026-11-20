//! Off-loop validation of a new publication before a live attempt is stopped.
//!
//! The pinned backend has no runtime reconfigure, so a resolution, frame-rate
//! or audio-scope change needs a new capture and a confirmed stop+start. This
//! module opens the requested capture and audio scope in a temporary attempt,
//! waits for a bounded first frame and for the loopback endpoint, then releases
//! both. Only a successful preflight may stop the working publication.
//!
//! The temporary capture never touches the live preview store: the on-air card
//! keeps showing the running publication while the new settings are checked.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crate::{
    publisher::PublishRequest,
    screen_capture::{CaptureFailure, LatestImage, ScreenCapture},
    system_audio::SystemAudio,
};

/// The whole preflight is bounded; a capture or loopback call that never
/// returns cannot hold the restart forever.
pub const PREFLIGHT_BUDGET: Duration = Duration::from_secs(10);
/// The temporary capture must deliver one frame inside this window. The rest of
/// the budget belongs to opening the loopback.
pub const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
/// The session cancelled the preflight between two stages.
pub const PREFLIGHT_CANCELLED: &str = "Проверка новых настроек отменена.";

/// What the temporary capture proved. Only the fact that the requested size
/// and scope opened and delivered a frame is used; the picture is discarded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreflightReport {
    pub width: u32,
    pub height: u32,
    /// Real scope of the opened loopback, as the engine will report it.
    pub audio_scope: Option<String>,
}

/// Validate `request` on a temporary capture and release it.
///
/// The cancellation token is checked before and after every blocking stage. A
/// stage that is already running may finish, but its capture is dropped here,
/// on the preparation thread, so a cancelled preflight never keeps a device
/// open and never reaches the live attempt.
pub fn run(
    request: &PublishRequest,
    cancelled: &AtomicBool,
    budget: Duration,
) -> Result<PreflightReport, String> {
    if cancelled.load(Ordering::SeqCst) {
        return Err(PREFLIGHT_CANCELLED.into());
    }
    let latest = Arc::new(LatestImage::default());
    let target = request.quality.resolution.size();
    let capture = ScreenCapture::start(
        request.source.clone(),
        target,
        request.quality.fps,
        latest.clone(),
    )
    .map_err(|error| format!("Новая настройка не открылась: {error}"))?;
    let deadline = Instant::now() + budget.min(FIRST_FRAME_TIMEOUT);
    let mut frame = None;
    while Instant::now() < deadline {
        if cancelled.load(Ordering::SeqCst) {
            return Err(PREFLIGHT_CANCELLED.into());
        }
        if let Some(image) = latest.take() {
            frame = Some((image.width, image.height));
            break;
        }
        if let Some(failure) = capture.failure() {
            // A compositor hiccup may still recover before the deadline; only
            // a permanent or ended source fails the preflight here.
            if !matches!(failure, CaptureFailure::Temporary) {
                return Err(failure.message().to_owned());
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let Some((width, height)) = frame else {
        return Err(
            "Новая настройка не прислала первый кадр за 5 секунд. Текущая трансляция не тронута."
                .into(),
        );
    };
    // The capturer is addressed by a numeric monitor id for the moment the
    // real capture starts. If the display configuration changed between the
    // picker and this first frame, that id may now name another screen.
    if let Err(error) = crate::screen_capture::confirm_source_topology(&request.source) {
        return Err(format!(
            "{}. Текущая трансляция не тронута.",
            error.message()
        ));
    }
    if cancelled.load(Ordering::SeqCst) {
        return Err(PREFLIGHT_CANCELLED.into());
    }
    let mut audio_scope = None;
    if request.audio {
        let scope = request
            .audio_scope
            .resolve(&request.source)
            .map_err(|error| format!("Системный звук недоступен: {error}"))?;
        // `SystemAudio::start` is itself bounded and waits for the endpoint to
        // report readiness. The temporary capture is released immediately
        // after; the live loopback keeps running untouched.
        let audio = SystemAudio::start(scope)
            .map_err(|error| format!("Системный звук недоступен: {error}"))?;
        drop(audio);
        audio_scope = Some(scope.describe());
    }
    if cancelled.load(Ordering::SeqCst) {
        return Err(PREFLIGHT_CANCELLED.into());
    }
    drop(capture);
    Ok(PreflightReport {
        width,
        height,
        audio_scope,
    })
}

/// The quality a preflight opened, for a test that cannot touch a device.
#[cfg(test)]
pub(crate) fn target_of(quality: crate::publisher::Quality) -> (u32, u32) {
    quality.resolution.size()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publisher::{Codec, PublishRequest, Quality};
    use crate::screen_capture::{ScreenSource, SourceKind};
    use crate::system_audio::AudioScope;

    fn request() -> PublishRequest {
        PublishRequest {
            source: ScreenSource {
                id: 1,
                kind: SourceKind::Monitor,
                title: "Экран 1".into(),
                display_id: -1,
                width: 1920,
                height: 1080,
                monitor: None,
                topology: None,
            },
            quality: Quality::default(),
            codec: Codec::Vp8,
            audio: false,
            audio_scope: AudioScope::Device,
        }
    }

    #[test]
    fn a_cancelled_preflight_opens_no_capture_and_no_loopback() {
        // The token is checked before the first device stage, so this runs
        // without a desktop or an audio endpoint.
        let cancelled = AtomicBool::new(true);
        let error = run(&request(), &cancelled, PREFLIGHT_BUDGET)
            .expect_err("a cancelled preflight must not open a device");
        assert!(error.contains("отменена"), "{error}");
    }

    #[test]
    fn the_preflight_targets_the_selected_resolution() {
        assert_eq!(target_of(Quality::default()), (1920, 1080));
        assert_eq!(
            target_of(Quality {
                resolution: crate::publisher::Resolution::P720,
                ..Quality::default()
            }),
            (1280, 720)
        );
    }
}
