//! WinMM `waveOut` sink for the software playout mixer.
//!
//! Four 10 ms buffers at 48 kHz mono 16-bit; a buffer is only refilled after
//! the driver reports `WHDR_DONE`, so the device clock paces the mixer. The
//! same polling style is used by `capture::winmm` for the microphone.

use std::{mem::size_of, ptr, thread::sleep, time::Duration};

const WAVE_FORMAT_PCM: u16 = 1;
const CALLBACK_NULL: usize = 0;
const WHDR_DONE: u32 = 0x0000_0001;
const BUFFERS: usize = 4;
const CLOSE_RETRIES: usize = 25;

#[repr(C)]
struct WaveFormatEx {
    format_tag: u16,
    channels: u16,
    samples_per_sec: u32,
    avg_bytes_per_sec: u32,
    block_align: u16,
    bits_per_sample: u16,
    cb_size: u16,
}

#[repr(C)]
struct WaveHdr {
    data: *mut u8,
    buffer_length: u32,
    bytes_recorded: u32,
    user: usize,
    flags: u32,
    loops: u32,
    next: *mut WaveHdr,
    reserved: usize,
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

#[link(name = "winmm")]
unsafe extern "system" {
    fn waveOutOpen(
        handle: *mut usize,
        device: u32,
        format: *const WaveFormatEx,
        callback: usize,
        instance: usize,
        flags: u32,
    ) -> u32;
    fn waveOutClose(handle: usize) -> u32;
    fn waveOutPrepareHeader(handle: usize, header: *mut WaveHdr, size: u32) -> u32;
    fn waveOutUnprepareHeader(handle: usize, header: *mut WaveHdr, size: u32) -> u32;
    fn waveOutWrite(handle: usize, header: *mut WaveHdr, size: u32) -> u32;
    fn waveOutReset(handle: usize) -> u32;
    fn waveOutGetNumDevs() -> u32;
    fn waveOutGetDevCapsW(id: usize, caps: *mut WaveOutCaps, size: u32) -> u32;
}

/// `None` if the id names no current WinMM output. An empty id is the Windows
/// default endpoint (`WAVE_MAPPER`), which Windows resolves at open time.
pub(super) const WAVE_MAPPER: u32 = 0xFFFF_FFFF;

pub(super) fn resolve_output(id: &str) -> Option<u32> {
    if id.is_empty() {
        return Some(WAVE_MAPPER);
    }
    let devices = output_devices();
    if let Some((index, _)) = devices
        .iter()
        .find(|(index, name)| format!("{index}:{name}") == id)
    {
        return Some(*index);
    }
    let wanted = id.split_once(':').map(|(_, name)| name).unwrap_or(id);
    let mut matches = devices
        .iter()
        .filter(|(_, name)| name == wanted)
        .map(|(index, _)| *index);
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn output_devices() -> Vec<(u32, String)> {
    let mut devices = Vec::new();
    for index in 0..unsafe { waveOutGetNumDevs() } {
        let mut caps: WaveOutCaps = unsafe { std::mem::zeroed() };
        if unsafe { waveOutGetDevCapsW(index as usize, &mut caps, size_of::<WaveOutCaps>() as u32) }
            == 0
        {
            let end = caps
                .name
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(caps.name.len());
            devices.push((index, String::from_utf16_lossy(&caps.name[..end])));
        }
    }
    devices
}

/// The sink behind the software mixer. A trait so the recovery tests can
/// inject an open sequence without a Windows device.
pub(super) trait Sink: Send {
    fn writable(&self) -> bool;
    fn write(&mut self, block: &[i16]) -> bool;
    fn failed(&self) -> bool;
    /// Whole 10 ms blocks the device still holds (used for the latency report).
    fn buffered_blocks(&self) -> u16;
    fn close(&mut self);
}

/// Opens one sink. The playout keeps the factory so a resume/device switch uses
/// the same implementation (the unit tests substitute a fake).
pub(super) type OpenSink = Box<dyn Fn(u32, u32) -> Result<Box<dyn Sink>, String> + Send + Sync>;

pub(super) fn default_open(index: u32, rate: u32) -> Result<Box<dyn Sink>, String> {
    Ok(Box::new(WinOut::open(index, rate)?))
}

impl Sink for WinOut {
    fn writable(&self) -> bool {
        WinOut::writable(self)
    }

    fn write(&mut self, block: &[i16]) -> bool {
        WinOut::write(self, block)
    }

    fn failed(&self) -> bool {
        WinOut::failed(self)
    }

    fn buffered_blocks(&self) -> u16 {
        (0..BUFFERS)
            .filter(|index| {
                self.in_flight[*index] && {
                    let flags =
                        unsafe { ptr::read_volatile(&self.headers[*index].flags as *const u32) };
                    flags & WHDR_DONE == 0
                }
            })
            .count() as u16
    }

    fn close(&mut self) {
        WinOut::close(self);
    }
}

pub(super) struct WinOut {
    handle: usize,
    buffers: Vec<Box<[i16; crate::mixer::BLOCK_SAMPLES]>>,
    headers: Box<[WaveHdr]>,
    /// Header indexes currently owned by the driver.
    in_flight: [bool; BUFFERS],
    prepared: usize,
    failed: bool,
}

// The handle and buffers live on the playout thread; the value only moves
// there once.
unsafe impl Send for WinOut {}

impl WinOut {
    pub(super) fn open(index: u32, rate: u32) -> Result<Self, String> {
        let format = WaveFormatEx {
            format_tag: WAVE_FORMAT_PCM,
            channels: 1,
            samples_per_sec: rate,
            avg_bytes_per_sec: rate * 2,
            block_align: 2,
            bits_per_sample: 16,
            cb_size: 0,
        };
        let mut handle = 0_usize;
        let code = unsafe { waveOutOpen(&mut handle, index, &format, CALLBACK_NULL, 0, 0) };
        if code != 0 {
            return Err(format!(
                "Не удалось открыть устройство воспроизведения Windows ({code})."
            ));
        }
        Ok(Self {
            handle,
            buffers: (0..BUFFERS)
                .map(|_| Box::new([0_i16; crate::mixer::BLOCK_SAMPLES]))
                .collect(),
            headers: (0..BUFFERS)
                .map(|_| WaveHdr {
                    data: ptr::null_mut(),
                    buffer_length: 0,
                    bytes_recorded: 0,
                    user: 0,
                    flags: 0,
                    loops: 0,
                    next: ptr::null_mut(),
                    reserved: 0,
                })
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            in_flight: [false; BUFFERS],
            prepared: 0,
            failed: false,
        })
    }

    fn free_slot(&self) -> Option<usize> {
        for index in 0..BUFFERS {
            if !self.in_flight[index] {
                return Some(index);
            }
            let flags = unsafe { ptr::read_volatile(&self.headers[index].flags as *const u32) };
            if flags & WHDR_DONE != 0 {
                return Some(index);
            }
        }
        None
    }

    /// Whether the driver can take another block now.
    pub(super) fn writable(&self) -> bool {
        !self.failed && self.free_slot().is_some()
    }

    /// Queue one block. Returns false when every buffer is still in flight or
    /// the device failed; the caller counts that as a dropped block.
    pub(super) fn write(&mut self, block: &[i16]) -> bool {
        if self.failed {
            return false;
        }
        let Some(index) = self.free_slot() else {
            return false;
        };
        let size = size_of::<WaveHdr>() as u32;
        let header = &mut self.headers[index] as *mut WaveHdr;
        if self.prepared <= index {
            unsafe {
                (*header).data = self.buffers[index].as_mut_ptr().cast();
                (*header).buffer_length = (crate::mixer::BLOCK_SAMPLES * 2) as u32;
                (*header).bytes_recorded = 0;
                (*header).flags = 0;
            }
            if unsafe { waveOutPrepareHeader(self.handle, header, size) } != 0 {
                self.failed = true;
                return false;
            }
            self.prepared = index + 1;
        }
        let buffer = &mut self.buffers[index];
        let count = block.len().min(buffer.len());
        buffer[..count].copy_from_slice(&block[..count]);
        for sample in buffer[count..].iter_mut() {
            *sample = 0;
        }
        unsafe {
            (*header).buffer_length = (buffer.len() * 2) as u32;
            // Keep WHDR_PREPARED set: only the completion bit is cleared before
            // the next write. Zeroing the whole field would unprepare the
            // header from the driver's point of view.
            (*header).flags &= !WHDR_DONE;
        }
        if unsafe { waveOutWrite(self.handle, header, size) } != 0 {
            self.failed = true;
            return false;
        }
        self.in_flight[index] = true;
        true
    }

    pub(super) fn failed(&self) -> bool {
        self.failed
    }

    pub(super) fn close(&mut self) {
        if self.handle == 0 {
            return;
        }
        let mut reset = 1;
        for _ in 0..CLOSE_RETRIES {
            reset = unsafe { waveOutReset(self.handle) };
            if reset == 0 {
                break;
            }
            sleep(Duration::from_millis(2));
        }
        if reset != 0 {
            self.failed = true;
            return;
        }
        for index in 0..self.prepared {
            if unsafe {
                waveOutUnprepareHeader(
                    self.handle,
                    &mut self.headers[index],
                    size_of::<WaveHdr>() as u32,
                )
            } != 0
            {
                self.failed = true;
                self.prepared = 0;
                return;
            }
        }
        if unsafe { waveOutClose(self.handle) } != 0 {
            self.failed = true;
            self.prepared = 0;
            return;
        }
        self.handle = 0;
        self.prepared = 0;
    }
}

impl Drop for WinOut {
    fn drop(&mut self) {
        self.close();
        if self.handle != 0 {
            // The driver still owns the headers and PCM. Freeing them now
            // would be a use-after-free inside winmm.
            std::mem::forget(std::mem::take(&mut self.buffers));
            std::mem::forget(std::mem::take(&mut self.headers));
        }
    }
}
