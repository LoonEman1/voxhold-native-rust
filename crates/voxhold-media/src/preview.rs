//! WinMM microphone preview and interface test tone.
//!
//! This is a direct port of the Go `cmd/voxhold/audio_windows.go` helpers:
//! three 50 ms buffers (`rate / 20` samples) polled every 50 ms, RMS with the
//! `min(1, sqrt(energy / samples) * 4.5)` scale and the 0.45/0.55 smoothing;
//! the tone is `0.55·sin(2π660t) + 0.45·sin(2π880t)` with the Go envelope over
//! `rate / 4` samples and a 350 ms wait. The preview never enters the WebRTC
//! transmission; it only feeds the settings meter.

use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicU32, Ordering},
};

/// Go `min(1, math.Sqrt(...)*4.5)` display scale.
pub const LEVEL_GAIN: f64 = 4.5;

/// Go smoothing: `smoothed*.45 + level*.55`.
pub fn smooth_level(previous: f64, measured: f64) -> f64 {
    previous * 0.45 + measured * 0.55
}

/// `sqrt(energy / samples)` over normalized i16 samples. An empty buffer is
/// silence, never a panic.
pub fn rms(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let energy: f64 = samples
        .iter()
        .map(|&sample| {
            let normalized = sample as f64 / 32768.0;
            normalized * normalized
        })
        .sum();
    (energy / samples.len() as f64).sqrt()
}

/// Display level from an RMS value, clamped to the 0..=1 meter range.
pub fn level_from_rms(rms: f64) -> f32 {
    (rms.max(0.0) * LEVEL_GAIN).min(1.0) as f32
}

/// Exact Go test tone. `volume` is the 0–100 signal volume.
pub fn tone_samples(rate: u32, volume: i64) -> Vec<i16> {
    let count = (rate / 4) as usize;
    let volume = volume.clamp(0, 100) as f64;
    (0..count)
        .map(|index| {
            let t = index as f64 / rate as f64;
            let envelope = (t / 0.012).min(1.0).min((0.25 - t) / 0.06);
            let tone = 0.55 * (2.0 * std::f64::consts::PI * 660.0 * t).sin()
                + 0.45 * (2.0 * std::f64::consts::PI * 880.0 * t).sin();
            (tone * envelope * 16000.0 * volume / 100.0) as i16
        })
        .collect()
}

/// One tone of a short interface cue, in the frontend's Web Audio terms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CueTone {
    pub frequency: f64,
    /// Seconds from the start of the cue.
    pub at: f64,
    pub duration: f64,
}

/// A synthesized interface cue. Every current frontend cue is a sine pair, so
/// the oscillator type is fixed to sine here.
#[derive(Clone, Debug, PartialEq)]
pub struct CueSpec {
    pub gain: f64,
    pub tones: Vec<CueTone>,
}

/// Attack ramp of `uiSounds.ts`: 0.0001 → 1 over 12 ms, then exponential back
/// to 0.0001 at the end of the tone.
const CUE_ATTACK: f64 = 0.012;
const CUE_FLOOR: f64 = 0.0001;

fn cue_envelope(local: f64, duration: f64) -> f64 {
    if local <= 0.0 || local >= duration {
        return 0.0;
    }
    if local < CUE_ATTACK {
        CUE_FLOOR * (1.0 / CUE_FLOOR).powf(local / CUE_ATTACK)
    } else {
        let release = duration - CUE_ATTACK;
        if release <= 0.0 {
            return 1.0;
        }
        CUE_FLOOR.powf((local - CUE_ATTACK) / release)
    }
}

/// Render one cue at the device rate. `volume` is the saved 0–100 interface
/// volume; the frontend multiplies the master gain by `min(1, volume / 100)`.
/// Zero volume is an empty buffer, never a silent waveform.
pub fn cue_samples(spec: &CueSpec, rate: u32, volume: i64) -> Vec<i16> {
    if rate == 0 || spec.tones.is_empty() {
        return Vec::new();
    }
    let volume = volume.clamp(0, 100) as f64;
    if volume <= 0.0 {
        return Vec::new();
    }
    let length = spec
        .tones
        .iter()
        .map(|tone| tone.at.max(0.0) + tone.duration.max(0.0))
        .fold(0.0_f64, f64::max);
    let count = (length * rate as f64).round() as usize;
    let mut mix = vec![0.0_f64; count];
    for tone in &spec.tones {
        let begin = tone.at.max(0.0);
        let end = begin + tone.duration.max(0.0);
        // Samples are evaluated at their own time, so the oscillator phase
        // starts at zero on `begin` exactly like a Web Audio oscillator.
        for (index, slot) in mix.iter_mut().enumerate() {
            let time = index as f64 / rate as f64;
            if time < begin || time >= end {
                continue;
            }
            let local = time - begin;
            *slot += (2.0 * std::f64::consts::PI * tone.frequency * local).sin()
                * cue_envelope(local, tone.duration);
        }
    }
    let scale = spec.gain * volume / 100.0 * 32767.0;
    mix.into_iter()
        .map(|value| (value * scale).clamp(-32768.0, 32767.0) as i16)
        .collect()
}

/// WinMM device id is `"index:name"`; an empty id means the system endpoint.
pub const WAVE_MAPPER: u32 = 0xffff_ffff;

/// `None` when the exact id and the fallback name both miss.
pub fn resolve_device(id: &str, devices: &[(u32, String)]) -> Option<u32> {
    if id.is_empty() {
        return Some(WAVE_MAPPER);
    }
    // Go compares the full `"index:name"` id first, so a stale index with a
    // different name is not accepted.
    if let Some((index, _)) = devices
        .iter()
        .find(|(index, name)| format!("{index}:{name}") == id)
    {
        return Some(*index);
    }
    let name = id.split_once(':').map(|(_, name)| name).unwrap_or("");
    devices
        .iter()
        .find(|(_, candidate)| candidate == name)
        .map(|(index, _)| *index)
}

/// Endpoint ids do not contain the WinMM display name. Resolve them only when
/// one current input has that name; two identical microphones are ambiguous.
pub fn unique_index_by_name(name: &str, devices: &[(u32, String)]) -> Option<u32> {
    if name.is_empty() {
        return None;
    }
    let mut matches = devices
        .iter()
        .filter(|(_, candidate)| candidate == name)
        .map(|(index, _)| *index);
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

/// Live WinMM preview owned by the settings tab. `stop` only signals; the
/// thread observes it within one 50 ms tick and exits on its own, so closing
/// the dialog never blocks the UI thread.
pub struct LevelPreview {
    stop: Arc<AtomicBool>,
    level: Arc<AtomicU32>,
    error: Arc<Mutex<Option<String>>>,
    #[cfg(windows)]
    thread: Option<std::thread::JoinHandle<()>>,
}

impl LevelPreview {
    /// Last smoothed level in 0..=1.
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    /// First open/capture error, if the device could not be used.
    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|error| error.clone())
    }

    /// Signal the thread and detach it; joining would block the UI.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        #[cfg(windows)]
        if let Some(thread) = self.thread.take() {
            drop(thread);
        }
    }
}

impl Drop for LevelPreview {
    fn drop(&mut self) {
        self.stop();
    }
}

pub const UNAVAILABLE_DEVICE: &str =
    "Выбранное аудиоустройство недоступно.";

/// Non-blocking local interface-cue player.
///
/// One background thread owns the WinMM device. Cues share a single pending
/// slot, so a burst of toggles can never grow a queue: a newer cue replaces one
/// that has not started yet, and playback never overlaps. `play` only stores a
/// request and wakes the worker, so the UI thread never opens a device or
/// sleeps. Dropping the player signals the worker and stops the waveform within
/// one poll tick; the thread is detached so shutdown never blocks.
pub struct CuePlayer {
    shared: Arc<CueShared>,
    #[cfg(windows)]
    _worker: Option<std::thread::JoinHandle<()>>,
}

struct CueJob {
    spec: CueSpec,
    volume: i64,
    device: String,
}

#[derive(Default)]
struct CueState {
    pending: Option<CueJob>,
    stopping: bool,
}

#[derive(Default)]
struct CueShared {
    state: Mutex<CueState>,
    ready: Condvar,
    stop: AtomicBool,
}

impl CuePlayer {
    pub fn new() -> Self {
        let shared = Arc::new(CueShared::default());
        #[cfg(windows)]
        let worker = {
            let worker_shared = shared.clone();
            std::thread::Builder::new()
                .name("voxhold-ui-cue".into())
                .spawn(move || cue_worker(worker_shared))
                .ok()
        };
        #[cfg(not(windows))]
        let worker = ();
        let player = Self {
            shared,
            #[cfg(windows)]
            _worker: worker,
        };
        #[cfg(windows)]
        if player._worker.is_none() {
            // Without a worker the slot would never drain; mark the player dead
            // so `play` is an explicit no-op instead of a leak.
            player.shared.stop.store(true, Ordering::Relaxed);
        }
        #[cfg(not(windows))]
        let _ = worker;
        player
    }

    /// Queue one cue. The device id is resolved on the worker, exactly like the
    /// settings test tone; an unknown id stays silent instead of falling back to
    /// another output.
    pub fn play(&self, spec: CueSpec, volume: i64, device_id: &str) {
        queue_cue(&self.shared, spec, volume, device_id);
    }

    /// Test-only player without a worker: the pending slot stays observable
    /// until the test inspects it.
    #[cfg(test)]
    fn detached() -> Self {
        Self {
            shared: Arc::new(CueShared::default()),
            #[cfg(windows)]
            _worker: None,
        }
    }
}

/// Shared by `play` and the Drop/stop test, so the guard that a stopped player
/// cannot be revived is exercised without spawning a thread.
fn queue_cue(shared: &CueShared, spec: CueSpec, volume: i64, device_id: &str) {
    if volume <= 0 || shared.stop.load(Ordering::Relaxed) {
        return;
    }
    let Ok(mut state) = shared.state.lock() else {
        return;
    };
    if state.stopping {
        return;
    }
    state.pending = Some(CueJob {
        spec,
        volume: volume.clamp(0, 100),
        device: device_id.to_owned(),
    });
    drop(state);
    shared.ready.notify_one();
}

impl Default for CuePlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CuePlayer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Ok(mut state) = self.shared.state.lock() {
            state.stopping = true;
            state.pending = None;
        }
        self.shared.ready.notify_all();
    }
}

#[cfg(windows)]
fn cue_worker(shared: Arc<CueShared>) {
    loop {
        let job = {
            let Ok(mut state) = shared.state.lock() else {
                return;
            };
            while state.pending.is_none() && !state.stopping && !shared.stop.load(Ordering::Relaxed)
            {
                state = match shared.ready.wait(state) {
                    Ok(state) => state,
                    Err(error) => error.into_inner(),
                };
            }
            if state.stopping || shared.stop.load(Ordering::Relaxed) {
                return;
            }
            state.pending.take()
        };
        if let Some(job) = job {
            windows::play_cue(&job, &shared.stop);
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::mem::{size_of, zeroed};
    use std::time::Duration;

    const WAVE_FORMAT_PCM: u16 = 1;
    const WHDR_DONE: u32 = 1;
    const CALLBACK_NULL: u32 = 0;

    #[repr(C)]
    struct WaveInCaps {
        manufacturer: u16,
        product: u16,
        version: u32,
        name: [u16; 32],
        formats: u32,
        channels: u16,
        reserved: u16,
    }

    #[repr(C)]
    struct WaveOutCaps {
        manufacturer: u16,
        product: u16,
        version: u32,
        name: [u16; 32],
        formats: u32,
        channels: u16,
        reserved: u16,
        support: u32,
    }

    #[repr(C)]
    struct WaveFormat {
        format: u16,
        channels: u16,
        samples: u32,
        bytes: u32,
        align: u16,
        bits: u16,
        extra: u16,
    }

    #[repr(C)]
    struct WaveHdr {
        data: *mut u8,
        length: u32,
        used: u32,
        user: usize,
        flags: u32,
        loops: u32,
        next: *mut WaveHdr,
        reserved: usize,
    }

    #[link(name = "winmm")]
    unsafe extern "system" {
        fn waveInGetNumDevs() -> u32;
        fn waveOutGetNumDevs() -> u32;
        fn waveInGetDevCapsW(id: usize, caps: *mut WaveInCaps, size: u32) -> u32;
        fn waveOutGetDevCapsW(id: usize, caps: *mut WaveOutCaps, size: u32) -> u32;
        fn waveInOpen(
            handle: *mut usize,
            device: u32,
            format: *const WaveFormat,
            callback: usize,
            instance: usize,
            flags: u32,
        ) -> u32;
        fn waveOutOpen(
            handle: *mut usize,
            device: u32,
            format: *const WaveFormat,
            callback: usize,
            instance: usize,
            flags: u32,
        ) -> u32;
        fn waveInClose(handle: usize) -> u32;
        fn waveOutClose(handle: usize) -> u32;
        fn waveInPrepareHeader(handle: usize, header: *mut WaveHdr, size: u32) -> u32;
        fn waveInUnprepareHeader(handle: usize, header: *mut WaveHdr, size: u32) -> u32;
        fn waveOutPrepareHeader(handle: usize, header: *mut WaveHdr, size: u32) -> u32;
        fn waveOutUnprepareHeader(handle: usize, header: *mut WaveHdr, size: u32) -> u32;
        fn waveInReset(handle: usize) -> u32;
        fn waveOutReset(handle: usize) -> u32;
        fn waveInAddBuffer(handle: usize, header: *mut WaveHdr, size: u32) -> u32;
        fn waveOutWrite(handle: usize, header: *mut WaveHdr, size: u32) -> u32;
        fn waveInStart(handle: usize) -> u32;
    }

    fn label(name: &[u16]) -> String {
        String::from_utf16_lossy(&name[..name.iter().position(|c| *c == 0).unwrap_or(name.len())])
    }

    pub(super) fn input_devices() -> Vec<(u32, String)> {
        let mut devices = Vec::new();
        unsafe {
            for index in 0..waveInGetNumDevs() {
                let mut caps: WaveInCaps = zeroed();
                if waveInGetDevCapsW(index as usize, &mut caps, size_of::<WaveInCaps>() as u32) == 0
                {
                    devices.push((index, label(&caps.name)));
                }
            }
        }
        devices
    }

    pub(super) fn output_devices() -> Vec<(u32, String)> {
        let mut devices = Vec::new();
        unsafe {
            for index in 0..waveOutGetNumDevs() {
                let mut caps: WaveOutCaps = zeroed();
                if waveOutGetDevCapsW(index as usize, &mut caps, size_of::<WaveOutCaps>() as u32)
                    == 0
                {
                    devices.push((index, label(&caps.name)));
                }
            }
        }
        devices
    }

    struct Opened {
        handle: usize,
        rate: u32,
    }

    fn format(rate: u32) -> WaveFormat {
        WaveFormat {
            format: WAVE_FORMAT_PCM,
            channels: 1,
            samples: rate,
            bytes: rate * 2,
            align: 2,
            bits: 16,
            extra: 0,
        }
    }

    fn open_device(device: u32, input: bool) -> Result<Opened, String> {
        let mut code = 0;
        for rate in [48_000_u32, 44_100] {
            let wave_format = format(rate);
            let mut handle: usize = 0;
            code = unsafe {
                if input {
                    waveInOpen(&mut handle, device, &wave_format, 0, 0, CALLBACK_NULL)
                } else {
                    waveOutOpen(&mut handle, device, &wave_format, 0, 0, CALLBACK_NULL)
                }
            };
            if code == 0 {
                return Ok(Opened { handle, rate });
            }
        }
        if code == 4 || code == 11 {
            return Err(
                "Нет доступа к устройству. Проверьте разрешения и настройки звука Windows.".into(),
            );
        }
        Err(format!(
            "Не удалось открыть аудиоустройство Windows ({code})."
        ))
    }

    fn wave_error(code: u32) -> String {
        format!("Аудиоустройство вернуло ошибку {code}.")
    }

    pub(super) fn run_level(preview: &LevelPreview, device: u32) -> Result<(), String> {
        let opened = open_device(device, true)?;
        let handle = opened.handle;
        let rate = opened.rate;
        // Heap allocations stay at a fixed address while WinMM owns them.
        let mut buffers: Vec<Vec<i16>> =
            (0..3).map(|_| vec![0_i16; (rate / 20) as usize]).collect();
        let mut headers: Vec<Box<WaveHdr>> = (0..3)
            .map(|_| {
                Box::new(WaveHdr {
                    data: std::ptr::null_mut(),
                    length: 0,
                    used: 0,
                    user: 0,
                    flags: 0,
                    loops: 0,
                    next: std::ptr::null_mut(),
                    reserved: 0,
                })
            })
            .collect();
        let mut prepared = 0_usize;
        let result = (|| -> Result<(), String> {
            for index in 0..buffers.len() {
                let header = &mut *headers[index];
                header.data = buffers[index].as_mut_ptr() as *mut u8;
                header.length = (buffers[index].len() * size_of::<i16>()) as u32;
                let code =
                    unsafe { waveInPrepareHeader(handle, header, size_of::<WaveHdr>() as u32) };
                if code != 0 {
                    return Err(wave_error(code));
                }
                prepared += 1;
                let code = unsafe { waveInAddBuffer(handle, header, size_of::<WaveHdr>() as u32) };
                if code != 0 {
                    return Err(wave_error(code));
                }
            }
            let code = unsafe { waveInStart(handle) };
            if code != 0 {
                return Err(format!(
                    "Не удалось запустить микрофон Windows ({code})."
                ));
            }
            let mut smoothed = 0.0_f64;
            while !preview.stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(50));
                if preview.stop.load(Ordering::Relaxed) {
                    break;
                }
                let mut energy = 0.0_f64;
                let mut samples = 0_usize;
                for index in 0..buffers.len() {
                    let header = &mut *headers[index];
                    // The driver writes these fields between our ticks; read
                    // them as volatile like the Go `atomic.LoadUint32` on
                    // WHDR_DONE and the byte count.
                    let flags = unsafe { std::ptr::read_volatile(&header.flags) };
                    if flags & WHDR_DONE == 0 {
                        continue;
                    }
                    let recorded = unsafe { std::ptr::read_volatile(&header.used) };
                    let used = buffers[index].len().min((recorded / 2) as usize);
                    for &sample in &buffers[index][..used] {
                        let normalized = sample as f64 / 32768.0;
                        energy += normalized * normalized;
                    }
                    samples += used;
                    let code =
                        unsafe { waveInAddBuffer(handle, header, size_of::<WaveHdr>() as u32) };
                    if code != 0 {
                        return Err(wave_error(code));
                    }
                }
                if samples > 0 {
                    let level = level_from_rms((energy / samples as f64).sqrt()) as f64;
                    smoothed = smooth_level(smoothed, level);
                    preview
                        .level
                        .store((smoothed as f32).to_bits(), Ordering::Relaxed);
                }
            }
            Ok(())
        })();
        unsafe {
            waveInReset(handle);
            for header in headers.iter_mut().take(prepared) {
                let _ = waveInUnprepareHeader(handle, &mut **header, size_of::<WaveHdr>() as u32);
            }
            waveInClose(handle);
        }
        result
    }

    pub(super) fn play_tone_device(device: u32, volume: i64) -> Result<(), String> {
        let opened = open_device(device, false)?;
        let handle = opened.handle;
        let mut samples = tone_samples(opened.rate, volume);
        let mut header = Box::new(WaveHdr {
            data: samples.as_mut_ptr() as *mut u8,
            length: (samples.len() * size_of::<i16>()) as u32,
            used: 0,
            user: 0,
            flags: 0,
            loops: 0,
            next: std::ptr::null_mut(),
            reserved: 0,
        });
        let code =
            unsafe { waveOutPrepareHeader(handle, &mut *header, size_of::<WaveHdr>() as u32) };
        if code != 0 {
            unsafe {
                waveOutClose(handle);
            }
            return Err(wave_error(code));
        }
        let code = unsafe { waveOutWrite(handle, &mut *header, size_of::<WaveHdr>() as u32) };
        if code != 0 {
            unsafe {
                waveOutReset(handle);
                waveOutUnprepareHeader(handle, &mut *header, size_of::<WaveHdr>() as u32);
                waveOutClose(handle);
            }
            return Err(wave_error(code));
        }
        std::thread::sleep(Duration::from_millis(350));
        unsafe {
            waveOutReset(handle);
            waveOutUnprepareHeader(handle, &mut *header, size_of::<WaveHdr>() as u32);
            waveOutClose(handle);
        }
        Ok(())
    }

    /// Play one interface cue. The header stays alive until the driver reports
    /// completion; a stop request (player dropped) resets the device and returns
    /// within one poll tick. Errors stay silent because a cue is optional.
    pub(super) fn play_cue(job: &CueJob, stop: &AtomicBool) {
        let devices = output_devices();
        let Some(device) = resolve_device(&job.device, &devices) else {
            return;
        };
        let _ = play_cue_device(device, &job.spec, job.volume, stop);
    }

    fn play_cue_device(
        device: u32,
        spec: &CueSpec,
        volume: i64,
        stop: &AtomicBool,
    ) -> Result<(), String> {
        let opened = open_device(device, false)?;
        let handle = opened.handle;
        let mut samples = cue_samples(spec, opened.rate, volume);
        if samples.is_empty() {
            unsafe {
                waveOutClose(handle);
            }
            return Ok(());
        }
        let mut header = Box::new(WaveHdr {
            data: samples.as_mut_ptr() as *mut u8,
            length: (samples.len() * size_of::<i16>()) as u32,
            used: 0,
            user: 0,
            flags: 0,
            loops: 0,
            next: std::ptr::null_mut(),
            reserved: 0,
        });
        let code =
            unsafe { waveOutPrepareHeader(handle, &mut *header, size_of::<WaveHdr>() as u32) };
        if code != 0 {
            unsafe {
                waveOutClose(handle);
            }
            return Err(wave_error(code));
        }
        let code = unsafe { waveOutWrite(handle, &mut *header, size_of::<WaveHdr>() as u32) };
        if code != 0 {
            unsafe {
                waveOutReset(handle);
                waveOutUnprepareHeader(handle, &mut *header, size_of::<WaveHdr>() as u32);
                waveOutClose(handle);
            }
            return Err(wave_error(code));
        }
        // Poll completion the same way the preview loop polls capture buffers.
        // The hard deadline is a safety net for a driver that never sets
        // WHDR_DONE; no PCM or device name is logged.
        let deadline = std::time::Instant::now() + Duration::from_millis(1_500);
        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let flags = unsafe { std::ptr::read_volatile(&header.flags) };
            if flags & WHDR_DONE != 0 || std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        unsafe {
            waveOutReset(handle);
            waveOutUnprepareHeader(handle, &mut *header, size_of::<WaveHdr>() as u32);
            waveOutClose(handle);
        }
        Ok(())
    }
}

#[cfg(windows)]
pub fn preview_available() -> bool {
    !windows::input_devices().is_empty()
}

#[cfg(not(windows))]
pub fn preview_available() -> bool {
    false
}

#[cfg(windows)]
pub fn tone_available() -> bool {
    !windows::output_devices().is_empty()
}

#[cfg(not(windows))]
pub fn tone_available() -> bool {
    false
}

#[cfg(windows)]
fn resolve_input_device(id: &str) -> Option<u32> {
    let devices = windows::input_devices();
    if let Some(index) = resolve_device(id, &devices) {
        return Some(index);
    }
    // New preferences store the Core Audio endpoint id (R07b). Resolve it to
    // the WinMM index only when the display name identifies one device.
    let name = crate::devices::endpoint_name(id)?;
    unique_index_by_name(&name, &devices)
}

/// Start the level preview on the selected WinMM input. An unknown id is
/// rejected before any thread is spawned; open/capture failures land in
/// `LevelPreview::error`.
#[cfg(windows)]
pub fn start_level(device_id: &str) -> Result<LevelPreview, String> {
    let Some(device) = resolve_input_device(device_id) else {
        return Err(UNAVAILABLE_DEVICE.into());
    };
    let stop = Arc::new(AtomicBool::new(false));
    let level = Arc::new(AtomicU32::new(0));
    let error = Arc::new(Mutex::new(None));
    let (worker_stop, worker_level, worker_error) = (stop.clone(), level.clone(), error.clone());
    let thread = std::thread::Builder::new()
        .name("voxhold-voice-preview".into())
        .spawn(move || {
            let worker = LevelPreview {
                stop: worker_stop,
                level: worker_level,
                error: worker_error,
                thread: None,
            };
            if let Err(message) = windows::run_level(&worker, device) {
                store_shared_error(&worker.error, message);
            }
        })
        .map_err(|error| format!("Не удалось запустить проверку микрофона: {error}"))?;
    Ok(LevelPreview {
        stop,
        level,
        error,
        thread: Some(thread),
    })
}

#[cfg(not(windows))]
pub fn start_level(_device_id: &str) -> Result<LevelPreview, String> {
    Err("Проверка микрофона доступна только в Windows.".into())
}

/// Play the Go interface tone on the selected WinMM output and wait 350 ms.
#[cfg(windows)]
pub fn play_tone(device_id: &str, volume: i64) -> Result<(), String> {
    let devices = windows::output_devices();
    let Some(device) = resolve_device(device_id, &devices) else {
        return Err(UNAVAILABLE_DEVICE.into());
    };
    windows::play_tone_device(device, volume)
}

#[cfg(not(windows))]
pub fn play_tone(_device_id: &str, _volume: i64) -> Result<(), String> {
    Err("Проверка сигналов доступна только в Windows.".into())
}

#[cfg(windows)]
fn store_shared_error(error: &Mutex<Option<String>>, message: String) {
    if let Ok(mut slot) = error.lock() {
        if slot.is_none() {
            *slot = Some(message);
        }
    }
}

#[cfg(test)]
mod tests;
