//! Codec negotiation of one screen publication.
//!
//! The SFU creates the offer and already restricts its video m-line to the
//! rendition the website asked for. The engine repeats that choice locally with
//! [`RtpTransceiver::set_codec_preferences`], verifies the delivered SDP and
//! only then answers: the requested codec, its SDR profile, the offered mid and
//! the RTX pair (`apt`) must survive into the answer. A later answer of a
//! re-offer (ICE restart) must keep the same mid, codec and payload pair.
//!
//! Nothing here hard-codes a payload type, an SSRC or an SDP line: payload
//! types are read from the offer and the RTX pairing comes from its `apt`.
//!
//! Connectivity is proved by ICE and the answer; the transmission is proved
//! later by the first encoded frames and their `RtcStats::Codec`
//! ([`confirm_codec`]). A missing FMTP falls back to the verified SDP profile
//! and its defaults (VP9/AV1 profile 0), as the pinned backend does.

use libwebrtc::{
    media_stream_track::MediaStreamTrack,
    peer_connection::{AnswerOptions, PeerConnection},
    rtp_parameters::RtpCodecCapability,
    rtp_sender::RtpSender,
    rtp_transceiver::RtpTransceiver,
    session_description::{SdpType, SessionDescription},
};

use crate::publisher::{
    Codec, CodecRole, CodecSnapshot, Quality, SdrProfile, bitrate::Limits,
    bitrate::apply_sender_limits, bitrate::set_video_frame_rate, parse_fmtp, sdr_compatible,
    sdr_profile,
};

use super::AppliedLimits;

/// The video payload the publisher will send and the RTX payload paired with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SelectedVideo {
    pub mid: String,
    pub codec: Codec,
    pub profile: SdrProfile,
    pub primary_pt: u8,
    pub fmtp: Option<String>,
    pub rtx_pt: Option<u8>,
    pub rtx_apt: Option<u8>,
    pub audio_mid: Option<String>,
}

/// What an answer really encoded, read back from the outbound statistics.
///
/// The SDP contract (mid, payload pair, profile) is verified before the answer
/// is applied, and the profile again in [`reported_profile`]; only the codec
/// itself travels on, because that is what the state machine and the UI show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ConfirmedCodec {
    pub codec: Codec,
}

/// The context of one offer: the requested codec, its checked SDR profile and
/// the negotiation that is already on air.
pub(super) struct OfferContext<'a> {
    pub codec: Codec,
    pub profile: SdrProfile,
    pub audio_active: bool,
    pub previous: Option<&'a SelectedVideo>,
    pub quality: Quality,
}

/// The SDR profile this build checked for publishing `codec`. `Err` carries the
/// capability reason the dialog already shows.
pub(super) fn sender_profile(codec: Codec) -> Result<SdrProfile, String> {
    let snapshot = CodecSnapshot::current();
    let availability = snapshot.availability(CodecRole::Sender, codec);
    availability.profile().ok_or_else(|| {
        availability
            .reason()
            .unwrap_or("Кодек трансляции недоступен в этой сборке.")
            .to_owned()
    })
}

/// Local codec preferences that pin the requested SDR payload. The RTX entry has
/// no `apt`: the answer derives the real pairing from the offer, so no payload
/// type is assumed here.
pub(super) fn preferences(
    capabilities: &[RtpCodecCapability],
    codec: Codec,
    profile: SdrProfile,
) -> Vec<RtpCodecCapability> {
    let mut preferences: Vec<RtpCodecCapability> = capabilities
        .iter()
        .filter(|capability| {
            Codec::parse(&capability.mime_type) == Some(codec)
                && sdr_profile(codec, capability.sdp_fmtp_line.as_deref())
                    .is_ok_and(|declared| declared.compatible_with(profile))
        })
        .cloned()
        .collect();
    preferences.push(RtpCodecCapability {
        channels: None,
        clock_rate: Some(90_000),
        mime_type: "video/rtx".into(),
        sdp_fmtp_line: None,
    });
    preferences
}

/// Pin the requested codec on the video transceiver that already carries the
/// send track, before any remote description.
pub(super) fn pin_video_codec(
    peer: &PeerConnection,
    capabilities: &[RtpCodecCapability],
    codec: Codec,
    profile: SdrProfile,
) -> Result<(), String> {
    let preferences = preferences(capabilities, codec, profile);
    apply_preferences(peer, preferences, codec)
}

fn apply_preferences(
    peer: &PeerConnection,
    preferences: Vec<RtpCodecCapability>,
    codec: Codec,
) -> Result<(), String> {
    if preferences.len() <= 1 {
        return Err(format!(
            "В этой сборке нет профиля {} для публикации.",
            codec.label()
        ));
    }
    let mut pinned = false;
    for transceiver in peer.transceivers() {
        if !sends_video(&transceiver) {
            continue;
        }
        transceiver
            .set_codec_preferences(preferences.clone())
            .map_err(|_| {
                format!(
                    "Не удалось закрепить кодек {} для трансляции.",
                    codec.label()
                )
            })?;
        pinned = true;
    }
    if !pinned {
        return Err("Видеодорожка трансляции не создана.".into());
    }
    Ok(())
}

fn sends_video(transceiver: &RtpTransceiver) -> bool {
    matches!(
        transceiver.sender().track(),
        Some(MediaStreamTrack::Video(_))
    )
}

/// The codec WebRTC will really encode with.
///
/// `GetParameters` puts the active send codec first. An offer that names
/// another codec first makes the send channel choose it, no matter what the
/// pinned answer says. The caller requires the requested codec, so a mismatch
/// is a terminal contract violation instead of a stream with the wrong codec.
fn verify_send_codec(sender: &RtpSender, codec: Codec) -> Result<(), String> {
    let parameters = sender.parameters();
    let Some(active) = parameters.codecs.first() else {
        return Err("Сервер не согласовал ни одного кодека трансляции.".into());
    };
    if Codec::parse(&active.mime_type) == Some(codec) {
        return Ok(());
    }
    Err(format!(
        "Сервер предложил первым {}, поэтому в эфир пошёл бы он вместо {}. Предложение должно ограничивать кодеки выбранным для трансляции.",
        Codec::parse(&active.mime_type)
            .map(Codec::label)
            .unwrap_or(active.mime_type.as_str()),
        codec.label()
    ))
}

/// Apply one server offer, verify the delivered answer and return both.
///
/// Every error is terminal for the attempt: the caller reports it once and
/// releases the peer instead of answering a contract violation that the SFU
/// would reject a moment later anyway.
pub(super) async fn apply_offer(
    peer: &PeerConnection,
    sender: &RtpSender,
    audio_sender: Option<&RtpSender>,
    sdp: &str,
    context: &OfferContext<'_>,
    limits: Option<&AppliedLimits>,
    applied: &mut Limits,
) -> Result<(String, SelectedVideo), String> {
    let selected = select_offer(sdp, context)?;
    let offer = SessionDescription::parse(sdp, SdpType::Offer)
        .map_err(|_| "Сервер прислал некорректное предложение трансляции.".to_owned())?;
    if peer.set_remote_description(offer).await.is_err() {
        return Err("Не удалось применить предложение трансляции.".into());
    }
    let answer = peer
        .create_answer(AnswerOptions::default())
        .await
        .map_err(|_| "Не удалось создать ответ трансляции.".to_owned())?;
    let text = answer.to_string();
    verify_answer(&text, &selected, context.audio_active)?;
    if peer.set_local_description(answer).await.is_err() {
        return Err("Не удалось применить ответ трансляции.".into());
    }
    // The answer names the codec the publisher wants, but WebRTC's send
    // channel picks the active send codec from the offer order. A backend
    // that offers several codecs can therefore run a different one than the
    // answer and the request. The pre-encoded pass-through then locks onto
    // that wrong codec and rejects every access unit as a mismatch, so the
    // attempt would only time out with "no encoded frame". Refuse it here.
    verify_send_codec(sender, context.codec)?;
    // The answer carries the frame rate the backend really applies; a refused
    // set is terminal, exactly like a refused answer.
    if !set_video_frame_rate(sender, context.quality.fps) {
        return Err("Не удалось применить частоту кадров трансляции.".into());
    }
    apply_confirmed_limits(peer, sender, audio_sender, context.quality, limits, applied);
    Ok((text, selected))
}

/// The initial answer applies the same limits, but its result is not a live
/// operation: a refused readback leaves the confirmed limits unknown instead
/// of failing the publication.
fn apply_confirmed_limits(
    peer: &PeerConnection,
    sender: &RtpSender,
    audio_sender: Option<&RtpSender>,
    quality: Quality,
    limits: Option<&AppliedLimits>,
    applied: &mut Limits,
) {
    if let Ok(confirmed) = apply_sender_limits(peer, sender, audio_sender, quality, *applied) {
        *applied = confirmed;
        if let Some(limits) = limits {
            limits.store(&confirmed);
        }
    }
}

/// Read the offered video/audio contract and pick the SDR payload.
///
/// The SFU may offer more than one payload of the codec (H.264 Constrained
/// Baseline plus legacy, VP9 SDR plus HDR); only a payload whose FMTP matches
/// the locally checked SDR profile is accepted, in offered order.
pub(super) fn select_offer(sdp: &str, context: &OfferContext<'_>) -> Result<SelectedVideo, String> {
    let sections = media_sections(sdp);
    let all_video: Vec<&Media> = sections
        .iter()
        .filter(|section| section.kind == Kind::Video)
        .collect();
    // A rejected m-line (port 0) is not a second video track: the SFU keeps
    // the old section with `m=video 0` when it disables one and adds the real
    // one next to it. Counting it would turn a normal offer into "several
    // video tracks".
    let video_sections: Vec<&Media> = all_video
        .iter()
        .copied()
        .filter(|section| !section.rejected)
        .collect();
    let video = match video_sections.as_slice() {
        [] if all_video.is_empty() => {
            return Err("Сервер не предложил видеодорожку трансляции.".into());
        }
        [] => return Err("Сервер выключил видеодорожку трансляции.".into()),
        [video] => *video,
        _ => return Err("Сервер предложил несколько видеодорожек трансляции.".into()),
    };
    if !video.direction.accepts_send() {
        return Err("Сервер выключил видеодорожку трансляции.".into());
    }
    if video.mid.is_empty() {
        return Err("Предложение трансляции не содержит mid видеодорожки.".into());
    }
    let mut declared_problem = None;
    let mut selected = None;
    for payload in video.payloads.iter() {
        if payload.clock != 90_000 || Codec::parse(&payload.encoding) != Some(context.codec) {
            continue;
        }
        match sdr_profile(context.codec, payload.fmtp.as_deref()) {
            Ok(profile) if profile.compatible_with(context.profile) => {
                selected = Some((payload, profile));
                break;
            }
            Ok(_) => {
                declared_problem.get_or_insert_with(|| {
                    format!(
                        "профиль {} не совпадает с проверенным SDR-профилем",
                        payload.encoding
                    )
                });
            }
            Err(rejection) => {
                declared_problem.get_or_insert_with(|| rejection.to_string());
            }
        }
    }
    let Some((primary, profile)) = selected else {
        let offered = offered_codecs(video);
        return Err(match declared_problem {
            Some(problem) => format!(
                "Сервер предложил {} с несовместимым профилем ({problem}).",
                context.codec.label()
            ),
            None => format!(
                "Сервер не предложил кодек {} (предложены: {offered}).",
                context.codec.label()
            ),
        });
    };
    let (rtx_pt, rtx_apt) = match video
        .payloads
        .iter()
        .find(|payload| payload.encoding == "rtx" && payload.apt() == Some(primary.pt))
    {
        Some(rtx) => (Some(rtx.pt), Some(primary.pt)),
        None => (None, None),
    };
    let audio_mid = match context.audio_active {
        false => sections
            .iter()
            .find(|section| section.kind == Kind::Audio && !section.rejected)
            .map(|section| section.mid.clone()),
        true => {
            // A rejected m=audio 0 is the previous section the SFU disabled on
            // a re-offer; the live track sits next to it. Looking at the first
            // audio section without this filter would report a track that was
            // deliberately turned off and ignore the real one.
            let audio = sections
                .iter()
                .find(|section| section.kind == Kind::Audio && !section.rejected);
            let Some(audio) = audio else {
                let had_audio = sections.iter().any(|section| section.kind == Kind::Audio);
                return Err(match had_audio {
                    true => "Сервер выключил звуковую дорожку трансляции.".into(),
                    false => {
                        "Сервер не предложил звуковую дорожку, хотя системный звук включён.".into()
                    }
                });
            };
            if !audio.direction.accepts_send() {
                return Err("Сервер выключил звуковую дорожку трансляции.".into());
            }
            if !audio
                .payloads
                .iter()
                .any(|payload| payload.encoding == "opus")
            {
                return Err("Сервер не предложил Opus для звуковой дорожки.".into());
            }
            Some(audio.mid.clone())
        }
    };
    let selected = SelectedVideo {
        mid: video.mid.clone(),
        codec: context.codec,
        profile,
        primary_pt: primary.pt,
        fmtp: primary.fmtp.clone(),
        rtx_pt,
        rtx_apt,
        audio_mid,
    };
    if let Some(previous) = context.previous {
        verify_reoffer(previous, &selected)?;
    }
    Ok(selected)
}

/// A re-offer of the same publication must keep the negotiated stream: a
/// changed codec, mid or payload pair would silently replace the running
/// rendition.
fn verify_reoffer(previous: &SelectedVideo, next: &SelectedVideo) -> Result<(), String> {
    if previous.mid != next.mid {
        return Err(format!(
            "Повторное предложение сменило видеодорожку: {} вместо {}.",
            next.mid, previous.mid
        ));
    }
    if previous.codec != next.codec {
        return Err(format!(
            "Повторное предложение сменило кодек: {} вместо {}.",
            next.codec.label(),
            previous.codec.label()
        ));
    }
    if previous.primary_pt != next.primary_pt || previous.rtx_pt != next.rtx_pt {
        return Err("Повторное предложение сменило payload кодека.".into());
    }
    if previous.audio_mid != next.audio_mid {
        return Err("Повторное предложение сменило звуковую дорожку.".into());
    }
    Ok(())
}

fn offered_codecs(video: &Media) -> String {
    let mut names: Vec<String> = Vec::new();
    for payload in video.payloads.iter() {
        if payload.encoding == "rtx" {
            continue;
        }
        let label = Codec::parse(&payload.encoding)
            .map(Codec::label)
            .unwrap_or(payload.encoding.as_str())
            .to_owned();
        if !names.contains(&label) {
            names.push(label);
        }
    }
    if names.is_empty() {
        "ничего".into()
    } else {
        names.join(", ")
    }
}

/// The answer must keep the offered mid, the chosen SDR payload, the RTX pair
/// and — when audio is published — the audio m-line.
pub(super) fn verify_answer(
    sdp: &str,
    selected: &SelectedVideo,
    audio_active: bool,
) -> Result<(), String> {
    let sections = media_sections(sdp);
    let video = sections
        .iter()
        .find(|section| section.kind == Kind::Video && section.mid == selected.mid)
        .ok_or_else(|| {
            format!(
                "Ответ трансляции потерял видеодорожку с mid {}.",
                selected.mid
            )
        })?;
    // The local send track creates a sendonly answer m-line: the media engine
    // is the offerer's peer and never receives the rendition it publishes.
    // A `sendrecv` answer would let the SFU push a second stream onto the same
    // payload, so the loopback of all four codecs pins `a=sendonly`.
    if video.rejected || video.direction != Direction::SendOnly {
        return Err(format!(
            "Ответ трансляции выключил видеодорожку {}.",
            selected.codec.label()
        ));
    }
    let primary = video
        .payloads
        .iter()
        .find(|payload| payload.pt == selected.primary_pt)
        .ok_or_else(|| {
            format!(
                "Ответ трансляции не содержит согласованный {} payload {}.",
                selected.codec.label(),
                selected.primary_pt
            )
        })?;
    if Codec::parse(&primary.encoding) != Some(selected.codec) {
        return Err(format!(
            "Ответ трансляции выбрал {} вместо {}.",
            primary.encoding,
            selected.codec.label()
        ));
    }
    if !sdr_compatible(
        selected.codec,
        primary.fmtp.as_deref(),
        selected.fmtp.as_deref(),
    ) {
        return Err(format!(
            "Ответ трансляции сменил SDR-профиль {}.",
            selected.codec.label()
        ));
    }
    if let Some(rtx_pt) = selected.rtx_pt {
        let paired = video.payloads.iter().any(|payload| {
            payload.pt == rtx_pt
                && payload.encoding == "rtx"
                && payload.apt() == Some(selected.primary_pt)
        });
        if !paired {
            return Err(format!(
                "Ответ трансляции потерял RTX для {} payload {}.",
                selected.codec.label(),
                selected.primary_pt
            ));
        }
    }
    if let Some(other) = video.payloads.iter().find(|payload| {
        payload.encoding != "rtx"
            && !matches!(payload.encoding.as_str(), "red" | "ulpfec" | "flexfec-03")
            && Codec::parse(&payload.encoding).is_some_and(|codec| codec != selected.codec)
    }) {
        return Err(format!(
            "Ответ трансляции содержит лишний кодек {}.",
            other.encoding
        ));
    }
    if audio_active {
        let audio_mid = selected.audio_mid.as_deref().unwrap_or_default();
        let audio = sections
            .iter()
            .find(|section| section.kind == Kind::Audio && section.mid == audio_mid)
            .ok_or_else(|| {
                format!("Ответ трансляции потерял звуковую дорожку с mid {audio_mid}.")
            })?;
        if audio.rejected || matches!(audio.direction, Direction::RecvOnly | Direction::Inactive) {
            return Err("Ответ трансляции выключил звуковую дорожку.".into());
        }
        if !audio
            .payloads
            .iter()
            .any(|payload| payload.encoding == "opus")
        {
            return Err("Ответ трансляции не содержит Opus.".into());
        }
    }
    Ok(())
}

/// The receiver's view of the outbound video stream: the raw `RtcStats` values
/// the confirmation compares against the checked SDP.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct WireVideo {
    pub mid: String,
    pub frames_encoded: u32,
    pub codec: Option<WireCodec>,
}

/// The `RtcStats::Codec` the outbound stream points at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct WireCodec {
    pub payload_type: u32,
    pub mime_type: String,
    pub fmtp: String,
}

/// Confirm the transmission from encoded frames and their codec statistics.
///
/// `Ok(None)` means the encoder has not produced a frame yet: ICE and the
/// answer alone never confirm the codec. `Err` is a terminal mismatch the caller
/// reports once before releasing the peer.
pub(super) fn confirm_codec(
    selected: &SelectedVideo,
    wire: &WireVideo,
) -> Result<Option<ConfirmedCodec>, String> {
    // An empty mid is missing data, not a match: a report that does not name
    // the video stream cannot confirm the negotiated one.
    if wire.mid.is_empty() {
        return Err("Статистика WebRTC не называет видеодорожку трансляции.".into());
    }
    if wire.mid != selected.mid {
        return Err(format!(
            "Статистика описывает другую видеодорожку: {} вместо {}.",
            wire.mid, selected.mid
        ));
    }
    if wire.frames_encoded == 0 {
        return Ok(None);
    }
    let Some(codec) = wire.codec.as_ref() else {
        return Err(format!(
            "Для {} нет подтверждения кодека в статистике WebRTC.",
            selected.codec.label()
        ));
    };
    if Codec::parse(&codec.mime_type) != Some(selected.codec) {
        return Err(format!(
            "В эфире {} вместо {}.",
            mime_label(&codec.mime_type),
            selected.codec.label()
        ));
    }
    if codec.payload_type != u32::from(selected.primary_pt) {
        return Err(format!(
            "В эфире {} payload {} вместо согласованного {}.",
            selected.codec.label(),
            codec.payload_type,
            selected.primary_pt
        ));
    }
    // The profile is verified here, never trusted from the request: the SDP
    // already named it, so a report that omits FMTP keeps that value.
    reported_profile(selected.codec, &codec.fmtp, selected.profile)?;
    Ok(Some(ConfirmedCodec {
        codec: selected.codec,
    }))
}

fn mime_label(mime: &str) -> &str {
    mime.strip_prefix("video/").unwrap_or(mime)
}

/// The profile the statistics really report. A driver may omit FMTP entirely
/// (the pinned build reports only its encoder-backend tag), so a missing
/// parameter keeps the SDP-verified profile and its defaults.
fn reported_profile(codec: Codec, fmtp: &str, fallback: SdrProfile) -> Result<SdrProfile, String> {
    let parameters = parse_fmtp(Some(fmtp));
    let declared = match codec {
        Codec::H264 => match parameters.get("profile-level-id") {
            Some(id) => format!("packetization-mode=1;profile-level-id={id}"),
            None => return Ok(fallback),
        },
        Codec::Vp9 => match parameters.get("profile-id") {
            Some(profile) => format!("profile-id={profile}"),
            None => return Ok(fallback),
        },
        Codec::Av1 => match parameters.get("profile") {
            Some(profile) => format!("profile={profile}"),
            None => return Ok(fallback),
        },
        Codec::Vp8 => return Ok(SdrProfile::Vp8),
    };
    match sdr_profile(codec, Some(&declared)) {
        Ok(profile) if profile.compatible_with(fallback) => Ok(profile),
        Ok(_) => Err(format!(
            "В эфире {} с другим SDR-профилем ({declared}).",
            codec.label()
        )),
        Err(rejection) => Err(format!(
            "В эфире {} с несовместимым профилем: {rejection}.",
            codec.label()
        )),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Kind {
    #[default]
    Other,
    Video,
    Audio,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Direction {
    #[default]
    SendRecv,
    SendOnly,
    RecvOnly,
    Inactive,
}

impl Direction {
    /// The remote side may receive our media on this m-line.
    const fn accepts_send(self) -> bool {
        matches!(self, Self::SendRecv | Self::RecvOnly)
    }
}

#[derive(Clone, Debug, Default)]
struct Payload {
    pt: u8,
    encoding: String,
    clock: u32,
    fmtp: Option<String>,
}

impl Payload {
    fn apt(&self) -> Option<u8> {
        parse_fmtp(self.fmtp.as_deref())
            .get("apt")
            .and_then(|value| value.parse().ok())
    }
}

#[derive(Clone, Debug, Default)]
struct Media {
    kind: Kind,
    mid: String,
    direction: Direction,
    rejected: bool,
    payloads: Vec<Payload>,
}

impl Media {
    fn payload_mut(&mut self, pt: u8) -> &mut Payload {
        if let Some(index) = self.payloads.iter().position(|payload| payload.pt == pt) {
            return &mut self.payloads[index];
        }
        self.payloads.push(Payload {
            pt,
            ..Payload::default()
        });
        self.payloads.last_mut().expect("just pushed")
    }
}

/// Split an SDP into m-sections and the attributes this negotiation needs.
fn media_sections(sdp: &str) -> Vec<Media> {
    let normalized = sdp.replace("\r\n", "\n");
    let mut sections: Vec<Media> = Vec::new();
    let mut current: Option<Media> = None;
    for line in normalized.lines() {
        let line = line.trim_end();
        if let Some(rest) = line.strip_prefix("m=") {
            if let Some(media) = current.take() {
                sections.push(media);
            }
            current = Some(parse_m_line(rest));
            continue;
        }
        let Some(media) = current.as_mut() else {
            continue;
        };
        if let Some(value) = line.strip_prefix("a=mid:") {
            media.mid = value.trim().to_owned();
        } else if line == "a=sendonly" {
            media.direction = Direction::SendOnly;
        } else if line == "a=recvonly" {
            media.direction = Direction::RecvOnly;
        } else if line == "a=sendrecv" {
            media.direction = Direction::SendRecv;
        } else if line == "a=inactive" {
            media.direction = Direction::Inactive;
        } else if let Some(rest) = line.strip_prefix("a=rtpmap:") {
            parse_rtpmap(media, rest);
        } else if let Some(rest) = line.strip_prefix("a=fmtp:") {
            parse_fmtp_line(media, rest);
        }
    }
    if let Some(media) = current {
        sections.push(media);
    }
    sections
}

fn parse_m_line(rest: &str) -> Media {
    let mut tokens = rest.split_whitespace();
    let kind = match tokens.next() {
        Some("video") => Kind::Video,
        Some("audio") => Kind::Audio,
        _ => Kind::Other,
    };
    let rejected = tokens.next().is_some_and(|port| port == "0");
    let mut media = Media {
        kind,
        rejected,
        ..Media::default()
    };
    for token in tokens {
        // The protocol token is not a payload type.
        if let Ok(pt) = token.parse::<u8>() {
            media.payload_mut(pt);
        }
    }
    media
}

fn parse_rtpmap(media: &mut Media, rest: &str) {
    let Some((pt, value)) = rest.split_once(' ') else {
        return;
    };
    let Ok(pt) = pt.trim().parse::<u8>() else {
        return;
    };
    let mut parts = value.trim().split('/');
    let encoding = parts.next().unwrap_or_default().to_ascii_lowercase();
    let clock = parts
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    let payload = media.payload_mut(pt);
    payload.encoding = encoding;
    payload.clock = clock;
}

fn parse_fmtp_line(media: &mut Media, rest: &str) {
    let Some((pt, value)) = rest.split_once(' ') else {
        return;
    };
    let Ok(pt) = pt.trim().parse::<u8>() else {
        return;
    };
    let payload = media.payload_mut(pt);
    payload.fmtp = Some(match payload.fmtp.take() {
        Some(current) => format!("{current};{}", value.trim()),
        None => value.trim().to_owned(),
    });
}

/// FMTP keys of the confirmation report, in the order defined by `parse_fmtp`.
#[cfg(test)]
mod tests {
    use super::*;

    const OFFER: &str = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 102 103 104 105\r\n\
a=mid:0\r\n\
a=recvonly\r\n\
a=rtpmap:102 H264/90000\r\n\
a=rtcp-fb:102 nack\r\n\
a=rtcp-fb:102 nack pli\r\n\
a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n\
a=rtpmap:103 rtx/90000\r\n\
a=fmtp:103 apt=102\r\n\
a=rtpmap:104 H264/90000\r\n\
a=fmtp:104 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42001f\r\n\
a=rtpmap:105 rtx/90000\r\n\
a=fmtp:105 apt=104\r\n";

    fn context<'a>(previous: Option<&'a SelectedVideo>) -> OfferContext<'a> {
        OfferContext {
            codec: Codec::H264,
            profile: SdrProfile::H264 {
                baseline: crate::publisher::H264Baseline::Constrained,
                level: 0x1f,
            },
            audio_active: false,
            previous,
            quality: super::super::Quality::default(),
        }
    }

    #[test]
    fn the_offer_selects_the_constrained_baseline_payload_and_its_rtx_pair() {
        let selected = select_offer(OFFER, &context(None)).expect("select");
        assert_eq!(selected.mid, "0");
        assert_eq!(selected.primary_pt, 102);
        assert_eq!(selected.rtx_pt, Some(103));
        assert_eq!(selected.rtx_apt, Some(102));
        assert!(matches!(
            selected.profile,
            SdrProfile::H264 {
                baseline: crate::publisher::H264Baseline::Constrained,
                ..
            }
        ));
    }

    #[test]
    fn a_legacy_only_h264_offer_is_refused_with_the_profile_reason() {
        const LEGACY: &str = "v=0\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 104 105\r\n\
a=mid:0\r\n\
a=recvonly\r\n\
a=rtpmap:104 H264/90000\r\n\
a=fmtp:104 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42001f\r\n\
a=rtpmap:105 rtx/90000\r\n\
a=fmtp:105 apt=104\r\n";
        let error = select_offer(LEGACY, &context(None))
            .expect_err("a legacy payload is not the constrained SDR profile");
        assert!(
            error.contains("несовместимым профилем"),
            "the reason names the profile: {error}"
        );
    }

    #[test]
    fn an_inactive_video_line_is_refused() {
        let inactive = OFFER.replace("a=recvonly", "a=inactive");
        let error = select_offer(&inactive, &context(None)).expect_err("inactive");
        assert!(error.contains("выключил видеодорожку"), "{error}");
    }

    #[test]
    fn a_reoffer_that_changed_the_payload_is_refused() {
        let previous = select_offer(OFFER, &context(None)).expect("select");
        let mut next = previous.clone();
        next.primary_pt = 104;
        let error = verify_reoffer(&previous, &next).expect_err("changed payload");
        assert!(error.contains("payload"), "{error}");
    }

    #[test]
    fn a_rejected_video_m_line_is_not_a_second_video_track() {
        // The SFU disables the previous section with `m=video 0` and adds the
        // real one after it. Only one m-line carries a stream.
        const WITH_DISABLED: &str = "v=0\r\n\
m=video 0 UDP/TLS/RTP/SAVPF 96\r\n\
a=mid:old\r\n\
a=inactive\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 102 103\r\n\
a=mid:0\r\n\
a=recvonly\r\n\
a=rtpmap:102 H264/90000\r\n\
a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n\
a=rtpmap:103 rtx/90000\r\n\
a=fmtp:103 apt=102\r\n";
        let selected = select_offer(WITH_DISABLED, &context(None)).expect("select");
        assert_eq!(selected.mid, "0");
        assert_eq!(selected.primary_pt, 102);
    }

    #[test]
    fn an_offer_with_only_a_rejected_video_line_is_refused() {
        const REJECTED: &str = "v=0\r\n\
m=video 0 UDP/TLS/RTP/SAVPF 102\r\n\
a=mid:0\r\n\
a=inactive\r\n\
a=rtpmap:102 H264/90000\r\n";
        let error = select_offer(REJECTED, &context(None)).expect_err("rejected");
        assert!(error.contains("выключил видеодорожку"), "{error}");
    }

    #[test]
    fn a_sendrecv_answer_is_not_the_supported_sendonly_contract() {
        let selected = select_offer(OFFER, &context(None)).expect("select");
        let answer = "v=0\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 102 103\r\n\
a=mid:0\r\n\
a=sendrecv\r\n\
a=rtpmap:102 H264/90000\r\n\
a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n\
a=rtpmap:103 rtx/90000\r\n\
a=fmtp:103 apt=102\r\n";
        let error = verify_answer(answer, &selected, false).expect_err("sendrecv");
        assert!(error.contains("выключил видеодорожку"), "{error}");
    }

    #[test]
    fn a_rejected_audio_line_does_not_hide_the_live_track_of_a_reoffer() {
        // The SFU disables the previous audio section with `m=audio 0` and
        // adds the live one after it.
        const REAUDIO: &str = "v=0\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 102 103\r\n\
a=mid:0\r\n\
a=recvonly\r\n\
a=rtpmap:102 H264/90000\r\n\
a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n\
a=rtpmap:103 rtx/90000\r\n\
a=fmtp:103 apt=102\r\n\
m=audio 0 UDP/TLS/RTP/SAVPF 111\r\n\
a=mid:old-audio\r\n\
a=inactive\r\n\
a=rtpmap:111 opus/48000/2\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=mid:audio\r\n\
a=recvonly\r\n\
a=rtpmap:111 opus/48000/2\r\n";
        let mut context = context(None);
        context.audio_active = true;
        let selected = select_offer(REAUDIO, &context).expect("the live audio track is offered");
        assert_eq!(selected.audio_mid.as_deref(), Some("audio"));
    }

    #[test]
    fn an_offer_with_only_a_rejected_audio_line_is_refused_when_audio_is_active() {
        const REJECTED_AUDIO: &str = "v=0\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 102 103\r\n\
a=mid:0\r\n\
a=recvonly\r\n\
a=rtpmap:102 H264/90000\r\n\
a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n\
a=rtpmap:103 rtx/90000\r\n\
a=fmtp:103 apt=102\r\n\
m=audio 0 UDP/TLS/RTP/SAVPF 111\r\n\
a=mid:audio\r\n\
a=inactive\r\n\
a=rtpmap:111 opus/48000/2\r\n";
        let mut context = context(None);
        context.audio_active = true;
        let error = select_offer(REJECTED_AUDIO, &context).expect_err("the audio track is off");
        assert!(error.contains("выключил звуковую дорожку"), "{error}");
    }

    #[test]
    fn a_statistics_report_without_a_mid_never_confirms() {
        let selected = select_offer(OFFER, &context(None)).expect("select");
        let wire = WireVideo {
            mid: String::new(),
            frames_encoded: 7,
            codec: Some(WireCodec {
                payload_type: 102,
                mime_type: "video/H264".into(),
                fmtp: String::new(),
            }),
        };
        let error = confirm_codec(&selected, &wire).expect_err("an empty mid is missing data");
        assert!(error.contains("не называет"), "{error}");
    }

    #[test]
    fn the_confirmation_keeps_the_sdp_profile_when_the_report_omits_fmtp() {
        let selected = select_offer(OFFER, &context(None)).expect("select");
        let wire = WireVideo {
            mid: "0".into(),
            frames_encoded: 7,
            codec: Some(WireCodec {
                payload_type: 102,
                mime_type: "video/H264".into(),
                fmtp: "x-livekit-video-encoder-backend=software".into(),
            }),
        };
        let confirmed = confirm_codec(&selected, &wire)
            .expect("the omitted FMTP falls back to the checked SDP")
            .expect("frames were encoded");
        assert_eq!(confirmed.codec, Codec::H264);
    }

    #[test]
    fn encoded_frames_of_another_codec_are_a_terminal_mismatch() {
        let selected = select_offer(OFFER, &context(None)).expect("select");
        let wire = WireVideo {
            mid: "0".into(),
            frames_encoded: 3,
            codec: Some(WireCodec {
                payload_type: 102,
                mime_type: "video/VP8".into(),
                fmtp: String::new(),
            }),
        };
        let error = confirm_codec(&selected, &wire).expect_err("another codec");
        assert!(error.contains("VP8"), "{error}");
    }

    #[test]
    fn a_connection_without_encoded_frames_never_confirms() {
        let selected = select_offer(OFFER, &context(None)).expect("select");
        let wire = WireVideo {
            mid: "0".into(),
            codec: Some(WireCodec {
                payload_type: 102,
                mime_type: "video/H264".into(),
                fmtp: String::new(),
            }),
            ..WireVideo::default()
        };
        assert_eq!(confirm_codec(&selected, &wire), Ok(None));
    }
}
