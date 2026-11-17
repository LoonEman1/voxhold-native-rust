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
}

pub struct StreamViewer {
    media: Media,
    muted: bool,
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
                output,
            })
            .map_err(|error| format!("{error:?}"))?;
        if muted {
            media
                .command(Command::ViewAudio { muted: true })
                .map_err(|error| format!("{error:?}"))?;
        }
        Ok(Self { media, muted })
    }

    pub fn set_muted(&mut self, muted: bool) -> Result<(), String> {
        self.media
            .command(Command::ViewAudio { muted })
            .map_err(|error| format!("{error:?}"))?;
        self.muted = muted;
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
            Event::Track { .. } | Event::Level { .. } | Event::DeviceError { .. } => None,
        }
    }
}
