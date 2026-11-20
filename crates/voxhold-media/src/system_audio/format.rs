//! Device mix format → 48 kHz interleaved i16 stereo.
//!
//! WASAPI shared mode requires the endpoint mix format, which is commonly
//! 48 kHz float32 stereo but may be 44.1 kHz, 24-bit or 5.1. The converter
//! down-mixes every channel into stereo, resamples linearly with a carried
//! fractional position and normalizes to i16. Mono is duplicated. Center and
//! surround channels fold into both outputs at −3 dB (0.7071); LFE is
//! omitted because it is an effects channel, not dialog, and folding it into
//! small speakers only eats headroom. The sum is clamped before i16. It is
//! pure and covered without a device.

use super::{CHANNELS, SAMPLE_RATE};

/// `WAVEFORMATEXTENSIBLE` speaker bits. The mix format orders channels by
/// ascending bit, so the bit tells us what a channel really is.
const SPEAKER_FRONT_LEFT: u32 = 0x1;
const SPEAKER_FRONT_RIGHT: u32 = 0x2;
const SPEAKER_FRONT_CENTER: u32 = 0x4;
const SPEAKER_LOW_FREQUENCY: u32 = 0x8;
const SPEAKER_BACK_LEFT: u32 = 0x10;
const SPEAKER_BACK_RIGHT: u32 = 0x20;
const SPEAKER_FRONT_LEFT_OF_CENTER: u32 = 0x40;
const SPEAKER_FRONT_RIGHT_OF_CENTER: u32 = 0x80;
const SPEAKER_BACK_CENTER: u32 = 0x100;
const SPEAKER_SIDE_LEFT: u32 = 0x200;
const SPEAKER_SIDE_RIGHT: u32 = 0x400;
const SPEAKER_TOP_CENTER: u32 = 0x800;
const SPEAKER_TOP_FRONT_LEFT: u32 = 0x1000;
const SPEAKER_TOP_FRONT_CENTER: u32 = 0x2000;
const SPEAKER_TOP_FRONT_RIGHT: u32 = 0x4000;
const SPEAKER_TOP_BACK_LEFT: u32 = 0x8000;
const SPEAKER_TOP_BACK_CENTER: u32 = 0x1_0000;
const SPEAKER_TOP_BACK_RIGHT: u32 = 0x2_0000;

/// One decoded mix format. `block_align` is the bytes of one interleaved
/// frame, exactly as WAVEFORMATEX reports it. `mask` is the
/// `WAVEFORMATEXTENSIBLE` speaker mask, or 0 when the format is plain
/// WAVEFORMATEX and the layout has to be inferred from the channel count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SourceFormat {
    pub(super) sample_rate: u32,
    pub(super) channels: u16,
    pub(super) bits: u16,
    pub(super) block_align: u16,
    pub(super) float: bool,
    pub(super) mask: u32,
}

impl SourceFormat {
    pub(super) fn frame_bytes(&self) -> usize {
        self.block_align as usize
    }

    pub(super) fn describe(&self) -> String {
        let kind = if self.float { "float" } else { "PCM" };
        format!(
            "{} Гц · {} канал(а) · {} бит {kind}",
            self.sample_rate, self.channels, self.bits
        )
    }
}

/// Stateful streaming resampler. It keeps the fractional position and the last
/// frame across WASAPI packets, so an output sample that interpolates across a
/// packet boundary waits for the next packet instead of restarting the phase.
pub(super) struct Converter {
    format: SourceFormat,
    step: f64,
    /// Absolute position of the next output sample, in input frames. It only
    /// grows by `step`, so splitting the same input into packets produces the
    /// same sequence of positions as one conversion.
    time: f64,
    /// Absolute number of input frames already handed to the converter.
    frames: u64,
    previous: Option<[f32; 2]>,
}

impl Converter {
    pub(super) fn new(format: SourceFormat) -> Self {
        let step = if format.sample_rate == 0 {
            1.0
        } else {
            f64::from(format.sample_rate) / f64::from(SAMPLE_RATE)
        };
        Self {
            format,
            step,
            time: 0.0,
            frames: 0,
            previous: None,
        }
    }

    /// Convert one packet. `silent` mirrors `AUDCLNT_BUFFERFLAGS_SILENT`: the
    /// data pointer may then be meaningless, so only the frame count matters.
    pub(super) fn convert(&mut self, bytes: &[u8], silent: bool, out: &mut Vec<i16>) {
        let frame_bytes = self.format.frame_bytes();
        if frame_bytes == 0 {
            return;
        }
        let frames = bytes.len() / frame_bytes;
        if frames == 0 {
            return;
        }
        if silent {
            // The payload of a silent packet is undefined; the real frame count
            // still advances the same time axis as a normal packet.
            self.resample(frames, |_| [0.0, 0.0], out);
            return;
        }
        let format = self.format;
        self.resample(
            frames,
            |frame| {
                decode(
                    &format,
                    &bytes[frame * frame_bytes..(frame + 1) * frame_bytes],
                )
            },
            out,
        );
    }

    /// Advance through a silent packet that has no usable payload at all.
    pub(super) fn convert_silence(&mut self, frames: usize, out: &mut Vec<i16>) {
        if frames == 0 {
            return;
        }
        self.resample(frames, |_| [0.0, 0.0], out);
    }

    /// One packet of the streaming resampler. Output sample `k` sits at
    /// `time` on the absolute input frame axis and interpolates between the two
    /// surrounding frames. If the right frame is the first frame of the next
    /// packet, the sample is left in `time` and finished there; that keeps the
    /// phase continuous across packet boundaries. Frame −1 is the carried last
    /// frame of the previous packet.
    fn resample(
        &mut self,
        packet_frames: usize,
        mut frame: impl FnMut(usize) -> [f32; 2],
        out: &mut Vec<i16>,
    ) {
        let base = self.frames;
        let end = base + packet_frames as u64;
        let last_local = (packet_frames - 1) as i64;
        let mut time = self.time;
        while time < end as f64 {
            let index = time.floor();
            let local = index as i64 - base as i64;
            let fraction = (time - index) as f32;
            if fraction > 0.0 && local + 1 > last_local {
                // The right endpoint belongs to the next packet.
                break;
            }
            let first = if local < 0 {
                // Frame −1 is the last frame of the previous packet.
                self.previous.unwrap_or_else(|| frame(0))
            } else {
                frame((local as usize).min(packet_frames - 1))
            };
            let second = if fraction == 0.0 || local + 1 < 0 {
                // An integral position only needs the left frame. This is the
                // 48 kHz case, which stays a direct sample-for-sample copy.
                first
            } else {
                frame((local + 1) as usize)
            };
            out.push(sample(first[0] + (second[0] - first[0]) * fraction));
            out.push(sample(first[1] + (second[1] - first[1]) * fraction));
            time += self.step;
        }
        self.time = time;
        self.frames = end;
        self.previous = Some(frame(packet_frames - 1));
    }

    /// Finish the stream. Only samples whose time is still inside the input are
    /// emitted; for them a single whole-input conversion holds the final frame,
    /// so the streamed result matches it exactly.
    pub(super) fn flush(&mut self, out: &mut Vec<i16>) {
        let Some(last) = self.previous else {
            return;
        };
        while self.time < self.frames as f64 {
            out.push(sample(last[0]));
            out.push(sample(last[1]));
            self.time += self.step;
        }
    }
}

/// Down-mix one source frame into stereo. Mono is duplicated; everything else
/// is folded by [`weight`] and clamped so the i16 conversion cannot wrap.
fn decode(format: &SourceFormat, frame: &[u8]) -> [f32; 2] {
    if format.channels == 1 {
        let mono = component(format, frame, 0);
        return [mono, mono];
    }
    let mut left = 0.0_f32;
    let mut right = 0.0_f32;
    for channel in 0..format.channels as usize {
        let value = component(format, frame, channel);
        let (to_left, to_right) = weight(format, channel);
        left += value * to_left;
        right += value * to_right;
    }
    // A full-scale center plus both fronts can exceed 1.0; clamp here so
    // the later i16 conversion cannot wrap.
    [left.clamp(-1.0, 1.0), right.clamp(-1.0, 1.0)]
}

/// Stereo contribution of one source channel. The −3 dB fold-down
/// (0.7071) keeps the summed power of a center or surround channel close
/// to the original instead of doubling it.
fn weight(format: &SourceFormat, channel: usize) -> (f32, f32) {
    let bit = if format.mask == 0 {
        fallback_speaker(format.channels, channel)
    } else {
        speaker_bit(format.mask, channel)
    };
    match bit {
        SPEAKER_FRONT_LEFT | SPEAKER_FRONT_LEFT_OF_CENTER => (1.0, 0.0),
        SPEAKER_FRONT_RIGHT | SPEAKER_FRONT_RIGHT_OF_CENTER => (0.0, 1.0),
        SPEAKER_FRONT_CENTER => (0.707_106_77, 0.707_106_77),
        // LFE is deliberately absent: it carries effects, not dialog,
        // and mixing it into stereo mostly steals headroom.
        SPEAKER_LOW_FREQUENCY => (0.0, 0.0),
        SPEAKER_BACK_LEFT | SPEAKER_SIDE_LEFT | SPEAKER_TOP_FRONT_LEFT | SPEAKER_TOP_BACK_LEFT => {
            (0.707_106_77, 0.0)
        }
        SPEAKER_BACK_RIGHT
        | SPEAKER_SIDE_RIGHT
        | SPEAKER_TOP_FRONT_RIGHT
        | SPEAKER_TOP_BACK_RIGHT => (0.0, 0.707_106_77),
        SPEAKER_BACK_CENTER => (0.5, 0.5),
        SPEAKER_TOP_CENTER => (0.5, 0.5),
        SPEAKER_TOP_FRONT_CENTER | SPEAKER_TOP_BACK_CENTER => (0.353_553_38, 0.353_553_38),
        // An unknown speaker or a channel beyond the mask contributes
        // nothing; the alternative is guessing and misplacing audio.
        _ => (0.0, 0.0),
    }
}

/// One channel of one frame, without a per-frame allocation.
fn component(format: &SourceFormat, frame: &[u8], channel: usize) -> f32 {
    let bytes = (format.bits as usize / 8).max(1);
    let start = channel * bytes;
    let Some(slice) = frame.get(start..start + bytes) else {
        return 0.0;
    };
    match format.bits {
        16 => f32::from(i16::from_le_bytes([slice[0], slice[1]])) / 32_768.0,
        24 => {
            // 24-bit PCM is packed in three little-endian bytes. Bit 23
            // is the sign, so the raw value needs explicit sign extension.
            let raw =
                i32::from(slice[0]) | (i32::from(slice[1]) << 8) | (i32::from(slice[2]) << 16);
            let value = if raw & 0x80_0000 != 0 {
                raw - 0x100_0000
            } else {
                raw
            };
            value as f32 / 8_388_608.0
        }
        32 if format.float => {
            f32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]).clamp(-1.0, 1.0)
        }
        32 => {
            let value = i32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]);
            value as f32 / 2_147_483_648.0
        }
        _ => 0.0,
    }
}

/// Positional layout for plain WAVEFORMATEX (mask 0). Windows devices with
/// more than two channels normally publish a `WAVEFORMATEXTENSIBLE` mask;
/// this is the documented fallback order for the common layouts.
fn fallback_speaker(channels: u16, channel: usize) -> u32 {
    const QUAD: [u32; 4] = [
        SPEAKER_FRONT_LEFT,
        SPEAKER_FRONT_RIGHT,
        SPEAKER_BACK_LEFT,
        SPEAKER_BACK_RIGHT,
    ];
    const FIVE: [u32; 5] = [
        SPEAKER_FRONT_LEFT,
        SPEAKER_FRONT_RIGHT,
        SPEAKER_FRONT_CENTER,
        SPEAKER_BACK_LEFT,
        SPEAKER_BACK_RIGHT,
    ];
    const FIVE_ONE: [u32; 6] = [
        SPEAKER_FRONT_LEFT,
        SPEAKER_FRONT_RIGHT,
        SPEAKER_FRONT_CENTER,
        SPEAKER_LOW_FREQUENCY,
        SPEAKER_BACK_LEFT,
        SPEAKER_BACK_RIGHT,
    ];
    const SIX_ONE: [u32; 7] = [
        SPEAKER_FRONT_LEFT,
        SPEAKER_FRONT_RIGHT,
        SPEAKER_FRONT_CENTER,
        SPEAKER_LOW_FREQUENCY,
        SPEAKER_BACK_CENTER,
        SPEAKER_SIDE_LEFT,
        SPEAKER_SIDE_RIGHT,
    ];
    const SEVEN_ONE: [u32; 8] = [
        SPEAKER_FRONT_LEFT,
        SPEAKER_FRONT_RIGHT,
        SPEAKER_FRONT_CENTER,
        SPEAKER_LOW_FREQUENCY,
        SPEAKER_BACK_LEFT,
        SPEAKER_BACK_RIGHT,
        SPEAKER_SIDE_LEFT,
        SPEAKER_SIDE_RIGHT,
    ];
    let layout: &[u32] = match channels {
        0 | 1 => return 0,
        2 => &QUAD[..2],
        3 => &FIVE[..3],
        4 => &QUAD,
        5 => &FIVE,
        6 => &FIVE_ONE,
        7 => &SIX_ONE,
        _ => &SEVEN_ONE,
    };
    layout.get(channel).copied().unwrap_or(0)
}

/// The `n`-th set bit of the speaker mask, i.e. the speaker of channel `n`.
fn speaker_bit(mask: u32, channel: usize) -> u32 {
    let mut seen = 0;
    for bit in 0..32 {
        let candidate = 1_u32 << bit;
        if mask & candidate != 0 {
            if seen == channel {
                return candidate;
            }
            seen += 1;
        }
    }
    0
}

fn sample(value: f32) -> i16 {
    (value.clamp(-1.0, 1.0) * 32_767.0).round() as i16
}

/// Stereo output is always two channels.
const _: () = assert!(CHANNELS == 2);

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm16_format(rate: u32, channels: u16) -> SourceFormat {
        SourceFormat {
            sample_rate: rate,
            channels,
            bits: 16,
            block_align: channels * 2,
            float: false,
            mask: 0,
        }
    }

    fn masked_pcm16_format(channels: u16, mask: u32) -> SourceFormat {
        SourceFormat {
            mask,
            ..pcm16_format(SAMPLE_RATE, channels)
        }
    }

    fn float_format(rate: u32, channels: u16) -> SourceFormat {
        SourceFormat {
            sample_rate: rate,
            channels,
            bits: 32,
            block_align: channels * 4,
            float: true,
            mask: 0,
        }
    }

    fn pcm16_mono(samples: &[i16]) -> Vec<u8> {
        samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect()
    }

    fn pcm16_frame(samples: &[i16]) -> Vec<u8> {
        samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect()
    }

    #[test]
    fn float32_stereo_is_converted_to_i16_stereo() {
        let mut converter = Converter::new(float_format(SAMPLE_RATE, 2));
        let mut bytes = Vec::new();
        for _ in 0..480 {
            bytes.extend_from_slice(&0.5_f32.to_le_bytes());
            bytes.extend_from_slice(&(-0.25_f32).to_le_bytes());
        }
        let mut out = Vec::new();
        converter.convert(&bytes, false, &mut out);
        assert_eq!(out.len(), 960);
        assert!((out[0] as i32 - 16_384).abs() <= 1);
        assert!((out[1] as i32 + 8192).abs() <= 1);
    }

    #[test]
    fn a_44100_hz_tone_keeps_its_frequency_and_length() {
        let rate = 44_100;
        let mut converter = Converter::new(pcm16_format(rate, 1));
        let frames = rate as usize / 100;
        let mut left = Vec::with_capacity(frames);
        for index in 0..frames {
            let value = (2.0 * std::f32::consts::PI * 400.0 * index as f32 / rate as f32).sin();
            left.push((value * 12_000.0) as i16);
        }
        let mut out = Vec::new();
        converter.convert(&pcm16_mono(&left), false, &mut out);
        converter.flush(&mut out);
        assert!(
            (480..=481).contains(&(out.len() / 2)),
            "one 10 ms output block at 48 kHz, got {}",
            out.len() / 2
        );
        let energy_400 = goertzel(&out, 400.0);
        let energy_1000 = goertzel(&out, 1000.0);
        assert!(
            energy_400 > energy_1000 * 4.0,
            "the resampler moved the tone: {energy_400} vs {energy_1000}"
        );
    }

    #[test]
    fn mono_is_duplicated_and_a_quad_layout_folds_into_stereo() {
        let mut mono = Converter::new(pcm16_format(SAMPLE_RATE, 1));
        let mut out = Vec::new();
        mono.convert(&1_000_i16.to_le_bytes(), false, &mut out);
        assert_eq!(out, vec![1_000, 1_000]);

        // Quad is FL, FR, BL, BR. The back pair folds into the matching front
        // channel at −3 dB instead of being discarded.
        let mut quad = Converter::new(masked_pcm16_format(4, 0x33));
        out.clear();
        quad.convert(&pcm16_frame(&[1_000, 2_000, 3_000, 4_000]), false, &mut out);
        assert_eq!(out.len(), 2);
        assert!((i32::from(out[0]) - 3_121).abs() <= 1, "L={}", out[0]);
        assert!((i32::from(out[1]) - 4_828).abs() <= 1, "R={}", out[1]);
        assert!(
            out[0] != 1_000 && out[1] != 2_000,
            "the surround pair must be mixed, not dropped"
        );
    }

    #[test]
    fn a_center_only_signal_reaches_both_stereo_channels() {
        // 5.1 with only the center channel active. −3 dB into L and R.
        let mut converter = Converter::new(masked_pcm16_format(6, 0x3F));
        let mut out = Vec::new();
        converter.convert(&pcm16_frame(&[0, 0, 16_000, 0, 0, 0]), false, &mut out);
        assert_eq!(out.len(), 2);
        let expected = (16_000.0_f32 * 0.707_106_77).round() as i32;
        assert!(
            (i32::from(out[0]) - expected).abs() <= 1 && (i32::from(out[1]) - expected).abs() <= 1,
            "center must reach L and R at −3 dB: {out:?}, expected {expected}"
        );
        assert!(out[0] > 0 && out[1] > 0);
    }

    #[test]
    fn a_5_1_and_7_1_layout_fold_every_position_into_stereo() {
        let surround = 0.707_106_77_f32;
        // 5.1: FL FR FC LFE BL BR.
        let mut five_one = Converter::new(masked_pcm16_format(6, 0x3F));
        let mut out = Vec::new();
        five_one.convert(
            &pcm16_frame(&[1_000, 2_000, 3_000, 4_000, 5_000, 6_000]),
            false,
            &mut out,
        );
        let left = 1_000.0 + 3_000.0 * surround + 5_000.0 * surround;
        let right = 2_000.0 + 3_000.0 * surround + 6_000.0 * surround;
        assert!((f32::from(out[0]) - left).abs() <= 2.0, "L={}", out[0]);
        assert!((f32::from(out[1]) - right).abs() <= 2.0, "R={}", out[1]);
        // LFE alone contributes nothing. A fresh converter avoids the
        // one-frame interpolation carry from the previous packet.
        let mut lfe_only = Converter::new(masked_pcm16_format(6, 0x3F));
        out.clear();
        lfe_only.convert(&pcm16_frame(&[0, 0, 0, 32_767, 0, 0]), false, &mut out);
        assert_eq!(out, vec![0, 0], "LFE is omitted from the stereo fold-down");

        // 7.1: FL FR FC LFE BL BR SL SR.
        let mut seven_one = Converter::new(masked_pcm16_format(8, 0x63F));
        out.clear();
        seven_one.convert(
            &pcm16_frame(&[1_000, 2_000, 3_000, 4_000, 5_000, 6_000, 7_000, 8_000]),
            false,
            &mut out,
        );
        let left = 1_000.0 + 3_000.0 * surround + 5_000.0 * surround + 7_000.0 * surround;
        let right = 2_000.0 + 3_000.0 * surround + 6_000.0 * surround + 8_000.0 * surround;
        assert!((f32::from(out[0]) - left).abs() <= 2.0, "L={}", out[0]);
        assert!((f32::from(out[1]) - right).abs() <= 2.0, "R={}", out[1]);
    }

    #[test]
    fn a_hot_downmix_is_clamped_instead_of_wrapping() {
        let mut converter = Converter::new(masked_pcm16_format(6, 0x3F));
        let mut out = Vec::new();
        converter.convert(
            &pcm16_frame(&[32_767, 32_767, 32_767, 32_767, 32_767, 32_767]),
            false,
            &mut out,
        );
        assert_eq!(out, vec![32_767, 32_767]);
        // A fresh converter for the negative packet: the first frame of a
        // packet interpolates from the carried previous frame.
        let mut negative = Converter::new(masked_pcm16_format(6, 0x3F));
        out.clear();
        negative.convert(
            &pcm16_frame(&[-32_768, -32_768, -32_768, -32_768, -32_768, -32_768]),
            false,
            &mut out,
        );
        assert_eq!(out, vec![-32_767, -32_767]);
    }

    #[test]
    fn the_silent_flag_produces_zeros_and_keeps_the_timeline() {
        let mut converter = Converter::new(pcm16_format(SAMPLE_RATE, 2));
        let mut out = Vec::new();
        converter.convert(&vec![0_u8; 480 * 4], true, &mut out);
        assert_eq!(out.len(), 960);
        assert!(out.iter().all(|value| *value == 0));
        // A following real packet starts at the next position, not at zero.
        let mut packet = Vec::new();
        for _ in 0..480 {
            packet.extend_from_slice(&2_000_i16.to_le_bytes());
            packet.extend_from_slice(&2_000_i16.to_le_bytes());
        }
        out.clear();
        converter.convert(&packet, false, &mut out);
        assert_eq!(out.len(), 960);
        // The first interpolated sample is the carried zero boundary; the
        // body of the packet is the real signal.
        assert_eq!(out[2], 2_000);
    }

    #[test]
    fn packed_24_bit_is_scaled() {
        let mut converter = Converter::new(SourceFormat {
            sample_rate: SAMPLE_RATE,
            channels: 1,
            bits: 24,
            block_align: 3,
            float: false,
            mask: 0,
        });
        let value = 0x40_0000_i32; // 0.5 full scale
        let bytes = value.to_le_bytes();
        let mut out = Vec::new();
        converter.convert(&bytes[..3], false, &mut out);
        assert!((out[0] as i32 - 16_384).abs() <= 1);
    }

    /// Convert mono input split into packets of the given sizes, flushing the
    /// end of the stream like a real capture does on shutdown.
    fn streamed(format: SourceFormat, input: &[i16], chunks: &[usize]) -> Vec<i16> {
        let mut converter = Converter::new(format);
        let mut out = Vec::new();
        let mut offset = 0;
        let mut chunk = 0;
        while offset < input.len() {
            let size = chunks[chunk % chunks.len()]
                .max(1)
                .min(input.len() - offset);
            converter.convert(&pcm16_mono(&input[offset..offset + size]), false, &mut out);
            offset += size;
            chunk += 1;
        }
        converter.flush(&mut out);
        out
    }

    fn whole(format: SourceFormat, input: &[i16]) -> Vec<i16> {
        streamed(format, input, &[input.len().max(1)])
    }

    #[test]
    fn packet_boundaries_match_one_whole_conversion() {
        let input: Vec<i16> = (0..2_001)
            .map(|index| ((index * 337) % 4_001) as i16 - 2_000)
            .collect();
        for rate in [SAMPLE_RATE, 44_100] {
            let format = pcm16_format(rate, 1);
            let reference = whole(format, &input);
            for chunks in [
                &[1_usize][..],
                &[7, 64, 333, 5][..],
                &[480, 17][..],
                &[input.len()][..],
            ] {
                assert_eq!(
                    streamed(format, &input, chunks),
                    reference,
                    "{rate} Hz differs when split into {chunks:?}"
                );
            }
        }
    }

    #[test]
    fn an_impulse_on_a_packet_boundary_interpolates_across_packets() {
        let rate = 44_100;
        let boundary = 441; // 10 ms at 44.1 kHz
        let mut input = vec![0_i16; boundary * 2];
        input[boundary] = 16_000;
        let format = pcm16_format(rate, 1);
        let reference = whole(format, &input);
        let split = streamed(format, &input, &[boundary, boundary]);
        assert_eq!(split, reference, "a boundary impulse must not be smeared");

        let step = f64::from(rate) / f64::from(SAMPLE_RATE);
        let output_index = (f64::from(boundary as u32) / step).round() as usize;
        let window = &split[(output_index - 2) * 2..(output_index + 3) * 2];
        assert!(
            window.iter().any(|sample| *sample != 0),
            "the impulse on the packet boundary was lost"
        );
        assert!(
            split.chunks(2).any(|pair| pair[0] > 0 && pair[0] < 16_000),
            "no sample interpolates between the silent packet and the impulse"
        );
    }

    #[test]
    fn a_silent_packet_keeps_the_time_axis_for_the_next_signal() {
        for rate in [SAMPLE_RATE, 44_100] {
            let boundary = if rate == SAMPLE_RATE { 480 } else { 441 };
            let format = pcm16_format(rate, 1);
            let signal: Vec<i16> = (0..boundary)
                .map(|index| ((index % 200) as i16 + 1) * 40)
                .collect();
            let mut input = vec![0_i16; boundary];
            input.extend_from_slice(&signal);
            let reference = whole(format, &input);

            let mut converter = Converter::new(format);
            let mut out = Vec::new();
            converter.convert_silence(boundary, &mut out);
            converter.convert(&pcm16_mono(&signal), false, &mut out);
            converter.flush(&mut out);

            assert_eq!(
                out, reference,
                "{rate} Hz silence → signal differs from one whole conversion"
            );
            assert!(
                out.iter().any(|sample| *sample != 0),
                "the signal after the silent packet must reach the output"
            );
            if rate != SAMPLE_RATE {
                assert!(
                    out.chunks(2).any(|pair| pair[0] > 0 && pair[0] < signal[0]),
                    "the transition must interpolate out of silence"
                );
            }
        }
    }

    #[test]
    fn end_of_stream_flushes_only_samples_inside_the_input() {
        let frames = 100_usize;
        let format = pcm16_format(44_100, 1);
        let input: Vec<i16> = (0..frames).map(|index| index as i16).collect();
        let step = 44_100.0 / 48_000.0;
        let expected = {
            let mut count = 0;
            while (count as f64) * step < frames as f64 {
                count += 1;
            }
            count
        };

        let mut converter = Converter::new(format);
        let mut out = Vec::new();
        converter.convert(&pcm16_mono(&input), false, &mut out);
        assert!(
            out.len() / 2 < expected,
            "the sample that needs the next packet must stay deferred"
        );
        converter.flush(&mut out);
        assert_eq!(out.len() / 2, expected);
        assert_eq!(
            &out[out.len() - 2..],
            &[frames as i16 - 1, frames as i16 - 1],
            "the flush holds the final frame like a whole-input conversion"
        );
        let before = out.len();
        converter.flush(&mut out);
        assert_eq!(out.len(), before, "a second flush cannot invent samples");

        // 48 kHz has no deferred sample: every input frame is already exact.
        let mut exact = Converter::new(pcm16_format(SAMPLE_RATE, 1));
        let mut out = Vec::new();
        exact.convert(&pcm16_mono(&[1, 2, 3]), false, &mut out);
        exact.flush(&mut out);
        assert_eq!(out, vec![1, 1, 2, 2, 3, 3]);
    }

    fn goertzel(samples: &[i16], frequency: f32) -> f32 {
        let rate = SAMPLE_RATE as f32;
        let omega = 2.0 * std::f32::consts::PI * frequency / rate;
        let coefficient = 2.0 * omega.cos();
        let (mut s1, mut s2) = (0.0_f32, 0.0_f32);
        for sample in samples.iter().step_by(2) {
            let value = f32::from(*sample) / 32_768.0;
            let s0 = value + coefficient * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        s1 * s1 + s2 * s2 - coefficient * s1 * s2
    }
}
