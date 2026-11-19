//! WASAPI loopback of the default render endpoint.
//!
//! Shared mode with `AUDCLNT_STREAMFLAGS_LOOPBACK` is the only capture that
//! needs no new dependency: `windows 0.61` already exposes the interfaces.
//! The input is the endpoint mix format; `super::format` converts it to the
//! 48 kHz i16 stereo blocks the publisher sends.
//!
//! A device that disappears or becomes the non-default endpoint is reopened
//! with a bounded retry count. Every wait and join here has a deadline.

use std::{
    ptr, slice,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::RPC_E_CHANGED_MODE,
        Media::Audio::{
            AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK, IAudioCaptureClient, IAudioClient, IMMDevice,
            IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, eConsole,
            eRender,
        },
        System::Com::{
            CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
            CoUninitialize,
        },
    },
    core::{GUID, IUnknown},
};

use super::{
    AudioDeviceInfo, BLOCK_SAMPLES, BlockBuffer, Shared,
    format::{Converter, SourceFormat},
    join_bounded, push_block,
};

const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const KSDATAFORMAT_SUBTYPE_PCM: GUID = GUID::from_u128(0x0000_0001_0000_0010_8000_00aa_0038_9b71);
const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: GUID =
    GUID::from_u128(0x0000_0003_0000_0010_8000_00aa_0038_9b71);
/// 200 ms shared buffer: enough for a scheduling hiccup, short enough to keep
/// latency and memory bounded.
const BUFFER_DURATION: i64 = 2_000_000;
const POLL: Duration = Duration::from_millis(5);
const REOPEN_DELAY: Duration = Duration::from_millis(300);
const MAX_REOPEN: u8 = 3;
const DEVICE_CHECK: Duration = Duration::from_secs(2);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

enum Failure {
    /// Reopen the default endpoint; the publication keeps running.
    Reopen(String),
    /// Report a terminal error to the state machine.
    Fatal(String),
}

struct Com {
    uninitialize: bool,
}

impl Com {
    fn init() -> Result<Self, String> {
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if result.is_ok() {
            Ok(Self { uninitialize: true })
        } else if result == RPC_E_CHANGED_MODE {
            // COM is already initialized in another apartment; the enumerator
            // is usable and this thread must not uninitialize it.
            Ok(Self {
                uninitialize: false,
            })
        } else {
            Err(format!("Не удалось инициализировать COM: {result:?}"))
        }
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.uninitialize {
            unsafe { CoUninitialize() };
        }
    }
}

/// Frees the `GetMixFormat` allocation on every path.
struct MixFormat(*mut WAVEFORMATEX);

impl Drop for MixFormat {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CoTaskMemFree(Some(self.0.cast())) };
        }
    }
}

struct Opened {
    info: AudioDeviceInfo,
    client: IAudioClient,
    capture: IAudioCaptureClient,
    format: SourceFormat,
}

impl Drop for Opened {
    fn drop(&mut self) {
        let _ = unsafe { self.client.Stop() };
    }
}

fn default_device() -> Result<IMMDevice, Failure> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None::<&IUnknown>, CLSCTX_ALL)
                .map_err(|error| Failure::Fatal(format!("Core Audio недоступен: {error}")))?;
        enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|error| Failure::Fatal(format!("Устройство вывода звука не найдено: {error}")))
    }
}

fn device_id(device: &IMMDevice) -> Option<String> {
    let id = unsafe { device.GetId() }.ok()?;
    let text = unsafe { id.to_string() }.ok();
    unsafe { CoTaskMemFree(Some(id.0.cast())) };
    text
}

fn parse_mix(pointer: *const WAVEFORMATEX) -> Result<SourceFormat, Failure> {
    if pointer.is_null() {
        return Err(Failure::Fatal(
            "Устройство вывода не сообщило формат микширования.".into(),
        ));
    }
    // The endpoint owns this allocation while `MixFormat` is alive.
    let format = unsafe { *pointer };
    let tag = format.wFormatTag;
    let mut mask = 0;
    let float = if tag == WAVE_FORMAT_EXTENSIBLE {
        let extensible = unsafe { *(pointer.cast::<WAVEFORMATEXTENSIBLE>()) };
        mask = extensible.dwChannelMask;
        let subformat = extensible.SubFormat;
        if subformat == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
            true
        } else if subformat == KSDATAFORMAT_SUBTYPE_PCM {
            false
        } else {
            return Err(Failure::Fatal(
                "Устройство вывода использует неизвестный формат звука.".into(),
            ));
        }
    } else if tag == WAVE_FORMAT_IEEE_FLOAT {
        true
    } else if tag == 1 {
        false
    } else {
        return Err(Failure::Fatal(format!(
            "Устройство вывода использует неподдерживаемый формат (тег {tag})."
        )));
    };
    let bits = format.wBitsPerSample;
    if !matches!(bits, 16 | 24 | 32) {
        return Err(Failure::Fatal(format!(
            "Устройство вывода использует {bits}-битный звук; поддерживаются 16/24/32 бита."
        )));
    }
    if float && bits != 32 {
        return Err(Failure::Fatal(format!(
            "Устройство вывода сообщает {bits}-битный float; поддерживается только 32 бита."
        )));
    }
    if format.nBlockAlign == 0 || format.nChannels == 0 {
        return Err(Failure::Fatal(
            "Устройство вывода сообщило пустой формат звука.".into(),
        ));
    }
    Ok(SourceFormat {
        sample_rate: format.nSamplesPerSec,
        channels: format.nChannels,
        bits,
        block_align: format.nBlockAlign,
        float,
        mask,
    })
}

fn open_endpoint() -> Result<Opened, Failure> {
    let device = default_device()?;
    let id = device_id(&device).unwrap_or_default();
    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
        .map_err(|error| Failure::Fatal(format!("Не удалось открыть аудиоклиент: {error}")))?;
    let mix = MixFormat(unsafe { client.GetMixFormat() }.map_err(|error| {
        Failure::Fatal(format!(
            "Не удалось получить формат устройства вывода: {error}"
        ))
    })?);
    let format = parse_mix(mix.0)?;
    unsafe {
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                BUFFER_DURATION,
                0,
                mix.0,
                None,
            )
            .map_err(|error| {
                Failure::Fatal(format!(
                    "Не удалось включить loopback системного звука: {error}"
                ))
            })?;
    }
    drop(mix);
    let capture: IAudioCaptureClient = unsafe { client.GetService() }
        .map_err(|error| Failure::Fatal(format!("Не удалось открыть захват loopback: {error}")))?;
    let _buffer_frames = unsafe { client.GetBufferSize() }.unwrap_or(0);
    unsafe { client.Start() }
        .map_err(|error| Failure::Fatal(format!("Не удалось запустить loopback: {error}")))?;
    Ok(Opened {
        info: AudioDeviceInfo {
            id,
            label: "Устройство вывода по умолчанию".into(),
            format: format.describe(),
        },
        client,
        capture,
        format,
    })
}

fn map_capture_error(error: windows::core::Error) -> Failure {
    if error.code() == AUDCLNT_E_DEVICE_INVALIDATED {
        Failure::Reopen("Устройство вывода изменилось.".into())
    } else {
        Failure::Fatal(format!("Ошибка чтения системного звука: {error}"))
    }
}

fn capture(shared: &Shared, opened: &Opened) -> Failure {
    let mut converter = Converter::new(opened.format);
    let mut scratch = Vec::with_capacity(BLOCK_SAMPLES * 4);
    let mut blocks = BlockBuffer::default();
    let mut next_device_check = Instant::now() + DEVICE_CHECK;
    loop {
        if shared.stopping() {
            return Failure::Reopen("остановлено".into());
        }
        if Instant::now() >= next_device_check {
            next_device_check = Instant::now() + DEVICE_CHECK;
            if let Ok(device) = default_device() {
                if device_id(&device).is_some_and(|current| current != opened.info.id) {
                    return Failure::Reopen("Устройство вывода по умолчанию изменилось.".into());
                }
            }
        }
        let packet = match unsafe { opened.capture.GetNextPacketSize() } {
            Ok(packet) => packet,
            Err(error) => return map_capture_error(error),
        };
        if packet == 0 {
            thread::sleep(POLL);
            continue;
        }
        let mut data: *mut u8 = ptr::null_mut();
        let mut frames = 0_u32;
        let mut flags = 0_u32;
        if let Err(error) = unsafe {
            opened
                .capture
                .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
        } {
            return map_capture_error(error);
        }
        if frames > 0 && !data.is_null() {
            let bytes = frames as usize * opened.format.frame_bytes();
            let silent = (flags as i32 & AUDCLNT_BUFFERFLAGS_SILENT.0) != 0;
            let packet = unsafe { slice::from_raw_parts(data, bytes) };
            scratch.clear();
            // With the silent flag the payload is undefined; the converter
            // advances the timeline and emits zeros without reading it.
            converter.convert(packet, silent, &mut scratch);
            blocks.push(&scratch, |block| push_block(shared, block));
        }
        if let Err(error) = unsafe { opened.capture.ReleaseBuffer(frames) } {
            return map_capture_error(error);
        }
    }
}

pub(super) fn run(shared: &Arc<Shared>, ready: &mpsc::Sender<Result<AudioDeviceInfo, String>>) {
    let _com = match Com::init() {
        Ok(com) => com,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let mut first = true;
    let mut reopens = 0_u8;
    loop {
        if shared.stopping() {
            return;
        }
        match open_endpoint() {
            Ok(opened) => {
                reopens = 0;
                shared.set_device(opened.info.clone());
                if first {
                    first = false;
                    if ready.send(Ok(opened.info.clone())).is_err() {
                        return;
                    }
                }
                match capture(shared, &opened) {
                    Failure::Fatal(message) => {
                        shared.fail(message);
                        return;
                    }
                    Failure::Reopen(_) if shared.stopping() => return,
                    Failure::Reopen(_) => {}
                }
                thread::sleep(REOPEN_DELAY);
            }
            Err(Failure::Reopen(message) | Failure::Fatal(message)) => {
                if first {
                    let _ = ready.send(Err(message));
                    return;
                }
                reopens = reopens.saturating_add(1);
                if reopens > MAX_REOPEN {
                    shared.fail(format!(
                        "Системный звук остановлен: {message} Повторное открытие не удалось."
                    ));
                    return;
                }
                thread::sleep(REOPEN_DELAY);
            }
        }
    }
}

/// Enumerate the default endpoint on a short-lived thread so the UI thread
/// never touches COM and cannot block on a stalled driver.
pub(super) fn probe() -> Result<AudioDeviceInfo, String> {
    let (sender, receiver) = mpsc::channel();
    let handle = thread::Builder::new()
        .name("voxhold-system-audio-probe".into())
        .spawn(move || {
            let result = (|| -> Result<AudioDeviceInfo, String> {
                let _com = Com::init()?;
                let device = default_device().map_err(|failure| match failure {
                    Failure::Reopen(message) | Failure::Fatal(message) => message,
                })?;
                let id = device_id(&device).unwrap_or_default();
                let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
                    .map_err(|error| format!("Не удалось открыть аудиоклиент: {error}"))?;
                let mix = MixFormat(
                    unsafe { client.GetMixFormat() }
                        .map_err(|error| format!("Не удалось получить формат: {error}"))?,
                );
                let format = parse_mix(mix.0).map_err(|failure| match failure {
                    Failure::Reopen(message) | Failure::Fatal(message) => message,
                })?;
                Ok(AudioDeviceInfo {
                    id,
                    label: "Устройство вывода по умолчанию".into(),
                    format: format.describe(),
                })
            })();
            let _ = sender.send(result);
        })
        .map_err(|_| "Не удалось запустить проверку системного звука.".to_owned())?;
    match receiver.recv_timeout(PROBE_TIMEOUT) {
        Ok(result) => {
            join_bounded(handle, PROBE_TIMEOUT);
            result
        }
        Err(_) => {
            // The probe thread has no device handle to leak: if it is stuck in
            // a driver call, detaching is the only bounded option.
            Err("Проверка устройства вывода не завершилась вовремя.".into())
        }
    }
}
