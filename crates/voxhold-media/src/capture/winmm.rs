use std::{collections::VecDeque, ptr, thread::sleep, time::Duration};

pub(super) const WAVE_MAPPER: u32 = 0xFFFF_FFFF;
const WAVE_FORMAT_PCM: u16 = 1;
const CALLBACK_NULL: usize = 0;
const WHDR_DONE: u32 = 0x0000_0001;
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
}

#[derive(Clone, Debug)]
pub(super) struct DeviceInfo {
    pub index: u32,
    pub name: String,
    pub endpoint: Option<String>,
}

fn label(name: &[u16]) -> String {
    String::from_utf16_lossy(&name[..name.iter().position(|c| *c == 0).unwrap_or(name.len())])
}

fn list() -> Vec<DeviceInfo> {
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
            .find(|device| device.endpoint.as_deref() == Some(saved))
            .map(|device| Chosen {
                index: device.index,
                expected_endpoint: device.endpoint.clone(),
                expected_name: None,
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
        let mut handle = 0_usize;
        let code = unsafe { waveInOpen(&mut handle, chosen.index, &format, CALLBACK_NULL, 0, 0) };
        if code != 0 {
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
        if self.handle == 0 {
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
            self.failed = true;
            return;
        }
        self.queued.clear();
        for index in 0..self.prepared {
            let header = &mut self.headers[index];
            if unsafe { waveInUnprepareHeader(self.handle, header, self.size) } != 0 {
                self.failed = true;
                self.prepared = 0;
                return;
            }
        }
        if unsafe { waveInClose(self.handle) } != 0 {
            self.failed = true;
            self.prepared = 0;
            return;
        }
        self.handle = 0;
        self.prepared = 0;
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
}
