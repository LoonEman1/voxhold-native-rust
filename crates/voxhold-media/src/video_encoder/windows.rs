//! Real Windows hardware encoding: DXGI adapters and Media Foundation MFTs.
//!
//! Everything in this file runs on the probe worker, which owns one MTA
//! apartment and one Media Foundation startup. The trial encode uses a
//! generated black NV12 frame, never user content, and its encoded buffer is
//! dropped before the function returns.
//!
//! The hardware encoders that current drivers register with
//! `MFT_ENUM_FLAG_HARDWARE` (NVENC, Quick Sync and the AMD MFT) are
//! asynchronous: they refuse types with `MF_E_TRANSFORM_ASYNC_LOCKED` until
//! the transform is unlocked, and they do not push a frame through a plain
//! `ProcessInput`/`ProcessOutput` pair. The probe therefore unlocks the
//! transform and drives it through its `IMFMediaEventGenerator` with a bounded
//! wait; a synchronous MFT, if one is registered, still uses the direct loop.

use std::{
    ffi::c_void,
    mem::ManuallyDrop,
    ptr, thread,
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{E_ACCESSDENIED, E_FAIL, E_OUTOFMEMORY},
        Graphics::Dxgi::{
            CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_DEVICE_REMOVED,
            DXGI_ERROR_DEVICE_RESET, DXGI_ERROR_NOT_CURRENTLY_AVAILABLE, IDXGIAdapter1,
            IDXGIFactory1,
        },
        Media::MediaFoundation::{
            IMFActivate, IMFAttributes, IMFMediaEventGenerator, IMFMediaType, IMFSample,
            IMFTransform, METransformDrainComplete, METransformHaveOutput, METransformNeedInput,
            MF_E_INVALIDMEDIATYPE, MF_E_INVALIDREQUEST, MF_E_NO_EVENTS_AVAILABLE,
            MF_E_NO_MORE_TYPES, MF_E_NOTACCEPTING, MF_E_TRANSFORM_ASYNC_LOCKED,
            MF_E_TRANSFORM_NEED_MORE_INPUT,
            MF_E_TRANSFORM_NOT_POSSIBLE_FOR_CURRENT_INPUT_MEDIATYPE, MF_E_TRANSFORM_STREAM_CHANGE,
            MF_E_TRANSFORM_STREAM_INVALID_RESOLUTION, MF_E_TRANSFORM_TYPE_NOT_SET,
            MF_EVENT_FLAG_NO_WAIT, MF_EVENT_MFT_INPUT_STREAM_ID, MF_MT_AVG_BITRATE,
            MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
            MF_MT_MPEG_SEQUENCE_HEADER, MF_MT_MPEG2_PROFILE, MF_MT_SUBTYPE, MF_TRANSFORM_ASYNC,
            MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION, MFCreateAttributes, MFCreateMediaType,
            MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFSTARTUP_FULL,
            MFSampleExtension_CleanPoint, MFShutdown, MFStartup, MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_ADAPTER_LUID, MFT_ENUM_FLAG_HARDWARE, MFT_MESSAGE_COMMAND_DRAIN,
            MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
            MFT_MESSAGE_NOTIFY_END_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
            MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO,
            MFTEnum2, MFVideoFormat_AV1, MFVideoFormat_H264, MFVideoFormat_NV12,
            MFVideoFormat_VP80, MFVideoFormat_VP90, MFVideoInterlace_Progressive,
            eAVEncH264VProfile_ConstrainedBase,
        },
        System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
    },
    core::{Error, GUID, Interface, Result as WinResult},
};

use super::{
    AdapterInfo, AdapterInventory, AdapterLuid, Codec, EncodeQuery, EncoderReason, ProbeCancel,
    ProbeOutcome, SdrProfile, av1, driver_version, h264, probe::HardwareProbe,
};

/// How long the trial encode may take on the event-driven path. The wait is a
/// guard against a driver that never answers, not a performance target.
const ENCODE_DEADLINE: Duration = Duration::from_secs(5);

/// Polling interval of the event queue while waiting for the encoder.
const EVENT_POLL: Duration = Duration::from_millis(2);

/// COM and Media Foundation lifetime of one probe worker.
pub(crate) struct WorkerPlatform {
    com: bool,
    media: bool,
}

impl WorkerPlatform {
    pub(crate) fn new() -> Self {
        let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        let media = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.is_ok();
        Self { com, media }
    }

    /// True when both the MTA apartment and Media Foundation are usable. A
    /// probe on a thread without them fails with `CO_E_NOTINITIALIZED`
    /// (`0x800401f0`), so the hardware tests assert this before probing.
    #[cfg(test)]
    pub(crate) fn is_ready(&self) -> bool {
        self.com && self.media
    }
}

impl Drop for WorkerPlatform {
    fn drop(&mut self) {
        if self.media {
            unsafe {
                let _ = MFShutdown();
            }
        }
        if self.com {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

/// Real DXGI/MF probe. A unit value: all state lives on the worker thread.
pub struct WindowsHardwareProbe;

impl HardwareProbe for WindowsHardwareProbe {
    fn inventory(&self) -> Result<AdapterInventory, String> {
        unsafe { enumerate_adapters() }.map_err(|error| format!("DXGI: {}", hresult(error)))
    }

    fn probe(
        &self,
        adapter: &AdapterInfo,
        codec: Codec,
        query: EncodeQuery,
        cancel: &ProbeCancel,
    ) -> ProbeOutcome {
        if cancel.is_cancelled() {
            return ProbeOutcome::Cancelled;
        }
        match unsafe { probe_stages(adapter, codec, codec_format(codec), query, cancel) } {
            Ok(profile) => ProbeOutcome::Confirmed(profile),
            Err(StageError::Cancelled) => ProbeOutcome::Cancelled,
            Err(StageError::Reason(reason)) => ProbeOutcome::Unavailable(reason),
        }
    }
}

unsafe fn enumerate_adapters() -> WinResult<AdapterInventory> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }?;
    let mut adapters = Vec::new();
    let mut primary: Option<AdapterLuid> = None;
    for index in 0..64u32 {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
            break;
        };
        // A single unreadable description must not truncate the rest of the
        // list; only an exhausted enumeration stops it.
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        let luid = AdapterLuid::new(desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart as u32);
        let software = desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0
            || (desc.VendorId == 0x1414 && desc.DeviceId == 0x8c);
        if !software && primary.is_none() && unsafe { owns_primary_output(&adapter) } {
            primary = Some(luid);
        }
        adapters.push(AdapterInfo {
            luid,
            name: adapter_name(&desc.Description),
            vendor_id: desc.VendorId,
            device_id: desc.DeviceId,
            driver_version: unsafe { driver_version(&adapter, desc.VendorId, desc.DeviceId) },
            dedicated_video_memory: desc.DedicatedVideoMemory as u64,
            software,
        });
    }
    Ok(AdapterInventory::new(adapters, primary))
}

/// The adapter that draws the top-left corner of the virtual desktop. In clone
/// mode several adapters can own an output at `(0, 0)`; the first suitable
/// adapter in the stable DXGI order then wins, which is the same tie-break the
/// automatic selection documents.
unsafe fn owns_primary_output(adapter: &IDXGIAdapter1) -> bool {
    for index in 0..8u32 {
        let Ok(output) = (unsafe { adapter.EnumOutputs(index) }) else {
            break;
        };
        let Ok(desc) = (unsafe { output.GetDesc() }) else {
            continue;
        };
        if desc.AttachedToDesktop.as_bool()
            && desc.DesktopCoordinates.left == 0
            && desc.DesktopCoordinates.top == 0
        {
            return true;
        }
    }
    false
}

/// Driver version as a cache identity.
///
/// `CheckInterfaceSupport(IID_ID3D10Device)` is the primary source; the D3D10
/// UMD reports the packed dotted driver version (`major.minor.build.revision`,
/// `32.0.16.1692` is `0x0020_0000_0010_069c`), the same number
/// `Win32_VideoController.DriverVersion` prints. A UMD without the D3D10
/// interface fails there (the D3D11 IID returns `DXGI_ERROR_UNSUPPORTED`), so
/// the display-class registry entry of the same vendor and device id is the
/// fallback and yields the identical encoding.
///
/// The KMT `KMTQAITYPE_DRIVERVERSION` query is deliberately not used as a
/// fallback: with its documented 4-byte `UINT` output it returns the WDDM model
/// version (for example 3200 for WDDM 3.2), which stays the same across driver
/// updates and would misrepresent the cache identity. Zero means neither
/// source answered; only then a driver update cannot change the key.
unsafe fn driver_version(adapter: &IDXGIAdapter1, vendor_id: u32, device_id: u32) -> u64 {
    // IID_ID3D10Device. `CheckInterfaceSupport` returns the D3D10 UMD version.
    const IID_ID3D10_DEVICE: GUID = GUID::from_u128(0x9b7e4c0f_342c_4106_a19f_4f2704f689f0);
    if let Ok(version) = unsafe { adapter.CheckInterfaceSupport(&IID_ID3D10_DEVICE) }
        && version > 0
    {
        return version as u64;
    }
    driver_version::registry(vendor_id, device_id).unwrap_or(0)
}

fn adapter_name(description: &[u16; 128]) -> String {
    let end = description
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(description.len());
    let name = String::from_utf16_lossy(&description[..end]);
    let name = name.trim();
    if name.is_empty() {
        "Видеокарта".into()
    } else {
        name.to_owned()
    }
}

/// The Media Foundation output subtype of a codec. All four SDR codecs have a
/// registered video subtype, so this cannot fail.
const fn codec_format(codec: Codec) -> GUID {
    match codec {
        Codec::H264 => MFVideoFormat_H264,
        Codec::Vp8 => MFVideoFormat_VP80,
        Codec::Vp9 => MFVideoFormat_VP90,
        Codec::Av1 => MFVideoFormat_AV1,
    }
}

enum StageError {
    Cancelled,
    Reason(EncoderReason),
}

impl From<EncoderReason> for StageError {
    fn from(reason: EncoderReason) -> Self {
        Self::Reason(reason)
    }
}

unsafe fn probe_stages(
    adapter: &AdapterInfo,
    codec: Codec,
    subtype: GUID,
    query: EncodeQuery,
    cancel: &ProbeCancel,
) -> Result<SdrProfile, StageError> {
    let activates = unsafe { enumerate_mfts(adapter, subtype) }
        .map_err(|error| EncoderReason::ProbeFailed(format!("MFTEnum2: {}", hresult(error))))?;
    if activates.is_empty() {
        return Err(EncoderReason::EncoderNotProvided(format!(
            "{} на адаптере {}",
            codec.label(),
            adapter.name
        ))
        .into());
    }
    if cancel.is_cancelled() {
        return Err(StageError::Cancelled);
    }
    let mut last_reason = None;
    for activate in &activates {
        if cancel.is_cancelled() {
            return Err(StageError::Cancelled);
        }
        match unsafe { probe_activate(activate, adapter, codec, subtype, query, cancel) } {
            Ok(profile) => return Ok(profile),
            Err(StageError::Cancelled) => return Err(StageError::Cancelled),
            Err(StageError::Reason(reason)) => last_reason = Some(reason),
        }
    }
    Err(last_reason
        .unwrap_or_else(|| {
            EncoderReason::ProbeFailed("ни один аппаратный MFT не подтвердил пару".into())
        })
        .into())
}

unsafe fn probe_activate(
    activate: &IMFActivate,
    adapter: &AdapterInfo,
    codec: Codec,
    subtype: GUID,
    query: EncodeQuery,
    cancel: &ProbeCancel,
) -> Result<SdrProfile, StageError> {
    let transform: IMFTransform = unsafe { activate.ActivateObject() }
        .map_err(|error| classify(error, Stage::Activate, adapter))?;
    if cancel.is_cancelled() {
        return Err(StageError::Cancelled);
    }
    let asynchronous = unsafe { unlock_async(&transform, activate) }
        .map_err(|error| classify(error, Stage::Activate, adapter))?;
    unsafe { configure_types(&transform, codec, subtype, query, adapter)? };
    if cancel.is_cancelled() {
        return Err(StageError::Cancelled);
    }
    let frame = unsafe { encode_keyframe(&transform, asynchronous, query, adapter, cancel)? };
    unsafe { verify_output_type(&transform, codec, subtype, query, adapter, &frame) }
}

/// Unlock an asynchronous MFT. The flag lives in the transform's own attribute
/// store: `GetAttributes()` returns that store, and setting
/// `MF_TRANSFORM_ASYNC_UNLOCK` on it is the documented way to allow
/// `SetOutputType`. A `QueryInterface(IMFAttributes)` may hand back a copy that
/// does not affect the transform, so it is not used.
unsafe fn unlock_async(transform: &IMFTransform, activate: &IMFActivate) -> WinResult<bool> {
    let attributes = unsafe { transform.GetAttributes() }.ok();
    let asynchronous = match &attributes {
        Some(attributes) => unsafe { attributes.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) != 0,
        None => unsafe { activate.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) != 0,
    };
    if !asynchronous {
        return Ok(false);
    }
    let Some(attributes) = attributes else {
        // The transform is asynchronous but exposes no attribute store to
        // unlock; every type negotiation would fail with a locked transform.
        return Err(Error::from_hresult(MF_E_TRANSFORM_ASYNC_LOCKED));
    };
    unsafe { attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) }?;
    Ok(true)
}

unsafe fn enumerate_mfts(adapter: &AdapterInfo, subtype: GUID) -> WinResult<Vec<IMFActivate>> {
    let mut attributes: Option<IMFAttributes> = None;
    unsafe { MFCreateAttributes(&mut attributes, 1) }?;
    let attributes = attributes.ok_or_else(|| Error::from_hresult(E_FAIL))?;
    unsafe { attributes.SetUINT64(&MFT_ENUM_ADAPTER_LUID, adapter.luid.to_u64()) }?;
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: subtype,
    };
    let mut raw: *mut Option<IMFActivate> = ptr::null_mut();
    let mut count = 0u32;
    unsafe {
        MFTEnum2(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE,
            Some(&input),
            Some(&output),
            &attributes,
            &mut raw,
            &mut count,
        )
    }?;
    let mut result = Vec::with_capacity(count as usize);
    if !raw.is_null() {
        for index in 0..count as usize {
            let activate = unsafe { ptr::read(raw.add(index)) };
            if let Some(activate) = activate {
                result.push(activate);
            }
        }
        unsafe { CoTaskMemFree(Some(raw as *const c_void)) };
    }
    Ok(result)
}

unsafe fn configure_types(
    transform: &IMFTransform,
    codec: Codec,
    subtype: GUID,
    query: EncodeQuery,
    adapter: &AdapterInfo,
) -> Result<(), StageError> {
    let bitrate = query
        .width
        .saturating_mul(query.height)
        .saturating_mul(query.fps)
        / 10;
    let profile = (codec == Codec::H264).then_some(eAVEncH264VProfile_ConstrainedBase.0 as u32);
    let output = unsafe { video_type(subtype, query, profile, bitrate) }
        .map_err(|error| classify(error, Stage::Profile, adapter))?;
    let input = unsafe { video_type(MFVideoFormat_NV12, query, None, 0) }
        .map_err(|error| classify(error, Stage::Profile, adapter))?;
    // Some drivers require the input type first. Try the usual output-first
    // order, then the reverse, and report the first real type error.
    if unsafe { transform.SetOutputType(0, &output, 0) }.is_ok() {
        unsafe { transform.SetInputType(0, &input, 0) }
            .map_err(|error| classify(error, Stage::Profile, adapter))?;
    } else {
        unsafe { transform.SetInputType(0, &input, 0) }
            .map_err(|error| classify(error, Stage::Profile, adapter))?;
        unsafe { transform.SetOutputType(0, &output, 0) }
            .map_err(|error| classify(error, Stage::Profile, adapter))?;
    }
    Ok(())
}

unsafe fn video_type(
    subtype: GUID,
    query: EncodeQuery,
    profile: Option<u32>,
    bitrate: u32,
) -> WinResult<IMFMediaType> {
    let media_type = unsafe { MFCreateMediaType() }?;
    unsafe {
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
        media_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_pair(query.width, query.height))?;
        media_type.SetUINT64(&MF_MT_FRAME_RATE, pack_pair(query.fps, 1))?;
        media_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        if bitrate > 0 {
            media_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate.max(500_000))?;
        }
        if let Some(profile) = profile {
            media_type.SetUINT32(&MF_MT_MPEG2_PROFILE, profile)?;
        }
    }
    Ok(media_type)
}

const fn pack_pair(high: u32, low: u32) -> u64 {
    (high as u64) << 32 | low as u64
}

/// Encode one neutral keyframe and return its bytes.
///
/// The transform is always left flushed and stopped, so a cancel or an error
/// does not keep a pending input or output buffer inside the driver.
unsafe fn encode_keyframe(
    transform: &IMFTransform,
    asynchronous: bool,
    query: EncodeQuery,
    adapter: &AdapterInfo,
    cancel: &ProbeCancel,
) -> Result<Vec<u8>, StageError> {
    let info = unsafe { transform.GetOutputStreamInfo(0) }
        .map_err(|error| classify(error, Stage::Profile, adapter))?;
    let provides_samples = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
    unsafe {
        let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
        let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
    }
    let result = if asynchronous {
        unsafe {
            pump_events(
                transform,
                query,
                info.cbSize,
                provides_samples,
                adapter,
                cancel,
            )
        }
    } else {
        unsafe {
            pump_frames(
                transform,
                query,
                info.cbSize,
                provides_samples,
                adapter,
                cancel,
            )
        }
    };
    unsafe {
        let _ = transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
    }
    result
}

/// Synchronous MFT: `ProcessInput` until the encoder produces a sample.
unsafe fn pump_frames(
    transform: &IMFTransform,
    query: EncodeQuery,
    output_size: u32,
    provides_samples: bool,
    adapter: &AdapterInfo,
    cancel: &ProbeCancel,
) -> Result<Vec<u8>, StageError> {
    let mut frames = 0u32;
    let mut supply_input = true;
    for _ in 0..64 {
        if cancel.is_cancelled() {
            return Err(StageError::Cancelled);
        }
        if supply_input && frames < 8 {
            let sample = unsafe { input_sample(query, frames) }
                .map_err(|error| classify(error, Stage::Input, adapter))?;
            match unsafe { transform.ProcessInput(0, &sample, 0) } {
                Ok(()) => frames += 1,
                Err(error) if error.code() == MF_E_NOTACCEPTING => {}
                Err(error) => return Err(classify(error, Stage::Input, adapter).into()),
            }
        }
        supply_input =
            match unsafe { receive_output(transform, output_size, provides_samples, adapter)? } {
                Some(sample) => {
                    return unsafe { sample_bytes(&sample) }
                        .map_err(|error| classify(error, Stage::Output, adapter).into());
                }
                None => true,
            };
    }
    Err(
        EncoderReason::ProbeFailed("кодировщик не выдал кадр за ограниченное число попыток".into())
            .into(),
    )
}

/// Asynchronous MFT: the encoder asks for input and announces output through
/// its event generator.
///
/// The events are read with `GetEvent(MF_EVENT_FLAG_NO_WAIT)` instead of
/// `BeginGetEvent`. Every `IMFMediaEventGenerator` requires a non-NULL
/// `IMFAsyncCallback` in `BeginGetEvent`, so the documented `NULL` argument is
/// `E_INVALIDARG` for any generator, not only for the hardware MFTs; the probe
/// has no callback to register and polls instead. The loop is bounded by
/// [`ENCODE_DEADLINE`] and checks cancellation between events, so a driver that
/// never answers cannot pin the worker.
unsafe fn pump_events(
    transform: &IMFTransform,
    query: EncodeQuery,
    output_size: u32,
    provides_samples: bool,
    adapter: &AdapterInfo,
    cancel: &ProbeCancel,
) -> Result<Vec<u8>, StageError> {
    let generator: IMFMediaEventGenerator = transform
        .cast()
        .map_err(|error| classify(error, Stage::Output, adapter))?;
    let deadline = Instant::now() + ENCODE_DEADLINE;
    let mut frames = 0u32;
    let mut drain_sent = false;
    loop {
        if cancel.is_cancelled() {
            return Err(StageError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(EncoderReason::ProbeFailed(
                "асинхронный кодировщик не выдал кадр за отведённое время".into(),
            )
            .into());
        }
        let event = match unsafe { generator.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
            Ok(event) => event,
            Err(error) if error.code() == MF_E_NO_EVENTS_AVAILABLE => {
                // Ask for the pending frames once the encoder had enough
                // input; a driver that ignores DRAIN still gets the deadline.
                if frames >= 3 && !drain_sent {
                    let _ = unsafe { transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0) };
                    drain_sent = true;
                }
                thread::sleep(EVENT_POLL);
                continue;
            }
            Err(error) => return Err(classify(error, Stage::Output, adapter).into()),
        };
        if let Ok(status) = unsafe { event.GetStatus() }
            && status.0 < 0
        {
            return Err(classify(Error::from_hresult(status), Stage::Output, adapter).into());
        }
        let kind =
            unsafe { event.GetType() }.map_err(|error| classify(error, Stage::Output, adapter))?;
        if kind == METransformNeedInput.0 as u32 {
            if frames >= 8 {
                return Err(EncoderReason::ProbeFailed(
                    "кодировщик требует больше 8 кадров до ключевого".into(),
                )
                .into());
            }
            let stream = unsafe { event.GetUINT32(&MF_EVENT_MFT_INPUT_STREAM_ID) }.unwrap_or(0);
            let sample = unsafe { input_sample(query, frames) }
                .map_err(|error| classify(error, Stage::Input, adapter))?;
            match unsafe { transform.ProcessInput(stream, &sample, 0) } {
                Ok(()) => frames += 1,
                Err(error) if error.code() == MF_E_NOTACCEPTING => {}
                Err(error) => return Err(classify(error, Stage::Input, adapter).into()),
            }
        } else if kind == METransformHaveOutput.0 as u32 {
            if let Some(sample) =
                unsafe { receive_output(transform, output_size, provides_samples, adapter)? }
            {
                return unsafe { sample_bytes(&sample) }
                    .map_err(|error| classify(error, Stage::Output, adapter).into());
            }
        } else if kind == METransformDrainComplete.0 as u32 {
            return Err(EncoderReason::ProbeFailed(
                "асинхронный кодировщик завершил поток без кадра".into(),
            )
            .into());
        }
    }
}

/// One `ProcessOutput` attempt. `Ok(None)` means "no sample yet": the encoder
/// needs more input or renegotiated its output type.
unsafe fn receive_output(
    transform: &IMFTransform,
    output_size: u32,
    provides_samples: bool,
    adapter: &AdapterInfo,
) -> Result<Option<IMFSample>, StageError> {
    let sample = if provides_samples {
        None
    } else {
        Some(
            unsafe { output_sample(output_size) }
                .map_err(|error| classify(error, Stage::Output, adapter))?,
        )
    };
    let mut data = MFT_OUTPUT_DATA_BUFFER {
        dwStreamID: 0,
        pSample: ManuallyDrop::new(sample),
        dwStatus: 0,
        pEvents: ManuallyDrop::new(None),
    };
    let mut status = 0u32;
    let result =
        unsafe { transform.ProcessOutput(0, std::slice::from_mut(&mut data), &mut status) };
    let sample = unsafe { ManuallyDrop::take(&mut data.pSample) };
    let events = unsafe { ManuallyDrop::take(&mut data.pEvents) };
    drop(events);
    match result {
        Ok(()) => Ok(sample),
        Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(None),
        Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
            unsafe { renegotiate_output(transform, adapter)? };
            Ok(None)
        }
        Err(error) => Err(classify(error, Stage::Output, adapter).into()),
    }
}

unsafe fn verify_output_type(
    transform: &IMFTransform,
    codec: Codec,
    subtype: GUID,
    query: EncodeQuery,
    adapter: &AdapterInfo,
    frame: &[u8],
) -> Result<SdrProfile, StageError> {
    let media_type = unsafe { transform.GetOutputCurrentType(0) }
        .map_err(|error| classify(error, Stage::Profile, adapter))?;
    let current_subtype = unsafe { media_type.GetGUID(&MF_MT_SUBTYPE) }
        .map_err(|error| classify(error, Stage::Profile, adapter))?;
    if current_subtype != subtype {
        return Err(EncoderReason::ProbeFailed(format!(
            "кодировщик вернул другой выходной формат на {}",
            adapter.name
        ))
        .into());
    }
    if let Ok(size) = unsafe { media_type.GetUINT64(&MF_MT_FRAME_SIZE) }
        && size != pack_pair(query.width, query.height)
    {
        return Err(EncoderReason::ProfileUnsupported(format!(
            "кодировщик изменил разрешение на {}",
            adapter.name
        ))
        .into());
    }
    if let Ok(rate) = unsafe { media_type.GetUINT64(&MF_MT_FRAME_RATE) } {
        let numerator = (rate >> 32) as u32;
        let denominator = rate as u32;
        if denominator == 0 || u64::from(numerator) != u64::from(query.fps) * u64::from(denominator)
        {
            return Err(EncoderReason::ProfileUnsupported(format!(
                "кодировщик изменил частоту кадров на {}",
                adapter.name
            ))
            .into());
        }
    }
    let header = unsafe { sequence_header(&media_type) };
    match codec {
        Codec::H264 => {
            let declared = unsafe { media_type.GetUINT32(&MF_MT_MPEG2_PROFILE) }.ok();
            let baseline = h264::keyframe_baseline(declared, frame, &header).map_err(|detail| {
                EncoderReason::ProfileUnsupported(format!("формат результата: {detail}"))
            })?;
            Ok(SdrProfile::H264 {
                baseline,
                level: query.h264_level(),
            })
        }
        other => {
            validate_frame(other, frame, &header).map_err(|detail| {
                EncoderReason::ProbeFailed(format!("формат результата: {detail}"))
            })?;
            Ok(match other {
                Codec::Vp8 => SdrProfile::Vp8,
                Codec::Vp9 => SdrProfile::Vp9,
                Codec::Av1 => SdrProfile::Av1,
                Codec::H264 => unreachable!("H.264 is handled above"),
            })
        }
    }
}

/// `MF_MT_MPEG_SEQUENCE_HEADER` of the output type: an Annex B blob with the
/// SPS/PPS that the encoder keeps out of the access unit.
unsafe fn sequence_header(media_type: &IMFMediaType) -> Vec<u8> {
    let Ok(size) = (unsafe { media_type.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) }) else {
        return Vec::new();
    };
    if size == 0 || size > 64 * 1024 {
        return Vec::new();
    }
    let mut blob = vec![0u8; size as usize];
    match unsafe { media_type.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut blob, None) } {
        Ok(()) => blob,
        Err(_) => Vec::new(),
    }
}

unsafe fn renegotiate_output(
    transform: &IMFTransform,
    adapter: &AdapterInfo,
) -> Result<(), StageError> {
    for index in 0..8u32 {
        let Ok(media_type) = (unsafe { transform.GetOutputAvailableType(0, index) }) else {
            break;
        };
        if unsafe { transform.SetOutputType(0, &media_type, 0) }.is_ok() {
            return Ok(());
        }
    }
    Err(EncoderReason::ProfileUnsupported(format!(
        "кодировщик не согласовал выходной формат на {}",
        adapter.name
    ))
    .into())
}

unsafe fn input_sample(query: EncodeQuery, frame: u32) -> WinResult<IMFSample> {
    let luma = query.width as usize * query.height as usize;
    let chroma = luma / 2;
    let size = (luma + chroma) as u32;
    let buffer = unsafe { MFCreateMemoryBuffer(size) }?;
    let mut data: *mut u8 = ptr::null_mut();
    let mut max_length = 0u32;
    unsafe { buffer.Lock(&mut data, Some(&mut max_length), None) }?;
    if data.is_null() || max_length < size {
        unsafe {
            let _ = buffer.Unlock();
        }
        return Err(Error::from_hresult(E_FAIL));
    }
    unsafe {
        // Generated neutral black, never a captured or personal frame.
        ptr::write_bytes(data, 0x10, luma);
        ptr::write_bytes(data.add(luma), 0x80, chroma);
        buffer.Unlock()?;
        buffer.SetCurrentLength(size)?;
    }
    let sample = unsafe { MFCreateSample() }?;
    let duration = (10_000_000i64 / i64::from(query.fps.max(1))).max(1);
    unsafe {
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(duration.saturating_mul(i64::from(frame)))?;
        sample.SetSampleDuration(duration)?;
        if frame == 0 {
            // The trial encode must produce an IDR. A clean point is how an
            // encoder is told that this sample starts a keyframe; the driver
            // may still ignore it, which the IDR check below detects.
            sample.SetUINT32(&MFSampleExtension_CleanPoint, 1)?;
        }
    }
    Ok(sample)
}

unsafe fn output_sample(cb_size: u32) -> WinResult<IMFSample> {
    let size = cb_size.max(256 * 1024).min(16 * 1024 * 1024);
    let buffer = unsafe { MFCreateMemoryBuffer(size) }?;
    let sample = unsafe { MFCreateSample() }?;
    unsafe { sample.AddBuffer(&buffer) }?;
    Ok(sample)
}

unsafe fn sample_bytes(sample: &IMFSample) -> WinResult<Vec<u8>> {
    let buffer = unsafe { sample.ConvertToContiguousBuffer() }?;
    let mut data: *mut u8 = ptr::null_mut();
    let mut length = 0u32;
    unsafe { buffer.Lock(&mut data, None, Some(&mut length)) }?;
    let result = if data.is_null() || length == 0 || length > 16 * 1024 * 1024 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(data, length as usize) }.to_vec()
    };
    unsafe { buffer.Unlock() }?;
    Ok(result)
}

/// The keyframe shape of the three non-H.264 codecs. H.264 needs the output
/// type as well (profile attribute and sequence header), so it is checked in
/// [`verify_output_type`] through [`h264::keyframe_baseline`].
fn validate_frame(codec: Codec, bytes: &[u8], sequence_header: &[u8]) -> Result<(), String> {
    if bytes.is_empty() {
        return Err("пустой буфер".into());
    }
    match codec {
        Codec::H264 => Err("H.264 требует выходной тип".into()),
        Codec::Vp8 => {
            if bytes[0] & 0x01 == 0 {
                Ok(())
            } else {
                Err("VP8: первый кадр не ключевой".into())
            }
        }
        Codec::Vp9 => {
            if bytes[0] >> 6 == 0b10 {
                Ok(())
            } else {
                Err("VP9: нет frame marker".into())
            }
        }
        Codec::Av1 => av1::keyframe(bytes, sequence_header),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Activate,
    Profile,
    Input,
    Output,
}

impl Stage {
    const fn label(self) -> &'static str {
        match self {
            Self::Activate => "активация",
            Self::Profile => "профиль",
            Self::Input => "подача кадра",
            Self::Output => "получение кадра",
        }
    }
}

fn classify(error: Error, stage: Stage, adapter: &AdapterInfo) -> EncoderReason {
    let code = error.code();
    let detail = format!("{} (0x{:08x})", stage.label(), code.0 as u32);
    if code == MF_E_TRANSFORM_ASYNC_LOCKED {
        return EncoderReason::ProbeFailed(format!("асинхронный MFT не разблокирован: {detail}"));
    }
    if code == DXGI_ERROR_DEVICE_REMOVED
        || code == DXGI_ERROR_DEVICE_RESET
        || code == E_ACCESSDENIED
    {
        return EncoderReason::DeviceDisabled(format!("{}: {detail}", adapter.name));
    }
    if code == DXGI_ERROR_NOT_CURRENTLY_AVAILABLE
        || code == E_OUTOFMEMORY
        || code == MF_E_INVALIDREQUEST
        || (stage == Stage::Activate && code == E_FAIL)
    {
        return EncoderReason::EncoderBusy(detail);
    }
    if code == MF_E_INVALIDMEDIATYPE
        || code == MF_E_NO_MORE_TYPES
        || code == MF_E_TRANSFORM_NOT_POSSIBLE_FOR_CURRENT_INPUT_MEDIATYPE
        || code == MF_E_TRANSFORM_TYPE_NOT_SET
        || code == MF_E_TRANSFORM_STREAM_INVALID_RESOLUTION
    {
        return EncoderReason::ProfileUnsupported(detail);
    }
    EncoderReason::ProbeFailed(detail)
}

fn hresult(error: Error) -> String {
    format!("0x{:08x}", error.code().0 as u32)
}
