use std::{collections::VecDeque, ptr, thread::sleep, time::Duration};

use super::WaitOutcome;

pub(super) const WAVE_MAPPER: u32 = 0xFFFF_FFFF;
const WAVE_FORMAT_PCM: u16 = 1;
/// `CALLBACK_EVENT`: `dwCallback` is a Win32 event handle that the driver
/// signals when a buffer completes. Readiness then comes from a real wait
/// instead of a fixed sleep poll.
const CALLBACK_EVENT: usize = 0x0005_0000;
const WHDR_DONE: u32 = 0x0000_0001;
const WAIT_OBJECT_0: u32 = 0x0000_0000;
const WAIT_TIMEOUT: u32 = 0x0000_0102;
const WAVERR_BADFORMAT: u32 = 32;
const MMSYSERR_BADDEVICEID: u32 = 2;
const MMSYSERR_ALLOCATED: u32 = 4;
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
struct WaveInCaps {
    manufacturer: u16,
    product: u16,
    version: u32,
    name: [u16; 32],
    formats: u32,
    channels: u16,
    reserved: u16,
}

#[link(name = "winmm")]
unsafe extern "system" {
    fn waveInOpen(
        handle: *mut usize,
        device: u32,
        format: *const WaveFormatEx,
        callback: usize,
        instance: usize,
        flags: u32,
    ) -> u32;
    fn waveInGetNumDevs() -> u32;
    fn waveInGetDevCapsW(id: usize, caps: *mut WaveInCaps, size: u32) -> u32;
    fn waveInGetID(handle: usize, id: *mut u32) -> u32;
    fn waveInPrepareHeader(handle: usize, header: *mut WaveHdr, size: u32) -> u32;
    fn waveInUnprepareHeader(handle: usize, header: *mut WaveHdr, size: u32) -> u32;
    fn waveInAddBuffer(handle: usize, header: *mut WaveHdr, size: u32) -> u32;
    fn waveInStart(handle: usize) -> u32;
    fn waveInReset(handle: usize) -> u32;
    fn waveInClose(handle: usize) -> u32;
    /// Raise the process timer resolution for the 10 ms wait. Without this,
    /// an unraised Windows timer can round a 10 ms `WaitForSingleObject` up to
    /// ~15.6 ms, which would make the output cadence skip ticks.
    fn timeBeginPeriod(period: u32) -> u32;
    fn timeEndPeriod(period: u32) -> u32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateEventW(
        attributes: *const std::ffi::c_void,
        manual_reset: i32,
        initial_state: i32,
        name: *const u16,
    ) -> usize;
    fn WaitForSingleObject(handle: usize, milliseconds: u32) -> u32;
    fn ResetEvent(handle: usize) -> i32;
    fn CloseHandle(handle: usize) -> i32;
}

/// Map a `WaitForSingleObject` result to the two expected outcomes. Any other
/// value (`WAIT_FAILED`, a non-event wait code, an abandoned mutex, ...) is an
/// error, never "no data yet".
fn wait_outcome(code: u32) -> WaitOutcome {
    match code {
        WAIT_OBJECT_0 => WaitOutcome::Signaled,
        WAIT_TIMEOUT => WaitOutcome::Timeout,
        _ => WaitOutcome::Error,
    }
}

/// `WaitForSingleObject` takes whole milliseconds. Round up so the last
/// fraction of a 10 ms deadline cannot become a zero-timeout busy poll, while
/// an exact zero still returns immediately.
fn wait_millis(timeout: Duration) -> u32 {
    ((timeout.as_nanos() + 999_999) / 1_000_000).min(u128::from(u32::MAX)) as u32
}

#[derive(Clone, Debug)]
pub(super) struct DeviceInfo {
    pub index: u32,
    pub name: String,
    /// Current Core Audio endpoint id, the value saved by new preferences.
    pub endpoint: Option<String>,
    /// PnP interface id saved by older builds; matched only for migration.
    pub legacy_interface: Option<String>,
}

fn label(name: &[u16]) -> String {
    String::from_utf16_lossy(&name[..name.iter().position(|c| *c == 0).unwrap_or(name.len())])
}

pub(super) fn list() -> Vec<DeviceInfo> {
    let mut devices = Vec::new();
    unsafe {
        for index in 0..waveInGetNumDevs() {
            let mut caps: WaveInCaps = std::mem::zeroed();
            if waveInGetDevCapsW(
                index as usize,
                &mut caps,
                std::mem::size_of::<WaveInCaps>() as u32,
            ) == 0
            {
                devices.push(DeviceInfo {
                    index,
                    name: label(&caps.name),
                    endpoint: crate::devices::endpoint_id(index),
                    legacy_interface: crate::devices::legacy_interface_id(index),
                });
            }
        }
    }
    devices
}

pub(super) struct Chosen {
    pub(super) index: u32,
    expected_endpoint: Option<String>,
    expected_name: Option<String>,
}
/// Resolve a saved microphone id. New settings store the Windows endpoint
/// interface id; the legacy `index:name` form is accepted only when its name is
/// unambiguous. A reused numeric index must never open a different device.
pub(super) fn choose_device(saved: Option<&str>, devices: &[DeviceInfo]) -> Result<Chosen, String> {
    let Some(saved) = saved.filter(|id| !id.is_empty()) else {
        return Ok(Chosen {
            index: WAVE_MAPPER,
            expected_endpoint: None,
            expected_name: None,
        });
    };
    let legacy = saved
        .split_once(':')
        .filter(|(index, _)| index.parse::<u32>().is_ok());
    match legacy {
        Some((_index, name)) if !name.is_empty() => {
            let exact: Vec<_> = devices
                .iter()
                .filter(|device| device.name == name)
                .collect();
            match exact.as_slice() {
                [device] => {
                    return Ok(Chosen {
                        index: device.index,
                        expected_endpoint: None,
                        expected_name: Some(device.name.clone()),
                    });
                }
                [] => {}
                _ => {
                    return Err(
                        "Несколько микрофонов с одинаковым именем; выберите устройство ввода повторно."
                            .into(),
                    );
                }
            }
            let prefix: Vec<_> = devices
                .iter()
                .filter(|device| device.name.starts_with(name))
                .collect();
            match prefix.as_slice() {
                [device] => Ok(Chosen {
                    index: device.index,
                    expected_endpoint: None,
                    expected_name: Some(device.name.clone()),
                }),
                [] => Err("Выбранное устройство ввода недоступно.".into()),
                _ => Err(
                    "Не удалось однозначно определить выбранный микрофон; выберите его повторно."
                        .into(),
                ),
            }
        }
        Some((_, _)) => Err(
            "В сохранённых настройках нет имени микрофона; выберите устройство ввода повторно."
                .into(),
        ),
        None => devices
            .iter()
            .find(|device| {
                device.endpoint.as_deref() == Some(saved)
                    || device.legacy_interface.as_deref() == Some(saved)
            })
            .map(|device| Chosen {
                index: device.index,
                expected_endpoint: device.endpoint.clone(),
                expected_name: Some(device.name.clone()),
            })
            .ok_or_else(|| "Выбранное устройство ввода недоступно.".to_owned()),
    }
}

fn actual_device(handle: usize) -> Option<DeviceInfo> {
    let mut index = 0_u32;
    if unsafe { waveInGetID(handle, &mut index) } != 0 {
        return None;
    }
    let mut caps: WaveInCaps = unsafe { std::mem::zeroed() };
    if unsafe {
        waveInGetDevCapsW(
            index as usize,
            &mut caps,
            std::mem::size_of::<WaveInCaps>() as u32,
        )
    } != 0
    {
        return None;
    }
    Some(DeviceInfo {
        index,
        name: label(&caps.name),
        endpoint: crate::devices::endpoint_id(index),
        legacy_interface: crate::devices::legacy_interface_id(index),
    })
}

/// Only the queue front may be emitted. A later buffer that completed first
/// must wait, otherwise a delayed poll would swap 20 ms blocks.
fn front_ready(queued: &VecDeque<usize>, mut done: impl FnMut(usize) -> bool) -> Option<usize> {
    let &index = queued.front()?;
    done(index).then_some(index)
}

pub struct WinIn {
    handle: usize,
    /// Auto-reset event given to the driver via `CALLBACK_EVENT`.
    event: usize,
    /// Whether this instance raised the process timer resolution and must
    /// balance it on close.
    timer_raised: bool,
    buffers: Vec<Box<[i16; super::BLOCK * 2]>>,
    headers: Box<[WaveHdr]>,
    /// Header indexes in the order they were queued. The driver may complete
    /// later buffers first; emitting only the queue front preserves the PCM
    /// order even when the poll is delayed.
    queued: VecDeque<usize>,
    prepared: usize,
    size: u32,
    failed: bool,
}

// The handle and buffers move from the caller to the capture thread once;
// no method can be used from two threads at the same time.
unsafe impl Send for WinIn {}

impl WinIn {
    pub fn open(input: Option<&str>) -> Result<Self, String> {
        let chosen = choose_device(input, &list())?;
        let format = WaveFormatEx {
            format_tag: WAVE_FORMAT_PCM,
            channels: super::CHANNELS as u16,
            samples_per_sec: super::SAMPLE_RATE,
            avg_bytes_per_sec: super::SAMPLE_RATE * 2,
            block_align: 2,
            bits_per_sample: 16,
            cb_size: 0,
        };
        // The event must exist before `waveInOpen`: with `CALLBACK_EVENT` the
        // driver stores this handle and signals it on every completion.
        let event = unsafe { CreateEventW(ptr::null(), 0, 0, ptr::null()) };
        if event == 0 {
            return Err("Не удалось создать событие микрофона.".into());
        }
        let mut handle = 0_usize;
        let code = unsafe {
            waveInOpen(
                &mut handle,
                chosen.index,
                &format,
                event,
                0,
                CALLBACK_EVENT as u32,
            )
        };
        if code != 0 {
            unsafe {
                CloseHandle(event);
            }
            return Err(device_error(code));
        }
        let actual = actual_device(handle);
        let identity_ok = actual.as_ref().is_some_and(|actual| {
            chosen
                .expected_endpoint
                .as_deref()
                .is_none_or(|expected| actual.endpoint.as_deref() == Some(expected))
                && chosen
                    .expected_name
                    .as_deref()
                    .is_none_or(|expected| actual.name.starts_with(expected))
        });
        if !identity_ok {
            unsafe {
                waveInClose(handle);
                CloseHandle(event);
            }
            return Err("Выбранное устройство ввода недоступно.".into());
        }
        let size = std::mem::size_of::<WaveHdr>() as u32;
        let buffers = (0..super::BUFFERS)
            .map(|_| Box::new([0_i16; super::BLOCK * 2]))
            .collect::<Vec<_>>();
        let headers = (0..super::BUFFERS)
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
            .into_boxed_slice();
        let mut device = Self {
            handle,
            event,
            timer_raised: false,
            buffers,
            headers,
            queued: VecDeque::with_capacity(super::BUFFERS),
            prepared: 0,
            size,
            failed: false,
        };
        for index in 0..super::BUFFERS {
            let buffer = device.buffers[index].as_mut_ptr();
            let header = &mut device.headers[index];
            header.data = buffer.cast();
            header.buffer_length = (super::BLOCK * 2 * 2) as u32;
            header.bytes_recorded = 0;
            header.flags = 0;
            if unsafe { waveInPrepareHeader(handle, header, size) } != 0 {
                device.close();
                return Err("Не удалось подготовить буфер микрофона.".into());
            }
            device.prepared += 1;
            if !device.queue(index) {
                device.close();
                return Err("Не удалось передать буфер микрофону.".into());
            }
        }
        if unsafe { waveInStart(handle) } != 0 {
            device.failed = true;
            device.close();
            return Err("Не удалось запустить захват микрофона.".into());
        }
        // Only needed while this capture runs; `close` balances it.
        if unsafe { timeBeginPeriod(1) } == 0 {
            device.timer_raised = true;
        }
        Ok(device)
    }

    fn queue(&mut self, index: usize) -> bool {
        let header = &mut self.headers[index] as *mut WaveHdr;
        if unsafe { waveInAddBuffer(self.handle, header, self.size) } != 0 {
            self.failed = true;
            return false;
        }
        self.queued.push_back(index);
        true
    }

    pub fn failed(&self) -> bool {
        self.failed
    }

    /// Bounded wait on the driver completion event. This is the only
    /// readiness mechanism: `Signaled` means at least one buffer may have
    /// completed, `Timeout` is the normal "no completion yet" result, and any
    /// unexpected kernel result is an error.
    pub fn wait(&mut self, timeout: Duration) -> WaitOutcome {
        if self.failed || self.handle == 0 || self.event == 0 {
            return WaitOutcome::Error;
        }
        let outcome =
            wait_outcome(unsafe { WaitForSingleObject(self.event, wait_millis(timeout)) });
        if outcome == WaitOutcome::Error {
            self.failed = true;
        }
        outcome
    }

    fn reset_event(&mut self) -> bool {
        if self.event == 0 {
            return false;
        }
        if unsafe { ResetEvent(self.event) } == 0 {
            self.failed = true;
            return false;
        }
        true
    }

    fn close_event(&mut self) {
        if self.event == 0 {
            return;
        }
        if unsafe { CloseHandle(self.event) } == 0 {
            self.failed = true;
        }
        self.event = 0;
    }

    /// Discard every driver buffer captured before a control transition and
    /// restart the queue. Used by mute/PTT changes so a 20 ms buffer that
    /// started before unmute cannot cross into the new state.
    pub fn flush(&mut self) -> bool {
        if self.failed || self.handle == 0 {
            return false;
        }
        if unsafe { waveInReset(self.handle) } != 0 {
            self.failed = true;
            return false;
        }
        // Reset marks every queued buffer done and may leave the event
        // signaled. Clear it before requeueing so the next wait reflects a
        // real completion, not the flush.
        if !self.reset_event() {
            return false;
        }
        self.queued.clear();
        for index in 0..self.prepared {
            if !self.queue(index) {
                return false;
            }
        }
        if unsafe { waveInStart(self.handle) } != 0 {
            self.failed = true;
            return false;
        }
        true
    }

    /// Polls completed buffers in queue order and requeues them. Copies only
    /// recorded bytes, so a short final buffer is not mistaken for silence. The
    /// driver writes `dwFlags`/`dwBytesRecorded` asynchronously, so both are
    /// read volatile. The caller consumes the slice, so no temporary `Vec` is
    /// allocated on the audio thread. A failed requeue marks the device failed
    /// and is handled by the capture recovery path.
    pub fn completed(&mut self, mut sink: impl FnMut(&[i16])) {
        if self.failed {
            return;
        }
        while let Some(index) = front_ready(&self.queued, |index| {
            let header = &self.headers[index];
            let flags = unsafe { ptr::read_volatile(&header.flags as *const u32) };
            (flags & WHDR_DONE) != 0
        }) {
            let header = &mut self.headers[index] as *mut WaveHdr;
            let recorded = (unsafe { ptr::read_volatile(&(*header).bytes_recorded) } as usize / 2)
                .min(unsafe { (*header).buffer_length } as usize / 2);
            let data = unsafe { (*header).data };
            if recorded > 0 && !data.is_null() {
                let samples = unsafe { std::slice::from_raw_parts(data.cast::<i16>(), recorded) };
                sink(samples);
            }
            self.queued.pop_front();
            if unsafe { waveInAddBuffer(self.handle, header, self.size) } != 0 {
                self.failed = true;
                self.queued.clear();
                break;
            }
            self.queued.push_back(index);
        }
    }

    pub fn close(&mut self) {
        self.close_device();
        if self.timer_raised {
            unsafe {
                timeEndPeriod(1);
            }
            self.timer_raised = false;
        }
    }

    fn close_device(&mut self) {
        if self.handle == 0 {
            self.close_event();
            return;
        }
        let mut reset = 1;
        for _ in 0..CLOSE_RETRIES {
            reset = unsafe { waveInReset(self.handle) };
            if reset == 0 {
                break;
            }
            sleep(Duration::from_millis(2));
        }
        if reset != 0 {
            // The driver may still own the handle and signal the event.
            // Dropping the event here could race with a reused handle; Drop
            // leaks it together with the driver-owned buffers.
            self.failed = true;
            return;
        }
        // Recording is stopped, so no new completion can signal the event.
        // Clear and close it on every remaining path, including failures.
        if !self.reset_event() {
            self.failed = true;
        }
        self.queued.clear();
        for index in 0..self.prepared {
            let header = &mut self.headers[index];
            if unsafe { waveInUnprepareHeader(self.handle, header, self.size) } != 0 {
                self.failed = true;
                self.prepared = 0;
                self.close_event();
                return;
            }
        }
        if unsafe { waveInClose(self.handle) } != 0 {
            self.failed = true;
            self.prepared = 0;
            self.close_event();
            return;
        }
        self.handle = 0;
        self.prepared = 0;
        self.close_event();
    }
}

impl Drop for WinIn {
    fn drop(&mut self) {
        self.close();
        if self.handle != 0 {
            // The driver still owns the headers/PCM. Leaking them is the only
            // safe option; freeing now would be a use-after-free in winmm.
            std::mem::forget(std::mem::take(&mut self.buffers));
            std::mem::forget(std::mem::take(&mut self.headers));
            // The driver may still signal this handle; leaving it open is
            // safer than closing a handle the driver can reuse.
            self.event = 0;
        }
    }
}

fn device_error(code: u32) -> String {
    match code {
        WAVERR_BADFORMAT => "Микрофон не поддерживает 48 кГц mono 16 бит.".into(),
        MMSYSERR_BADDEVICEID => "Выбранное устройство ввода недоступно.".into(),
        MMSYSERR_ALLOCATED => "Микрофон занят другим приложением.".into(),
        _ => format!("Не удалось открыть микрофон Windows ({code})."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_buffers_are_only_taken_in_queue_order() {
        let queued = VecDeque::from([3, 0, 1]);
        assert_eq!(front_ready(&queued, |index| index == 0), None);
        assert_eq!(front_ready(&queued, |index| index == 3), Some(3));
        let empty: VecDeque<usize> = VecDeque::new();
        assert_eq!(front_ready(&empty, |_| true), None);
    }

    #[test]
    fn only_signaled_and_timeout_are_expected_wait_results() {
        assert_eq!(wait_outcome(WAIT_OBJECT_0), WaitOutcome::Signaled);
        assert_eq!(wait_outcome(WAIT_TIMEOUT), WaitOutcome::Timeout);
        assert_eq!(wait_outcome(0xFFFF_FFFF), WaitOutcome::Error);
        // WAIT_ABANDONED is not valid for an event and must not be treated
        // as "nothing completed".
        assert_eq!(wait_outcome(0x0000_0080), WaitOutcome::Error);
    }

    #[test]
    fn wait_millis_rounds_up_instead_of_polling() {
        assert_eq!(wait_millis(Duration::ZERO), 0);
        assert_eq!(wait_millis(Duration::from_nanos(1)), 1);
        assert_eq!(wait_millis(Duration::from_micros(500)), 1);
        assert_eq!(wait_millis(Duration::from_millis(10)), 10);
        assert_eq!(wait_millis(Duration::from_micros(10_001)), 11);
    }
}
