//! Device mix format → 48 kHz interleaved i16 stereo.
//!
//! WASAPI shared mode requires the endpoint mix format, which is commonly
//! 48 kHz float32 stereo but may be 44.1 kHz, 24-bit or 5.1. The converter
//! down-mixes to stereo (first two channels; mono is duplicated), resamples
//! linearly with a carried fractional position and normalizes to i16. It is
//! pure and covered without a device.

use super::{CHANNELS, SAMPLE_RATE};

/// One decoded mix format. `block_align` is the bytes of one interleaved
/// frame, exactly as WAVEFORMATEX reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SourceFormat {
    pub(super) sample_rate: u32,
    pub(super) channels: u16,
    pub(super) bits: u16,
    pub(super) block_align: u16,
    pub(super) float: bool,
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

/// Stateful because resampling must keep the fractional position and the last
/// frame across WASAPI packets.
pub(super) struct Converter {
    format: SourceFormat,
    step: f64,
    position: f64,
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
            position: 0.0,
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
            let produced = self.advance(frames);
            out.extend(std::iter::repeat_n(0, produced * 2));
            self.previous = Some([0.0, 0.0]);
            return;
        }
        let mut decoded = Vec::with_capacity(frames);
        for frame in 0..frames {
            let base = frame * frame_bytes;
            decoded.push(self.decode(&bytes[base..base + frame_bytes]));
        }
        let mut position = self.position;
        while position < frames as f64 {
            let index = position.floor() as usize;
            let fraction = (position - index as f64) as f32;
            let first = if index == 0 {
                self.previous.unwrap_or(decoded[0])
            } else {
                decoded[index]
            };
            let second = decoded.get(index + 1).copied().unwrap_or(first);
            out.push(sample(first[0] + (second[0] - first[0]) * fraction));
            out.push(sample(first[1] + (second[1] - first[1]) * fraction));
            position += self.step;
        }
        self.position = position - frames as f64;
        if self.position < 0.0 {
            self.position = 0.0;
        }
        self.previous = decoded.last().copied();
    }

    fn advance(&mut self, frames: usize) -> usize {
        let mut position = self.position;
        let mut produced = 0;
        while position < frames as f64 {
            produced += 1;
            position += self.step;
        }
        self.position = (position - frames as f64).max(0.0);
        produced
    }

    fn decode(&self, frame: &[u8]) -> [f32; 2] {
        let left = self.component(frame, 0);
        let right = if self.format.channels >= 2 {
            self.component(frame, 1)
        } else {
            left
        };
        [left, right]
    }

    /// One channel of one frame, without a per-frame allocation.
    fn component(&self, frame: &[u8], channel: usize) -> f32 {
        let bytes = (self.format.bits as usize / 8).max(1);
        let start = channel * bytes;
        let Some(slice) = frame.get(start..start + bytes) else {
            return 0.0;
        };
        match self.format.bits {
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
            32 if self.format.float => {
                f32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]).clamp(-1.0, 1.0)
            }
            32 => {
                let value = i32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]);
                value as f32 / 2_147_483_648.0
            }
            _ => 0.0,
        }
    }
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
        }
    }

    fn float_format(rate: u32, channels: u16) -> SourceFormat {
        SourceFormat {
            sample_rate: rate,
            channels,
            bits: 32,
            block_align: channels * 4,
            float: true,
        }
    }

    fn pcm16_mono(samples: &[i16]) -> Vec<u8> {
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
    fn mono_is_duplicated_and_multichannel_keeps_the_first_two() {
        let mut mono = Converter::new(pcm16_format(SAMPLE_RATE, 1));
        let mut out = Vec::new();
        mono.convert(&1_000_i16.to_le_bytes(), false, &mut out);
        assert_eq!(out, vec![1_000, 1_000]);

        // Four channels: FL=1000, FR=2000, C=3000, LFE=4000.
        let mut quad = Converter::new(pcm16_format(SAMPLE_RATE, 4));
        let mut frame = Vec::new();
        for value in [1_000_i16, 2_000, 3_000, 4_000] {
            frame.extend_from_slice(&value.to_le_bytes());
        }
        out.clear();
        quad.convert(&frame, false, &mut out);
        assert_eq!(out, vec![1_000, 2_000]);
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
        });
        let value = 0x40_0000_i32; // 0.5 full scale
        let bytes = value.to_le_bytes();
        let mut out = Vec::new();
        converter.convert(&bytes[..3], false, &mut out);
        assert!((out[0] as i32 - 16_384).abs() <= 1);
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
