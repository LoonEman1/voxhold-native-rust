use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use libwebrtc::{
    native::yuv_helper::i420_to_abgr,
    video_frame::{BoxVideoFrame, I420Buffer, VideoBuffer},
    video_stream::native::NativeVideoStream,
    video_track::RtcVideoTrack,
};
use tokio::sync::watch;
use tokio_stream::StreamExt;

use crate::{Event, Events};

const MAX_SIDE: u32 = 4096;
const MAX_PIXELS: u64 = 4096 * 2304;

#[derive(Default)]
pub struct VideoRead {
    sequence: u64,
    pub(crate) viewport_width: u32,
    pub(crate) viewport_height: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

struct Frame {
    sequence: u64,
    video: BoxVideoFrame,
}

#[derive(Default)]
pub struct VideoStore {
    next_sequence: AtomicU64,
    latest: Mutex<Option<Arc<Frame>>>,
    scratch: Mutex<Option<I420Buffer>>,
}

impl VideoStore {
    fn publish(&self, video: BoxVideoFrame) -> bool {
        let w = video.buffer.width();
        let h = video.buffer.height();
        if w == 0
            || h == 0
            || w > MAX_SIDE
            || h > MAX_SIDE
            || u64::from(w) * u64::from(h) > MAX_PIXELS
        {
            return false;
        }
        let sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        *self.latest.lock().unwrap() = Some(Arc::new(Frame { sequence, video }));
        true
    }

    pub fn clear(&self) {
        self.latest.lock().unwrap().take();
        self.scratch.lock().unwrap().take();
    }

    pub fn read(&self, request: &mut VideoRead, output: &mut [u8]) -> isize {
        let Some(frame) = self.latest.lock().unwrap().clone() else {
            return 0;
        };
        let source = &frame.video.buffer;
        let (width, height) = fit(
            source.width(),
            source.height(),
            request.viewport_width,
            request.viewport_height,
        );
        if width == 0
            || height == 0
            || (request.sequence == frame.sequence
                && request.width == width
                && request.height == height)
        {
            return 0;
        }
        let len = (width * height * 4) as usize;
        if output.len() < len {
            return -(len as isize);
        }

        let converted;
        let source = if let Some(buffer) = source.as_i420() {
            buffer
        } else {
            converted = source.to_i420();
            &converted
        };
        if width == source.width() && height == source.height() {
            rgba(source, output, width, height);
        } else {
            let mut scratch = self.scratch.lock().unwrap();
            if scratch
                .as_ref()
                .is_none_or(|image| image.width() != width || image.height() != height)
            {
                *scratch = Some(I420Buffer::new(width, height));
            }
            let scaled = scratch.as_mut().unwrap();
            if !scale_i420(source, scaled) {
                return 0;
            }
            rgba(scaled, output, width, height);
        }
        request.sequence = frame.sequence;
        request.width = width;
        request.height = height;
        len as isize
    }
}

fn fit(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if max_w == 0 || max_h == 0 {
        return (0, 0);
    }
    let max_w = u64::from(MAX_SIDE.min(max_w));
    let max_h = u64::from(2304.min(max_h));
    let w = u64::from(w);
    let h = u64::from(h);
    if max_w * h <= max_h * w {
        (max_w as u32, (max_w * h / w).max(1) as u32)
    } else {
        ((max_h * w / h).max(1) as u32, max_h as u32)
    }
}

fn rgba(source: &I420Buffer, output: &mut [u8], width: u32, height: u32) {
    let (y, u, v) = source.data();
    let (sy, su, sv) = source.strides();
    // libyuv uses register order in the name. ABGR writes RGBA bytes on Windows.
    i420_to_abgr(
        y,
        sy,
        u,
        su,
        v,
        sv,
        output,
        width * 4,
        width as i32,
        height as i32,
    );
}

unsafe extern "C" {
    fn I420Scale(
        src_y: *const u8,
        sy: i32,
        src_u: *const u8,
        su: i32,
        src_v: *const u8,
        sv: i32,
        source_width: i32,
        source_height: i32,
        dst_y: *mut u8,
        dy: i32,
        dst_u: *mut u8,
        du: i32,
        dst_v: *mut u8,
        dv: i32,
        output_width: i32,
        output_height: i32,
        filter: i32,
    ) -> i32;
}

fn scale_i420(source: &I420Buffer, output: &mut I420Buffer) -> bool {
    let (width, height) = (output.width(), output.height());
    let (sy, su, sv) = source.strides();
    let (dy, du, dv) = output.strides();
    let (src_y, src_u, src_v) = source.data();
    let (dst_y, dst_u, dst_v) = output.data_mut();
    // Each plane and its stride come from matching WebRTC buffers.
    unsafe {
        I420Scale(
            src_y.as_ptr(),
            sy as i32,
            src_u.as_ptr(),
            su as i32,
            src_v.as_ptr(),
            sv as i32,
            source.width() as i32,
            source.height() as i32,
            dst_y.as_mut_ptr(),
            dy as i32,
            dst_u.as_mut_ptr(),
            du as i32,
            dst_v.as_mut_ptr(),
            dv as i32,
            width as i32,
            height as i32,
            2, // kFilterBilinear
        ) == 0
    }
}

pub async fn consume(
    mut tracks: watch::Receiver<Option<RtcVideoTrack>>,
    frames: Arc<VideoStore>,
    events: Arc<Events>,
) {
    while tracks.changed().await.is_ok() {
        'current: loop {
            let Some(track) = tracks.borrow_and_update().clone() else {
                break;
            };
            let mut stream = NativeVideoStream::new(track);
            loop {
                tokio::select! {
                    biased;
                    changed = tracks.changed() => {
                        if changed.is_err() {
                            return;
                        }
                        continue 'current;
                    }
                    frame = stream.next() => {
                        let Some(frame) = frame else { break 'current };
                        if frames.publish(frame) {
                            events.push(Event::Frame);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitted_picture_respects_source_and_viewport() {
        assert_eq!(fit(1920, 1080, 360, 200), (355, 200));
        assert_eq!(fit(480, 640, 1920, 1080), (810, 1080));
        assert_eq!(fit(480, 640, 0, 0), (0, 0));
    }

    #[test]
    fn libyuv_scales_into_reusable_rgba_output() {
        let source = I420Buffer::new_black(64, 32);
        let mut target = I420Buffer::new(24, 12);
        assert!(scale_i420(&source, &mut target));
        let mut bytes = [0; 24 * 12 * 4];
        rgba(&target, &mut bytes, 24, 12);
        assert_eq!(&bytes[..4], &[0, 0, 0, 255]);
    }
}
