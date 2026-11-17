use std::ptr;

pub(super) const WAVE_MAPPER: u32 = 0xFFFF_FFFF;
const WAVE_FORMAT_PCM: u16 = 1;
const CALLBACK_NULL: usize = 0;
const WHDR_DONE: u32 = 0x0000_0001;
const WAVERR_BADFORMAT: u32 = 32;
const MMSYSERR_BADDEVICEID: u32 = 2;
const MMSYSERR_ALLOCATED: u32 = 4;

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

fn label(name: &[u16]) -> String {
    String::from_utf16_lossy(&name[..name.iter().position(|c| *c == 0).unwrap_or(name.len())])
}

fn list() -> Vec<(u32, String)> {
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
                devices.push((index, label(&caps.name)));
            }
        }
    }
    devices
}

/// Resolve the saved WinMM id by its device name, not by the numeric index:
/// indexes shift when a USB device is added or removed, and the same index
/// may then point at a different microphone. An exact name wins; a unique
/// name prefix is accepted for the truncated legacy label. Missing or
/// ambiguous names report an error so the caller falls back to the system
/// default instead of opening a different device silently.
pub(super) fn choose_device(saved: Option<&str>, devices: &[(u32, String)]) -> Result<u32, String> {
    let Some(saved) = saved.filter(|id| !id.is_empty()) else {
        return Ok(WAVE_MAPPER);
    };
    let (index, name) = match saved.split_once(':') {
        Some((index, name)) => (index, name),
        None => {
            return saved
                .parse::<u32>()
                .map_err(|_| "Выбранное устройство ввода недоступно.".to_owned());
        }
    };
    if name.is_empty() {
        return index
            .parse::<u32>()
            .map_err(|_| "Выбранное устройство ввода недоступно.".to_owned());
    }
    if let Some((index, _)) = devices.iter().find(|(_, candidate)| candidate == name) {
        return Ok(*index);
    }
    let matches: Vec<_> = devices
        .iter()
        .filter(|(_, candidate)| candidate.starts_with(name))
        .collect();
    match matches.as_slice() {
        [(index, _)] => Ok(*index),
        [] => Err("Выбранное устройство ввода недоступно.".into()),
        _ => Err("Не удалось однозначно определить выбранный микрофон.".into()),
    }
}

pub struct WinIn {
    handle: usize,
    buffers: Vec<Box<[i16; super::BLOCK * 2]>>,
    headers: Box<[WaveHdr]>,
    prepared: usize,
    size: u32,
}

// The handle and buffers move from the caller to the capture thread once;
// no method can be used from two threads at the same time.
unsafe impl Send for WinIn {}

impl WinIn {
    pub fn open(input: Option<&str>) -> Result<Self, String> {
        let device = choose_device(input, &list())?;
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
        let code = unsafe { waveInOpen(&mut handle, device, &format, CALLBACK_NULL, 0, 0) };
        if code != 0 {
            return Err(device_error(code));
        }
        if let Some(name) = input
            .and_then(|id| id.split_once(':'))
            .map(|(_, name)| name)
            .filter(|name| !name.is_empty())
        {
            let mut actual = 0_u32;
            let mut caps: WaveInCaps = unsafe { std::mem::zeroed() };
            let described = unsafe {
                waveInGetID(handle, &mut actual) == 0
                    && waveInGetDevCapsW(
                        actual as usize,
                        &mut caps,
                        std::mem::size_of::<WaveInCaps>() as u32,
                    ) == 0
            };
            if !described || !label(&caps.name).starts_with(name) {
                unsafe {
                    waveInClose(handle);
                }
                return Err("Выбранное устройство ввода недоступно.".into());
            }
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
            prepared: 0,
            size,
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
            if unsafe { waveInAddBuffer(handle, header, size) } != 0 {
                device.close();
                return Err("Не удалось передать буфер микрофону.".into());
            }
        }
        if unsafe { waveInStart(handle) } != 0 {
            device.close();
            return Err("Не удалось запустить захват микрофона.".into());
        }
        Ok(device)
    }

    /// Polls completed buffers and requeues them. Copies only recorded
    /// bytes, so a short final buffer is not mistaken for silence. The
    /// driver writes `dwFlags`/`dwBytesRecorded` asynchronously, so both
    /// are read volatile. The caller consumes the slice, so no temporary
    /// `Vec` is allocated on the audio thread.
    pub fn completed(&mut self, mut sink: impl FnMut(&[i16])) {
        for header in self.headers.iter_mut() {
            let flags = unsafe { ptr::read_volatile(&header.flags) };
            if flags & WHDR_DONE == 0 {
                continue;
            }
            let recorded = (unsafe { ptr::read_volatile(&header.bytes_recorded) } as usize / 2)
                .min(header.buffer_length as usize / 2);
            let data = header.data;
            if recorded > 0 && !data.is_null() {
                let samples = unsafe { std::slice::from_raw_parts(data.cast::<i16>(), recorded) };
                sink(samples);
            }
            // The driver clears WHDR_DONE when the buffer is queued again.
            // Writing flags here would also drop WHDR_PREPARED.
            let _ = unsafe { waveInAddBuffer(self.handle, header as *mut _, self.size) };
        }
    }

    pub fn close(&mut self) {
        if self.handle == 0 {
            return;
        }
        unsafe {
            waveInReset(self.handle);
        }
        if self.prepared > 0 {
            for index in 0..self.prepared {
                let header = &mut self.headers[index];
                unsafe {
                    waveInUnprepareHeader(self.handle, header, self.size);
                }
            }
            self.prepared = 0;
        }
        unsafe {
            waveInClose(self.handle);
        }
        self.handle = 0;
    }
}

impl Drop for WinIn {
    fn drop(&mut self) {
        self.close();
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
