//! Windows system audio for a screen publication.
//!
//! The microphone path is WinMM (`capture/winmm.rs`) and belongs to the voice
//! peer. This module is a separate capture of the default **render** endpoint
//! through WASAPI loopback, feeding one `NativeAudioSource` in the screen-share
//! peer. The two never share a track, a peer or `CaptureGate`.
//!
//! Windows offers two loopback scopes. The plain endpoint loopback captures
//! everything the default render endpoint plays, including Voxhold's own room
//! and stream player. Windows 10 2004+ and Windows 11 also expose process
//! loopback (`ActivateAudioInterfaceAsync` with
//! `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE` /
//! `EXCLUDE_TARGET_PROCESS_TREE`), which is the only way to keep the room voice
//! out of a window share or Voxhold out of a whole-screen share. Microsoft
//! documents this API from Windows 10 build 20348; an older host returns an
//! explicit activation error, and the publication fails instead of silently
//! capturing the whole endpoint. The UI and
//! this module state the real scope instead of promising echo-free audio. The
//! local «Ваш экран» preview stays a BGRA picture and never plays the captured
//! audio.
//!
//! Layout:
//! - [`AudioProducer`] is the block source the publisher engine pumps. The
//!   production implementation is [`SystemAudio`]; tests inject deterministic
//!   tone/silence blocks.
//! - the capture thread keeps a bounded queue of 10 ms stereo blocks. The
//!   engine turns an empty queue into silence, so the audio track continues
//!   even when nothing is playing.
//! - `format` converts the device mix format (commonly 48 kHz float32 stereo)
//!   into interleaved 48 kHz i16 stereo without a new dependency.
//! - `wasapi` owns COM, the endpoint or process-loopback client and the bounded
//!   reopen on a device change or removal.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::screen_capture::{ScreenSource, SourceKind};

/// The WebRTC audio pipeline and Opus use 48 kHz.
pub const SAMPLE_RATE: u32 = 48_000;
/// Interleaved stereo keeps the spatial position of the desktop mix.
pub const CHANNELS: u32 = 2;
/// One WebRTC audio frame is 10 ms.
pub const BLOCK_FRAMES: u32 = SAMPLE_RATE / 100;
/// Interleaved i16 samples of one block.
pub const BLOCK_SAMPLES: usize = BLOCK_FRAMES as usize * CHANNELS as usize;
pub const BLOCK_INTERVAL: Duration = Duration::from_millis(10);
/// Four blocks (40 ms) bound the latency and absorb a scheduling hiccup. The
/// queue drops its oldest block instead of growing.
pub const MAX_QUEUE_BLOCKS: usize = 4;

const READY_TIMEOUT: Duration = Duration::from_secs(3);
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// What the UI may show about the system-audio endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDeviceInfo {
    /// Windows endpoint id, kept for diagnostics. Not shown in the UI.
    pub id: String,
    pub label: String,
    /// Human-readable mix format, for the manual loopback test.
    pub format: String,
}

/// What the user asked the system-audio track to cover. The engine resolves it
/// to a concrete [`CaptureScope`] off the UI thread, right before the capture
/// starts, so a stale picker never captures another process.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AudioScope {
    /// The whole default render endpoint. This is the explicit
    /// "everything on the device" choice, never a silent fallback.
    #[default]
    Device,
    /// The process tree of the selected window. Requires a window source.
    WindowApplication,
    /// The whole endpoint except this client's process tree.
    EverythingExceptSelf,
}

impl AudioScope {
    /// Resolve the user's choice against the source of the same attempt. A
    /// window scope without a window source is an explicit error instead of a
    /// whole-device capture.
    pub fn resolve(self, source: &ScreenSource) -> Result<CaptureScope, String> {
        self.resolve_with(source, platform::window_process_id)
    }

    /// The pure part of [`Self::resolve`]; tests inject the window lookup.
    pub fn resolve_with(
        self,
        source: &ScreenSource,
        window_process_id: impl FnOnce(u64) -> Result<u32, String>,
    ) -> Result<CaptureScope, String> {
        match self {
            Self::Device => Ok(CaptureScope::Device),
            Self::WindowApplication => {
                if source.kind != SourceKind::Window {
                    return Err("Звук приложения можно захватить только для окна.".into());
                }
                if source.id == 0 {
                    return Err("Выбранное окно не сообщило идентификатор.".into());
                }
                Ok(CaptureScope::Process {
                    pid: window_process_id(source.id)?,
                })
            }
            Self::EverythingExceptSelf => Ok(CaptureScope::ExcludeProcess {
                pid: std::process::id(),
            }),
        }
    }
}

/// The scope with every process id resolved. Passed to the platform capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureScope {
    Device,
    Process { pid: u32 },
    ExcludeProcess { pid: u32 },
}

impl CaptureScope {
    /// Human-readable scope of the capture that really runs. The process name
    /// is read from Windows; an unavailable name falls back to the process id.
    pub fn describe(self) -> String {
        match self {
            Self::Device => "Всё устройство вывода".into(),
            Self::Process { pid } => match platform::process_name(pid) {
                Some(name) => format!("Только звук {name} (PID {pid})"),
                None => format!("Только процессы PID {pid}"),
            },
            Self::ExcludeProcess { pid } => match platform::process_name(pid) {
                Some(name) => format!("Всё, кроме {name} (PID {pid})"),
                None => format!("Всё, кроме процессов PID {pid}"),
            },
        }
    }
}

/// The publisher engine pulls one 10 ms block at a time. `None` means "no
/// sound right now" and becomes a silent frame; an error is terminal.
pub trait AudioProducer: Send {
    fn next_block(&mut self) -> Option<Vec<i16>>;
    fn failure(&self) -> Option<String>;
    fn dropped(&self) -> u64;
}

struct Shared {
    blocks: Mutex<VecDeque<Vec<i16>>>,
    stop: AtomicBool,
    failed: Mutex<Option<String>>,
    dropped: AtomicU64,
    device: Mutex<Option<AudioDeviceInfo>>,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            blocks: Mutex::new(VecDeque::with_capacity(MAX_QUEUE_BLOCKS)),
            stop: AtomicBool::new(false),
            failed: Mutex::new(None),
            dropped: AtomicU64::new(0),
            device: Mutex::new(None),
        }
    }
}

impl Shared {
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn failure(&self) -> Option<String> {
        self.failed.lock().ok().and_then(|failure| failure.clone())
    }

    fn fail(&self, message: impl Into<String>) {
        if let Ok(mut failure) = self.failed.lock() {
            if failure.is_none() {
                *failure = Some(message.into());
            }
        }
    }

    fn set_device(&self, info: AudioDeviceInfo) {
        if let Ok(mut device) = self.device.lock() {
            *device = Some(info);
        }
    }

    fn device(&self) -> Option<AudioDeviceInfo> {
        self.device.lock().ok().and_then(|device| device.clone())
    }
}

/// Keep only the newest blocks. A slow encoder loses old audio instead of
/// delaying the whole publication.
fn push_block(shared: &Shared, block: Vec<i16>) {
    let Ok(mut queue) = shared.blocks.lock() else {
        return;
    };
    if queue.len() >= MAX_QUEUE_BLOCKS {
        queue.pop_front();
        shared.dropped.fetch_add(1, Ordering::Relaxed);
    }
    queue.push_back(block);
}

fn next_block(shared: &Shared) -> Option<Vec<i16>> {
    shared.blocks.lock().ok()?.pop_front()
}

/// Reassembles converted WASAPI packets into exactly [`BLOCK_SAMPLES`]
/// blocks. A loopback packet is often shorter than 10 ms, so a per-packet
/// `chunks` loop silently dropped the tail; the remainder is carried until
/// the next packet completes it.
#[derive(Default)]
struct BlockBuffer {
    pending: Vec<i16>,
}

impl BlockBuffer {
    fn push(&mut self, samples: &[i16], mut emit: impl FnMut(Vec<i16>)) {
        self.pending.extend_from_slice(samples);
        while self.pending.len() >= BLOCK_SAMPLES {
            let tail = self.pending.split_off(BLOCK_SAMPLES);
            let block = std::mem::replace(&mut self.pending, tail);
            emit(block);
        }
    }
}

/// One loopback capture of the default render endpoint.
pub struct SystemAudio {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl SystemAudio {
    /// Open the requested scope. The call is bounded: a driver or the async
    /// process-loopback activation that never reports readiness is an error,
    /// not a hang, and never falls back to the whole endpoint.
    pub fn start(scope: CaptureScope) -> Result<Self, String> {
        let shared = Arc::new(Shared::default());
        let (ready_tx, ready_rx) = mpsc::channel();
        let worker = shared.clone();
        let thread = thread::Builder::new()
            .name("voxhold-system-audio".into())
            .spawn(move || {
                platform::run(&worker, &ready_tx, scope);
                if !worker.stopping() && worker.failure().is_none() {
                    worker.fail("Захват системного звука завершился.");
                }
            })
            .map_err(|_| "Не удалось запустить поток системного звука.".to_owned())?;
        match ready_rx.recv_timeout(READY_TIMEOUT) {
            Ok(Ok(info)) => {
                shared.set_device(info);
                Ok(Self {
                    shared,
                    thread: Some(thread),
                })
            }
            Ok(Err(error)) => {
                shared.stop.store(true, Ordering::SeqCst);
                join_bounded(thread, STOP_TIMEOUT);
                Err(error)
            }
            Err(_) => {
                shared.stop.store(true, Ordering::SeqCst);
                join_bounded(thread, STOP_TIMEOUT);
                Err(format!("Системный звук не открылся за {READY_TIMEOUT:?}."))
            }
        }
    }

    pub fn device(&self) -> Option<AudioDeviceInfo> {
        self.shared.device()
    }

    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }
}

impl AudioProducer for SystemAudio {
    fn next_block(&mut self) -> Option<Vec<i16>> {
        next_block(&self.shared)
    }

    fn failure(&self) -> Option<String> {
        self.shared.failure()
    }

    fn dropped(&self) -> u64 {
        self.dropped()
    }
}

impl Drop for SystemAudio {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            join_bounded(thread, STOP_TIMEOUT);
        }
    }
}

/// Probe the default render endpoint without opening a stream. Used by the
/// picker to show availability before the publication starts.
pub fn probe() -> Result<AudioDeviceInfo, String> {
    platform::probe()
}

/// Join without an unbounded wait. If the driver ignores the stop flag, the
/// thread is detached; its `Arc<Shared>` keeps it from touching freed memory.
fn join_bounded(thread: JoinHandle<()>, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while !thread.is_finished() {
        if Instant::now() >= deadline {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
    let _ = thread.join();
}

mod format;

#[cfg(windows)]
mod wasapi;

#[cfg(windows)]
mod platform {
    use super::*;

    pub(super) fn run(
        shared: &Arc<Shared>,
        ready: &mpsc::Sender<Result<AudioDeviceInfo, String>>,
        scope: CaptureScope,
    ) {
        wasapi::run(shared, ready, scope);
    }

    pub(super) fn probe() -> Result<AudioDeviceInfo, String> {
        wasapi::probe()
    }

    /// `GetWindowThreadProcessId` for the HWND the capturer reports as a
    /// window's source id.
    pub(super) fn window_process_id(window: u64) -> Result<u32, String> {
        wasapi::window_process_id(window)
    }

    /// File name of one process, for the honest scope label.
    pub(super) fn process_name(pid: u32) -> Option<String> {
        wasapi::process_name(pid)
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub(super) fn run(
        _shared: &Arc<Shared>,
        ready: &mpsc::Sender<Result<AudioDeviceInfo, String>>,
        _scope: CaptureScope,
    ) {
        let _ = ready.send(Err("Системный звук доступен только в Windows.".into()));
    }

    pub(super) fn probe() -> Result<AudioDeviceInfo, String> {
        Err("Системный звук доступен только в Windows.".into())
    }

    pub(super) fn window_process_id(_window: u64) -> Result<u32, String> {
        Err("Окно можно сопоставить с процессом только в Windows.".into())
    }

    pub(super) fn process_name(_pid: u32) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_keeps_the_newest_blocks_and_counts_drops() {
        let shared = Shared::default();
        for index in 0..MAX_QUEUE_BLOCKS {
            push_block(&shared, vec![index as i16; BLOCK_SAMPLES]);
        }
        push_block(&shared, vec![99; BLOCK_SAMPLES]);
        assert_eq!(shared.dropped.load(Ordering::Relaxed), 1);
        let first = next_block(&shared).expect("first retained block");
        assert_eq!(first[0], 1, "the oldest block must be the dropped one");
        let mut last = first;
        while let Some(block) = next_block(&shared) {
            last = block;
        }
        assert_eq!(last[0], 99);
        assert!(next_block(&shared).is_none());
    }

    #[test]
    fn an_empty_queue_is_silence_not_a_failure() {
        let shared = Shared::default();
        assert!(shared.failure().is_none());
        assert!(next_block(&shared).is_none());
    }

    #[test]
    fn a_failure_is_reported_once_and_is_terminal() {
        let shared = Shared::default();
        shared.fail("устройство исчезло");
        shared.fail("другая ошибка");
        assert_eq!(shared.failure().as_deref(), Some("устройство исчезло"));
    }

    fn window_source() -> ScreenSource {
        ScreenSource {
            id: 0x1234,
            kind: SourceKind::Window,
            title: "Демонстрация".into(),
            display_id: -1,
            width: 1280,
            height: 720,
        }
    }

    fn monitor_source() -> ScreenSource {
        ScreenSource {
            id: 0,
            kind: SourceKind::Monitor,
            title: "Экран 1".into(),
            display_id: -1,
            width: 1920,
            height: 1080,
        }
    }

    #[test]
    fn a_window_scope_resolves_the_window_hwnd_to_its_process() {
        let mut asked = None;
        let scope = AudioScope::WindowApplication
            .resolve_with(&window_source(), |window| {
                asked = Some(window);
                Ok(4_321)
            })
            .expect("a window has a process");
        assert_eq!(asked, Some(0x1234), "the HWND of the selected window");
        assert_eq!(scope, CaptureScope::Process { pid: 4_321 });
    }

    #[test]
    fn a_window_scope_without_a_window_is_an_error_not_the_device() {
        let error = AudioScope::WindowApplication
            .resolve_with(&monitor_source(), |_| Ok(99))
            .expect_err("a monitor has no window process");
        assert!(error.contains("окна"), "{error}");
    }

    #[test]
    fn the_exclude_scope_names_this_process_and_the_device_scope_is_explicit() {
        assert_eq!(
            AudioScope::EverythingExceptSelf
                .resolve_with(&monitor_source(), |_| Ok(1))
                .expect("the monitor scope needs no window"),
            CaptureScope::ExcludeProcess {
                pid: std::process::id()
            }
        );
        assert_eq!(
            AudioScope::Device
                .resolve_with(&monitor_source(), |_| Ok(1))
                .expect("the device scope is always valid"),
            CaptureScope::Device
        );
    }

    #[test]
    fn the_scope_description_names_the_real_area() {
        assert_eq!(CaptureScope::Device.describe(), "Всё устройство вывода");
        assert!(
            CaptureScope::Process { pid: 4_242 }
                .describe()
                .contains("4242")
        );
        assert!(
            CaptureScope::ExcludeProcess { pid: 4_242 }
                .describe()
                .contains("кроме")
        );
        assert_ne!(
            CaptureScope::Process { pid: 1 },
            CaptureScope::ExcludeProcess { pid: 1 },
            "include and exclude are different captures"
        );
    }

    fn stereo_packet(frames: usize, left: i16, right: i16) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(frames * 4);
        for _ in 0..frames {
            bytes.extend_from_slice(&left.to_le_bytes());
            bytes.extend_from_slice(&right.to_le_bytes());
        }
        bytes
    }

    /// A 240-frame packet is 5 ms. The old per-packet `chunks(960)` loop threw
    /// its 480 samples away and the next 720-frame packet lost another 480.
    /// The carry buffer must deliver two complete blocks and lose nothing.
    #[test]
    fn a_short_wasapi_packet_carries_its_tail_into_the_next_packet() {
        let format = format::SourceFormat {
            sample_rate: SAMPLE_RATE,
            channels: 2,
            bits: 16,
            block_align: 4,
            float: false,
            mask: 0x3,
        };
        let mut converter = format::Converter::new(format);
        let mut blocks = BlockBuffer::default();
        let mut delivered = Vec::new();
        let mut scratch = Vec::new();

        let first = stereo_packet(240, 1_000, -1_000);
        converter.convert(&first, false, &mut scratch);
        assert_eq!(scratch.len(), 480, "240 frames are half a block");
        blocks.push(&scratch, |block| delivered.push(block));
        assert!(delivered.is_empty(), "no partial block may be published");

        scratch.clear();
        let second = stereo_packet(720, 2_000, -2_000);
        converter.convert(&second, false, &mut scratch);
        assert_eq!(scratch.len(), 1_440, "720 frames are one and a half blocks");
        blocks.push(&scratch, |block| delivered.push(block));

        assert_eq!(delivered.len(), 2, "480 + 1440 samples are two blocks");
        assert_eq!(
            delivered.iter().map(Vec::len).sum::<usize>(),
            1_920,
            "no sample may be dropped at a packet boundary"
        );
        // Blocks are interleaved. The first 240 frames of the first block are
        // the carried packet; the rest is the second packet (whose first
        // frame interpolates from the carry at fraction 0).
        assert_eq!(&delivered[0][..2], &[1_000, -1_000]);
        assert_eq!(&delivered[0][958..], &[2_000, -2_000]);
        assert!(
            delivered[0]
                .chunks(2)
                .all(|pair| pair == [1_000, -1_000] || pair == [2_000, -2_000]),
            "the first block mixes both packets"
        );
        assert!(
            delivered[1].chunks(2).all(|pair| pair == [2_000, -2_000]),
            "the second block is entirely the later packet"
        );
    }
}

/// Manual proof against the real endpoint. It plays a short quiet 440 Hz WAV
/// through the default render device (winmm) and reads it back from WASAPI
/// loopback. Ignored because it needs a desktop with an audio output device and
/// makes a sound; run it explicitly on the stand.
#[cfg(all(test, windows))]
mod wasapi_manual {
    use super::*;
    use std::{
        thread,
        time::{Duration, Instant},
    };
    use windows::{
        Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT},
        core::PCWSTR,
    };

    fn wav_tone(frequency: f64, millis: u32) -> Vec<u8> {
        let rate = 48_000_u32;
        let frames = rate * millis / 1000;
        let mut data = Vec::with_capacity(frames as usize * 2);
        for frame in 0..frames {
            let value =
                (2.0 * std::f64::consts::PI * frequency * f64::from(frame) / f64::from(rate)).sin();
            let sample = (value * 8_000.0) as i16;
            data.extend_from_slice(&sample.to_le_bytes());
        }
        let mut wav = Vec::with_capacity(44 + data.len());
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 2).to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        wav
    }

    fn collect(capture: &mut SystemAudio, millis: u64) -> (u64, i32) {
        let deadline = Instant::now() + Duration::from_millis(millis);
        let (mut blocks, mut peak) = (0_u64, 0_i32);
        while Instant::now() < deadline {
            if let Some(block) = capture.next_block() {
                blocks += 1;
                peak = peak.max(
                    block
                        .iter()
                        .map(|sample| i32::from(*sample).abs())
                        .max()
                        .unwrap_or(0),
                );
            }
            thread::sleep(Duration::from_millis(5));
        }
        (blocks, peak)
    }

    #[test]
    #[ignore = "manual: opens the real render endpoint and plays a short tone"]
    fn wasapi_loopback_captures_a_played_tone() {
        let device = probe().expect("default render endpoint");
        println!(
            "wasapi: device={} format={} id={}",
            device.label, device.format, device.id
        );
        let mut capture = SystemAudio::start(CaptureScope::Device).expect("loopback start");
        if let Some(opened) = capture.device() {
            println!("wasapi: opened {}", opened.format);
        }
        let (baseline_blocks, baseline_peak) = collect(&mut capture, 400);
        println!("wasapi: baseline blocks={baseline_blocks} peak={baseline_peak}");

        let wav = wav_tone(440.0, 1000);
        let played = unsafe {
            PlaySoundW(
                PCWSTR(wav.as_ptr().cast()),
                None,
                SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
            )
        };
        println!("wasapi: PlaySoundW played={}", played.as_bool());
        let (blocks, peak) = collect(&mut capture, 1200);
        println!(
            "wasapi: tone blocks={blocks} peak={peak} dropped={} failure={:?}",
            capture.dropped(),
            capture.failure()
        );
        // Keep the WAV alive for the asynchronous winmm playback.
        drop(wav);
        assert!(played.as_bool(), "winmm did not accept the in-memory WAV");
        assert!(blocks > 0, "loopback delivered no block during the tone");
        assert!(
            peak > 500,
            "loopback delivered silence (peak {peak}); check the default output device"
        );
    }

    /// Manual proof of the window scope: process loopback INCLUDE of this test
    /// process must carry the tone this process plays. Needs Windows 10 2004+
    /// or Windows 11. No other application has to be quiet for the positive
    /// assertion; the negative one is checked by the next test.
    #[test]
    #[ignore = "manual: requires Windows process loopback and plays a short tone"]
    fn process_loopback_of_this_process_captures_its_own_tone() {
        let pid = std::process::id();
        let mut capture = SystemAudio::start(CaptureScope::Process { pid })
            .expect("process loopback INCLUDE this process");
        if let Some(opened) = capture.device() {
            println!(
                "wasapi: opened process scope {} ({})",
                opened.label, opened.format
            );
        }
        let (baseline_blocks, baseline_peak) = collect(&mut capture, 400);
        println!("wasapi: process baseline blocks={baseline_blocks} peak={baseline_peak}");

        let wav = wav_tone(440.0, 1000);
        let played = unsafe {
            PlaySoundW(
                PCWSTR(wav.as_ptr().cast()),
                None,
                SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
            )
        };
        let (blocks, peak) = collect(&mut capture, 1200);
        println!(
            "wasapi: process tone blocks={blocks} peak={peak} dropped={} failure={:?}",
            capture.dropped(),
            capture.failure()
        );
        drop(wav);
        assert!(played.as_bool(), "winmm did not accept the in-memory WAV");
        assert!(
            blocks > 0,
            "process loopback delivered no block during the tone"
        );
        assert!(
            peak > 500,
            "process loopback did not carry this process's own tone (peak {peak}); \
             record the host Windows version and HRESULT instead of falling back"
        );
    }

    /// The whole-screen scope must not carry Voxhold's own playback. Ignored:
    /// it plays a tone from this process and needs process loopback EXCLUDE.
    #[test]
    #[ignore = "manual: requires Windows process loopback and plays a short tone"]
    fn the_exclude_scope_does_not_capture_this_process() {
        let pid = std::process::id();
        let mut capture = SystemAudio::start(CaptureScope::ExcludeProcess { pid })
            .expect("process loopback EXCLUDE this process");
        let (baseline_blocks, baseline_peak) = collect(&mut capture, 400);
        println!("wasapi: exclude baseline blocks={baseline_blocks} peak={baseline_peak}");

        let wav = wav_tone(440.0, 1000);
        let played = unsafe {
            PlaySoundW(
                PCWSTR(wav.as_ptr().cast()),
                None,
                SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
            )
        };
        let (blocks, peak) = collect(&mut capture, 1200);
        println!(
            "wasapi: exclude tone blocks={blocks} peak={peak} dropped={} failure={:?}",
            capture.dropped(),
            capture.failure()
        );
        drop(wav);
        assert!(played.as_bool(), "winmm did not accept the in-memory WAV");
        assert!(blocks > 0, "the exclude stream must still deliver silence");
        assert!(
            peak < 500,
            "the exclude scope carried this process's own tone (peak {peak}); \
             this is the defect the scope must prevent"
        );
    }

    /// An unknown target process must be an explicit error, never a fallback to
    /// the whole endpoint. PID 0 is the system idle process and is not a valid
    /// process-loopback target.
    #[test]
    fn process_loopback_with_an_invalid_target_fails_instead_of_falling_back() {
        let result = SystemAudio::start(CaptureScope::Process { pid: 0 });
        match result {
            Ok(capture) => {
                drop(capture);
                panic!("an invalid process target must not open a capture");
            }
            Err(error) => {
                println!("wasapi: process loopback rejected PID 0: {error}");
                assert!(
                    !error.contains("Устройство вывода по умолчанию"),
                    "the failure must not describe a whole-device capture: {error}"
                );
            }
        }
    }
}
