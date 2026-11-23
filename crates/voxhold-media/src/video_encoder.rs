//! Optional hardware video encoding for Windows screen publication.
//!
//! This is the media model of the GPU slice. It enumerates the real DXGI
//! adapters, probes a hardware MFT per adapter and codec, and exposes one
//! immutable snapshot with a generation. The publisher engine, the loopback
//! encoder and the picker are deliberately not wired here: this module answers
//! only "which adapter and which encoder can this Windows machine confirm for
//! these parameters", so the following slices can preflight a publication
//! without changing the current CPU path.
//!
//! Identity of an adapter is its DXGI LUID, never its display name. The
//! automatic choice is the suitable adapter that owns the Windows primary
//! desktop output, then the first adapter in the stable DXGI order. WARP and
//! other software adapters are not candidates at all.
//!
//! GPU availability is bound to an adapter, a driver version, a device-list
//! generation and the requested codec/resolution/FPS. A cached confirmation is
//! dropped when the inventory changes, and an activation error always removes
//! it. A real publication must call [`EncoderProbeService::revalidate`] on the
//! media worker before `stream.start`; a snapshot alone is not a launch permit.
//!
//! The worker owns its COM apartment and every MFT object. Requests are
//! bounded to one active probe; a newer request cancels the old one and a late
//! result is discarded by generation. Cancel, stop and drop release the
//! residual frames, transforms and activations on that worker.

mod probe;
mod service;
mod session;

#[cfg(windows)]
mod av1;
#[cfg(windows)]
mod driver_version;
#[cfg(windows)]
mod h264;
#[cfg(not(windows))]
mod unsupported;
#[cfg(windows)]
mod windows;

#[cfg(test)]
mod fake;
#[cfg(all(test, windows))]
mod hardware_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use session::FakeGpuEncoder;

use std::fmt;

#[cfg(test)]
use crate::publisher::H264Baseline;
use crate::publisher::{Codec, CodecIntent, CodecRole, CodecSnapshot, SdrProfile};
use crate::screen_capture::SourceIdentity;

pub use probe::{HardwareProbe, ProbeCancel, ProbeOutcome};
pub use service::{EncoderProbeService, EncoderRequest, EncoderRequestError, RevalidateError};
pub use session::{
    AdapterEvidence, EncodeSubmit, EncodedAccessUnit, EncoderCounters, GpuEncoder,
    GpuEncoderFailure, Nv12Frame, open_gpu_encoder,
};

#[cfg(not(windows))]
pub use unsupported::UnsupportedHardwareProbe as PlatformProbe;
#[cfg(windows)]
pub use windows::WindowsHardwareProbe as PlatformProbe;
#[cfg(all(test, windows))]
pub(crate) use windows::WorkerPlatform;

/// The publisher's four SDR codecs in the website's automatic order.
pub const ENCODER_CODECS: [Codec; 4] = Codec::ALL;

/// One DXGI adapter LUID. It is the stable in-process identity of an adapter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AdapterLuid {
    pub low: u32,
    pub high: u32,
}

impl AdapterLuid {
    pub const fn new(low: u32, high: u32) -> Self {
        Self { low, high }
    }

    /// The `UINT64` packing Media Foundation expects in
    /// `MFT_ENUM_ADAPTER_LUID`: `HighPart << 32 | LowPart`.
    pub const fn to_u64(self) -> u64 {
        (self.high as u64) << 32 | self.low as u64
    }
}

impl fmt::Display for AdapterLuid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:08x}:{:08x}", self.high, self.low)
    }
}

/// A suitable (non-software) adapter as this machine reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterInfo {
    pub luid: AdapterLuid,
    pub name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    /// Driver version as the cache identity: the packed dotted
    /// `major.minor.build.revision` triple (`32.0.16.1692` is
    /// `0x0020_0000_0010_069c`), the same number
    /// `Win32_VideoController.DriverVersion` prints. Zero means neither the
    /// D3D10 UMD query nor the display-class registry entry answered, so a
    /// driver update cannot change the identity on that machine.
    pub driver_version: u64,
    pub dedicated_video_memory: u64,
    /// WARP and the Microsoft Basic Render Driver are marked here by the
    /// platform probe and removed by [`AdapterInventory::new`].
    pub software: bool,
}

impl AdapterInfo {
    /// The part of the identity a cached probe belongs to.
    pub fn key(&self) -> AdapterKey {
        AdapterKey {
            luid: self.luid,
            driver_version: self.driver_version,
        }
    }
}

/// Cache identity of one adapter and driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AdapterKey {
    pub luid: AdapterLuid,
    pub driver_version: u64,
}

/// The adapter list of one enumeration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterInventory {
    adapters: Vec<AdapterInfo>,
    primary: Option<AdapterLuid>,
    generation: u64,
}

impl AdapterInventory {
    /// Keep only hardware adapters, validate the primary LUID and derive a
    /// generation from the whole list. Any adapter, driver version, name or
    /// primary-output change yields another generation.
    pub fn new(adapters: Vec<AdapterInfo>, primary: Option<AdapterLuid>) -> Self {
        let mut adapters: Vec<AdapterInfo> = adapters
            .into_iter()
            .filter(|adapter| !adapter.software)
            .collect();
        let primary = primary.filter(|luid| adapters.iter().any(|adapter| adapter.luid == *luid));
        adapters.shrink_to_fit();
        let generation = inventory_signature(&adapters, primary);
        Self {
            adapters,
            primary,
            generation,
        }
    }

    /// The content signature of this enumeration. It is an equality token, not
    /// an ordered counter: two enumerations with the same adapters, driver
    /// versions, names and primary output share it, and any difference changes
    /// it. The probe cache compares it only for equality.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn adapters(&self) -> &[AdapterInfo] {
        &self.adapters
    }

    pub fn primary(&self) -> Option<AdapterLuid> {
        self.primary
    }

    pub fn find(&self, luid: AdapterLuid) -> Option<&AdapterInfo> {
        self.adapters.iter().find(|adapter| adapter.luid == luid)
    }

    /// Resolve the user's adapter selection. The automatic choice is the
    /// Windows primary suitable adapter, else the first suitable adapter in
    /// the stable DXGI order.
    pub fn resolve(&self, selection: AdapterSelection) -> Result<&AdapterInfo, EncoderReason> {
        match selection {
            AdapterSelection::Auto => {
                if let Some(primary) = self.primary
                    && let Some(adapter) = self.find(primary)
                {
                    return Ok(adapter);
                }
                self.adapters.first().ok_or_else(|| {
                    EncoderReason::DeviceDisabled(
                        "нет аппаратной видеокарты, кроме WARP или программного рендера".into(),
                    )
                })
            }
            AdapterSelection::Explicit(luid) => self.find(luid).ok_or_else(|| {
                EncoderReason::DeviceDisabled(format!(
                    "видеокарта {luid} не подключена или отключена"
                ))
            }),
        }
    }
}

fn inventory_signature(adapters: &[AdapterInfo], primary: Option<AdapterLuid>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    primary.hash(&mut hasher);
    for adapter in adapters {
        adapter.luid.hash(&mut hasher);
        adapter.name.hash(&mut hasher);
        adapter.vendor_id.hash(&mut hasher);
        adapter.device_id.hash(&mut hasher);
        adapter.driver_version.hash(&mut hasher);
    }
    hasher.finish()
}

/// Which adapter the user (or the automatic rule) wants.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AdapterSelection {
    #[default]
    Auto,
    Explicit(AdapterLuid),
}

/// The backend preference of the settings dialog.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EncoderBackend {
    /// The WebRTC software encoders of the pinned build. This is the default.
    #[default]
    Cpu,
    /// A hardware MFT bound to the selected adapter.
    Gpu,
}

/// The user's backend choice. `allow_fallback` only permits replacing an
/// unusable GPU backend as a whole with CPU while keeping the codec. A codec
/// that a working GPU lacks is always refused, never silently replaced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VideoEncoderPreference {
    pub backend: EncoderBackend,
    pub adapter: AdapterSelection,
    pub allow_fallback: bool,
}

impl VideoEncoderPreference {
    pub const CPU: Self = Self {
        backend: EncoderBackend::Cpu,
        adapter: AdapterSelection::Auto,
        allow_fallback: false,
    };

    pub const fn gpu(adapter: AdapterSelection, allow_fallback: bool) -> Self {
        Self {
            backend: EncoderBackend::Gpu,
            adapter,
            allow_fallback,
        }
    }
}

/// Identity of a hardware encoder session that is really open.
///
/// The UI shows «GPU» only when this value matches the attempt's
/// [`EncoderPlan`] and encoded frames were delivered. The adapter is named by
/// its LUID, never by an order or a display string.
///
/// [`Self::adapter_evidence`] states how `luid` was obtained. Only
/// [`AdapterEvidence::MftAttribute`] is a fact read from the open MFT;
/// [`AdapterEvidence::PlanEcho`] repeats the requested adapter because the
/// driver exposes no adapter attribute, and must not be described as proof of
/// the session's adapter. `generation` likewise names the snapshot the request
/// was built from (it travels with the request and is echoed by the engine), not
/// a value measured on the transform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HardwareEncoderIdentity {
    pub luid: AdapterLuid,
    pub adapter: String,
    pub driver_version: u64,
    pub codec: Codec,
    /// Generation of the snapshot the attempt was built from.
    pub generation: u64,
    /// How the adapter LUID was obtained.
    pub adapter_evidence: AdapterEvidence,
}

impl HardwareEncoderIdentity {
    /// The open session is the pair the request asked for.
    pub fn matches(&self, plan: &EncoderPlan, codec: Codec) -> bool {
        plan.is_gpu()
            && plan.generation == self.generation
            && plan.adapter.as_ref().map(|adapter| adapter.luid) == Some(self.luid)
            && self.codec == codec
    }
}

/// The concrete encoder of one publication attempt.
///
/// The picker resolves the user's [`VideoEncoderPreference`] against one
/// [`VideoEncoderSnapshot`] and stores the answer here, inside
/// [`crate::publisher::PublishRequest`]. The codec itself stays
/// `PublishRequest::codec`: a request never carries two codecs that could
/// disagree. The generation names the snapshot the answer belongs to, so a late
/// probe of an older dialog cannot silently become the encoder of this attempt.
///
/// The default is the CPU path of the pinned build, which is what every request
/// that does not mention an encoder means.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EncoderPlan {
    pub backend: EncoderBackend,
    /// `Some` exactly for the GPU backend.
    pub adapter: Option<AdapterInfo>,
    /// The SDR profile the probe confirmed on that backend and codec.
    pub profile: Option<SdrProfile>,
    /// Generation of the [`VideoEncoderSnapshot`] this answer came from.
    pub generation: u64,
}

impl EncoderPlan {
    pub const fn cpu() -> Self {
        Self {
            backend: EncoderBackend::Cpu,
            adapter: None,
            profile: None,
            generation: 0,
        }
    }

    pub fn gpu(adapter: AdapterInfo, profile: SdrProfile, generation: u64) -> Self {
        Self {
            backend: EncoderBackend::Gpu,
            adapter: Some(adapter),
            profile: Some(profile),
            generation,
        }
    }

    pub const fn is_gpu(&self) -> bool {
        matches!(self.backend, EncoderBackend::Gpu)
    }

    /// The same backend and the same checked pair.
    ///
    /// The snapshot generation is deliberately **not** part of the live
    /// comparison: every dialog probe owns a fresh [`EncoderProbeService`]
    /// whose generation starts over, so a re-opened dialog would otherwise
    /// report an unchanged GPU request as a restart. The generation still
    /// travels with the attempt and is compared by
    /// [`HardwareEncoderIdentity::matches`]; it identifies the snapshot the
    /// request was built from, not the open MFT session.
    pub fn same_pair(&self, other: &Self) -> bool {
        self.backend == other.backend
            && self.adapter.as_ref().map(AdapterInfo::key)
                == other.adapter.as_ref().map(AdapterInfo::key)
            && self.profile == other.profile
    }

    /// The pair a live preflight revalidates. `None` when the plan is CPU or
    /// lacks the profile/adapter a GPU publication needs.
    pub fn resolved(&self, codec: Codec, query: EncodeQuery) -> Option<ResolvedEncoder> {
        let adapter = self.adapter.clone()?;
        let profile = self.profile?;
        Some(ResolvedEncoder {
            backend: self.backend,
            codec,
            adapter: Some(adapter),
            profile,
            query,
        })
    }

    /// The adapter name for the dialog and diagnostics.
    pub fn adapter_name(&self) -> Option<&str> {
        self.adapter.as_ref().map(|adapter| adapter.name.as_str())
    }
}

/// The codec/resolution/FPS the encoder has to accept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodeQuery {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl Default for EncodeQuery {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 30,
        }
    }
}

impl EncodeQuery {
    pub const fn new(width: u32, height: u32, fps: u32) -> Self {
        Self { width, height, fps }
    }

    pub fn from_quality(quality: crate::publisher::Quality) -> Self {
        Self::from_source(quality, (0, 0))
    }

    /// Use the dimensions capture will produce and preserve the requested
    /// rate so unsupported GPU settings are refused during validation.
    pub fn from_source(quality: crate::publisher::Quality, source: (u32, u32)) -> Self {
        let (width, height) = quality.resolution.capture_size(source.0, source.1);
        Self::new(width, height, quality.fps)
    }

    /// NV12 has no odd chroma dimension, and the MFT contract is bounded.
    pub fn validate(self) -> Result<Self, EncoderRequestError> {
        if self.width == 0 || self.height == 0 {
            return Err(EncoderRequestError::ZeroSize);
        }
        if self.width % 2 != 0 || self.height % 2 != 0 {
            return Err(EncoderRequestError::OddSize);
        }
        if self.width > 4096 || self.height > 4096 {
            return Err(EncoderRequestError::TooLarge);
        }
        if !(1..=120).contains(&self.fps) {
            return Err(EncoderRequestError::Fps);
        }
        Ok(self)
    }

    /// H.264 level byte for the SDR Constrained Baseline profile. The pinned
    /// SFU ignores the level when relaying a compatible payload, but the
    /// dialog and a driver may need a concrete value.
    pub fn h264_level(self) -> u8 {
        let macroblocks = self.width.div_ceil(16) as u64 * self.height.div_ceil(16) as u64;
        let rate = macroblocks * u64::from(self.fps);
        match rate {
            0..=108_000 => 0x1f,       // 3.1
            108_001..=216_000 => 0x20, // 3.2
            216_001..=245_760 => 0x28, // 4.0
            245_761..=522_240 => 0x2a, // 4.2
            522_241..=983_040 => 0x33, // 5.1
            _ => 0x34,                 // 5.2
        }
    }
}

/// The SDR profile the process-wide CPU catalog reports for a codec. The
/// hardware probe never assumes a profile: it reads what the driver produced.
#[cfg(test)]
pub(crate) fn codec_profile(codec: Codec, query: EncodeQuery) -> SdrProfile {
    match codec {
        Codec::H264 => SdrProfile::H264 {
            baseline: H264Baseline::Constrained,
            level: query.h264_level(),
        },
        Codec::Vp8 => SdrProfile::Vp8,
        Codec::Vp9 => SdrProfile::Vp9,
        Codec::Av1 => SdrProfile::Av1,
    }
}

/// Why a confirmed pair is not available. The five categories are the ones
/// the diagnostics of the dialog must distinguish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EncoderReason {
    /// The encoder exists but rejects this profile, resolution, FPS or NV12.
    ProfileUnsupported(String),
    /// Windows/driver does not register an encoder for this output format.
    EncoderNotProvided(String),
    /// The encoder cannot open another session (for example NVENC limits).
    EncoderBusy(String),
    /// The adapter was removed, disabled, reset or is not hardware at all.
    DeviceDisabled(String),
    /// The probe itself failed: enumeration, activation, format, keyframe.
    ProbeFailed(String),
}

impl EncoderReason {
    /// Stable diagnostic token, not a user-facing sentence.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::ProfileUnsupported(_) => "profile_unsupported",
            Self::EncoderNotProvided(_) => "encoder_not_provided",
            Self::EncoderBusy(_) => "encoder_busy",
            Self::DeviceDisabled(_) => "device_disabled",
            Self::ProbeFailed(_) => "probe_failed",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            Self::ProfileUnsupported(detail)
            | Self::EncoderNotProvided(detail)
            | Self::EncoderBusy(detail)
            | Self::DeviceDisabled(detail)
            | Self::ProbeFailed(detail) => detail,
        }
    }
}

impl fmt::Display for EncoderReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProfileUnsupported(detail) => {
                write!(formatter, "Профиль не поддерживается: {detail}")
            }
            Self::EncoderNotProvided(detail) => {
                write!(formatter, "Кодировщик не предоставлен: {detail}")
            }
            Self::EncoderBusy(detail) => write!(formatter, "Кодировщик занят: {detail}"),
            Self::DeviceDisabled(detail) => write!(formatter, "Устройство отключено: {detail}"),
            Self::ProbeFailed(detail) => write!(formatter, "Ошибка проверки: {detail}"),
        }
    }
}

/// One backend entry of one codec in a snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncoderSlot {
    checked: bool,
    profile: Option<SdrProfile>,
    reason: Option<EncoderReason>,
}

impl EncoderSlot {
    pub(crate) fn confirmed(profile: SdrProfile) -> Self {
        Self {
            checked: true,
            profile: Some(profile),
            reason: None,
        }
    }

    pub(crate) fn unavailable(reason: EncoderReason) -> Self {
        Self {
            checked: true,
            profile: None,
            reason: Some(reason),
        }
    }

    /// The backend was not probed for this snapshot (for example the GPU path
    /// while the dialog is in CPU mode).
    pub(crate) const fn unchecked() -> Self {
        Self {
            checked: false,
            profile: None,
            reason: None,
        }
    }

    pub const fn is_checked(&self) -> bool {
        self.checked
    }

    pub const fn is_available(&self) -> bool {
        self.profile.is_some()
    }

    pub const fn profile(&self) -> Option<SdrProfile> {
        self.profile
    }

    pub fn reason(&self) -> Option<&EncoderReason> {
        self.reason.as_ref()
    }
}

/// CPU/GPU availability of one codec. The list is always in
/// [`ENCODER_CODECS`] order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecEncoderState {
    codec: Codec,
    cpu: EncoderSlot,
    gpu: EncoderSlot,
}

impl CodecEncoderState {
    pub(crate) fn new(codec: Codec, cpu: EncoderSlot, gpu: EncoderSlot) -> Self {
        Self { codec, cpu, gpu }
    }

    pub const fn codec(&self) -> Codec {
        self.codec
    }

    pub fn cpu(&self) -> &EncoderSlot {
        &self.cpu
    }

    pub fn gpu(&self) -> &EncoderSlot {
        &self.gpu
    }

    pub fn slot(&self, backend: EncoderBackend) -> &EncoderSlot {
        match backend {
            EncoderBackend::Cpu => &self.cpu,
            EncoderBackend::Gpu => &self.gpu,
        }
    }
}

/// The dialog intent and the picker source a probe belongs to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EncoderProbeTarget {
    /// Dialog intent generation. A new open, source switch or settings change
    /// increments it before the probe request.
    pub intent: u64,
    /// Stable picker identity of the selected source, if any.
    pub source: Option<SourceIdentity>,
}

/// What the dialog should render: "Проверяем кодировщик…" or the result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncoderProbeState {
    Checking,
    Ready,
}

/// One immutable answer of one probe. The UI holds this value and never edits
/// it; a newer request produces another generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoEncoderSnapshot {
    generation: u64,
    state: EncoderProbeState,
    preference: VideoEncoderPreference,
    query: EncodeQuery,
    target: EncoderProbeTarget,
    adapters: Vec<AdapterInfo>,
    adapter: Option<AdapterInfo>,
    codecs: Vec<CodecEncoderState>,
}

impl VideoEncoderSnapshot {
    pub(crate) fn checking(
        generation: u64,
        preference: VideoEncoderPreference,
        query: EncodeQuery,
        target: EncoderProbeTarget,
    ) -> Self {
        let codecs = ENCODER_CODECS
            .into_iter()
            .map(|codec| {
                CodecEncoderState::new(codec, EncoderSlot::unchecked(), EncoderSlot::unchecked())
            })
            .collect();
        Self {
            generation,
            state: EncoderProbeState::Checking,
            preference,
            query,
            target,
            adapters: Vec::new(),
            adapter: None,
            codecs,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn ready(
        generation: u64,
        preference: VideoEncoderPreference,
        query: EncodeQuery,
        target: EncoderProbeTarget,
        inventory: AdapterInventory,
        adapter: Option<AdapterInfo>,
        codecs: Vec<CodecEncoderState>,
    ) -> Self {
        Self {
            generation,
            state: EncoderProbeState::Ready,
            preference,
            query,
            target,
            adapters: inventory.adapters().to_vec(),
            adapter,
            codecs,
        }
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub const fn state(&self) -> EncoderProbeState {
        self.state
    }

    pub const fn is_ready(&self) -> bool {
        matches!(self.state, EncoderProbeState::Ready)
    }

    pub const fn preference(&self) -> VideoEncoderPreference {
        self.preference
    }

    pub const fn query(&self) -> EncodeQuery {
        self.query
    }

    pub fn target(&self) -> &EncoderProbeTarget {
        &self.target
    }

    /// Suitable adapters of this enumeration, in the stable DXGI order.
    pub fn adapters(&self) -> &[AdapterInfo] {
        &self.adapters
    }

    /// The adapter the current preference resolved to, if any.
    pub fn adapter(&self) -> Option<&AdapterInfo> {
        self.adapter.as_ref()
    }

    pub fn codecs(&self) -> &[CodecEncoderState] {
        &self.codecs
    }

    pub fn codec(&self, codec: Codec) -> Option<&CodecEncoderState> {
        self.codecs.iter().find(|entry| entry.codec == codec)
    }

    pub fn cpu(&self, codec: Codec) -> Option<&EncoderSlot> {
        self.codec(codec).map(CodecEncoderState::cpu)
    }

    pub fn gpu(&self, codec: Codec) -> Option<&EncoderSlot> {
        self.codec(codec).map(CodecEncoderState::gpu)
    }

    /// The GPU backend can encode at least one SDR codec on this machine.
    pub fn gpu_available(&self) -> bool {
        self.adapter.is_some()
            && self
                .codecs
                .iter()
                .any(|entry| entry.gpu.is_checked() && entry.gpu.is_available())
    }

    /// The selected adapter exists, even if no codec was confirmed.
    pub fn has_hardware_adapter(&self) -> bool {
        self.adapter.is_some()
    }

    /// Resolve the picker intent against this snapshot. Auto is the first
    /// available codec of the current device. An explicit unavailable codec is
    /// refused with its reason and is never replaced. A fallback preference
    /// may replace the whole unusable GPU backend with CPU while keeping the
    /// codec; a working GPU with a missing codec is never bypassed.
    pub fn resolve(&self, intent: CodecIntent) -> Result<ResolvedEncoder, EncoderRejection> {
        if self.state != EncoderProbeState::Ready {
            return Err(EncoderRejection::Checking {
                generation: self.generation,
                backend: self.preference.backend,
            });
        }
        let backend = self.preference.backend;
        match backend {
            EncoderBackend::Cpu => self.resolve_side(intent, EncoderBackend::Cpu),
            EncoderBackend::Gpu if self.gpu_available() => {
                self.resolve_side(intent, EncoderBackend::Gpu)
            }
            EncoderBackend::Gpu if self.preference.allow_fallback => {
                self.resolve_side(intent, EncoderBackend::Cpu)
            }
            EncoderBackend::Gpu => Err(self.gpu_rejection(intent)),
        }
    }

    fn resolve_side(
        &self,
        intent: CodecIntent,
        backend: EncoderBackend,
    ) -> Result<ResolvedEncoder, EncoderRejection> {
        match intent {
            CodecIntent::Auto => {
                for entry in &self.codecs {
                    let slot = entry.slot(backend);
                    if let Some(profile) = slot.profile() {
                        return Ok(self.resolved(entry.codec, backend, profile));
                    }
                }
                Err(EncoderRejection::NoCodec {
                    backend,
                    reason: self.first_reason(backend),
                })
            }
            CodecIntent::Explicit(codec) => {
                let Some(entry) = self.codec(codec) else {
                    return Err(EncoderRejection::NoCodec {
                        backend,
                        reason: None,
                    });
                };
                let slot = entry.slot(backend);
                match (slot.profile(), slot.reason()) {
                    (Some(profile), _) => Ok(self.resolved(codec, backend, profile)),
                    (None, Some(reason)) => Err(EncoderRejection::Unavailable {
                        codec: Some(codec),
                        backend,
                        reason: reason.clone(),
                    }),
                    (None, None) => Err(EncoderRejection::Checking {
                        generation: self.generation,
                        backend,
                    }),
                }
            }
        }
    }

    fn resolved(
        &self,
        codec: Codec,
        backend: EncoderBackend,
        profile: SdrProfile,
    ) -> ResolvedEncoder {
        ResolvedEncoder {
            backend,
            codec,
            adapter: match backend {
                EncoderBackend::Gpu => self.adapter.clone(),
                EncoderBackend::Cpu => None,
            },
            profile,
            query: self.query,
        }
    }

    fn gpu_rejection(&self, intent: CodecIntent) -> EncoderRejection {
        if let CodecIntent::Explicit(codec) = intent
            && let Some(entry) = self.codec(codec)
            && entry.gpu.is_checked()
            && let Some(reason) = entry.gpu.reason()
        {
            return EncoderRejection::Unavailable {
                codec: Some(codec),
                backend: EncoderBackend::Gpu,
                reason: reason.clone(),
            };
        }
        EncoderRejection::Unavailable {
            codec: match intent {
                CodecIntent::Explicit(codec) => Some(codec),
                CodecIntent::Auto => None,
            },
            backend: EncoderBackend::Gpu,
            reason: self.first_reason(EncoderBackend::Gpu).unwrap_or_else(|| {
                EncoderReason::DeviceDisabled(
                    "аппаратное кодирование недоступно на этом устройстве".into(),
                )
            }),
        }
    }

    fn first_reason(&self, backend: EncoderBackend) -> Option<EncoderReason> {
        self.codecs
            .iter()
            .find_map(|entry| entry.slot(backend).reason().cloned())
    }
}

/// A confirmed pair the media worker may preflight before `stream.start`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedEncoder {
    pub backend: EncoderBackend,
    pub codec: Codec,
    /// `Some` exactly for the GPU backend.
    pub adapter: Option<AdapterInfo>,
    pub profile: SdrProfile,
    pub query: EncodeQuery,
}

impl ResolvedEncoder {
    pub fn adapter_luid(&self) -> Option<AdapterLuid> {
        self.adapter.as_ref().map(|adapter| adapter.luid)
    }
}

/// Why an intent cannot be started. The message is ready for the dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EncoderRejection {
    /// No confirmed answer for this request yet.
    Checking {
        generation: u64,
        backend: EncoderBackend,
    },
    /// A concrete pair was checked and is not available.
    Unavailable {
        codec: Option<Codec>,
        backend: EncoderBackend,
        reason: EncoderReason,
    },
    /// The whole backend has no available SDR codec.
    NoCodec {
        backend: EncoderBackend,
        reason: Option<EncoderReason>,
    },
}

impl EncoderRejection {
    pub const fn codec(&self) -> Option<Codec> {
        match self {
            Self::Checking { .. } | Self::NoCodec { .. } => None,
            Self::Unavailable { codec, .. } => *codec,
        }
    }

    pub const fn backend(&self) -> EncoderBackend {
        match self {
            Self::Checking { backend, .. } => *backend,
            Self::Unavailable { backend, .. } | Self::NoCodec { backend, .. } => *backend,
        }
    }

    pub fn reason(&self) -> Option<&EncoderReason> {
        match self {
            Self::Checking { .. } => None,
            Self::Unavailable { reason, .. } => Some(reason),
            Self::NoCodec { reason, .. } => reason.as_ref(),
        }
    }

    pub const fn is_checking(&self) -> bool {
        matches!(self, Self::Checking { .. })
    }
}

impl fmt::Display for EncoderRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Checking { .. } => formatter.write_str("Проверяем кодировщик…"),
            Self::Unavailable { reason, .. } => write!(formatter, "{reason}"),
            Self::NoCodec {
                backend: EncoderBackend::Cpu,
                ..
            } => formatter.write_str("В этой сборке нет доступного SDR-кодека для публикации."),
            Self::NoCodec { reason, .. } => match reason {
                Some(reason) => write!(formatter, "Аппаратное кодирование недоступно: {reason}"),
                None => formatter.write_str("Аппаратное кодирование недоступно."),
            },
        }
    }
}

impl std::error::Error for EncoderRejection {}

/// CPU availability of the pinned WebRTC build, in [`Codec::ALL`] order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CpuEncoders {
    entries: Vec<(Codec, EncoderSlot)>,
}

impl CpuEncoders {
    /// Reuse the codec model of the software slice. The sender side answers an
    /// encoder question, and the reason is kept for the unavailable item.
    pub fn from_codec_snapshot(snapshot: &CodecSnapshot) -> Self {
        let entries = Codec::ALL
            .into_iter()
            .map(|codec| {
                let availability = snapshot.availability(CodecRole::Sender, codec);
                let slot = match availability.profile() {
                    Some(profile) => EncoderSlot::confirmed(profile),
                    None => EncoderSlot::unavailable(EncoderReason::EncoderNotProvided(
                        availability.reason().unwrap_or("сборка WebRTC").to_owned(),
                    )),
                };
                (codec, slot)
            })
            .collect();
        Self { entries }
    }

    #[cfg(test)]
    pub(crate) fn all_available() -> Self {
        let entries = Codec::ALL
            .into_iter()
            .map(|codec| {
                (
                    codec,
                    EncoderSlot::confirmed(codec_profile(codec, EncodeQuery::default())),
                )
            })
            .collect();
        Self { entries }
    }

    #[cfg(test)]
    pub(crate) fn remove(&mut self, codec: Codec) {
        self.entries.retain(|(entry, _)| *entry != codec);
    }

    pub fn slot(&self, codec: Codec) -> EncoderSlot {
        self.entries
            .iter()
            .find(|(entry, _)| *entry == codec)
            .map(|(_, slot)| slot.clone())
            .unwrap_or_else(|| {
                EncoderSlot::unavailable(EncoderReason::EncoderNotProvided(
                    "сборка WebRTC не содержит этого кодировщика".into(),
                ))
            })
    }
}

/// Supplies the CPU catalog on the probe worker, away from the UI thread.
pub trait CpuEncoderSource: Send + Sync {
    fn catalog(&self) -> CpuEncoders;
}

/// The process-wide snapshot of the pinned WebRTC build.
pub struct WebRtcCpuEncoders;

impl CpuEncoderSource for WebRtcCpuEncoders {
    fn catalog(&self) -> CpuEncoders {
        CpuEncoders::from_codec_snapshot(CodecSnapshot::current())
    }
}
