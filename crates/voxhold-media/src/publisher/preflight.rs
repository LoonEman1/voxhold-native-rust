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
    screen_capture::{CaptureFailure, LatestImage, PreviewDemand, ScreenCapture, ScreenSource},
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

/// The temporary capture of one preflight.
///
/// The production implementation is [`ScreenCapture`]. The trait exists so the
/// test of the first-frame confirmation can succeed through the same code path
/// without opening a device.
trait PreflightCapture {
    fn latest(&self) -> &Arc<LatestImage>;
    fn failure(&self) -> Option<CaptureFailure>;
}

impl PreflightCapture for ScreenCapture {
    fn latest(&self) -> &Arc<LatestImage> {
        ScreenCapture::latest(self)
    }

    fn failure(&self) -> Option<CaptureFailure> {
        ScreenCapture::failure(self)
    }
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
    run_with(request, cancelled, budget, |source, target, fps, latest| {
        // A preflight validates the device only: its temporary capture never
        // feeds the on-air card, so the preview demand stays disabled.
        let demand = Arc::new(PreviewDemand::default());
        ScreenCapture::start(source, target, fps, latest, demand)
            .map_err(|error| format!("Новая настройка не открылась: {error}"))
            .map(|capture| Box::new(capture) as Box<dyn PreflightCapture>)
    })
}

/// The body of [`run`] with the capture stage injectable for tests.
fn run_with<F>(
    request: &PublishRequest,
    cancelled: &AtomicBool,
    budget: Duration,
    start_capture: F,
) -> Result<PreflightReport, String>
where
    F: FnOnce(
        ScreenSource,
        (u32, u32),
        u32,
        Arc<LatestImage>,
    ) -> Result<Box<dyn PreflightCapture>, String>,
{
    if cancelled.load(Ordering::SeqCst) {
        return Err(PREFLIGHT_CANCELLED.into());
    }
    let latest = Arc::new(LatestImage::default());
    let target = request.capture_target();
    let capture = start_capture(
        request.source.clone(),
        target,
        request.quality.fps,
        latest.clone(),
    )?;
    let deadline = Instant::now() + budget.min(FIRST_FRAME_TIMEOUT);
    let mut frame = None;
    while Instant::now() < deadline {
        if cancelled.load(Ordering::SeqCst) {
            return Err(PREFLIGHT_CANCELLED.into());
        }
        if let Some(image) = capture.latest().take() {
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
            },
            quality: Quality::default(),
            codec: Codec::Vp8,
            audio: false,
            audio_scope: AudioScope::Device,
            encoder: Default::default(),
        }
    }

    /// A capture that succeeds without a device. It publishes one frame, so
    /// the only possible failure left in `run_with` is the display check.
    struct TestCapture {
        latest: Arc<LatestImage>,
    }

    impl PreflightCapture for TestCapture {
        fn latest(&self) -> &Arc<LatestImage> {
            &self.latest
        }

        fn failure(&self) -> Option<CaptureFailure> {
            None
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_preflight_whose_confirmed_display_vanished_fails_before_any_restart() {
        use crate::screen_capture::{CapturedImage, MonitorIdentity, MonitorRect};
        use libwebrtc::video_frame::I420Buffer;

        let mut request = request();
        request.source.monitor = Some(MonitorIdentity {
            device: r"\\.\DISPLAY999".into(),
            bounds: MonitorRect::new(0, 0, 1920, 1080),
            work: MonitorRect::new(0, 0, 1920, 1080),
            width: 1920,
            height: 1080,
        });
        let cancelled = AtomicBool::new(false);
        // The capture stage would otherwise pass: it reports one real frame.
        let error = run_with(
            &request,
            &cancelled,
            PREFLIGHT_BUDGET,
            |_source, _target, _fps, latest| {
                latest.publish(CapturedImage {
                    width: 1920,
                    height: 1080,
                    i420: I420Buffer::new_black(1920, 1080),
                    preview: None,
                });
                Ok(Box::new(TestCapture {
                    latest: latest.clone(),
                }) as Box<dyn PreflightCapture>)
            },
        )
        .expect_err("a display that does not exist must fail the preflight");
        assert!(
            error.contains("Обновите список и выберите экран снова"),
            "the preflight must report the display configuration: {error}"
        );
        assert!(
            error.contains("Текущая трансляция не тронута"),
            "the live stream must be named as untouched: {error}"
        );
        assert!(
            !error.contains("не открылась") && !error.contains("первый кадр"),
            "the capture and its first frame really succeeded: {error}"
        );
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
