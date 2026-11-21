//! The four SDR video codecs of the web client, the picker intent and the
//! immutable capability snapshot behind it.
//!
//! The wire order is the website's «Автоматически» order: H.264, VP8, VP9, AV1
//! (`AUTO_CODEC_ORDER` in `src/services/stream.ts`). Publishing resolves a codec
//! against the sender side (encoders and sender profiles of this build), while
//! watching resolves it against the receiver side. A snapshot carries a
//! generation and one availability entry with a reason per codec, so the UI can
//! show an unavailable item without changing the snapshot itself.
//!
//! FMTP parsing follows the SFU relay contract (`relayCodecCompatible` in the
//! pinned backend `internal/stream/session.go`): parameter order, case and
//! spaces do not matter. Only the SDR profile is compared, never the receive
//! limits (`max-fs`, `max-fr`) and never the H.264 level byte.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use libwebrtc::{
    MediaType, peer_connection_factory::PeerConnectionFactory, rtp_sender::VideoEncoderBackend,
    video_codec::VideoCodec,
};

/// One video codec the web client publishes or watches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    H264,
    Vp8,
    Vp9,
    Av1,
}

impl Codec {
    /// The website's automatic resolution order. It is also the stable order of
    /// every capability list and of the picker.
    pub const ALL: [Codec; 4] = [Self::H264, Self::Vp8, Self::Vp9, Self::Av1];

    pub const fn wire(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::Vp8 => "vp8",
            Self::Vp9 => "vp9",
            Self::Av1 => "av1",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::Vp8 => "VP8",
            Self::Vp9 => "VP9",
            Self::Av1 => "AV1",
        }
    }

    /// Parse a wire value or an SDP MIME subtype. The wire contract is lowercase
    /// (`stream.start.data.codec`), the SDP uses `video/H264`; both are accepted
    /// case-insensitively.
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim().to_ascii_lowercase();
        match value.strip_prefix("video/").unwrap_or(&value) {
            "h264" | "h.264" => Some(Self::H264),
            "vp8" => Some(Self::Vp8),
            "vp9" => Some(Self::Vp9),
            "av1" => Some(Self::Av1),
            _ => None,
        }
    }
}

/// What the user asked for. The media request carries the resolved [`Codec`],
/// never the intent, so the engine and the wire always see one concrete value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CodecIntent {
    /// Resolve the first available codec in [`Codec::ALL`] order.
    #[default]
    Auto,
    Explicit(Codec),
}

impl CodecIntent {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "Автоматически",
            Self::Explicit(codec) => codec.label(),
        }
    }
}

/// Which side of the SFU the capability question belongs to: a publisher
/// encodes, a viewer decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecRole {
    Sender,
    Receiver,
}

/// One SDP video codec entry produced by the pinned WebRTC build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoCodecCapability {
    pub codec: Codec,
    pub clock_rate: u32,
    pub fmtp: Option<String>,
}

impl VideoCodecCapability {
    pub fn new(codec: Codec, clock_rate: u32, fmtp: Option<&str>) -> Self {
        Self {
            codec,
            clock_rate,
            fmtp: fmtp.map(str::to_owned),
        }
    }
}

/// The raw question asked of the WebRTC build. Kept explicit so a fake matrix
/// can replace the real probe without touching the resolution rules.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CodecProbe {
    /// Codecs advertised by `video_encoder_codecs(Software)`.
    pub encoders: Vec<Codec>,
    /// `get_rtp_sender_capabilities(Video)`.
    pub sender: Vec<VideoCodecCapability>,
    /// `get_rtp_receiver_capabilities(Video)`.
    pub receiver: Vec<VideoCodecCapability>,
}

impl CodecProbe {
    /// Probe the pinned build. The encoder question uses the software backend
    /// on purpose: the picker of this stage publishes CPU frames only.
    pub(crate) fn probe() -> Self {
        let factory = PeerConnectionFactory::default();
        let encoders = factory
            .video_encoder_codecs(VideoEncoderBackend::Software)
            .into_iter()
            .filter_map(codec_from_webrtc)
            .collect();
        Self {
            encoders,
            sender: capabilities(factory.get_rtp_sender_capabilities(MediaType::Video)),
            receiver: capabilities(factory.get_rtp_receiver_capabilities(MediaType::Video)),
        }
    }
}

fn capabilities(value: libwebrtc::rtp_parameters::RtpCapabilities) -> Vec<VideoCodecCapability> {
    value
        .codecs
        .into_iter()
        .filter_map(|capability| {
            let codec = Codec::parse(&capability.mime_type)?;
            Some(VideoCodecCapability {
                codec,
                clock_rate: capability
                    .clock_rate
                    .and_then(|rate| u32::try_from(rate).ok())
                    .unwrap_or(0),
                fmtp: capability.sdp_fmtp_line,
            })
        })
        .collect()
}

fn codec_from_webrtc(codec: VideoCodec) -> Option<Codec> {
    match codec {
        VideoCodec::VP8 => Some(Codec::Vp8),
        VideoCodec::VP9 => Some(Codec::Vp9),
        VideoCodec::AV1 => Some(Codec::Av1),
        VideoCodec::H264 => Some(Codec::H264),
        _ => None,
    }
}

/// One codec in a capability snapshot. An unavailable entry keeps the reason
/// the UI shows next to the greyed-out item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecAvailability {
    codec: Codec,
    profile: Option<SdrProfile>,
    reason: Option<String>,
}

impl CodecAvailability {
    fn supported(codec: Codec, profile: SdrProfile) -> Self {
        Self {
            codec,
            profile: Some(profile),
            reason: None,
        }
    }

    fn unavailable(codec: Codec, reason: String) -> Self {
        Self {
            codec,
            profile: None,
            reason: Some(reason),
        }
    }

    /// The checked SDR profile of an available codec. `None` when unavailable.
    pub const fn profile(&self) -> Option<SdrProfile> {
        self.profile
    }

    /// Why this codec is unavailable on this side. `None` when available.
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    pub const fn available(&self) -> bool {
        self.reason.is_none()
    }
}

/// A refused intent. The message is ready for the dialog's error line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecRejection {
    codec: Option<Codec>,
    reason: String,
}

impl CodecRejection {
    pub const fn codec(&self) -> Option<Codec> {
        self.codec
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl fmt::Display for CodecRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.reason)
    }
}

impl std::error::Error for CodecRejection {}

/// An immutable answer of one probe. The UI holds this value and never edits
/// it; a new probe yields a new generation instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecSnapshot {
    generation: u64,
    sender: Vec<CodecAvailability>,
    receiver: Vec<CodecAvailability>,
}

impl CodecSnapshot {
    /// Probe the pinned build with the next generation.
    pub fn probe() -> Self {
        Self::from_probe(next_generation(), &CodecProbe::probe())
    }

    /// The process-wide snapshot. Created on first use, then immutable.
    pub fn current() -> &'static Self {
        static CURRENT: OnceLock<CodecSnapshot> = OnceLock::new();
        CURRENT.get_or_init(Self::probe)
    }

    /// Build a snapshot from a probe. Every codec is described in
    /// [`Codec::ALL`] order on both sides.
    pub(crate) fn from_probe(generation: u64, probe: &CodecProbe) -> Self {
        let mut sender = Vec::with_capacity(Codec::ALL.len());
        let mut receiver = Vec::with_capacity(Codec::ALL.len());
        for codec in Codec::ALL {
            sender.push(sender_availability(probe, codec));
            receiver.push(receiver_availability(probe, codec));
        }
        Self {
            generation,
            sender,
            receiver,
        }
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub fn sender(&self) -> &[CodecAvailability] {
        &self.sender
    }

    pub fn receiver(&self) -> &[CodecAvailability] {
        &self.receiver
    }

    pub fn availability(&self, role: CodecRole, codec: Codec) -> &CodecAvailability {
        let entries = match role {
            CodecRole::Sender => &self.sender,
            CodecRole::Receiver => &self.receiver,
        };
        entries
            .iter()
            .find(|entry| entry.codec == codec)
            .expect("every codec is described on both sides")
    }

    pub fn supports(&self, role: CodecRole, codec: Codec) -> bool {
        self.availability(role, codec).available()
    }

    /// The first available codec in the website's automatic order.
    pub fn auto(&self, role: CodecRole) -> Option<Codec> {
        let entries = match role {
            CodecRole::Sender => &self.sender,
            CodecRole::Receiver => &self.receiver,
        };
        entries
            .iter()
            .find(|entry| entry.available())
            .map(|entry| entry.codec)
    }

    pub fn sender_available(&self) -> bool {
        self.auto(CodecRole::Sender).is_some()
    }

    /// Resolve the picker intent on the requested side. Auto picks the first
    /// available codec; an explicit unavailable codec is refused with the
    /// snapshot's reason and never silently replaced.
    pub fn resolve(&self, intent: CodecIntent, role: CodecRole) -> Result<Codec, CodecRejection> {
        match intent {
            CodecIntent::Auto => self.auto(role).ok_or_else(|| CodecRejection {
                codec: None,
                reason: match role {
                    CodecRole::Sender => {
                        "В этой сборке нет доступного SDR-кодека для публикации.".to_owned()
                    }
                    CodecRole::Receiver => {
                        "В этой сборке нет декодера ни одного SDR-кодека трансляции.".to_owned()
                    }
                },
            }),
            CodecIntent::Explicit(codec) => {
                let availability = self.availability(role, codec);
                match availability.reason() {
                    None => Ok(codec),
                    Some(reason) => Err(CodecRejection {
                        codec: Some(codec),
                        reason: reason.to_owned(),
                    }),
                }
            }
        }
    }
}

fn sender_availability(probe: &CodecProbe, codec: Codec) -> CodecAvailability {
    if !probe.encoders.contains(&codec) {
        return CodecAvailability::unavailable(
            codec,
            format!(
                "Кодировщик {} отсутствует в этой сборке WebRTC.",
                codec.label()
            ),
        );
    }
    match best_profile(&probe.sender, codec) {
        Ok(profile) => CodecAvailability::supported(codec, profile),
        Err(rejection) => CodecAvailability::unavailable(
            codec,
            format!(
                "Профиль {} не объявлен кодировщиком для SDR: {rejection}.",
                codec.label()
            ),
        ),
    }
}

fn receiver_availability(probe: &CodecProbe, codec: Codec) -> CodecAvailability {
    match best_profile(&probe.receiver, codec) {
        Ok(profile) => CodecAvailability::supported(codec, profile),
        Err(rejection) => CodecAvailability::unavailable(
            codec,
            format!(
                "Декодер {} не поддерживает профиль SDR: {rejection}.",
                codec.label()
            ),
        ),
    }
}

/// Pick the preferred SDR profile of one codec among the build's capability
/// entries. Constrained Baseline wins over the legacy H.264 profile; every
/// other codec has exactly one SDR profile.
fn best_profile(
    capabilities: &[VideoCodecCapability],
    codec: Codec,
) -> Result<SdrProfile, ProfileRejection> {
    let mut best: Option<SdrProfile> = None;
    let mut last_rejection: Option<ProfileRejection> = None;
    let mut declared = false;
    for capability in capabilities.iter().filter(|entry| entry.codec == codec) {
        declared = true;
        if capability.clock_rate != 90_000 {
            last_rejection.get_or_insert(ProfileRejection::ClockRate(capability.clock_rate));
            continue;
        }
        match sdr_profile(codec, capability.fmtp.as_deref()) {
            Ok(profile) => {
                if best.is_none_or(|current| profile.priority() < current.priority()) {
                    best = Some(profile);
                }
            }
            Err(rejection) => {
                last_rejection.get_or_insert(rejection);
            }
        }
    }
    match (best, declared) {
        (Some(profile), _) => Ok(profile),
        (None, true) => Err(last_rejection.unwrap_or(ProfileRejection::Missing)),
        (None, false) => Err(ProfileRejection::Missing),
    }
}

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_generation() -> u64 {
    NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
}

/// The SDR profile found in an FMTP line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SdrProfile {
    H264 {
        baseline: H264Baseline,
        /// The level byte of `profile-level-id`. It may differ on the other
        /// side of a compatible SDP and is never compared.
        level: u8,
    },
    Vp8,
    /// Only `profile-id=0` is SDR; a missing parameter means 0.
    Vp9,
    /// Only Main `profile=0` is SDR; a missing parameter means 0. The AV1
    /// `level-idx` is negotiated by the SFU independently, so it is ignored.
    Av1,
}

impl SdrProfile {
    /// Lower value wins when a build advertises more than one SDR variant.
    const fn priority(self) -> u8 {
        match self {
            Self::H264 {
                baseline: H264Baseline::Constrained,
                ..
            } => 0,
            Self::H264 {
                baseline: H264Baseline::Legacy,
                ..
            } => 1,
            _ => 0,
        }
    }

    /// Two SDP profile descriptions describe the same SDR payload. Levels and
    /// receiver limits are deliberately not compared.
    pub const fn compatible_with(self, other: Self) -> bool {
        match (self, other) {
            (Self::H264 { baseline: a, .. }, Self::H264 { baseline: b, .. }) => {
                matches!(
                    (a, b),
                    (H264Baseline::Constrained, H264Baseline::Constrained)
                ) || matches!((a, b), (H264Baseline::Legacy, H264Baseline::Legacy))
            }
            (Self::Vp8, Self::Vp8) | (Self::Vp9, Self::Vp9) | (Self::Av1, Self::Av1) => true,
            _ => false,
        }
    }
}

/// The two H.264 SDR baselines the SFU registers separately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum H264Baseline {
    /// `42e0xx`: preferred Constrained Baseline.
    Constrained,
    /// `4200xx`: legacy Baseline-compatible clients. A separate payload, never
    /// interchangeable with `42e0xx`.
    Legacy,
}

/// Why an FMTP line does not describe an SDR payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileRejection {
    /// The capability list has no entry for the codec at all.
    Missing,
    /// RTP video is `90000 Hz`; a different clock is another payload.
    ClockRate(u32),
    H264Packetization(String),
    H264Profile(String),
    H264Malformed(String),
    Vp9Profile(String),
    Av1Profile(String),
}

impl fmt::Display for ProfileRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(formatter, "кодек не объявлен"),
            Self::ClockRate(rate) => {
                write!(formatter, "частота {rate} Гц вместо 90000 Гц")
            }
            Self::H264Packetization(mode) => {
                write!(formatter, "packetization-mode={mode}, для SDR нужен 1")
            }
            Self::H264Profile(profile) => {
                write!(formatter, "profile-level-id={profile} не SDR Baseline")
            }
            Self::H264Malformed(profile) => {
                write!(formatter, "повреждённый profile-level-id={profile}")
            }
            Self::Vp9Profile(profile) => {
                write!(formatter, "profile-id={profile}, для SDR нужен 0")
            }
            Self::Av1Profile(profile) => {
                write!(formatter, "profile={profile}, для SDR нужен Main (0)")
            }
        }
    }
}

impl std::error::Error for ProfileRejection {}

/// Tolerant FMTP parser: keys and values are case-insensitive, whitespace
/// around separators is ignored and parameter order does not matter.
///
/// A repeated key keeps the last value. The pinned backend reads the first
/// occurrence; browsers emit every parameter once in a normal SDP, so the two
/// readings only differ on an input outside that contract. The fixture in
/// `tests.rs` pins the choice.
pub fn parse_fmtp(fmtp: Option<&str>) -> BTreeMap<String, String> {
    let mut parameters = BTreeMap::new();
    let Some(fmtp) = fmtp else {
        return parameters;
    };
    for part in fmtp.split(';') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        if key.is_empty() {
            continue;
        }
        parameters.insert(key, value.trim().to_ascii_lowercase());
    }
    parameters
}

fn parameter<'a>(parameters: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    parameters.get(name).map(String::as_str)
}

/// Parse one codec's FMTP into its SDR profile. The defaults match the pinned
/// backend: a missing VP9 `profile-id`/AV1 `profile` means 0, a missing H.264
/// `profile-level-id` means `42e01f` and a missing packetization mode means 0.
pub fn sdr_profile(codec: Codec, fmtp: Option<&str>) -> Result<SdrProfile, ProfileRejection> {
    let parameters = parse_fmtp(fmtp);
    match codec {
        Codec::H264 => {
            let mode = parameter(&parameters, "packetization-mode").unwrap_or("0");
            if mode != "1" {
                return Err(ProfileRejection::H264Packetization(mode.to_owned()));
            }
            let id = parameter(&parameters, "profile-level-id").unwrap_or("42e01f");
            if id.len() != 6 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(ProfileRejection::H264Malformed(id.to_owned()));
            }
            let baseline = match &id[..4] {
                "42e0" => H264Baseline::Constrained,
                "4200" => H264Baseline::Legacy,
                _ => return Err(ProfileRejection::H264Profile(id.to_owned())),
            };
            let level = u8::from_str_radix(&id[4..], 16)
                .map_err(|_| ProfileRejection::H264Malformed(id.to_owned()))?;
            Ok(SdrProfile::H264 { baseline, level })
        }
        Codec::Vp8 => Ok(SdrProfile::Vp8),
        Codec::Vp9 => match parameter(&parameters, "profile-id").unwrap_or("0") {
            "0" => Ok(SdrProfile::Vp9),
            profile => Err(ProfileRejection::Vp9Profile(profile.to_owned())),
        },
        Codec::Av1 => match parameter(&parameters, "profile").unwrap_or("0") {
            "0" => Ok(SdrProfile::Av1),
            profile => Err(ProfileRejection::Av1Profile(profile.to_owned())),
        },
    }
}

/// Whether two FMTP lines describe the same SDR payload of this codec. This is
/// the local equivalent of the SFU relay contract, so a legacy H.264 payload or
/// a negotiated receive limit can never turn into another profile.
pub fn sdr_compatible(codec: Codec, first: Option<&str>, second: Option<&str>) -> bool {
    match (sdr_profile(codec, first), sdr_profile(codec, second)) {
        (Ok(first), Ok(second)) => first.compatible_with(second),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
