//! Receiver-side codec contract of one viewing attempt.
//!
//! Watching depends on the decoders of this build, never on the local encoder
//! mode: a CPU or GPU publication choice and a missing `stream.start` sender
//! cannot make a stream unviewable, and a hardware encoder cannot make one
//! viewable. The catalogue names the codec before `stream.watch`; the offer
//! names the real SDP profile. A codec whose decoder is known to be missing is
//! refused before the watch, while a profile first discovered in the offer
//! ends the attempt with the ordinary `stream.leave` release.
//!
//! Nothing here hard-codes a payload type or matches SDP text: payloads are
//! read from `m=`/`a=rtpmap`/`a=fmtp` and compared through the shared SDR
//! profile parser of the publisher.

use crate::controller::Publication;
use crate::publisher::{Codec, CodecRole, CodecSnapshot, ProfileRejection, sdr_profile};

use super::Viewer;

impl Viewer {
    /// The receiver capabilities of this build: the injected matrix in a test,
    /// the process-wide probe otherwise.
    pub(super) fn codec_snapshot(&self) -> &CodecSnapshot {
        match &self.snapshot {
            Some(snapshot) => snapshot,
            None => CodecSnapshot::current(),
        }
    }

    /// Test entry point: inject the receiver capability matrix of this build,
    /// so "no decoder" and "decoder present" are both reproducible.
    #[cfg(test)]
    pub(crate) fn with_snapshot(server: i64, channel: i64, snapshot: CodecSnapshot) -> Self {
        let mut viewer = Self::new(server, channel);
        viewer.snapshot = Some(snapshot);
        viewer
    }

    /// Why the catalogue's codec cannot be viewed. `None` means the decoder
    /// exists or the value cannot identify a codec, so the offer decides.
    pub(super) fn catalogue_refusal(&self, publication: &Publication) -> Option<String> {
        catalogue_unavailable(&publication.codec, self.codec_snapshot())
    }

    /// Why the offer cannot be decoded. `None` means at least the published
    /// rendition is decodable (or the offer carries no video payload to judge).
    pub(super) fn offer_refusal(&self, sdp: &str, publication: &Publication) -> Option<String> {
        check_offer(sdp, Codec::parse(&publication.codec), self.codec_snapshot()).err()
    }
}

/// Why this build cannot decode `codec` as the catalogue names it. `None`
/// means the decoder exists (or the catalogue value is not a known codec, in
/// which case the offer decides).
pub(super) fn catalogue_unavailable(codec: &str, snapshot: &CodecSnapshot) -> Option<String> {
    let codec = Codec::parse(codec)?;
    let availability = snapshot.availability(CodecRole::Receiver, codec);
    if availability.available() {
        return None;
    }
    Some(format!(
        "Нет декодера {} для этой трансляции: {}",
        codec.label(),
        availability
            .reason()
            .unwrap_or("декодер отсутствует в этой сборке")
    ))
}

/// Check one received offer against the receiver capabilities.
///
/// `expected` is the codec the catalogue promised. When it is known, the offer
/// must carry that codec with an SDR profile this build decodes: the SFU relays
/// exactly the rendition that was published, so another decodable payload in
/// the same m-line never carries its RTP. Without an expected codec any offered
/// payload this build can decode is enough. An offer whose codecs cannot be
/// identified is left to the engine instead of guessing that a decoder is
/// missing.
pub(super) fn check_offer(
    sdp: &str,
    expected: Option<Codec>,
    snapshot: &CodecSnapshot,
) -> Result<(), String> {
    let raw = video_payloads(sdp);
    // Nothing that looks like a video payload: leave the contract to the media
    // engine, which reports a real negotiation failure. Guessing "no decoder"
    // from an unparsable or generic description would be wrong.
    if raw.is_empty() {
        return Ok(());
    }
    let known: Vec<Offered> = raw.into_iter().filter_map(Offered::parse).collect();
    // A video m-line whose payloads cannot be identified is not evidence of a
    // missing decoder: hand the whole description to the engine, which parses
    // it completely and reports its own negotiation failure.
    if known.is_empty() {
        return Ok(());
    }
    if let Some(expected) = expected {
        let candidates: Vec<&Offered> = known
            .iter()
            .filter(|offered| offered.codec == expected)
            .collect();
        if candidates.is_empty() {
            return Err(format!(
                "Сервер не предложил кодек {} для просмотра{}.",
                expected.label(),
                offered_names(&known)
            ));
        }
        if candidates.iter().any(|offered| offered.decodable(snapshot)) {
            return Ok(());
        }
        return Err(expected_refusal(expected, &candidates, snapshot));
    }
    if known.iter().any(|offered| offered.decodable(snapshot)) {
        return Ok(());
    }
    Err(offered_refusal(&known, snapshot))
}

/// One primary video payload of an offer.
struct Offered {
    codec: Codec,
    profile: Result<crate::publisher::SdrProfile, ProfileRejection>,
}

impl Offered {
    fn parse(raw: RawPayload) -> Option<Self> {
        if raw.clock != 90_000 {
            return None;
        }
        if matches!(
            raw.encoding.as_str(),
            "rtx" | "red" | "ulpfec" | "flexfec-03"
        ) {
            return None;
        }
        let codec = Codec::parse(&raw.encoding)?;
        let profile = sdr_profile(codec, raw.fmtp.as_deref());
        Some(Self { codec, profile })
    }

    /// This build decodes the payload's codec and profile.
    fn decodable(&self, snapshot: &CodecSnapshot) -> bool {
        let Ok(profile) = &self.profile else {
            return false;
        };
        snapshot
            .availability(CodecRole::Receiver, self.codec)
            .profile()
            .is_some_and(|supported| supported.compatible_with(*profile))
    }
}

fn expected_refusal(codec: Codec, candidates: &[&Offered], snapshot: &CodecSnapshot) -> String {
    let availability = snapshot.availability(CodecRole::Receiver, codec);
    if !availability.available() {
        return format!(
            "Нет декодера {} для этой трансляции: {}",
            codec.label(),
            availability
                .reason()
                .unwrap_or("декодер отсутствует в этой сборке")
        );
    }
    let detail = candidates
        .iter()
        .find_map(|offered| match &offered.profile {
            Err(rejection) => Some(rejection.to_string()),
            Ok(_) => None,
        })
        .unwrap_or_else(|| "предложен другой SDR-профиль".to_owned());
    format!(
        "Декодер {} в этой сборке не поддерживает профиль трансляции: {detail}.",
        codec.label()
    )
}

fn offered_refusal(known: &[Offered], snapshot: &CodecSnapshot) -> String {
    let mut reasons: Vec<String> = Vec::new();
    for offered in known {
        let availability = snapshot.availability(CodecRole::Receiver, offered.codec);
        let reason = match (availability.reason(), &offered.profile) {
            (Some(reason), _) => reason.to_owned(),
            (None, Err(rejection)) => rejection.to_string(),
            (None, Ok(_)) => "профиль не совпадает с SDR-профилем этой сборки".to_owned(),
        };
        let line = format!("{}: {reason}", offered.codec.label());
        if !reasons.contains(&line) {
            reasons.push(line);
        }
    }
    if reasons.is_empty() {
        return "В этой сборке нет декодера предложенного кодека.".into();
    }
    format!(
        "Нет декодера ни одного предложенного кодека: {}.",
        reasons.join("; ")
    )
}

fn offered_names(known: &[Offered]) -> String {
    let mut names: Vec<&'static str> = Vec::new();
    for offered in known {
        let name = offered.codec.label();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    if names.is_empty() {
        String::new()
    } else {
        format!(" (предложены: {})", names.join(", "))
    }
}

/// One `a=rtpmap` entry of a non-rejected video m-line.
struct RawPayload {
    pt: u8,
    encoding: String,
    clock: u32,
    fmtp: Option<String>,
}

/// Payloads of every non-rejected `m=video` section. A rejected section (port
/// 0) carries no stream and is ignored; audio m-lines belong to the separate
/// voice path and are never inspected here.
fn video_payloads(sdp: &str) -> Vec<RawPayload> {
    let normalized = sdp.replace("\r\n", "\n");
    let mut payloads: Vec<RawPayload> = Vec::new();
    let mut video = false;
    for line in normalized.lines() {
        let line = line.trim_end();
        if let Some(rest) = line.strip_prefix("m=") {
            let mut tokens = rest.split_whitespace();
            let kind = tokens.next();
            let port = tokens.next();
            video = kind == Some("video") && port.is_some_and(|port| port != "0");
            if !video {
                continue;
            }
            for token in tokens {
                if let Ok(pt) = token.parse::<u8>() {
                    payloads.push(RawPayload {
                        pt,
                        encoding: String::new(),
                        clock: 0,
                        fmtp: None,
                    });
                }
            }
            continue;
        }
        if !video {
            continue;
        }
        if let Some(rest) = line.strip_prefix("a=rtpmap:") {
            set_rtpmap(&mut payloads, rest);
        } else if let Some(rest) = line.strip_prefix("a=fmtp:") {
            set_fmtp(&mut payloads, rest);
        }
    }
    payloads
}

fn set_rtpmap(payloads: &mut [RawPayload], rest: &str) {
    let Some((pt, value)) = rest.split_once(' ') else {
        return;
    };
    let Ok(pt) = pt.trim().parse::<u8>() else {
        return;
    };
    let Some(payload) = payloads.iter_mut().find(|payload| payload.pt == pt) else {
        return;
    };
    let mut parts = value.trim().split('/');
    payload.encoding = parts.next().unwrap_or_default().to_ascii_lowercase();
    payload.clock = parts
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
}

fn set_fmtp(payloads: &mut [RawPayload], rest: &str) {
    let Some((pt, value)) = rest.split_once(' ') else {
        return;
    };
    let Ok(pt) = pt.trim().parse::<u8>() else {
        return;
    };
    let Some(payload) = payloads.iter_mut().find(|payload| payload.pt == pt) else {
        return;
    };
    payload.fmtp = Some(match payload.fmtp.take() {
        Some(current) => format!("{current};{}", value.trim()),
        None => value.trim().to_owned(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publisher::VideoCodecCapability;

    fn probe(receiver: &[Codec]) -> CodecSnapshot {
        let receiver = receiver
            .iter()
            .map(|codec| {
                let fmtp = match codec {
                    Codec::H264 => Some("packetization-mode=1;profile-level-id=42e01f"),
                    Codec::Vp8 => None,
                    Codec::Vp9 => Some("profile-id=0"),
                    Codec::Av1 => Some("profile=0"),
                };
                VideoCodecCapability::new(*codec, 90_000, fmtp)
            })
            .collect();
        CodecSnapshot::receiver_only(receiver)
    }

    fn offer(codec: Codec, fmtp: Option<&str>) -> String {
        let name = match codec {
            Codec::H264 => "H264",
            Codec::Vp8 => "VP8",
            Codec::Vp9 => "VP9",
            Codec::Av1 => "AV1",
        };
        let mut sdp = format!(
            "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96 97\r\na=mid:0\r\na=sendonly\r\na=rtpmap:96 {name}/90000\r\n"
        );
        if let Some(fmtp) = fmtp {
            sdp.push_str(&format!("a=fmtp:96 {fmtp}\r\n"));
        }
        sdp.push_str("a=rtpmap:97 rtx/90000\r\na=fmtp:97 apt=96\r\n");
        sdp
    }

    #[test]
    fn a_rejected_first_video_m_line_is_ignored() {
        let sdp = format!(
            "v=0\r\nm=video 0 UDP/TLS/RTP/SAVPF 90\r\na=mid:old\r\na=inactive\r\n{}",
            offer(Codec::Vp8, None)
        );
        assert_eq!(video_payloads(&sdp).len(), 2, "only the live m-line");
        check_offer(&sdp, Some(Codec::Vp8), &probe(&[Codec::Vp8])).expect("the live line");
    }

    #[test]
    fn an_audio_m_line_is_not_part_of_the_video_decision() {
        let sdp = format!(
            "{}m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:1\r\na=sendonly\r\na=rtpmap:111 opus/48000/2\r\n",
            offer(Codec::Vp9, Some("profile-id=0"))
        );
        check_offer(&sdp, Some(Codec::Vp9), &probe(&[Codec::Vp9])).expect("video decodes");
    }

    #[test]
    fn an_offer_without_a_video_payload_is_left_to_the_engine() {
        let sdp =
            "v=0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:1\r\na=rtpmap:111 opus/48000/2\r\n";
        check_offer(sdp, None, &probe(&[Codec::Vp8])).expect("no video payload to judge");
        check_offer(
            "v=0\r\na=ice-ufrag:early\r\n",
            Some(Codec::Vp8),
            &probe(&[Codec::Vp8]),
        )
        .expect("a generic offer is not a decoder decision");
    }

    #[test]
    fn rtx_and_fec_payloads_are_not_candidate_codecs() {
        let sdp = offer(Codec::Av1, Some("profile=0"));
        let offered = video_payloads(&sdp)
            .into_iter()
            .filter_map(Offered::parse)
            .map(|offered| offered.codec)
            .collect::<Vec<_>>();
        assert_eq!(offered, vec![Codec::Av1]);
    }
}
