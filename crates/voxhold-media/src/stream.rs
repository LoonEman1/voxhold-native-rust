//! One WebRTC engine per viewing attempt. The deterministic decisions live in
//! `viewer`; this file only drives the media engine and reports what it sees.

use crate::{Candidate, Command, Event, IceServer, Media, VideoFrame};
use std::time::Duration;

/// An engine event that belongs to the current attempt. Signaling is returned
/// without request ids; `viewer` stamps the attempt's ids.
pub enum EngineUpdate {
    /// A fresh RTP frame was decoded and prepared for the player. It confirms
    /// the connection and ends recovery exactly like `Decoded`; of the two,
    /// only `Frame` carries a newly prepared picture.
    Frame,
    /// A fresh RTP frame was decoded but the player did not ask for a picture,
    /// so only the last prepared frame stayed in the surface. It confirms the
    /// connection exactly like `Frame`; the picture is prepared on the next
    /// request instead of once per decoded frame.
    Decoded,
    /// The cached last frame was re-read at a new viewport size or on a player
    /// request. It refreshes the picture only: connection state, status text
    /// and the recovery deadline must stay exactly as they were. It is never
    /// video progress.
    Repaint,
    /// Cumulative inbound video counters of this attempt. A resize repaint and
    /// a missing report must not be confused with these values.
    Stats {
        attempt: u64,
        bytes_received: u64,
        frames_decoded: u32,
    },
    Answer(String),
    Ice(Candidate),
    State(&'static str),
    Error(String),
    /// Audio output switch result. It is not a stream failure: the peer and the
    /// decoded picture stay alive after a successful rollback.
    Output {
        applied: Option<String>,
        audible: bool,
        working: bool,
        error: Option<String>,
    },
}

pub struct StreamViewer {
    media: Media,
    muted: bool,
    /// The last output requested from the player versus the endpoint it has
    /// actually confirmed. A failed switch leaves `applied` on the previous
    /// device so the same request can be retried instead of being dropped.
    requested_output: Option<String>,
    applied_output: Option<String>,
    output_working: bool,
    /// Viewport applied to the last decoded read. A different size re-reads
    /// the still-current frame so a static stream repaints at the new texture
    /// size instead of waiting for another RTP frame. It starts at `(0, 0)`,
    /// so the first frame of a freshly started engine is always a surface
    /// change: the starting picture is prepared without a player request.
    viewport: (u32, u32),
    /// Viewer attempt this engine was created for. Every statistics report is
    /// stamped with it so the state machine can reject a late report of a
    /// replaced engine.
    attempt: u64,
}

impl StreamViewer {
    /// `mode` was validated by the viewer; unknown modes never reach here.
    pub fn start(
        servers: Vec<IceServer>,
        policy: String,
        output: Option<String>,
        muted: bool,
        attempt: u64,
    ) -> Result<Self, String> {
        let media = Media::new().map_err(|error| format!("{error:?}"))?;
        media
            .command(Command::ViewStart {
                ice_servers: servers,
                ice_transport_policy: policy,
                output: output.clone(),
            })
            .map_err(|error| format!("{error:?}"))?;
        if muted {
            media
                .command(Command::ViewAudio { muted: true })
                .map_err(|error| format!("{error:?}"))?;
        }
        Ok(Self {
            media,
            muted,
            requested_output: output.clone(),
            applied_output: output,
            output_working: true,
            viewport: (0, 0),
            attempt,
        })
    }

    /// Headless engine for crate tests: no worker and no socket. It keeps the
    /// real `Events` queue and `VideoStore`, so `poll` takes the production
    /// path without decoding or a device.
    #[cfg(test)]
    pub(crate) fn detached_for_tests() -> (
        Self,
        tokio::sync::mpsc::Receiver<Command>,
        std::sync::Arc<crate::Events>,
        std::sync::Arc<crate::video::VideoStore>,
    ) {
        Self::detached_for_attempt(0)
    }

    /// Headless engine with an explicit attempt id, so a test can prove that
    /// `poll` stamps statistics with the attempt the runtime installed.
    #[cfg(test)]
    pub(crate) fn detached_for_attempt(
        attempt: u64,
    ) -> (
        Self,
        tokio::sync::mpsc::Receiver<Command>,
        std::sync::Arc<crate::Events>,
        std::sync::Arc<crate::video::VideoStore>,
    ) {
        let (commands, receiver) = tokio::sync::mpsc::channel(8);
        let events = std::sync::Arc::new(crate::Events::default());
        let video = std::sync::Arc::new(crate::video::VideoStore::default());
        let media = Media {
            commands,
            events: events.clone(),
            capture: std::sync::Arc::new(crate::voice::CaptureGate::default()),
            video: video.clone(),
            stop: std::sync::Arc::new(tokio::sync::Notify::new()),
            worker: None,
        };
        (
            Self {
                media,
                muted: false,
                requested_output: None,
                applied_output: None,
                output_working: true,
                viewport: (0, 0),
                attempt,
            },
            receiver,
            events,
            video,
        )
    }

    /// Headless engine whose worker blocks until the gate opens. The runtime
    /// tests use it to prove that closing a player never joins the peer on the
    /// signaling thread.
    #[cfg(test)]
    pub(crate) fn blocked_for_tests(
        gate: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
        released: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        let (commands, _receiver) = tokio::sync::mpsc::channel(1);
        let events = std::sync::Arc::new(crate::Events::default());
        let video = std::sync::Arc::new(crate::video::VideoStore::default());
        let worker = std::thread::Builder::new()
            .name("voxhold-media-test".into())
            .spawn(move || {
                let (open, changed) = &*gate;
                let mut open = open.lock().unwrap();
                while !*open {
                    let (next, timeout) = changed
                        .wait_timeout(open, std::time::Duration::from_secs(2))
                        .unwrap();
                    open = next;
                    if timeout.timed_out() {
                        break;
                    }
                }
                released.store(true, std::sync::atomic::Ordering::SeqCst);
            })
            .expect("the test media worker starts");
        Self {
            media: Media {
                commands,
                events,
                capture: std::sync::Arc::new(crate::voice::CaptureGate::default()),
                video,
                stop: std::sync::Arc::new(tokio::sync::Notify::new()),
                worker: Some(worker),
            },
            muted: false,
            requested_output: None,
            applied_output: None,
            output_working: true,
            viewport: (0, 0),
            attempt: 0,
        }
    }

    pub fn set_muted(&mut self, muted: bool) -> Result<(), String> {
        self.media
            .command(Command::ViewAudio { muted })
            .map_err(|error| format!("{error:?}"))?;
        self.muted = muted;
        Ok(())
    }

    /// The output device setting is shared with the voice session, so a
    /// running player must follow it too. A repeat of a request that rolled
    /// back is sent again because `applied_output` still names the old device.
    pub fn set_output(&mut self, output: Option<String>) -> Result<(), String> {
        if self.requested_output == output && self.applied_output == output && self.output_working {
            return Ok(());
        }
        self.command_output(output)
    }

    /// Explicit selection from settings. The engine's `OutputState` is the
    /// authority: the viewer cache may still name A while an unplug fallback
    /// event is queued but not polled yet.
    pub fn select_output(&mut self, output: Option<String>) -> Result<(), String> {
        self.command_output(output)
    }

    fn command_output(&mut self, output: Option<String>) -> Result<(), String> {
        self.media
            .command(Command::ViewOutput {
                output: output.clone(),
            })
            .map_err(|error| format!("{error:?}"))?;
        self.requested_output = output;
        Ok(())
    }

    pub fn offer(&self, sdp: String) -> Result<(), String> {
        self.media
            .command(Command::Offer { sdp })
            .map_err(|error| format!("{error:?}"))
    }

    pub fn ice(&self, candidate: Candidate) -> Result<(), String> {
        self.media
            .command(Command::Ice(candidate))
            .map_err(|error| format!("{error:?}"))
    }

    pub fn muted(&self) -> bool {
        self.muted
    }

    /// Non-blocking: one queued engine event per call, so a burst of frames
    /// cannot starve signaling or the voice session on the same worker.
    ///
    /// A queued event always wins. A fresh RTP frame is reported as `Frame`
    /// only when the picture was prepared; otherwise it is `Decoded`, which
    /// still confirms the transport but reuses the last prepared surface. A
    /// changed viewport or a `request` from the player re-reads the frame
    /// `VideoStore` already holds and is reported as `Repaint`, never as a
    /// connection-confirming `Frame`. The read is size- and sequence-aware, so
    /// an unchanged picture reports nothing and the caller settles instead of
    /// polling in a loop.
    ///
    /// The first frame of an engine reads even without a `request`: `viewport`
    /// starts at `(0, 0)`, so the starting surface is a real size change. That
    /// one conversion is what shows a picture before the player's first
    /// `take_frame`; it is a surface change, not a bound violation.
    ///
    /// `request` is a last-value flag owned by the player: it is cleared here
    /// once this poll used it for a read, so one `take_frame` prepares at most
    /// one picture. A failed read keeps it pending for the next poll.
    pub fn poll(
        &mut self,
        frame: &mut VideoFrame,
        width: u32,
        height: u32,
        request: &mut bool,
    ) -> Option<EngineUpdate> {
        let Some(event) = self.media.poll(Duration::ZERO) else {
            // No engine event: a player request or a new picture surface is
            // the only remaining reason to touch the decoder.
            if !*request && (width, height) == self.viewport {
                return None;
            }
            if !self.media.read_video(frame, width, height) {
                // Nothing newer was decoded, or the surface could not be
                // read. Keep the request: it means "prepare the next
                // picture", so it is served by the next decoded frame instead
                // of forcing the player to ask again.
                return None;
            }
            // Only a completed read counts. A failed attempt leaves the
            // viewport unchanged so the next poll retries it.
            *request = false;
            self.viewport = (width, height);
            return Some(EngineUpdate::Repaint);
        };
        match event {
            Event::Frame => {
                if *request || (width, height) != self.viewport {
                    if !self.media.read_video(frame, width, height) {
                        // A queued decode could not be prepared yet. Keep the
                        // request and the old surface baseline so a later poll
                        // retries instead of settling on a stale picture.
                        return None;
                    }
                    *request = false;
                    // The fresh frame now owns the picture surface, so the
                    // idle path must use this size as its baseline. Without
                    // it a later smaller viewport looks unchanged and the
                    // stale larger texture survives a fullscreen exit.
                    self.viewport = (width, height);
                    return Some(EngineUpdate::Frame);
                }
                // Video arrived, but no picture was requested. Still report
                // the decode so the state machine confirms the connection;
                // the picture is prepared on the next request or resize.
                Some(EngineUpdate::Decoded)
            }
            Event::Answer { sdp } => Some(EngineUpdate::Answer(sdp)),
            Event::Ice(candidate) => Some(EngineUpdate::Ice(candidate)),
            Event::State { state } => Some(EngineUpdate::State(state)),
            Event::VideoStats {
                bytes_received,
                frames_decoded,
            } => Some(EngineUpdate::Stats {
                attempt: self.attempt,
                bytes_received,
                frames_decoded,
            }),
            Event::Error { message } => Some(EngineUpdate::Error(message.into())),
            Event::Output {
                applied,
                audible,
                working,
                error,
            } => {
                self.applied_output = applied.clone();
                self.requested_output = applied.clone();
                self.output_working = working;
                Some(EngineUpdate::Output {
                    applied,
                    audible,
                    working,
                    error: error.map(str::to_owned),
                })
            }
            Event::Track { .. }
            | Event::Level { .. }
            | Event::InputLevel { .. }
            | Event::Audio { .. }
            | Event::Playout(_)
            | Event::InputApplied { .. }
            | Event::DeviceError { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Event, Events, VideoFrame, video::VideoStore};
    use std::sync::Arc;
    use tokio::sync::mpsc;

    fn headless(
        output: Option<&str>,
    ) -> (
        StreamViewer,
        mpsc::Receiver<Command>,
        Arc<Events>,
        Arc<VideoStore>,
    ) {
        let (mut viewer, receiver, events, video) = StreamViewer::detached_for_tests();
        let output = output.map(str::to_owned);
        viewer.requested_output = output.clone();
        viewer.applied_output = output;
        (viewer, receiver, events, video)
    }

    #[test]
    fn a_rolled_back_output_can_be_requested_again() {
        let (mut viewer, mut commands, events, _video) = headless(Some("usb"));
        viewer.set_output(Some("hdmi".into())).unwrap();
        assert!(matches!(
            commands.try_recv(),
            Ok(Command::ViewOutput { output: Some(output) }) if output == "hdmi"
        ));
        // The engine could not open HDMI and restored USB. The peer and the
        // picture stay alive; only the applied endpoint changes.
        events.push(Event::Output {
            applied: Some("usb".into()),
            audible: true,
            working: true,
            error: Some("Не удалось переключить устройство воспроизведения, прежнее работает."),
        });
        let mut frame = VideoFrame::default();
        let mut request = false;
        assert!(matches!(
            viewer.poll(&mut frame, 1, 1, &mut request),
            Some(EngineUpdate::Output { applied: Some(applied), .. }) if applied == "usb"
        ));
        // Repeating the same preference must reach the engine again instead of
        // being skipped because the requested slot still said HDMI.
        viewer.set_output(Some("hdmi".into())).unwrap();
        assert!(matches!(
            commands.try_recv(),
            Ok(Command::ViewOutput { output: Some(output) }) if output == "hdmi"
        ));
    }

    #[test]
    fn an_unplug_fallback_does_not_hide_a_reconnected_device() {
        let (mut viewer, mut commands, events, _video) = headless(Some("usb-a"));
        // The hot-plug check switched playback to the default endpoint.
        events.push(Event::Output {
            applied: None,
            audible: true,
            working: true,
            error: Some("Устройство воспроизведения отключено, используется системное."),
        });
        let mut frame = VideoFrame::default();
        let mut request = false;
        assert!(matches!(
            viewer.poll(&mut frame, 1, 1, &mut request),
            Some(EngineUpdate::Output {
                applied: None,
                working: true,
                ..
            })
        ));
        // Reconnecting A and selecting it must still reach the engine.
        viewer.set_output(Some("usb-a".into())).unwrap();
        assert!(matches!(
            commands.try_recv(),
            Ok(Command::ViewOutput { output: Some(output) }) if output == "usb-a"
        ));
    }

    #[test]
    fn a_viewport_change_re_reads_a_static_frame_without_a_new_rtp_frame() {
        let (mut viewer, _commands, events, video) = headless(None);
        assert!(video.publish_i420(libwebrtc::video_frame::I420Buffer::new_black(320, 180)));
        events.push(Event::Frame);
        let mut frame = VideoFrame::default();
        let mut request = false;
        // A queued fresh RTP frame wins over the viewport re-read.
        assert!(matches!(
            viewer.poll(&mut frame, 160, 90, &mut request),
            Some(EngineUpdate::Frame)
        ));
        assert_eq!((frame.width(), frame.height()), (160, 90));

        // Fullscreen: no `Event::Frame` is queued, only the picture surface
        // changed. The viewer must re-read the frame it already decodes, as a
        // repaint rather than a connection-confirming fresh frame.
        assert!(matches!(
            viewer.poll(&mut frame, 320, 278, &mut request),
            Some(EngineUpdate::Repaint)
        ));
        assert_eq!((frame.width(), frame.height()), (320, 180));

        // The same size reports nothing, so the worker settles after one
        // repaint instead of forcing frames in a loop.
        assert!(viewer.poll(&mut frame, 320, 278, &mut request).is_none());
    }

    #[test]
    fn a_cached_repaint_returns_to_the_window_after_a_fullscreen_frame() {
        let (mut viewer, _commands, events, video) = headless(None);
        assert!(video.publish_i420(libwebrtc::video_frame::I420Buffer::new_black(320, 180)));
        events.push(Event::Frame);
        let mut frame = VideoFrame::default();
        let mut request = false;
        // Floating window first.
        assert!(matches!(
            viewer.poll(&mut frame, 160, 90, &mut request),
            Some(EngineUpdate::Frame)
        ));
        assert_eq!((frame.width(), frame.height()), (160, 90));

        // A fresh RTP frame arrives while fullscreen. The decode uses the
        // fullscreen surface, so the cached baseline must move to 320x278.
        events.push(Event::Frame);
        assert!(matches!(
            viewer.poll(&mut frame, 320, 278, &mut request),
            Some(EngineUpdate::Frame)
        ));
        assert_eq!((frame.width(), frame.height()), (320, 180));

        // Leaving fullscreen queues no frame. The cached picture must be
        // re-read at the window surface, not stay at the fullscreen size.
        assert!(matches!(
            viewer.poll(&mut frame, 160, 90, &mut request),
            Some(EngineUpdate::Repaint)
        ));
        assert_eq!((frame.width(), frame.height()), (160, 90));
        assert!(viewer.poll(&mut frame, 160, 90, &mut request).is_none());
    }

    #[test]
    fn a_cached_repaint_shrinks_a_fullscreen_picture_back_to_the_window() {
        let (mut viewer, _commands, events, video) = headless(None);
        assert!(video.publish_i420(libwebrtc::video_frame::I420Buffer::new_black(320, 180)));
        events.push(Event::Frame);
        let mut frame = VideoFrame::default();
        let mut request = false;
        // The last decode happened at the fullscreen surface.
        assert!(matches!(
            viewer.poll(&mut frame, 320, 278, &mut request),
            Some(EngineUpdate::Frame)
        ));
        assert_eq!((frame.width(), frame.height()), (320, 180));

        // Exiting fullscreen only changes the surface: the same picture is
        // re-read smaller and reported as a repaint.
        assert!(matches!(
            viewer.poll(&mut frame, 160, 90, &mut request),
            Some(EngineUpdate::Repaint)
        ));
        assert_eq!((frame.width(), frame.height()), (160, 90));
        assert!(viewer.poll(&mut frame, 160, 90, &mut request).is_none());
    }

    #[test]
    fn a_cached_repaint_retries_after_a_failed_read() {
        let (mut viewer, _commands, events, video) = headless(None);
        events.push(Event::Frame);
        let mut frame = VideoFrame::default();
        let mut request = true;
        // Nothing was published, so the fresh frame cannot be decoded. The
        // failed attempt must not record the viewport as applied.
        assert!(viewer.poll(&mut frame, 160, 90, &mut request).is_none());
        assert!(matches!(viewer.viewport, (0, 0)));
        assert!(request, "a failed read keeps the request for the next poll");

        // A later attempt at the same size must still read the picture
        // instead of settling because the size looks unchanged.
        assert!(video.publish_i420(libwebrtc::video_frame::I420Buffer::new_black(320, 180)));
        assert!(matches!(
            viewer.poll(&mut frame, 160, 90, &mut request),
            Some(EngineUpdate::Repaint)
        ));
        assert_eq!((frame.width(), frame.height()), (160, 90));
    }

    #[test]
    fn a_video_stats_event_is_stamped_with_the_engine_attempt() {
        let (mut viewer, _commands, events, _video) = headless(None);
        events.push(Event::VideoStats {
            bytes_received: 10,
            frames_decoded: 2,
        });
        let mut frame = VideoFrame::default();
        let mut request = false;
        assert!(matches!(
            viewer.poll(&mut frame, 1, 1, &mut request),
            Some(EngineUpdate::Stats {
                attempt: 0,
                bytes_received: 10,
                frames_decoded: 2
            })
        ));
    }

    #[test]
    fn an_explicit_selection_bypasses_the_stale_output_cache() {
        let (mut viewer, mut commands, _events, _video) = headless(Some("usb-a"));
        // The fallback notice is still queued: the viewer cache says A is
        // applied and working. An explicit settings selection must still reach
        // `Session::ViewOutput`, whose own state knows about the unplug.
        viewer.select_output(Some("usb-a".into())).unwrap();
        assert!(matches!(
            commands.try_recv(),
            Ok(Command::ViewOutput { output: Some(output) }) if output == "usb-a"
        ));
    }
}
