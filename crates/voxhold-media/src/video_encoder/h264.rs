//! H.264 Annex B inspection for the hardware probe.
//!
//! These helpers are pure byte parsing: no Media Foundation and no driver, so
//! the profile decision of the probe is testable without a GPU. The probe uses
//! them for two questions only: does the produced access unit really carry an
//! IDR with parameter sets, and which SDR baseline does its SPS describe.

use crate::publisher::H264Baseline;

/// What the SPS says about the profile. `Missing` is not a rejection: a
/// driver may omit the SPS from the access unit and keep it in
/// `MF_MT_MPEG_SEQUENCE_HEADER`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpsProfile {
    Baseline(H264Baseline),
    Other(u8),
    Missing,
}

/// NAL units of an Annex B byte stream, without the start code.
pub(crate) fn nal_units(bytes: &[u8]) -> Vec<&[u8]> {
    let mut units = Vec::new();
    let mut index = 0;
    let mut start: Option<usize> = None;
    while index + 2 < bytes.len() {
        if bytes[index] == 0 && bytes[index + 1] == 0 && bytes[index + 2] == 1 {
            if let Some(current) = start {
                let mut end = index;
                while end > current && bytes[end - 1] == 0 {
                    end -= 1;
                }
                if end > current {
                    units.push(&bytes[current..end]);
                }
            }
            start = Some(index + 3);
            index += 3;
        } else {
            index += 1;
        }
    }
    if let Some(current) = start
        && current < bytes.len()
    {
        let mut end = bytes.len();
        while end > current && bytes[end - 1] == 0 {
            end -= 1;
        }
        if end > current {
            units.push(&bytes[current..end]);
        }
    }
    units
}

/// NAL unit types in stream order. `7` is SPS, `8` is PPS, `5` is an IDR
/// slice.
pub(crate) fn nal_types(bytes: &[u8]) -> Vec<u8> {
    nal_units(bytes)
        .into_iter()
        .map(|unit| unit[0] & 0x1f)
        .collect()
}

/// The profile of the first SPS found in any of the sources, in order.
///
/// The SPS carries `profile_idc` and `constraint_set1_flag`, which together
/// are the `profile-level-id` of the SDP: `42e0xx` is Constrained Baseline and
/// `4200xx` is legacy Baseline with the same `profile_idc`.
pub(crate) fn sps_profile(sources: &[&[u8]]) -> SpsProfile {
    for source in sources {
        for unit in nal_units(source) {
            if unit[0] & 0x1f != 7 || unit.len() < 3 {
                continue;
            }
            let profile_idc = unit[1];
            let constraint_set1 = unit[2] & 0x40 != 0;
            return match profile_idc {
                66 if constraint_set1 => SpsProfile::Baseline(H264Baseline::Constrained),
                66 => SpsProfile::Baseline(H264Baseline::Legacy),
                other => SpsProfile::Other(other),
            };
        }
    }
    SpsProfile::Missing
}

/// The SDR baseline the encoder produced, or why it is not one.
///
/// `profile_attribute` is `MF_MT_MPEG2_PROFILE` of the output type. The SPS of
/// the real bitstream wins when it is present, because it is what the SFU and
/// the receiver parse. Without an SPS the attribute is used, and a missing
/// attribute is a rejection rather than an assumption of Constrained Baseline.
pub(crate) fn keyframe_baseline(
    profile_attribute: Option<u32>,
    frame: &[u8],
    sequence_header: &[u8],
) -> Result<H264Baseline, String> {
    let types = nal_types(frame);
    if types.is_empty() {
        return Err("нет Annex B start code".into());
    }
    if !types.contains(&5) {
        return Err("нет IDR-кадра".into());
    }
    let header = nal_types(sequence_header);
    let has_sps = types.contains(&7) || header.contains(&7);
    let has_pps = types.contains(&8) || header.contains(&8);
    if !has_sps || !has_pps {
        return Err("нет SPS/PPS ни в кадре, ни в MF_MT_MPEG_SEQUENCE_HEADER".into());
    }
    let declared = match profile_attribute {
        None => None,
        Some(256) => Some(H264Baseline::Constrained),
        Some(66) => Some(H264Baseline::Legacy),
        Some(other) => {
            return Err(format!(
                "драйвер выдал H.264 profile {other} вместо Baseline"
            ));
        }
    };
    match sps_profile(&[frame, sequence_header]) {
        SpsProfile::Baseline(baseline) => Ok(baseline),
        SpsProfile::Other(profile) => Err(format!(
            "SPS объявляет H.264 profile {profile} вместо Baseline"
        )),
        SpsProfile::Missing => declared
            .ok_or_else(|| "нет MF_MT_MPEG2_PROFILE и нет SPS, профиль нельзя подтвердить".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `00 00 01 <type> <payload>`; the payload is arbitrary for NAL typing.
    fn nal(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut unit = vec![0, 0, 1, kind];
        unit.extend_from_slice(payload);
        unit
    }

    fn sps(profile_idc: u8, constraints: u8) -> Vec<u8> {
        nal(7, &[profile_idc, constraints, 0x1f, 0x80])
    }

    fn idr() -> Vec<u8> {
        nal(5, &[0x88, 0x84, 0x00])
    }

    fn pps() -> Vec<u8> {
        nal(8, &[0xce, 0x3c])
    }

    fn stream(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    #[test]
    fn nal_units_accept_three_and_four_byte_start_codes() {
        let mut bytes = vec![0, 0, 0, 1, 9];
        bytes.extend_from_slice(&nal(7, &[0x42, 0xe0, 0x1f]));
        bytes.extend_from_slice(&[0, 0, 0, 1, 8, 0xce]);
        assert_eq!(nal_types(&bytes), [9, 7, 8]);
        let units = nal_units(&bytes);
        assert_eq!(units.len(), 3);
        assert_eq!(units[1][1], 0x42, "the SPS payload starts after the header");
    }

    #[test]
    fn trailing_zero_bytes_stay_out_of_the_units() {
        let mut bytes = nal(7, &[0x42, 0xe0, 0x1f]);
        bytes.extend_from_slice(&[0, 0, 0, 1, 5, 0x88, 0]);
        let units = nal_units(&bytes);
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].len(), 4);
    }

    #[test]
    fn sps_constraint_set1_decides_the_baseline() {
        let constrained = stream(&[sps(66, 0xe0), pps(), idr()]);
        let legacy = stream(&[sps(66, 0x00), pps(), idr()]);
        assert_eq!(
            keyframe_baseline(None, &constrained, &[]),
            Ok(H264Baseline::Constrained)
        );
        assert_eq!(
            keyframe_baseline(None, &legacy, &[]),
            Ok(H264Baseline::Legacy)
        );
        // The SPS wins over a driver attribute that cannot express the flag.
        assert_eq!(
            keyframe_baseline(Some(66), &constrained, &[]),
            Ok(H264Baseline::Constrained)
        );
    }

    #[test]
    fn high_profile_is_rejected_even_with_a_baseline_attribute() {
        let high = stream(&[sps(100, 0x00), pps(), idr()]);
        assert!(keyframe_baseline(Some(256), &high, &[]).is_err());
        assert!(keyframe_baseline(None, &high, &[]).is_err());
        assert!(keyframe_baseline(Some(100), &high, &[]).is_err());
    }

    #[test]
    fn a_missing_sps_is_not_assumed_from_the_attribute() {
        let only_idr = stream(&[pps(), idr()]);
        assert!(keyframe_baseline(None, &only_idr, &[]).is_err());
        assert!(
            keyframe_baseline(Some(256), &only_idr, &[]).is_err(),
            "parameter sets are required, the attribute alone is not enough"
        );
        let no_pps = stream(&[sps(66, 0xe0), idr()]);
        assert!(keyframe_baseline(Some(256), &no_pps, &[]).is_err());
        assert_eq!(
            keyframe_baseline(Some(256), &stream(&[sps(66, 0xe0), pps(), idr()]), &[]),
            Ok(H264Baseline::Constrained)
        );
    }

    #[test]
    fn a_truncated_sps_falls_back_to_the_attribute() {
        let frame = stream(&[idr()]);
        let mut header = vec![0, 0, 1, 7];
        header.extend_from_slice(&pps());
        assert!(
            keyframe_baseline(None, &frame, &header).is_err(),
            "an unreadable SPS cannot decide the baseline"
        );
        assert_eq!(
            keyframe_baseline(Some(256), &frame, &header),
            Ok(H264Baseline::Constrained)
        );
    }

    #[test]
    fn parameter_sets_may_live_in_the_sequence_header() {
        let frame = stream(&[idr()]);
        let header = stream(&[sps(66, 0xe0), pps()]);
        assert_eq!(
            keyframe_baseline(None, &frame, &header),
            Ok(H264Baseline::Constrained)
        );
        assert!(keyframe_baseline(None, &frame, &[]).is_err());
        assert!(keyframe_baseline(None, &stream(&[sps(66, 0xe0), pps()]), &[]).is_err());
    }

    #[test]
    fn a_missing_idr_is_a_rejected_access_unit() {
        let no_idr = stream(&[sps(66, 0xe0), pps()]);
        assert!(keyframe_baseline(Some(256), &no_idr, &[]).is_err());
        assert!(keyframe_baseline(Some(256), &[], &[]).is_err());
    }
}
