//! Real Windows hardware encoding: DXGI adapters and Media Foundation MFTs.
//!
//! Everything in this file runs on the probe worker, which owns one MTA
//! apartment and one Media Foundation startup. The trial encode uses a
//! generated black NV12 frame, never user content, and its encoded buffer is
//! dropped before the function returns.

use std::{ffi::c_void, mem::ManuallyDrop, ptr};

use windows::{
    Win32::{
        Foundation::{E_ACCESSDENIED, E_FAIL, E_OUTOFMEMORY},
        Graphics::Dxgi::{
            CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_DEVICE_REMOVED,
            DXGI_ERROR_DEVICE_RESET, DXGI_ERROR_NOT_CURRENTLY_AVAILABLE, IDXGIAdapter1,
            IDXGIFactory1,
        },
        Media::MediaFoundation::{
            IMFActivate, IMFAttributes, IMFMediaType, IMFSample, IMFTransform,
            MF_E_INVALIDMEDIATYPE, MF_E_INVALIDREQUEST, MF_E_NO_MORE_TYPES, MF_E_NOTACCEPTING,
            MF_E_TRANSFORM_NEED_MORE_INPUT,
            MF_E_TRANSFORM_NOT_POSSIBLE_FOR_CURRENT_INPUT_MEDIATYPE, MF_E_TRANSFORM_STREAM_CHANGE,
            MF_E_TRANSFORM_STREAM_INVALID_RESOLUTION, MF_E_TRANSFORM_TYPE_NOT_SET,
            MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
            MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_SUBTYPE, MF_VERSION, MFCreateAttributes,
            MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video,
            MFSTARTUP_FULL, MFShutdown, MFStartup, MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_ADAPTER_LUID, MFT_ENUM_FLAG_HARDWARE, MFT_MESSAGE_COMMAND_FLUSH,
            MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
            MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO,
            MFTEnum2, MFVideoFormat_AV1, MFVideoFormat_H264, MFVideoFormat_NV12,
            MFVideoFormat_VP80, MFVideoFormat_VP90, MFVideoInterlace_Progressive,
            eAVEncH264VProfile_ConstrainedBase,
        },
        System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
    },
    core::{Error, GUID, Result as WinResult},
};

use super::{
    AdapterInfo, AdapterInventory, AdapterLuid, Codec, EncodeQuery, EncoderReason, H264Baseline,
    ProbeCancel, ProbeOutcome, SdrProfile, probe::HardwareProbe,
};

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
        let Some(subtype) = codec_format(codec) else {
            return ProbeOutcome::Unavailable(EncoderReason::EncoderNotProvided(format!(
                "Windows MFT не предоставляет формат {}",
                codec.label()
            )));
        };
        match unsafe { probe_stages(adapter, codec, subtype, query, cancel) } {
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
        let desc = unsafe { adapter.GetDesc1() }?;
        let luid = AdapterLuid::new(desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart as u32);
        let software = desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0
            || (desc.VendorId == 0x1414 && desc.DeviceId == 0x8c);
        if !software && primary.is_none() && unsafe { owns_primary_output(&adapter) } {
            primary = Some(luid);
        }
        let driver_version = unsafe { driver_version(&adapter) }.unwrap_or(0);
        adapters.push(AdapterInfo {
            luid,
            name: adapter_name(&desc.Description),
            vendor_id: desc.VendorId,
            device_id: desc.DeviceId,
            driver_version,
            dedicated_video_memory: desc.DedicatedVideoMemory as u64,
            software,
        });
    }
    Ok(AdapterInventory::new(adapters, primary))
}

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

unsafe fn driver_version(adapter: &IDXGIAdapter1) -> WinResult<u64> {
    // IID_ID3D11Device. `CheckInterfaceSupport` returns the D3D11 UMD version
    // for a D3D10.1/D3D11 interface without creating a device.
    const ID3D11_DEVICE: GUID = GUID::from_u128(0xdb6f6ddb_ac77_4e88_8253_819df9bbf140);
    unsafe { adapter.CheckInterfaceSupport(&ID3D11_DEVICE) }.map(|version| version as u64)
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

fn codec_format(codec: Codec) -> Option<GUID> {
    Some(match codec {
        Codec::H264 => MFVideoFormat_H264,
        Codec::Vp8 => MFVideoFormat_VP80,
        Codec::Vp9 => MFVideoFormat_VP90,
        Codec::Av1 => MFVideoFormat_AV1,
    })
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
    unsafe { configure_types(&transform, codec, subtype, query, adapter)? };
    if cancel.is_cancelled() {
        return Err(StageError::Cancelled);
    }
    unsafe { encode_keyframe(&transform, codec, query, adapter, cancel)? };
    unsafe { verify_output_type(&transform, codec, subtype, query, adapter) }
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

unsafe fn encode_keyframe(
    transform: &IMFTransform,
    codec: Codec,
    query: EncodeQuery,
    adapter: &AdapterInfo,
    cancel: &ProbeCancel,
) -> Result<(), StageError> {
    let info = unsafe { transform.GetOutputStreamInfo(0) }
        .map_err(|error| classify(error, Stage::Profile, adapter))?;
    let provides_samples = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
    unsafe {
        let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
        let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
    }
    let mut frames = 0;
    let mut need_input = true;
    for _ in 0..64 {
        if cancel.is_cancelled() {
            let _ = unsafe { transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0) };
            return Err(StageError::Cancelled);
        }
        if need_input && frames < 8 {
            let sample = unsafe { input_sample(query, frames) }
                .map_err(|error| classify(error, Stage::Encode, adapter))?;
            match unsafe { transform.ProcessInput(0, &sample, 0) } {
                Ok(()) => {
                    frames += 1;
                    need_input = false;
                }
                Err(error) if error.code() == MF_E_NOTACCEPTING => {}
                Err(error) => return Err(classify(error, Stage::Encode, adapter).into()),
            }
        }
        let sample = if provides_samples {
            None
        } else {
            Some(
                unsafe { output_sample(info.cbSize) }
                    .map_err(|error| classify(error, Stage::Encode, adapter))?,
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
            Ok(()) => {
                let Some(sample) = sample else {
                    continue;
                };
                let bytes = unsafe { sample_bytes(&sample) }
                    .map_err(|error| classify(error, Stage::Encode, adapter))?;
                validate_keyframe(codec, &bytes).map_err(|detail| {
                    EncoderReason::ProbeFailed(format!("формат результата: {detail}"))
                })?;
                return Ok(());
            }
            Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                need_input = true;
            }
            Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                unsafe { renegotiate_output(transform, adapter)? };
                need_input = true;
            }
            Err(error) => return Err(classify(error, Stage::Encode, adapter).into()),
        }
    }
    Err(
        EncoderReason::ProbeFailed("кодировщик не выдал кадр за ограниченное число попыток".into())
            .into(),
    )
}

unsafe fn verify_output_type(
    transform: &IMFTransform,
    codec: Codec,
    subtype: GUID,
    query: EncodeQuery,
    adapter: &AdapterInfo,
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
    match codec {
        Codec::H264 => {
            let profile = unsafe { media_type.GetUINT32(&MF_MT_MPEG2_PROFILE) }.unwrap_or(256);
            let baseline = match profile {
                256 => H264Baseline::Constrained,
                66 => H264Baseline::Legacy,
                other => {
                    return Err(EncoderReason::ProfileUnsupported(format!(
                        "драйвер выдал H.264 profile {other} вместо Baseline"
                    ))
                    .into());
                }
            };
            Ok(SdrProfile::H264 {
                baseline,
                level: query.h264_level(),
            })
        }
        Codec::Vp8 => Ok(SdrProfile::Vp8),
        Codec::Vp9 => Ok(SdrProfile::Vp9),
        Codec::Av1 => Ok(SdrProfile::Av1),
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

fn validate_keyframe(codec: Codec, bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() {
        return Err("пустой буфер".into());
    }
    match codec {
        Codec::H264 => {
            let types = h264_nal_types(bytes);
            if types.is_empty() {
                return Err("нет Annex B start code".into());
            }
            if !types.contains(&5) {
                return Err("нет IDR-кадра".into());
            }
            if !types.contains(&7) && !types.contains(&8) {
                return Err("нет SPS/PPS".into());
            }
            Ok(())
        }
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
        Codec::Av1 => {
            let mut index = 0;
            while index < bytes.len() && index < 64 {
                let header = bytes[index];
                if header & 0x80 != 0 {
                    return Err("AV1: forbidden bit".into());
                }
                let obu_type = (header >> 3) & 0x0f;
                let has_size = header & 0x02 != 0;
                if obu_type == 1 || obu_type == 6 {
                    return Ok(());
                }
                if !has_size {
                    break;
                }
                index += 1;
                let mut size = 0usize;
                let mut shift = 0;
                loop {
                    let Some(byte) = bytes.get(index) else {
                        break;
                    };
                    index += 1;
                    size |= ((byte & 0x7f) as usize) << shift;
                    if byte & 0x80 == 0 {
                        break;
                    }
                    shift += 7;
                    if shift > 28 {
                        break;
                    }
                }
                index = index.saturating_add(size);
            }
            Err("AV1: нет sequence header или frame OBU".into())
        }
    }
}

fn h264_nal_types(bytes: &[u8]) -> Vec<u8> {
    let mut types = Vec::new();
    let mut index = 0;
    while index + 3 < bytes.len() {
        let start = if bytes[index] == 0
            && bytes[index + 1] == 0
            && bytes[index + 2] == 0
            && bytes[index + 3] == 1
        {
            Some(index + 4)
        } else if bytes[index] == 0 && bytes[index + 1] == 0 && bytes[index + 2] == 1 {
            Some(index + 3)
        } else {
            None
        };
        if let Some(header) = start {
            if let Some(byte) = bytes.get(header) {
                types.push(byte & 0x1f);
            }
            index = header;
        } else {
            index += 1;
        }
    }
    types
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Activate,
    Profile,
    Encode,
}

impl Stage {
    const fn label(self) -> &'static str {
        match self {
            Self::Activate => "активация",
            Self::Profile => "профиль",
            Self::Encode => "пробное кодирование",
        }
    }
}

fn classify(error: Error, stage: Stage, adapter: &AdapterInfo) -> EncoderReason {
    let code = error.code();
    let detail = format!("{} (0x{:08x})", stage.label(), code.0 as u32);
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
