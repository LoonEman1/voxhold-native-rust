//! WASAPI loopback of the default render endpoint and, on Windows 10 build
//! 20348+ or Windows 11, process loopback of one process tree.
//!
//! Shared mode with `AUDCLNT_STREAMFLAGS_LOOPBACK` is the only capture that
//! needs no new dependency: `windows 0.61` already exposes the interfaces. The
//! input is the endpoint mix format; `super::format` converts it to the 48 kHz
//! i16 stereo blocks the publisher sends. Process loopback is activated through
//! `ActivateAudioInterfaceAsync` with `AUDIOCLIENT_ACTIVATION_PARAMS`; a host
//! that refuses it is an explicit error, never a whole-device capture.
//!
//! A device that disappears or becomes the non-default endpoint is reopened
//! with a bounded retry budget. An open failure and a client that is
//! invalidated right after opening both spend the same budget; it resets only
//! after a session polled the client for a stable window, so a silent device
//! counts as healthy. Every wait and join here has a deadline.

use std::{
    ffi::c_void,
    mem::{ManuallyDrop, size_of},
    ptr, slice,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{HWND, RPC_E_CHANGED_MODE},
        Media::Audio::{
            AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_LOOPBACK,
            AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, AUDIOCLIENT_ACTIVATION_PARAMS,
            AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, ActivateAudioInterfaceAsync,
            IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
            IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient,
            IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, PROCESS_LOOPBACK_MODE,
            PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
            PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, eConsole,
            eRender,
        },
        System::{
            Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0_0, PROPVARIANT_0_0_0},
            Com::{
                BLOB, CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree, CoUninitialize,
            },
            Threading::{
                OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
                QueryFullProcessImageNameW,
            },
            Variant::VT_BLOB,
        },
        UI::WindowsAndMessaging::GetWindowThreadProcessId,
    },
    core::{GUID, HRESULT, IUnknown, Interface, PWSTR, Ref, implement},
};

use super::{
    AudioDeviceInfo, BLOCK_SAMPLES, BlockBuffer, CaptureScope, SAMPLE_RATE, Shared,
    format::{Converter, SourceFormat},
    join_bounded, push_block,
};

mod retry;

use retry::{Health, SessionEnd};

/// Open and capture until the publication stops or the retry budget is spent.
pub(super) fn run(
    shared: &Arc<Shared>,
    ready: &mpsc::Sender<Result<AudioDeviceInfo, String>>,
    scope: CaptureScope,
) {
    retry::run(shared, ready, scope);
}

const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const KSDATAFORMAT_SUBTYPE_PCM: GUID = GUID::from_u128(0x0000_0001_0000_0010_8000_00aa_0038_9b71);
const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: GUID =
    GUID::from_u128(0x0000_0003_0000_0010_8000_00aa_0038_9b71);
/// 200 ms shared buffer: enough for a scheduling hiccup, short enough to keep
/// latency and memory bounded.
const BUFFER_DURATION: i64 = 2_000_000;
const POLL: Duration = Duration::from_millis(5);
const DEVICE_CHECK: Duration = Duration::from_secs(2);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Process-loopback activation runs on a COM worker; a host that never answers
/// must not hold the preparation thread.
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(3);

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
    /// Only the endpoint scope has a "default device" that can move; a process
    /// loopback keeps capturing the same process tree.
    check_default: bool,
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

/// Compare the endpoint a session opened with the current default. An empty
/// opened id means `GetId` failed, so the endpoint is unknown rather than
/// moved: comparing it would report a change (or a false terminal stop) on a
/// device that never moved. The id is re-learned when the next session opens.
fn default_device_changed(current: &str, opened: &str) -> bool {
    !opened.is_empty() && current != opened
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
        check_default: true,
    })
}

fn open_scope(scope: CaptureScope) -> Result<Opened, Failure> {
    match scope {
        CaptureScope::Device => open_endpoint(),
        CaptureScope::Process { pid } => {
            open_process(pid, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE)
        }
        CaptureScope::ExcludeProcess { pid } => {
            open_process(pid, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE)
        }
    }
}

/// The activation payload of one process-loopback client. Pure so the layout
/// the API receives is tested without a device.
fn process_loopback_params(pid: u32, mode: PROCESS_LOOPBACK_MODE) -> AUDIOCLIENT_ACTIVATION_PARAMS {
    AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: mode,
            },
        },
    }
}

/// Owns the activation payload and the `VT_BLOB` `PROPVARIANT` for the whole
/// asynchronous activation. The parameters live in a `Box`, so moving this
/// value into the COM handler never moves the bytes `pBlobData` points at.
/// The variant is deliberately `ManuallyDrop`: the crate's `Drop` calls
/// `PropVariantClear`, which would `CoTaskMemFree` the caller-owned params
/// through `pBlobData`. Releasing this struct after the callback drops only the
/// boxed params; the variant needs no clear.
struct ActivationKeepAlive {
    _params: Box<AUDIOCLIENT_ACTIVATION_PARAMS>,
    variant: Box<ManuallyDrop<PROPVARIANT>>,
}

impl ActivationKeepAlive {
    fn new(pid: u32, mode: PROCESS_LOOPBACK_MODE) -> Self {
        let mut params = Box::new(process_loopback_params(pid, mode));
        let variant = Box::new(activation_blob(&mut params));
        Self {
            _params: params,
            variant,
        }
    }

    fn variant(&self) -> &PROPVARIANT {
        &self.variant
    }

    fn variant_ptr(&self) -> *const PROPVARIANT {
        ptr::from_ref(self.variant())
    }
}

/// `ActivateAudioInterfaceAsync` expects the parameters as a `VT_BLOB`
/// `PROPVARIANT`. The blob points at `params`, which must outlive the call.
fn activation_blob(params: &mut AUDIOCLIENT_ACTIVATION_PARAMS) -> ManuallyDrop<PROPVARIANT> {
    let mut variant = PROPVARIANT::default();
    variant.Anonymous.Anonymous = ManuallyDrop::new(PROPVARIANT_0_0 {
        vt: VT_BLOB,
        wReserved1: 0,
        wReserved2: 0,
        wReserved3: 0,
        Anonymous: PROPVARIANT_0_0_0 {
            blob: BLOB {
                cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                pBlobData: ptr::from_mut(params).cast(),
            },
        },
    });
    ManuallyDrop::new(variant)
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationHandler {
    sender: mpsc::Sender<Result<IAudioClient, String>>,
    /// Released only when COM releases the handler after `ActivateCompleted`.
    /// The activation blob points into this owned parameter block.
    _keepalive: ActivationKeepAlive,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationHandler_Impl {
    fn ActivateCompleted(
        &self,
        activateoperation: Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let mut activate_result = HRESULT(0);
        let mut interface: Option<IUnknown> = None;
        let call = match activateoperation.ok() {
            Ok(operation) => unsafe {
                operation.GetActivateResult(&mut activate_result, &mut interface)
            },
            Err(error) => Err(error),
        };
        let outcome = match call {
            Ok(()) if activate_result.is_ok() => match interface {
                Some(unknown) => unknown.cast::<IAudioClient>().map_err(|error| {
                    format!("Активация process loopback вернула другой интерфейс: {error}")
                }),
                None => Err("Активация process loopback не вернула аудиоклиент.".into()),
            },
            Ok(()) => Err(format!(
                "Windows не активировала process loopback (HRESULT {:#010x}); захват всего устройства не включён.",
                activate_result.0
            )),
            Err(error) => Err(format!(
                "Windows не активировала process loopback: {error}; захват всего устройства не включён."
            )),
        };
        let _ = self.sender.send(outcome);
        Ok(())
    }
}

/// The COM pair of one activation that has not answered yet. On a timeout the
/// pair is deliberately leaked: dropping the operation may cancel a callback
/// that is already in flight, and dropping the handler would release the
/// `ActivationKeepAlive` while Windows may still read the parameter blob.
struct PendingActivation {
    _handler: IActivateAudioInterfaceCompletionHandler,
    _operation: IActivateAudioInterfaceAsyncOperation,
}

/// The timeout branch must not release the COM pair and its keepalive while
/// Windows may still call the handler.
trait LeakOnTimeout {
    fn leak(self);
}

impl LeakOnTimeout for PendingActivation {
    fn leak(self) {
        std::mem::forget(self);
    }
}

/// Wait for the completion handler with a deadline. On timeout `pending` is
/// leaked instead of dropped; the callback may otherwise read freed
/// parameters. `pending` is released normally when the callback answered.
fn await_activation<T>(
    receiver: &mpsc::Receiver<Result<T, String>>,
    timeout: Duration,
    pending: impl LeakOnTimeout,
) -> Result<T, String> {
    match receiver.recv_timeout(timeout) {
        Ok(result) => result,
        Err(_) => {
            pending.leak();
            Err(format!(
                "Активация process loopback не завершилась за {timeout:?}."
            ))
        }
    }
}

/// Activate a process-loopback client. `AUDIOCLIENT_ACTIVATION_PARAMS` tells
/// Windows which process tree to include or exclude; an unavailable API or a
/// rejected target is returned to the caller unchanged.
fn activate_process_loopback(
    pid: u32,
    mode: PROCESS_LOOPBACK_MODE,
) -> Result<IAudioClient, String> {
    let keepalive = ActivationKeepAlive::new(pid, mode);
    // Both the params and the variant are boxed, so these allocations stay
    // valid when the keepalive is moved into the COM handler below.
    let variant = keepalive.variant_ptr();
    let (sender, receiver) = mpsc::channel();
    let handler = ActivationHandler {
        sender,
        _keepalive: keepalive,
    };
    let handler: IActivateAudioInterfaceCompletionHandler = handler.into();
    let operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&*variant),
            &handler,
        )
    }
    .map_err(|error| format!("Не удалось запустить активацию process loopback: {error}"))?;
    let pending = PendingActivation {
        _handler: handler,
        _operation: operation,
    };
    // Windows may still call the handler later. On timeout the operation and
    // handler are intentionally leaked, keeping both the COM objects and their
    // boxed parameter blob alive; the callback then sends into a disconnected
    // channel and is ignored. This is one bounded leak per hung activation, and
    // activation only starts after an explicit user action.
    await_activation(&receiver, ACTIVATION_TIMEOUT, pending)
}

fn open_process(pid: u32, mode: PROCESS_LOOPBACK_MODE) -> Result<Opened, Failure> {
    open_process_with(pid, mode, activate_process_loopback)
}

/// The process-loopback open with the activation call injectable, so a test
/// can prove an activation failure is returned unchanged instead of falling
/// back to the whole endpoint.
fn open_process_with(
    pid: u32,
    mode: PROCESS_LOOPBACK_MODE,
    activate: impl FnOnce(u32, PROCESS_LOOPBACK_MODE) -> Result<IAudioClient, String>,
) -> Result<Opened, Failure> {
    if pid == 0 {
        return Err(Failure::Fatal(
            "Процесс для захвата звука не определён.".into(),
        ));
    }
    let client = activate(pid, mode).map_err(Failure::Fatal)?;
    // Process loopback rejects `GetMixFormat` and `IsFormatSupported` with
    // E_NOTIMPL (the client is AudioSes!CMixerClient, not an endpoint). The
    // documented path is to initialise it with an explicit format and let the
    // audio engine convert; 48 kHz stereo i16 is exactly what the publisher
    // consumes, so the converter only has to pass the samples through.
    let format = SourceFormat {
        sample_rate: SAMPLE_RATE,
        channels: 2,
        bits: 16,
        block_align: 4,
        float: false,
        mask: 0,
    };
    let wave = WAVEFORMATEX {
        wFormatTag: 1,
        nChannels: 2,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * 4,
        nBlockAlign: 4,
        wBitsPerSample: 16,
        cbSize: 0,
    };
    unsafe {
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                0,
                0,
                &wave,
                None,
            )
            .map_err(|error| {
                Failure::Fatal(format!(
                    "Не удалось включить process loopback системного звука: {error}"
                ))
            })?;
    }
    let capture: IAudioCaptureClient = unsafe { client.GetService() }.map_err(|error| {
        Failure::Fatal(format!(
            "Не удалось открыть захват process loopback: {error}"
        ))
    })?;
    let _buffer_frames = unsafe { client.GetBufferSize() }.unwrap_or(0);
    unsafe { client.Start() }.map_err(|error| {
        Failure::Fatal(format!("Не удалось запустить process loopback: {error}"))
    })?;
    let scope = if mode == PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE {
        CaptureScope::Process { pid }
    } else {
        CaptureScope::ExcludeProcess { pid }
    };
    Ok(Opened {
        info: AudioDeviceInfo {
            id: format!("process-loopback-{pid}"),
            label: scope.describe(),
            format: format.describe(),
        },
        client,
        capture,
        format,
        check_default: false,
    })
}

/// `GetWindowThreadProcessId` for a capturer window source id.
pub(super) fn window_process_id(window: u64) -> Result<u32, String> {
    if window == 0 {
        return Err("Выбранное окно не сообщило дескриптор.".into());
    }
    let mut pid = 0_u32;
    let thread = unsafe { GetWindowThreadProcessId(HWND(window as *mut c_void), Some(&mut pid)) };
    if thread == 0 || pid == 0 {
        return Err("Windows не определила процесс выбранного окна.".into());
    }
    Ok(pid)
}

/// File name of one process for the scope label. A short read is a name
/// refusal, not an error of the capture.
pub(super) fn process_name(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0_u16; 512];
        let mut size = buffer.len() as u32;
        let outcome = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        );
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        outcome.ok()?;
        let path = String::from_utf16_lossy(&buffer[..size as usize]);
        path.rsplit(['\\', '/'])
            .next()
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    }
}

fn map_capture_error(error: windows::core::Error) -> Failure {
    if error.code() == AUDCLNT_E_DEVICE_INVALIDATED {
        Failure::Reopen("Устройство вывода изменилось.".into())
    } else {
        Failure::Fatal(format!("Ошибка чтения системного звука: {error}"))
    }
}

/// One capture session. It reports how it ended; the retry policy in
/// [`retry`] decides whether the session proved itself stable.
fn capture(shared: &Shared, opened: &Opened) -> SessionEnd {
    // A fresh converter for every client: a reopen starts a new time axis, and
    // the old fractional position must not leak into a different device.
    let mut converter = Converter::new(opened.format);
    let mut scratch = Vec::with_capacity(BLOCK_SAMPLES * 4);
    let mut blocks = BlockBuffer::default();
    // One `now` for both clocks: a switch within the first check window must be
    // classified against the same origin the health streak started from.
    let now = Instant::now();
    let mut next_device_check = now + DEVICE_CHECK;
    let mut health = Health::new(now);
    loop {
        if shared.stopping() {
            // End of stream: finish the sample that was waiting for the next
            // packet so the tail matches a whole-input conversion.
            scratch.clear();
            converter.flush(&mut scratch);
            blocks.push(&scratch, |block| push_block(shared, block));
            return SessionEnd::Stopped;
        }
        if Instant::now() >= next_device_check {
            next_device_check = Instant::now() + DEVICE_CHECK;
            if opened.check_default {
                if let Ok(device) = default_device() {
                    if device_id(&device)
                        .is_some_and(|current| default_device_changed(&current, &opened.info.id))
                    {
                        return ended(
                            Failure::Reopen("Устройство вывода по умолчанию изменилось.".into()),
                            &mut health,
                        );
                    }
                }
            }
        }
        let packet = match unsafe { opened.capture.GetNextPacketSize() } {
            Ok(packet) => packet,
            Err(error) => return ended(map_capture_error(error), &mut health),
        };
        // A silent endpoint answers this poll with zero packets. The answered
        // call still proves the client works, so it extends the healthy streak.
        health.record(Instant::now());
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
            return ended(map_capture_error(error), &mut health);
        }
        health.record(Instant::now());
        if frames > 0 {
            let frames = frames as usize;
            scratch.clear();
            let silent = (flags as i32 & AUDCLNT_BUFFERFLAGS_SILENT.0) != 0;
            if silent {
                // With the silent flag the payload is undefined; only the
                // frame count matters, and it advances the same time axis.
                converter.convert_silence(frames, &mut scratch);
            } else if !data.is_null() {
                let bytes = frames * opened.format.frame_bytes();
                let packet = unsafe { slice::from_raw_parts(data, bytes) };
                converter.convert(packet, false, &mut scratch);
            }
            blocks.push(&scratch, |block| push_block(shared, block));
        }
        if let Err(error) = unsafe { opened.capture.ReleaseBuffer(frames) } {
            return ended(map_capture_error(error), &mut health);
        }
        health.record(Instant::now());
    }
}

/// Attach the measured stability to a reopen decision. The failing call is
/// recorded first, so time spent blocked without a poll cannot count as
/// healthy. A fatal error never reopens, so it carries no stability.
fn ended(failure: Failure, health: &mut Health) -> SessionEnd {
    match failure {
        Failure::Fatal(message) => SessionEnd::Fatal(message),
        Failure::Reopen(message) => {
            health.record(Instant::now());
            SessionEnd::Reopen {
                stable: health.stable(),
                message,
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

#[cfg(test)]
mod tests;
