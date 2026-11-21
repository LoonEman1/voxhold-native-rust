//! Where the frame pump takes its pictures.
//!
//! The production producer pulls the Windows desktop capturer; the loopback
//! tests inject a synthetic pattern and a stalled source. The engine only sees
//! [`FrameProducer`], so the capture failure policy, the drop counter and the
//! synthetic frames stay independent of the peer and statistics code.

use std::sync::Arc;

use crate::screen_capture::{
    CaptureFailure, CapturedImage, LatestImage, ScreenCapture, preview_from_i420,
};

use libwebrtc::video_frame::I420Buffer;

/// Where the pump takes its pictures. The production producer pulls the
/// Windows desktop capturer; the loopback test injects a synthetic pattern.
pub trait FrameProducer: Send {
    fn next_frame(&mut self) -> Option<CapturedImage>;
    fn failure(&self) -> Option<CaptureFailure>;
    fn dropped(&self) -> u64;
}

pub(super) struct DesktopProducer {
    pub(super) capture: ScreenCapture,
    pub(super) latest: Arc<LatestImage>,
}

impl FrameProducer for DesktopProducer {
    fn next_frame(&mut self) -> Option<CapturedImage> {
        self.latest.take()
    }

    fn failure(&self) -> Option<CaptureFailure> {
        self.capture.failure()
    }

    fn dropped(&self) -> u64 {
        self.latest.dropped()
    }
}

/// A known moving I420 pattern. It exercises the real encoder and RTP path
/// without a desktop session, which keeps the loopback test CI-safe.
pub struct SyntheticProducer {
    width: u32,
    height: u32,
    frame: u64,
    preview: bool,
}

impl SyntheticProducer {
    pub fn new(width: u32, height: u32, preview: bool) -> Self {
        Self {
            width: width.max(2) & !1,
            height: height.max(2) & !1,
            frame: 0,
            preview,
        }
    }
}

impl FrameProducer for SyntheticProducer {
    fn next_frame(&mut self) -> Option<CapturedImage> {
        let mut buffer = I420Buffer::new(self.width, self.height);
        let (stride_y, stride_u, stride_v) = buffer.strides();
        let chroma = (
            buffer.chroma_width() as usize,
            buffer.chroma_height() as usize,
        );
        let (data_y, data_u, data_v) = buffer.data_mut();
        // A vertical bar moves one pixel per frame and the background changes
        // luminance every second, so a frozen or single-colour stream is
        // visible in the encoded output.
        let bar = (self.frame % u64::from(self.width)) as u32;
        let value = if (self.frame / 30) % 2 == 0 { 220 } else { 80 };
        for y in 0..self.height as usize {
            for x in 0..self.width as usize {
                let bright = x as u32 >= bar && (x as u32) < bar + self.width / 8;
                data_y[y * stride_y as usize + x] = if bright { 235 } else { value };
            }
        }
        for y in 0..chroma.1 {
            for x in 0..chroma.0 {
                data_u[y * stride_u as usize + x] = 128;
                data_v[y * stride_v as usize + x] = 128;
            }
        }
        self.frame = self.frame.wrapping_add(1);
        let preview = if self.preview {
            preview_from_i420(&buffer, 640)
        } else {
            None
        };
        Some(CapturedImage {
            width: self.width,
            height: self.height,
            i420: buffer,
            preview,
        })
    }

    fn failure(&self) -> Option<CaptureFailure> {
        None
    }

    fn dropped(&self) -> u64 {
        0
    }
}

/// A source that never delivers a frame: the encoder has nothing to encode,
/// while ICE and DTLS can still connect. Test-only.
#[cfg(test)]
pub(super) struct StalledProducer;

#[cfg(test)]
impl FrameProducer for StalledProducer {
    fn next_frame(&mut self) -> Option<CapturedImage> {
        None
    }

    fn failure(&self) -> Option<CaptureFailure> {
        None
    }

    fn dropped(&self) -> u64 {
        0
    }
}
