//! One long-lived hardware encoder session of a publication.
//!
//! The probe of [`super`] answers "can this machine encode this pair once".
//! A live publication needs the same transform kept open and driven for its
//! whole duration, so this module owns one encoder thread with its own COM
//! apartment and Media Foundation startup. The publisher engine only submits
//! captured frames and polls encoded access units; it never touches an MFT.
//!
//! The input queue is bounded and the drop happens **before** the encoder:
//! a frame that was never encoded cannot break a reference chain, so an input
//! drop needs no recovery. A dropped **encoded** packet does: the receiver
//! lost a frame later frames refer to, so the session requests a keyframe.
//! The same rule covers a PLI/FIR from the pass-through encoder and a driver
//! error that left the bitstream incomplete.
//!
//! Codec containers produced by Media Foundation here:
//! - H.264: Annex B with SPS/PPS and IDR;
//! - AV1: low-overhead OBU temporal units with the sequence header;
//! - VP8/VP9: the complete access unit.
//!
//! Timestamps are the capture time in microseconds and strictly increase.

use std::fmt;

use libwebrtc::video_frame::I420Buffer;

use super::{AdapterInfo, Codec, EncodeQuery};
use crate::screen_capture::CapturedImage;

#[cfg(not(windows))]
mod unsupported;
#[cfg(windows)]
mod windows;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use fake::FakeGpuEncoder;
/// One captured frame in the encoder's input format.
///
/// Media Foundation encoders of the four SDR codecs take NV12. The conversion
/// from the captured I420 happens on the engine's frame thread; no user content
/// is logged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nv12Frame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    /// Capture time relative to the engine start, in microseconds.
    pub timestamp_us: i64,
}

impl Nv12Frame {
    pub fn from_captured(image: &CapturedImage, timestamp_us: i64) -> Self {
        Self {
            width: image.width,
            height: image.height,
            data: nv12_from_i420(&image.i420, image.width, image.height),
            timestamp_us,
        }
    }
}

/// Repack the captured I420 planes into the NV12 layout an MFT takes.
///
/// This is a bounded copy, not a colour conversion: the screen path already
/// delivers I420 in the target range and the chroma planes are only
/// interleaved U/V, with no BT.601 maths here.
fn nv12_from_i420(buffer: &I420Buffer, width: u32, height: u32) -> Vec<u8> {
    let (stride_y, stride_u, stride_v) = buffer.strides();
    let (chroma_width, chroma_height) = (buffer.chroma_width(), buffer.chroma_height());
    let luma = width as usize * height as usize;
    let mut data = vec![0u8; luma + 2 * chroma_width as usize * chroma_height as usize];
    let (data_y, data_u, data_v) = buffer.data();
    for row in 0..height as usize {
        let source = row * stride_y as usize;
        let target = row * width as usize;
        let count = width.min(stride_y) as usize;
        data[target..target + count].copy_from_slice(&data_y[source..source + count]);
    }
    let uv = luma;
    for row in 0..chroma_height as usize {
        let source_u = row * stride_u as usize;
        let source_v = row * stride_v as usize;
        let target = uv + row * chroma_width as usize * 2;
        for column in 0..chroma_width as usize {
            data[target + column * 2] = data_u[source_u + column];
            data[target + column * 2 + 1] = data_v[source_v + column];
        }
    }
    data
}

/// What [`GpuEncoder::submit`] did with one captured frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeSubmit {
    /// The bounded queue accepted the frame.
    Queued,
    /// The queue was full; the newest frame was dropped before the encoder.
    Dropped,
    /// The session already ended.
    Closed,
}

/// One encoded video access unit, ready for the pass-through encoder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedAccessUnit {
    pub payload: Vec<u8>,
    pub keyframe: bool,
    /// Capture timestamp in microseconds; strictly increasing per session.
    pub timestamp_us: i64,
}

/// Everything a failed session must tell the publisher. `device_lost` selects
/// the one explicit user message; the publisher still never replaces the codec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuEncoderFailure {
    pub message: String,
    pub device_lost: bool,
}

/// Measured counters of one hardware session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EncoderCounters {
    pub submitted: u64,
    /// Frames rejected by the bounded input queue before encoding.
    pub dropped_input: u64,
    /// Access units the encoder produced.
    pub encoded: u64,
    pub keyframes: u64,
    /// Encoded access units dropped because the output queue was full.
    pub dropped_output: u64,
    /// Keyframe requests the session received (PLI/FIR, driver recovery).
    pub keyframe_requests: u64,
    /// Last bitrate the driver accepted.
    pub bitrate_bps: u32,
    /// The driver confirmed the mean bitrate through its codec API.
    pub bitrate_applied: bool,
}

impl EncoderCounters {
    /// `true` only when frames really left the hardware encoder: the
    /// confirmation the UI and the diagnostics use.
    pub fn delivered(&self) -> bool {
        self.encoded > self.dropped_output
    }
}

/// Parse the vendor part of `MFT_ENUM_HARDWARE_VENDOR_ID_Attribute`.
///
/// NVIDIA reports `"VEN_10DE"` on the transform's attribute store. The value
/// names the hardware vendor, not the adapter, so it is a cross-check that can
/// refuse a transform from another vendor but cannot prove which of several
/// adapters of the same vendor was opened.
pub(crate) fn hardware_vendor(text: &str) -> Option<u32> {
    let rest = text.trim().strip_prefix("VEN_")?;
    u32::from_str_radix(rest.trim_end_matches('\0'), 16)
        .ok()
        .filter(|vendor| *vendor != 0)
}

/// What the open session itself reported about its adapter.
///
/// Media Foundation does not promise that an activation object exposes the
/// adapter it was enumerated for. The session always reads what the open MFT
/// has: the adapter LUID where a driver provides it and the hardware vendor id
/// otherwise. When neither distinguishes the adapter, the session can only
/// repeat the adapter it was asked to open, and that value must not be
/// presented as proof of the open transform.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AdapterEvidence {
    /// The adapter LUID was read from the open MFT (or its activation) and
    /// matched the requested plan.
    MftAttribute,
    /// The driver exposes no adapter LUID; the vendor id was checked where it
    /// exists, but the adapter itself is the plan echoed back by this session,
    /// not evidence about the open transform.
    #[default]
    PlanEcho,
}

/// The live encoder of one attempt. The engine owns exactly one.
pub trait GpuEncoder: Send {
    /// Enqueue a captured frame without blocking.
    fn submit(&mut self, frame: Nv12Frame) -> EncodeSubmit;

    /// The hardware session reserves input space before allocating its NV12
    /// copy. Other encoders can retain the ordinary `submit` implementation.
    fn submit_captured(&mut self, frame: &CapturedImage, timestamp_us: i64) -> EncodeSubmit {
        self.submit(Nv12Frame::from_captured(frame, timestamp_us))
    }

    /// Take the oldest encoded access unit, if any.
    fn next_packet(&mut self) -> Option<EncodedAccessUnit>;

    /// The pass-through encoder asked for a keyframe (PLI/FIR or an internal
    /// reconfiguration).
    fn request_keyframe(&mut self);

    /// A new driver-side rate-control target.
    fn set_bitrate(&mut self, target_bps: u32);

    fn counters(&self) -> EncoderCounters;

    /// The adapter the open session really uses. The engine compares it with
    /// the attempt's plan before the GUI may call the stream «GPU».
    fn adapter(&self) -> Option<AdapterInfo>;

    /// Where [`Self::adapter`] came from. The default is the honest answer for
    /// a double: it repeats the plan and proves nothing about a driver.
    fn adapter_evidence(&self) -> AdapterEvidence {
        AdapterEvidence::PlanEcho
    }

    /// The terminal failure of this session, if it already ended.
    fn failure(&self) -> Option<GpuEncoderFailure>;

    /// Stop the encoder thread and release its COM/MFT objects.
    fn shutdown(&mut self);
}

/// Open one hardware session on `adapter` for `codec`. The call returns after
/// the worker activated the transform and configured its types, so an
/// activation failure is reported before any publication is created.
pub fn open_gpu_encoder(
    adapter: &AdapterInfo,
    codec: Codec,
    query: EncodeQuery,
    bitrate_bps: u32,
) -> Result<Box<dyn GpuEncoder>, String> {
    #[cfg(windows)]
    {
        windows::HardwareEncoder::open(adapter, codec, query, bitrate_bps)
            .map(|encoder| Box::new(encoder) as Box<dyn GpuEncoder>)
    }
    #[cfg(not(windows))]
    {
        let _ = (adapter, codec, query, bitrate_bps);
        Err(unsupported::MESSAGE.to_owned())
    }
}

impl fmt::Display for GpuEncoderFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[cfg(test)]
mod conversion_tests {
    use super::*;
    use libwebrtc::video_frame::I420Buffer;

    #[test]
    fn i420_converts_to_a_packed_nv12_frame() {
        let mut buffer = I420Buffer::new(4, 2);
        let chroma = (
            buffer.chroma_width() as usize,
            buffer.chroma_height() as usize,
        );
        let (stride_y, stride_u, stride_v) = buffer.strides();
        {
            let (y, u, v) = buffer.data_mut();
            for row in 0..2 {
                for column in 0..4 {
                    y[row * stride_y as usize + column] = (row * 4 + column) as u8;
                }
            }
            for row in 0..chroma.1 {
                for column in 0..chroma.0 {
                    u[row * stride_u as usize + column] = 10 + column as u8;
                    v[row * stride_v as usize + column] = 20 + column as u8;
                }
            }
        }
        let data = nv12_from_i420(&buffer, 4, 2);
        assert_eq!(data.len(), 4 * 2 + 2 * 2);
        assert_eq!(&data[..8], &[0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(&data[8..], &[10, 20, 11, 21]);
    }
}
