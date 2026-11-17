//! One WebRTC engine per viewing attempt. The deterministic decisions live in
//! `viewer`; this file only drives the media engine and reports what it sees.

use crate::{Candidate, Command, Event, IceServer, Media, VideoFrame};
use std::time::Duration;

/// An engine event that belongs to the current attempt. Signaling is returned
/// without request ids; `viewer` stamps the attempt's ids.
pub enum EngineUpdate {
    Frame,
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
}

impl StreamViewer {
    /// `mode` was validated by the viewer; unknown modes never reach here.
    pub fn start(
        servers: Vec<IceServer>,
        policy: String,
        output: Option<String>,
        muted: bool,
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
        })
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
    pub fn poll(
        &mut self,
        frame: &mut VideoFrame,
        width: u32,
        height: u32,
    ) -> Option<EngineUpdate> {
        let event = self.media.poll(Duration::ZERO)?;
        match event {
            Event::Frame => self
                .media
                .read_video(frame, width, height)
                .then_some(EngineUpdate::Frame),
            Event::Answer { sdp } => Some(EngineUpdate::Answer(sdp)),
            Event::Ice(candidate) => Some(EngineUpdate::Ice(candidate)),
            Event::State { state } => Some(EngineUpdate::State(state)),
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
            Event::Track { .. } | Event::Level { .. } | Event::DeviceError { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Event, Events, Media, VideoFrame, video::VideoStore, voice::CaptureGate};
    use std::sync::Arc;
    use tokio::sync::mpsc;

    fn headless(output: Option<&str>) -> (StreamViewer, mpsc::Receiver<Command>, Arc<Events>) {
        let (commands, receiver) = mpsc::channel(8);
        let events = Arc::new(Events::default());
        let media = Media {
            commands,
            events: events.clone(),
            capture: Arc::new(CaptureGate::default()),
            video: Arc::new(VideoStore::default()),
            worker: None,
        };
        let output = output.map(str::to_owned);
        (
            StreamViewer {
                media,
                muted: false,
                requested_output: output.clone(),
                applied_output: output,
                output_working: true,
            },
            receiver,
            events,
        )
    }

    #[test]
    fn a_rolled_back_output_can_be_requested_again() {
        let (mut viewer, mut commands, events) = headless(Some("usb"));
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
        assert!(matches!(
            viewer.poll(&mut frame, 1, 1),
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
        let (mut viewer, mut commands, events) = headless(Some("usb-a"));
        // The hot-plug check switched playback to the default endpoint.
        events.push(Event::Output {
            applied: None,
            audible: true,
            working: true,
            error: Some("Устройство воспроизведения отключено, используется системное."),
        });
        let mut frame = VideoFrame::default();
        assert!(matches!(
            viewer.poll(&mut frame, 1, 1),
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
    fn an_explicit_selection_bypasses_the_stale_output_cache() {
        let (mut viewer, mut commands, _events) = headless(Some("usb-a"));
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
