//! AV1 OBU inspection for the hardware probe.
//!
//! The probe must know whether the first encoded buffer is a key frame and not
//! only a sequence header. The old check returned success on the sequence
//! header OBU itself, which every encoder emits before the first frame and
//! which says nothing about the frame type.
//!
//! This is pure byte parsing: a sequence header OBU gives
//! `reduced_still_picture_header` and the frame id width, and the uncompressed
//! frame header then carries `frame_type == KEY_FRAME` (0) after the
//! `show_existing_frame` bit and the frame id. The helpers are tested with
//! synthetic OBUs, so the decision does not need a GPU.

/// `true` when the access unit carries a real key frame.
///
/// `sequence_header` is the `MF_MT_MPEG_SEQUENCE_HEADER` blob of the output
/// type; it is used when the buffer itself starts below the sequence header,
/// which hardware encoders do after the first access unit.
pub(crate) fn keyframe(frame: &[u8], sequence_header: &[u8]) -> Result<(), String> {
    if frame.is_empty() {
        return Err("пустой буфер".into());
    }
    let frame_obus = obus(frame)?;
    let hint_obus = if sequence_header.is_empty() {
        Vec::new()
    } else {
        obus(sequence_header).unwrap_or_default()
    };
    let sequence = frame_obus
        .iter()
        .chain(hint_obus.iter())
        .filter(|obu| obu.kind == KIND_SEQUENCE_HEADER)
        .find_map(|obu| sequence_header_info(obu.payload))
        .ok_or_else(|| {
            "AV1: sequence header не найден, тип кадра нельзя подтвердить".to_string()
        })?;
    let mut saw_frame = false;
    for obu in &frame_obus {
        if obu.kind == KIND_FRAME_HEADER || obu.kind == KIND_FRAME {
            saw_frame = true;
            match frame_kind(obu.payload, &sequence) {
                Some(FrameKind::Key) => return Ok(()),
                Some(FrameKind::Other) => {}
                None => return Err("AV1: заголовок кадра обрезан".into()),
            }
        }
    }
    if saw_frame {
        Err("AV1: первый кадр не ключевой".into())
    } else {
        Err("AV1: в буфере только sequence header, кадра нет".into())
    }
}

const KIND_SEQUENCE_HEADER: u8 = 1;
const KIND_FRAME_HEADER: u8 = 3;
const KIND_FRAME: u8 = 6;

/// The parts of the sequence header the frame header depends on.
struct Sequence {
    /// In this mode every frame is intra: the spec forces
    /// `frame_type == KEY_FRAME` and omits the field from the frame header.
    reduced_still_picture: bool,
    /// `delta_frame_id_length_minus_2 + 2 + additional_frame_id_length_minus_1 + 1`.
    frame_id_bits: u32,
}

enum FrameKind {
    Key,
    Other,
}

struct Obu<'a> {
    kind: u8,
    payload: &'a [u8],
}

/// The OBUs of a low-overhead AV1 stream, split at their `obu_size` fields.
fn obus(bytes: &[u8]) -> Result<Vec<Obu<'_>>, String> {
    let mut units = Vec::new();
    let mut index = 0usize;
    for _ in 0..512 {
        let Some(&header) = bytes.get(index) else {
            return Ok(units);
        };
        index += 1;
        if header & 0x80 != 0 {
            return Err("AV1: forbidden bit".into());
        }
        let kind = (header >> 3) & 0x0f;
        let extension = header & 0x04 != 0;
        let has_size = header & 0x02 != 0;
        if extension {
            let Some(_) = bytes.get(index) else {
                return Ok(units);
            };
            index += 1;
        }
        let size = if has_size {
            let mut size = 0usize;
            let mut shift = 0u32;
            loop {
                let Some(&byte) = bytes.get(index) else {
                    return Ok(units);
                };
                index += 1;
                size |= usize::from(byte & 0x7f) << shift;
                if byte & 0x80 == 0 {
                    break;
                }
                shift += 7;
                if shift > 28 {
                    return Err("AV1: obu_size слишком велик".into());
                }
            }
            size
        } else {
            // The last OBU of the temporal unit has no size field and runs to
            // the end of the buffer.
            bytes.len() - index
        };
        let end = index.saturating_add(size).min(bytes.len());
        units.push(Obu {
            kind,
            payload: &bytes[index..end],
        });
        index = end;
    }
    Err("AV1: слишком много OBU в буфере".into())
}

/// Reads the fields up to `frame_id_numbers_present_flag` of §5.5.1.
fn sequence_header_info(payload: &[u8]) -> Option<Sequence> {
    let mut bits = Bits::new(payload);
    bits.bits(3)?; // seq_profile
    bits.bit()?; // still_picture: no decision depends on it
    let reduced = bits.bit()? == 1; // reduced_still_picture_header
    if reduced {
        bits.bits(5)?; // seq_level_idx[0]
        return Some(Sequence {
            reduced_still_picture: true,
            frame_id_bits: 0,
        });
    }
    let timing_info_present = bits.bit()? == 1;
    let mut decoder_model_info_present = false;
    let mut buffer_delay_bits = 0u32;
    if timing_info_present {
        bits.skip(64)?; // num_units_in_display_tick, time_scale
        if bits.bit()? == 1 {
            bits.uvlc()?; // num_ticks_per_picture_minus_1
        }
        decoder_model_info_present = bits.bit()? == 1;
        if decoder_model_info_present {
            buffer_delay_bits = bits.bits(5)? + 1;
            bits.skip(32)?; // num_units_in_decoding_tick
            bits.bits(5)?; // buffer_removal_time_length_minus_1
            bits.bits(5)?; // frame_presentation_time_length_minus_1
        }
    }
    let initial_display_delay_present = bits.bit()? == 1;
    let operating_points = bits.bits(5)? + 1;
    for _ in 0..operating_points {
        bits.bits(12)?; // operating_point_idc
        let level = bits.bits(5)?; // seq_level_idx
        if level > 7 {
            bits.bit()?; // seq_tier
        }
        if decoder_model_info_present && bits.bit()? == 1 {
            // decoder_buffer_delay, encoder_buffer_delay, low_delay_mode_flag.
            bits.skip((2 * buffer_delay_bits + 1) as usize)?;
        }
        if initial_display_delay_present && bits.bit()? == 1 {
            bits.bits(4)?; // initial_display_delay_minus_1
        }
    }
    let width_bits = bits.bits(4)? + 1;
    let height_bits = bits.bits(4)? + 1;
    bits.skip((width_bits + height_bits) as usize)?; // max frame size
    let frame_id_bits = if bits.bit()? == 1 {
        let delta = bits.bits(4)? + 2;
        let additional = bits.bits(3)? + 1;
        delta + additional
    } else {
        0
    };
    Some(Sequence {
        reduced_still_picture: false,
        frame_id_bits,
    })
}

fn frame_kind(payload: &[u8], sequence: &Sequence) -> Option<FrameKind> {
    if sequence.reduced_still_picture {
        return Some(FrameKind::Key);
    }
    let mut bits = Bits::new(payload);
    if bits.bit()? == 1 {
        // `show_existing_frame`: the encoder re-sends a stored picture instead
        // of coding a new key frame.
        return Some(FrameKind::Other);
    }
    bits.skip(sequence.frame_id_bits as usize)?; // current_frame_id
    match bits.bits(2)? {
        0 => Some(FrameKind::Key),
        _ => Some(FrameKind::Other),
    }
}

/// MSB-first bit reader. Every read past the payload is `None`, so a truncated
/// OBU is an error and never an accidental key frame.
struct Bits<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Bits<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.position / 8)?;
        let shift = 7 - (self.position % 8);
        self.position += 1;
        Some(u32::from((byte >> shift) & 1))
    }

    fn bits(&mut self, count: u32) -> Option<u32> {
        let mut value = 0u32;
        for _ in 0..count {
            value = (value << 1) | self.bit()?;
        }
        Some(value)
    }

    fn skip(&mut self, count: usize) -> Option<()> {
        let end = self.position.checked_add(count)?;
        if end > self.data.len() * 8 {
            return None;
        }
        self.position = end;
        Some(())
    }

    /// AV1 `uvlc()`: leading zeros, then that many payload bits.
    fn uvlc(&mut self) -> Option<u32> {
        let mut zeros = 0u32;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros >= 32 {
                return None;
            }
        }
        if zeros == 0 {
            return Some(0);
        }
        Some((1u32 << zeros) - 1 + self.bits(zeros)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Writer {
        bits: Vec<u32>,
    }

    impl Writer {
        fn put(&mut self, value: u32, count: u32) {
            for shift in (0..count).rev() {
                self.bits.push((value >> shift) & 1);
            }
        }

        fn bytes(&self) -> Vec<u8> {
            let mut bytes = vec![0u8; self.bits.len().div_ceil(8)];
            for (index, bit) in self.bits.iter().enumerate() {
                bytes[index / 8] |= (*bit as u8) << (7 - index % 8);
            }
            bytes
        }
    }

    /// An OBU with `has_size`; the payload length fits in one size byte.
    fn obu(kind: u8, payload: &[u8]) -> Vec<u8> {
        assert!(payload.len() < 0x80);
        let mut bytes = vec![(kind << 3) | 0x02, payload.len() as u8];
        bytes.extend_from_slice(payload);
        bytes
    }

    fn sequence(reduced: bool, frame_id: bool) -> Vec<u8> {
        let mut writer = Writer::default();
        writer.put(0, 3); // seq_profile
        writer.put(u32::from(reduced), 1); // still_picture
        writer.put(u32::from(reduced), 1); // reduced_still_picture_header
        if reduced {
            writer.put(0, 5); // seq_level_idx[0]
        } else {
            writer.put(0, 1); // timing_info_present_flag
            writer.put(0, 1); // initial_display_delay_present_flag
            writer.put(0, 5); // operating_points_cnt_minus_1
            writer.put(0, 12); // operating_point_idc[0]
            writer.put(0, 5); // seq_level_idx[0]
        }
        writer.put(0, 4); // frame_width_bits_minus_1
        writer.put(0, 4); // frame_height_bits_minus_1
        writer.put(0, 1); // max_frame_width_minus_1
        writer.put(0, 1); // max_frame_height_minus_1
        if !reduced {
            writer.put(u32::from(frame_id), 1); // frame_id_numbers_present_flag
            if frame_id {
                writer.put(0, 4); // delta_frame_id_length_minus_2
                writer.put(0, 3); // additional_frame_id_length_minus_1
            }
        }
        writer.bytes()
    }

    fn frame(frame_id_bits: u32, key: bool, show_existing: bool) -> Vec<u8> {
        let mut writer = Writer::default();
        writer.put(u32::from(show_existing), 1);
        if show_existing {
            writer.put(0, 3); // frame_to_show_map_idx
        } else {
            writer.put(0, frame_id_bits); // current_frame_id
            writer.put(if key { 0 } else { 1 }, 2); // frame_type
        }
        writer.put(0, 16); // tile data does not matter here
        writer.bytes()
    }

    #[test]
    fn a_sequence_header_alone_is_not_a_key_frame() {
        let bytes = obu(KIND_SEQUENCE_HEADER, &sequence(false, false));
        let error = keyframe(&bytes, &[]).unwrap_err();
        assert!(error.contains("кадра нет"), "{error}");
    }

    #[test]
    fn a_key_frame_after_the_sequence_header_passes() {
        let mut bytes = obu(KIND_SEQUENCE_HEADER, &sequence(false, false));
        bytes.extend_from_slice(&obu(KIND_FRAME, &frame(0, true, false)));
        assert_eq!(keyframe(&bytes, &[]), Ok(()));
    }

    #[test]
    fn an_inter_frame_is_rejected() {
        let mut bytes = obu(KIND_SEQUENCE_HEADER, &sequence(false, false));
        bytes.extend_from_slice(&obu(KIND_FRAME, &frame(0, false, false)));
        assert!(keyframe(&bytes, &[]).is_err());
    }

    #[test]
    fn frame_id_numbers_are_skipped_before_the_frame_type() {
        let header = obu(KIND_SEQUENCE_HEADER, &sequence(false, true));
        // delta 0 + 2 + additional 0 + 1 = 3 frame id bits.
        let key = frame(3, true, false);
        let inter = frame(3, false, false);
        let mut bytes = header.clone();
        bytes.extend_from_slice(&obu(KIND_FRAME, &key));
        assert_eq!(keyframe(&bytes, &[]), Ok(()));
        let mut bytes = header;
        bytes.extend_from_slice(&obu(KIND_FRAME, &inter));
        assert!(keyframe(&bytes, &[]).is_err());
    }

    #[test]
    fn reduced_still_picture_frames_have_no_frame_type_field() {
        let mut bytes = obu(KIND_SEQUENCE_HEADER, &sequence(true, false));
        bytes.extend_from_slice(&obu(KIND_FRAME, &[]));
        assert_eq!(keyframe(&bytes, &[]), Ok(()));
    }

    #[test]
    fn show_existing_frame_is_not_a_new_key_frame() {
        let mut bytes = obu(KIND_SEQUENCE_HEADER, &sequence(false, false));
        bytes.extend_from_slice(&obu(KIND_FRAME, &frame(0, true, true)));
        assert!(keyframe(&bytes, &[]).is_err());
    }

    #[test]
    fn a_sequence_header_from_the_output_type_is_accepted() {
        let bytes = obu(KIND_FRAME, &frame(0, true, false));
        assert_eq!(
            keyframe(&bytes, &obu(KIND_SEQUENCE_HEADER, &sequence(false, false))),
            Ok(())
        );
    }

    #[test]
    fn a_frame_without_any_sequence_header_is_not_confirmed() {
        let bytes = obu(KIND_FRAME, &frame(0, true, false));
        let error = keyframe(&bytes, &[]).unwrap_err();
        assert!(error.contains("sequence header"), "{error}");
    }

    #[test]
    fn a_truncated_frame_header_is_rejected() {
        let mut bytes = obu(KIND_SEQUENCE_HEADER, &sequence(false, true));
        // The frame OBU has no payload at all: even `show_existing_frame` is
        // missing, so the frame type cannot be read.
        bytes.extend_from_slice(&obu(KIND_FRAME, &[]));
        assert!(keyframe(&bytes, &[]).is_err());
    }

    #[test]
    fn a_forbidden_bit_is_rejected() {
        let mut bytes = obu(KIND_SEQUENCE_HEADER, &sequence(false, false));
        bytes.push(0x80);
        assert!(keyframe(&bytes, &[]).is_err());
    }
}
