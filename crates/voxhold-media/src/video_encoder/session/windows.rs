//! Windows hardware encoder session: one activated MFT driven on its own
//! thread.
//!
//! The thread owns the MTA apartment, Media Foundation startup, the transform
//! and every buffer it hands to the driver. The engine thread only submits
//! NV12 frames through a bounded channel and polls encoded access units, so no
//! driver call can run on the UI or the voice loop.
//!
//! Keyframe requests (PLI/FIR forwarded by the pass-through encoder) and
//! bitrate targets are atomics, not commands: they must never be dropped by a
//! full frame queue. An output packet dropped because the consumer is behind
//! requests a keyframe, because the receiver missed a frame the following ones
//! refer to; a frame dropped **before** the encoder does not.

use std::{
    collections::VecDeque,
    mem::ManuallyDrop,
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::E_FAIL,
        Media::MediaFoundation::{
            CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncVideoForceKeyFrame, ICodecAPI,
            IMFActivate, IMFAttributes, IMFMediaEventGenerator, IMFMediaType, IMFSample,
            IMFTransform, METransformHaveOutput, METransformNeedInput, MF_E_NO_EVENTS_AVAILABLE,
            MF_E_NOTACCEPTING, MF_EVENT_FLAG_NO_WAIT, MF_EVENT_MFT_INPUT_STREAM_ID, MF_MT_SUBTYPE,
            MFCreateMemoryBuffer, MFCreateSample, MFSampleExtension_CleanPoint,
            MFT_ENUM_ADAPTER_LUID, MFT_ENUM_HARDWARE_VENDOR_ID_Attribute,
            MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_COMMAND_FLUSH,
            MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_STREAMING,
            MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
        },
        System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VT_UI4},
    },
    core::{Error, GUID, Interface, Result as WinResult},
};

use crate::screen_capture::CapturedImage;
use crate::video_encoder::{
    AdapterInfo, AdapterLuid, Codec, EncodeQuery, EncoderReason, av1, h264,
    windows::{
        EVENT_POLL, Stage, StageError, classify, codec_format, configure_types_for, enumerate_mfts,
        receive_output, sample_bytes, sequence_header, unlock_async,
    },
};
use tokio::sync::mpsc as input_queue;

use super::{
    AdapterEvidence, EncodeSubmit, EncodedAccessUnit, EncoderCounters, GpuEncoder,
    GpuEncoderFailure, Nv12Frame,
};

/// Frames waiting for the encoder. Two frames is the whole overrun a driver
/// may accumulate before the newest capture is dropped.
const INPUT_QUEUE: usize = 2;
/// Encoded packets waiting for the engine thread.
const OUTPUT_QUEUE: usize = 8;
/// Bound of a teardown join; the release thread must not hang on a driver.
const SHUTDOWN_JOIN: Duration = Duration::from_secs(3);
/// Bound of the activation handshake of [`HardwareEncoder::open`].
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// One live session handle. Dropping it releases the encoder thread and its
/// transform.
pub(crate) struct HardwareEncoder {
    frames: Option<input_queue::Sender<Nv12Frame>>,
    packets: Receiver<EncodedAccessUnit>,
    shared: Arc<Shared>,
    adapter: AdapterInfo,
    evidence: AdapterEvidence,
    thread: Option<JoinHandle<()>>,
    released: bool,
}

struct Shared {
    force_key: AtomicBool,
    bitrate: AtomicU32,
    stop: AtomicBool,
    failure: Mutex<Option<GpuEncoderFailure>>,
    counters: Counters,
}

#[derive(Default)]
struct Counters {
    submitted: AtomicU64,
    dropped_input: AtomicU64,
    encoded: AtomicU64,
    keyframes: AtomicU64,
    dropped_output: AtomicU64,
    keyframe_requests: AtomicU64,
    bitrate_bps: AtomicU32,
    bitrate_applied: AtomicBool,
}

impl Shared {
    fn new(bitrate_bps: u32) -> Self {
        let counters = Counters::default();
        counters.bitrate_bps.store(bitrate_bps, Ordering::Release);
        Self {
            force_key: AtomicBool::new(true),
            bitrate: AtomicU32::new(bitrate_bps),
            stop: AtomicBool::new(false),
            failure: Mutex::new(None),
            counters,
        }
    }

    fn counters(&self) -> EncoderCounters {
        EncoderCounters {
            submitted: self.counters.submitted.load(Ordering::Relaxed),
            dropped_input: self.counters.dropped_input.load(Ordering::Relaxed),
            encoded: self.counters.encoded.load(Ordering::Relaxed),
            keyframes: self.counters.keyframes.load(Ordering::Relaxed),
            dropped_output: self.counters.dropped_output.load(Ordering::Relaxed),
            keyframe_requests: self.counters.keyframe_requests.load(Ordering::Relaxed),
            bitrate_bps: self.counters.bitrate_bps.load(Ordering::Relaxed),
            bitrate_applied: self.counters.bitrate_applied.load(Ordering::Relaxed),
        }
    }

    fn fail(&self, message: String, device_lost: bool) {
        let mut failure = self.failure.lock().unwrap();
        if failure.is_none() {
            *failure = Some(GpuEncoderFailure {
                message,
                device_lost,
            });
        }
    }
}

impl HardwareEncoder {
    /// Activate one hardware transform and start its thread. An activation or
    /// type-negotiation error is returned here, before a publication exists.
    pub(crate) fn open(
        adapter: &AdapterInfo,
        codec: Codec,
        query: EncodeQuery,
        bitrate_bps: u32,
    ) -> Result<Self, String> {
        let shared = Arc::new(Shared::new(bitrate_bps));
        let (frames, frame_rx) = input_queue::channel(INPUT_QUEUE);
        let (packet_tx, packets) = mpsc::sync_channel(OUTPUT_QUEUE);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker_shared = shared.clone();
        let worker_adapter = adapter.clone();
        let thread = thread::Builder::new()
            .name("voxhold-hw-encoder".into())
            .spawn(move || {
                worker(
                    worker_adapter,
                    codec,
                    query,
                    worker_shared,
                    frame_rx,
                    packet_tx,
                    ready_tx,
                )
            })
            .map_err(|error| {
                format!("Не удалось запустить поток аппаратного кодировщика: {error}")
            })?;
        match ready_rx.recv_timeout(OPEN_TIMEOUT) {
            Ok(Ok(evidence)) => Ok(Self {
                frames: Some(frames),
                packets,
                shared,
                adapter: adapter.clone(),
                evidence,
                thread: Some(thread),
                released: false,
            }),
            Ok(Err(message)) => {
                join_bounded(thread);
                Err(message)
            }
            Err(_) => {
                shared.stop.store(true, Ordering::Release);
                join_bounded(thread);
                Err("Аппаратный кодировщик не открылся за 10 секунд.".into())
            }
        }
    }
    fn submit_input(&self, make_frame: impl FnOnce() -> Nv12Frame) -> EncodeSubmit {
        if self.shared.stop.load(Ordering::Acquire) {
            return EncodeSubmit::Closed;
        }
        let Some(frames) = self.frames.as_ref() else {
            return EncodeSubmit::Closed;
        };
        reserve_input(frames, &self.shared.counters.dropped_input, make_frame)
    }
}

fn reserve_input(
    frames: &input_queue::Sender<Nv12Frame>,
    dropped: &AtomicU64,
    make_frame: impl FnOnce() -> Nv12Frame,
) -> EncodeSubmit {
    match frames.try_reserve() {
        Ok(permit) => {
            permit.send(make_frame());
            EncodeSubmit::Queued
        }
        Err(input_queue::error::TrySendError::Full(())) => {
            dropped.fetch_add(1, Ordering::Relaxed);
            EncodeSubmit::Dropped
        }
        Err(input_queue::error::TrySendError::Closed(())) => EncodeSubmit::Closed,
    }
}

impl GpuEncoder for HardwareEncoder {
    fn submit(&mut self, frame: Nv12Frame) -> EncodeSubmit {
        self.submit_input(|| frame)
    }

    fn submit_captured(&mut self, frame: &CapturedImage, timestamp_us: i64) -> EncodeSubmit {
        self.submit_input(|| Nv12Frame::from_captured(frame, timestamp_us))
    }

    fn next_packet(&mut self) -> Option<EncodedAccessUnit> {
        match self.packets.try_recv() {
            Ok(packet) => Some(packet),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }

    fn request_keyframe(&mut self) {
        self.shared.force_key.store(true, Ordering::Release);
    }

    fn set_bitrate(&mut self, target_bps: u32) {
        if target_bps > 0 {
            self.shared.bitrate.store(target_bps, Ordering::Release);
        }
    }

    fn counters(&self) -> EncoderCounters {
        self.shared.counters()
    }

    fn adapter(&self) -> Option<AdapterInfo> {
        Some(self.adapter.clone())
    }

    fn adapter_evidence(&self) -> AdapterEvidence {
        self.evidence
    }

    fn failure(&self) -> Option<GpuEncoderFailure> {
        self.shared.failure.lock().unwrap().clone()
    }

    fn shutdown(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.shared.stop.store(true, Ordering::Release);
        self.frames.take();
        if let Some(thread) = self.thread.take() {
            join_bounded(thread);
        }
    }
}

impl Drop for HardwareEncoder {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn join_bounded(thread: JoinHandle<()>) {
    let deadline = Instant::now() + SHUTDOWN_JOIN;
    while !thread.is_finished() {
        if Instant::now() >= deadline {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }
    let _ = thread.join();
}

#[allow(clippy::too_many_arguments)]
fn worker(
    adapter: AdapterInfo,
    codec: Codec,
    query: EncodeQuery,
    shared: Arc<Shared>,
    mut frames: input_queue::Receiver<Nv12Frame>,
    packets: SyncSender<EncodedAccessUnit>,
    ready: SyncSender<Result<AdapterEvidence, String>>,
) {
    let platform = super::super::windows::WorkerPlatform::new();
    if !platform.is_ready() {
        let _ = ready.send(Err(
            "COM и Media Foundation не инициализировались на потоке кодировщика.".into(),
        ));
        return;
    }
    let subtype = codec_format(codec);
    let (transform, asynchronous, evidence) =
        match unsafe { activate(&adapter, codec, subtype, query, shared_bitrate(&shared)) } {
            Ok(pair) => pair,
            Err(message) => {
                let _ = ready.send(Err(message));
                return;
            }
        };
    let info = match unsafe { transform.GetOutputStreamInfo(0) } {
        Ok(info) => info,
        Err(error) => {
            let _ = ready.send(Err(format!(
                "Кодировщик не описал выходной поток: {}",
                error.code().0 as u32
            )));
            return;
        }
    };
    let provides_samples = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
    let header = match unsafe { transform.GetOutputCurrentType(0) } {
        Ok(media_type) => unsafe { sequence_header(&media_type) },
        Err(_) => Vec::new(),
    };
    let generator: Option<IMFMediaEventGenerator> = if asynchronous {
        transform.cast().ok()
    } else {
        None
    };
    let codec_api: Option<ICodecAPI> = transform.cast().ok();
    // The mean bitrate must be set before the encoder starts streaming: some
    // MFTs stop announcing input when it is changed on a live stream. Later
    // changes (rate control) use the same codec API on the running session.
    //
    // A driver that exposes no codec API (or refuses the value this early)
    // keeps the target and retries it once streaming has started, instead of
    // dropping the requested bitrate or spinning on a rejecting driver.
    let initial = shared.bitrate.swap(0, Ordering::AcqRel);
    let mut initial_retry =
        (initial > 0 && !apply_bitrate(codec_api.as_ref(), &shared, initial)).then_some(initial);
    unsafe {
        let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
        let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
    }
    if ready.send(Ok(evidence)).is_err() {
        unsafe { stop_stream(&transform) };
        return;
    }

    let mut pending: VecDeque<Nv12Frame> = VecDeque::new();
    let mut key_pending = true;
    let mut kicked = false;
    let mut last_timestamp = 0i64;
    let duration = (10_000_000i64 / i64::from(query.fps.max(1))).max(1);
    let mut disconnected = false;
    loop {
        if shared.stop.load(Ordering::Acquire) {
            break;
        }
        if shared.force_key.swap(false, Ordering::AcqRel) {
            shared
                .counters
                .keyframe_requests
                .fetch_add(1, Ordering::Relaxed);
            key_pending = true;
        }
        let requested = shared.bitrate.swap(0, Ordering::AcqRel);
        if requested > 0 {
            let applied = apply_bitrate(codec_api.as_ref(), &shared, requested);
            let starting = shared.counters.submitted.load(Ordering::Relaxed) == 0;
            initial_retry = (!applied && starting).then_some(requested);
        }
        while pending.len() < INPUT_QUEUE {
            match frames.try_recv() {
                Ok(frame) => pending.push_back(frame),
                Err(input_queue::error::TryRecvError::Empty) => break,
                Err(input_queue::error::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        let mut have_output = generator.is_none();
        let mut input_stream = 0u32;
        if let Some(generator) = generator.as_ref() {
            loop {
                match unsafe { generator.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                    Ok(event) => {
                        if let Ok(status) = unsafe { event.GetStatus() }
                            && status.0 < 0
                        {
                            fail(
                                &shared,
                                classify(Error::from_hresult(status), Stage::Output, &adapter),
                            );
                            unsafe { stop_stream(&transform) };
                            return;
                        }
                        if let Ok(kind) = unsafe { event.GetType() } {
                            if kind == METransformNeedInput.0 as u32 {
                                input_stream =
                                    unsafe { event.GetUINT32(&MF_EVENT_MFT_INPUT_STREAM_ID) }
                                        .unwrap_or(0);
                            } else if kind == METransformHaveOutput.0 as u32 {
                                have_output = true;
                            }
                        }
                    }
                    Err(error) if error.code() == MF_E_NO_EVENTS_AVAILABLE => break,
                    Err(error) => {
                        fail(&shared, classify(error, Stage::Output, &adapter));
                        unsafe { stop_stream(&transform) };
                        return;
                    }
                }
            }
        }
        let mut worked = false;
        if let Some(frame) = pending.front() {
            let timestamp = frame.timestamp_us.max(last_timestamp + 1);
            if key_pending {
                force_keyframe(codec_api.as_ref());
            }
            let sample = match unsafe { input_sample(frame, timestamp * 10, duration, key_pending) }
            {
                Ok(sample) => sample,
                Err(error) => {
                    fail(&shared, classify(error, Stage::Input, &adapter));
                    unsafe { stop_stream(&transform) };
                    return;
                }
            };
            match unsafe { transform.ProcessInput(input_stream, &sample, 0) } {
                Ok(()) => {
                    pending.pop_front();
                    last_timestamp = timestamp;
                    shared.counters.submitted.fetch_add(1, Ordering::Relaxed);
                    kicked = true;
                    worked = true;
                }
                Err(error) if error.code() == MF_E_NOTACCEPTING => {}
                Err(error) => {
                    fail(&shared, classify(error, Stage::Input, &adapter));
                    unsafe { stop_stream(&transform) };
                    return;
                }
            }
        }
        if kicked {
            have_output = true;
        }
        if let Some(target) = initial_retry
            && shared.counters.submitted.load(Ordering::Relaxed) > 0
        {
            // One retry on the live session: some MFTs only accept the mean
            // bitrate after the first input. It is not re-armed forever.
            initial_retry = None;
            let _ = apply_bitrate(codec_api.as_ref(), &shared, target);
        }
        if have_output {
            have_output = false;
            match unsafe { receive_output(&transform, info.cbSize, provides_samples, &adapter) } {
                Ok(Some(sample)) => {
                    worked = true;
                    match emit(codec, &header, &sample, &packets, &shared) {
                        Emit::Stop => {
                            unsafe { stop_stream(&transform) };
                            return;
                        }
                        Emit::Encoded { keyframe } => {
                            // Only a keyframe access unit ends the keyframe
                            // request: a driver that ignored CleanPoint on the
                            // first input must get the flag again, otherwise the
                            // stream starts with an undecodable delta frame.
                            if keyframe {
                                key_pending = false;
                            }
                        }
                        Emit::Empty => {}
                    }
                }
                Ok(None) => {}
                Err(StageError::Reason(EncoderReason::ProbeFailed(detail)))
                    if detail.contains("0x8000ffff") =>
                {
                    // A synchronous MIL/async driver answers E_UNEXPECTED when
                    // ProcessOutput runs before a frame is ready. It is "no
                    // output yet", not a dead session.
                }
                Err(StageError::Reason(reason)) => {
                    fail(&shared, reason);
                    unsafe { stop_stream(&transform) };
                    return;
                }
                Err(StageError::Cancelled) => {}
            }
        }
        if !worked {
            thread::sleep(EVENT_POLL);
        }
        if disconnected && pending.is_empty() {
            break;
        }
    }
    unsafe { stop_stream(&transform) };
}

fn shared_bitrate(shared: &Shared) -> u32 {
    shared.counters.bitrate_bps.load(Ordering::Acquire)
}

unsafe fn activate(
    adapter: &AdapterInfo,
    codec: Codec,
    subtype: GUID,
    query: EncodeQuery,
    bitrate: u32,
) -> Result<(IMFTransform, bool, AdapterEvidence), String> {
    let activates = unsafe { enumerate_mfts(adapter, subtype) }.map_err(|error| {
        format!(
            "Не удалось перечислить аппаратные кодировщики {}: 0x{:08x}",
            codec.label(),
            error.code().0 as u32
        )
    })?;
    if activates.is_empty() {
        return Err(EncoderReason::EncoderNotProvided(format!(
            "{} на адаптере {}",
            codec.label(),
            adapter.name
        ))
        .to_string());
    }
    let mut last = None;
    for activate in &activates {
        let result = (|| -> Result<(IMFTransform, bool, AdapterEvidence), EncoderReason> {
            let transform: IMFTransform = unsafe { activate.ActivateObject() }
                .map_err(|error| classify(error, Stage::Activate, adapter))?;
            let asynchronous = unsafe { unlock_async(&transform, activate) }
                .map_err(|error| classify(error, Stage::Activate, adapter))?;
            unsafe { configure_types_for(&transform, codec, subtype, query, bitrate, adapter) }
                .map_err(|error| match error {
                    StageError::Reason(reason) => reason,
                    StageError::Cancelled => {
                        EncoderReason::ProbeFailed("активация отменена".into())
                    }
                })?;
            let current: IMFMediaType = unsafe { transform.GetOutputCurrentType(0) }
                .map_err(|error| classify(error, Stage::Profile, adapter))?;
            let chosen = unsafe { current.GetGUID(&MF_MT_SUBTYPE) }
                .map_err(|error| classify(error, Stage::Profile, adapter))?;
            if chosen != subtype {
                return Err(EncoderReason::ProbeFailed(
                    "кодировщик сменил выходной формат".into(),
                ));
            }
            let evidence = unsafe { mft_adapter_evidence(&transform, activate, adapter) }?;
            Ok((transform, asynchronous, evidence))
        })();
        match result {
            Ok(pair) => return Ok(pair),
            Err(reason) => last = Some(reason),
        }
    }
    Err(last
        .map(|reason| reason.to_string())
        .unwrap_or_else(|| "Ни один аппаратный кодировщик не открылся.".into()))
}

/// What the activated transform reports about its own adapter.
///
/// `MFT_ENUM_ADAPTER_LUID` is the enumeration attribute Media Foundation uses
/// for a per-adapter hardware enumeration. A driver may keep it on the
/// activation object, in the transform's attribute store, or expose neither;
/// only a value that is really present is treated as evidence. The value is
/// compared with the requested LUID, so an enumeration that silently picked
/// another adapter is refused instead of being presented as the plan. When the
/// LUID cannot be read the hardware vendor id is compared where the driver
/// exposes it, and the session keeps the requested adapter and reports
/// [`AdapterEvidence::PlanEcho`]; that value is not proof of which adapter of
/// the vendor is open.
unsafe fn mft_adapter_evidence(
    transform: &IMFTransform,
    activate: &IMFActivate,
    requested: &AdapterInfo,
) -> Result<AdapterEvidence, EncoderReason> {
    let read =
        |attributes: &IMFAttributes| unsafe { attributes.GetUINT64(&MFT_ENUM_ADAPTER_LUID).ok() };
    let raw = unsafe { transform.GetAttributes() }
        .ok()
        .and_then(|attributes| read(&attributes))
        .or_else(|| read(activate));
    let Some(raw) = raw.filter(|value| *value != 0) else {
        // Media Foundation does not expose the adapter LUID on this driver. The
        // hardware vendor id is a weaker but real attribute: it can prove that
        // the MFT belongs to another vendor, even though it cannot distinguish
        // two adapters of the same vendor.
        if let Some(vendor) = unsafe { mft_vendor(transform, activate) }
            && vendor != requested.vendor_id
        {
            return Err(EncoderReason::DeviceDisabled(format!(
                "MFT активировался на оборудовании производителя 0x{vendor:04x}, а запрошен {} (0x{:04x})",
                requested.name, requested.vendor_id
            )));
        }
        return Ok(AdapterEvidence::PlanEcho);
    };
    let found = AdapterLuid::new(raw as u32, (raw >> 32) as u32);
    if found != requested.luid {
        return Err(EncoderReason::DeviceDisabled(format!(
            "MFT активировался на адаптере {found}, а запрошен {}",
            requested.luid
        )));
    }
    Ok(AdapterEvidence::MftAttribute)
}

/// `MFT_ENUM_HARDWARE_VENDOR_ID_Attribute` of the open transform or its
/// activation, parsed from the `"VEN_10DE"` form the driver reports.
unsafe fn mft_vendor(transform: &IMFTransform, activate: &IMFActivate) -> Option<u32> {
    let read = |attributes: &IMFAttributes| {
        let length =
            unsafe { attributes.GetStringLength(&MFT_ENUM_HARDWARE_VENDOR_ID_Attribute) }.ok()?;
        let mut buffer = vec![0u16; length as usize + 1];
        unsafe { attributes.GetString(&MFT_ENUM_HARDWARE_VENDOR_ID_Attribute, &mut buffer, None) }
            .ok()?;
        super::hardware_vendor(&String::from_utf16_lossy(&buffer))
    };
    unsafe { transform.GetAttributes() }
        .ok()
        .and_then(|attributes| read(&attributes))
        .or_else(|| read(activate))
}

unsafe fn input_sample(
    frame: &Nv12Frame,
    sample_time: i64,
    duration: i64,
    key: bool,
) -> WinResult<IMFSample> {
    let size = u32::try_from(frame.data.len()).map_err(|_| Error::from_hresult(E_FAIL))?;
    if size == 0 {
        return Err(Error::from_hresult(E_FAIL));
    }
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
        ptr::copy_nonoverlapping(frame.data.as_ptr(), data, frame.data.len());
        buffer.Unlock()?;
        buffer.SetCurrentLength(size)?;
    }
    let sample = unsafe { MFCreateSample() }?;
    unsafe {
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(sample_time)?;
        sample.SetSampleDuration(duration)?;
        if key {
            sample.SetUINT32(&MFSampleExtension_CleanPoint, 1)?;
        }
    }
    Ok(sample)
}

/// What one encoded sample did.
enum Emit {
    /// A packet was queued; `keyframe` is its real type.
    Encoded { keyframe: bool },
    /// The sample was empty (the driver only announced output).
    Empty,
    /// The consumer is gone; the session must stop.
    Stop,
}

fn emit(
    codec: Codec,
    header: &[u8],
    sample: &IMFSample,
    packets: &SyncSender<EncodedAccessUnit>,
    shared: &Shared,
) -> Emit {
    let timestamp = unsafe { sample.GetSampleTime() }.unwrap_or(0);
    let bytes = match unsafe { sample_bytes(sample) } {
        Ok(bytes) => bytes,
        Err(error) => {
            shared.fail(
                format!(
                    "Кодировщик не отдал буфер кадра: 0x{:08x}",
                    error.code().0 as u32
                ),
                false,
            );
            return Emit::Stop;
        }
    };
    if bytes.is_empty() {
        return Emit::Empty;
    }
    let keyframe = is_keyframe(codec, &bytes, header);
    let mut payload = bytes;
    if keyframe {
        payload = with_decoder_config(codec, payload, header);
    }
    shared.counters.encoded.fetch_add(1, Ordering::Relaxed);
    if keyframe {
        shared.counters.keyframes.fetch_add(1, Ordering::Relaxed);
    }
    let packet = EncodedAccessUnit {
        payload,
        keyframe,
        timestamp_us: (timestamp / 10).max(0),
    };
    match packets.try_send(packet) {
        Ok(()) => Emit::Encoded { keyframe },
        Err(TrySendError::Full(_)) => {
            // An encoded frame the receiver never gets breaks every following
            // delta frame; only a new keyframe repairs that chain.
            shared
                .counters
                .dropped_output
                .fetch_add(1, Ordering::Relaxed);
            shared.force_key.store(true, Ordering::Release);
            Emit::Encoded { keyframe }
        }
        Err(TrySendError::Disconnected(_)) => {
            shared.stop.store(true, Ordering::Release);
            Emit::Stop
        }
    }
}

fn is_keyframe(codec: Codec, bytes: &[u8], header: &[u8]) -> bool {
    match codec {
        Codec::H264 => h264::nal_types(bytes).contains(&5),
        Codec::Vp8 => bytes.first().is_some_and(|byte| byte & 0x01 == 0),
        // VP9 uncompressed header: frame_marker (2 bits), profile low/high,
        // show_existing_frame, then frame_type where 0 is KEY_FRAME.
        Codec::Vp9 => bytes
            .first()
            .is_some_and(|byte| byte >> 6 == 0b10 && byte & 0x04 == 0),
        Codec::Av1 => av1::keyframe(bytes, header).is_ok(),
    }
}

/// H.264 needs SPS/PPS in the stream, AV1 needs its sequence header. A driver
/// may keep both only in the output type, so the first access unit of a
/// keyframe gets them prepended when the buffer itself lacks them.
fn with_decoder_config(codec: Codec, mut payload: Vec<u8>, header: &[u8]) -> Vec<u8> {
    if header.is_empty() {
        return payload;
    }
    let needs = match codec {
        Codec::H264 => {
            let types = h264::nal_types(&payload);
            !(types.contains(&7) && types.contains(&8))
        }
        Codec::Av1 => !av1::has_sequence_header(&payload),
        _ => false,
    };
    if !needs {
        return payload;
    }
    let mut combined = Vec::with_capacity(header.len() + payload.len());
    combined.extend_from_slice(header);
    combined.append(&mut payload);
    combined
}

fn apply_bitrate(api: Option<&ICodecAPI>, shared: &Shared, target_bps: u32) -> bool {
    let applied = api.is_some_and(|api| {
        let value = ui4_variant(target_bps);
        unsafe { api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &value) }.is_ok()
    });
    shared
        .counters
        .bitrate_bps
        .store(target_bps, Ordering::Release);
    shared
        .counters
        .bitrate_applied
        .store(applied, Ordering::Release);
    applied
}

fn force_keyframe(api: Option<&ICodecAPI>) {
    if let Some(api) = api {
        let value = ui4_variant(1);
        let _ = unsafe { api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &value) };
    }
}

fn ui4_variant(value: u32) -> VARIANT {
    let mut variant = VARIANT::default();
    unsafe {
        variant.Anonymous = VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_UI4,
                ..Default::default()
            }),
        };
        let inner: &mut VARIANT_0_0 = &mut variant.Anonymous.Anonymous;
        inner.Anonymous.ulVal = value;
    }
    variant
}

fn fail(shared: &Shared, reason: EncoderReason) {
    let device_lost = matches!(reason, EncoderReason::DeviceDisabled(_));
    shared.fail(
        format!("Аппаратный кодировщик остановился: {reason}"),
        device_lost,
    );
}

unsafe fn stop_stream(transform: &IMFTransform) {
    unsafe {
        let _ = transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
        let _ = transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
    }
}

#[cfg(test)]
mod input_queue_tests {
    use super::*;

    #[test]
    fn a_full_or_closed_queue_drops_before_converting_capture() {
        let (sender, mut receiver) = input_queue::channel(1);
        let dropped = AtomicU64::new(0);
        assert_eq!(
            reserve_input(&sender, &dropped, || Nv12Frame {
                width: 2,
                height: 2,
                data: vec![0; 6],
                timestamp_us: 1,
            }),
            EncodeSubmit::Queued
        );
        assert_eq!(
            reserve_input(&sender, &dropped, || panic!("no queue space")),
            EncodeSubmit::Dropped
        );
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        assert_eq!(receiver.try_recv().unwrap().timestamp_us, 1);
        drop(receiver);
        assert_eq!(
            reserve_input(&sender, &dropped, || panic!("encoder is gone")),
            EncodeSubmit::Closed
        );
    }
}
