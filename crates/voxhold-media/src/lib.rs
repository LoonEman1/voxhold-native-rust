pub mod catalog;
pub mod controller;
mod playout_volume;
mod video;
pub mod viewer;
mod voice;

use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};

use tokio::sync::mpsc;
use video::VideoStore;
use voice::CaptureGate;

const COMMAND_CAPACITY: usize = 128;
const EVENT_CAPACITY: usize = 256;

#[derive(Clone, Debug)]
pub struct IceServer {
    pub urls: Vec<String>,

    pub username: String,

    pub credential: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub candidate: String,

    pub sdp_mid: Option<String>,

    pub sdp_mline_index: Option<u16>,

    pub username_fragment: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Controls {
    pub muted: bool,
    pub deafened: bool,
    pub input: Option<String>,
    pub output: Option<String>,
    pub bitrate_kbps: Option<u32>,
    /// Output gain in 0..=2; `None` is unity. Input gain belongs to the DSP
    /// capture path (R07b), not to this control set.
    pub output_gain: Option<f64>,
}

#[derive(Clone, Debug)]

pub enum Command {
    Start {
        ice_servers: Vec<IceServer>,

        ice_transport_policy: String,

        controls: Controls,
    },
    ViewStart {
        ice_servers: Vec<IceServer>,

        ice_transport_policy: String,
        output: Option<String>,
    },
    ViewAudio {
        muted: bool,
    },
    /// Switch the playout device of an already running stream player.
    ViewOutput {
        output: Option<String>,
    },
    Offer {
        sdp: String,
    },
    Ice(Candidate),
    State(Controls),
    Volume {
        track_id: String,
        gain: f64,
    },
    Stop,
}

#[derive(Clone, Debug)]

pub enum Event {
    Answer {
        sdp: String,
    },
    Ice(Candidate),
    State {
        state: &'static str,
    },
    Track {
        track_id: String,
        stream_id: Option<String>,
        mid: Option<String>,
    },
    Level {
        track_id: Option<String>,
        level: f64,
    },
    Frame,
    Error {
        message: &'static str,
    },
    /// A device switch failed but the previous, working path is still live.
    DeviceError {
        message: &'static str,
    },
}

#[derive(Default)]
struct Events {
    queue: Mutex<EventQueue>,
    ready: Condvar,
}

#[derive(Default)]
struct EventQueue {
    items: VecDeque<Event>,
    overflow: bool,
    frame_pending: bool,
}

impl Events {
    fn push(&self, event: Event) {
        let mut queue = self.queue.lock().unwrap();
        if queue.overflow {
            return;
        }
        if matches!(event, Event::Frame) {
            if queue.frame_pending || queue.items.len() >= EVENT_CAPACITY {
                return;
            }
            queue.frame_pending = true;
        }
        if queue.items.len() == EVENT_CAPACITY {
            if matches!(event, Event::Level { .. }) {
                return;
            }
            queue.items.clear();
            queue.overflow = true;
            queue.items.push_back(Event::Error {
                message: "Медиадвижок не успевает обработать события WebRTC.",
            });
        } else {
            queue.items.push_back(event);
        }
        self.ready.notify_one();
    }

    fn poll(&self, wait: Duration) -> Option<Event> {
        let queue = self.queue.lock().unwrap();
        let (mut queue, _) = self
            .ready
            .wait_timeout_while(queue, wait, |q| q.items.is_empty())
            .unwrap();
        let event = queue.items.pop_front()?;
        if matches!(event, Event::Frame) {
            queue.frame_pending = false;
        }
        Some(event)
    }
}

pub struct Media {
    commands: mpsc::Sender<Command>,
    events: Arc<Events>,
    capture: Arc<CaptureGate>,
    video: Arc<VideoStore>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaError {
    Runtime,
    Worker,
    CommandQueueFull,
    CommandQueueClosed,
}

impl Media {
    pub fn new() -> Result<Self, MediaError> {
        Self::new_with_capture(Arc::new(CaptureGate::default()))
    }

    /// The voice session shares its capture gate with `VoiceHandle`, so leave
    /// and push-to-talk release can silence the track without the worker.
    pub fn new_with_capture(capture: Arc<CaptureGate>) -> Result<Self, MediaError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|_| MediaError::Runtime)?;
        let (commands, rx) = mpsc::channel(COMMAND_CAPACITY);
        let events = Arc::new(Events::default());
        let video = Arc::new(VideoStore::default());
        let (worker_events, worker_capture, worker_video) =
            (events.clone(), capture.clone(), video.clone());
        let worker = thread::Builder::new()
            .name("voxhold-media".into())
            .spawn(move || {
                runtime.block_on(voice::run(rx, worker_events, worker_capture, worker_video))
            })
            .map_err(|_| MediaError::Worker)?;
        Ok(Self {
            commands,
            events,
            capture,
            video,
            worker: Some(worker),
        })
    }
    pub fn command(&self, command: Command) -> Result<(), MediaError> {
        // The microphone starts closed and only a confirmed media state opens
        // it. `Start` alone must never open the device: the join may still be
        // refused, and push-to-talk may hold the key.
        match &command {
            Command::Start { .. } | Command::ViewStart { .. } => self.capture.reset(),
            Command::Stop => self.capture.latch(),
            Command::State(controls) => {
                self.capture
                    .set_local(!controls.muted && !controls.deafened);
            }
            _ => {}
        }
        self.commands.try_send(command).map_err(|err| match err {
            mpsc::error::TrySendError::Full(_) => MediaError::CommandQueueFull,
            mpsc::error::TrySendError::Closed(_) => MediaError::CommandQueueClosed,
        })
    }
    /// Confirmed join. Separate from `State` so a late local control command
    /// cannot open the microphone on its own.
    pub fn set_joined(&self, joined: bool) {
        self.capture.set_joined(joined);
    }

    /// Server mute/deafen folded into the media permission.
    pub fn set_server_allowed(&self, allowed: bool) {
        self.capture.set_server_allowed(allowed);
    }

    pub fn poll(&self, wait: Duration) -> Option<Event> {
        self.events.poll(wait)
    }
    pub fn read_video(&self, frame: &mut VideoFrame, width: u32, height: u32) -> bool {
        frame.request.viewport_width = width;
        frame.request.viewport_height = height;
        let result = self.video.read(&mut frame.request, &mut frame.bgra);
        let result = if result < 0 {
            frame.bgra.resize(result.unsigned_abs(), 0);
            self.video.read(&mut frame.request, &mut frame.bgra)
        } else {
            result
        };
        result > 0
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        self.capture.latch();
        // Close the channel before joining the worker.
        let (replacement, _) = mpsc::channel(1);
        drop(std::mem::replace(&mut self.commands, replacement));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Default)]
pub struct VideoFrame {
    request: video::VideoRead,
    /// BGRA bytes in the layout GPUI uploads without a per-pixel swap.
    pub bgra: Vec<u8>,
}
impl VideoFrame {
    pub fn width(&self) -> u32 {
        self.request.width
    }
    pub fn height(&self) -> u32 {
        self.request.height
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn events_are_typed_and_frame_notifications_coalesce() {
        let events = Events::default();
        events.push(Event::Frame);
        events.push(Event::Frame);
        assert!(matches!(events.poll(Duration::ZERO), Some(Event::Frame)));
        assert!(events.poll(Duration::ZERO).is_none());
    }

    fn headless_media() -> (Media, Arc<CaptureGate>, mpsc::Receiver<Command>) {
        let (commands, receiver) = mpsc::channel(8);
        let capture = Arc::new(CaptureGate::default());
        (
            Media {
                commands,
                events: Arc::new(Events::default()),
                capture: capture.clone(),
                video: Arc::new(VideoStore::default()),
                worker: None,
            },
            capture,
            receiver,
        )
    }

    #[test]
    fn the_microphone_starts_closed_and_opens_only_on_a_confirmed_state() {
        let (media, capture, _commands) = headless_media();
        media
            .command(Command::Start {
                ice_servers: Vec::new(),
                ice_transport_policy: "all".into(),
                controls: Controls {
                    muted: false,
                    ..Controls::default()
                },
            })
            .unwrap();
        assert!(
            !capture.enabled(),
            "joining must not open the microphone before voice.joined"
        );
        media.set_joined(true);
        media.set_server_allowed(true);
        media
            .command(Command::State(Controls {
                muted: false,
                ..Controls::default()
            }))
            .unwrap();
        assert!(capture.enabled());
        media
            .command(Command::State(Controls {
                muted: true,
                ..Controls::default()
            }))
            .unwrap();
        assert!(!capture.enabled());
        // A late unmute after Stop must not reopen the attempt.
        media.command(Command::Stop).unwrap();
        media
            .command(Command::State(Controls {
                muted: false,
                ..Controls::default()
            }))
            .unwrap();
        assert!(!capture.enabled());
    }
}
pub mod devices;
pub mod runtime;
pub mod stream;
