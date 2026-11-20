use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use libwebrtc::{
    native::yuv_helper::i420_to_argb,
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

impl VideoRead {
    /// Forget which frame was decoded last, but keep the viewport bounds. A
    /// replacement engine has its own frame sequence; its first frame may even
    /// reuse sequence 1 at the same fitted size.
    pub(crate) fn forget_frame(&mut self) {
        self.sequence = 0;
        self.width = 0;
        self.height = 0;
    }
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
            bgra(source, output, width, height);
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
            bgra(scaled, output, width, height);
        }
        request.sequence = frame.sequence;
        request.width = width;
        request.height = height;
        len as isize
    }
}

#[cfg(test)]
impl VideoStore {
    /// Test-only: publish a frame through the production path so a viewer test
    /// can prove a viewport change re-reads an already decoded picture.
    pub(crate) fn publish_i420(&self, buffer: I420Buffer) -> bool {
        self.publish(libwebrtc::video_frame::VideoFrame::new(
            libwebrtc::video_frame::VideoRotation::VideoRotation0,
            Box::new(buffer) as Box<dyn VideoBuffer>,
        ))
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

fn bgra(source: &I420Buffer, output: &mut [u8], width: u32, height: u32) {
    let (y, u, v) = source.data();
    let (sy, su, sv) = source.strides();
    // libyuv names its fourcc in register order. On little-endian Windows
    // `I420ToARGB` writes B,G,R,A bytes, exactly the BGRA surface GPUI uploads,
    // so the UI no longer swaps every pixel. Rows are written top to bottom and
    // the picture keeps the source orientation.
    i420_to_argb(
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

    /// Fill an I420 frame with two known BT.601 limited-range colours, top and
    /// bottom, so a channel swap or a vertical flip is visible in the bytes.
    fn synthetic(width: u32, height: u32, top: (u8, u8, u8), bottom: (u8, u8, u8)) -> I420Buffer {
        let mut buffer = I420Buffer::new(width, height);
        let (sy, su, sv) = buffer.strides();
        let (y, u, v) = buffer.data_mut();
        for row in 0..height as usize {
            let colour = if row < height as usize / 2 {
                top
            } else {
                bottom
            };
            for column in 0..width as usize {
                y[row * sy as usize + column] = colour.0;
            }
        }
        let chroma_height = height.div_ceil(2) as usize;
        let chroma_width = width.div_ceil(2) as usize;
        for row in 0..chroma_height {
            let colour = if row < chroma_height / 2 { top } else { bottom };
            for column in 0..chroma_width {
                u[row * su as usize + column] = colour.1;
                v[row * sv as usize + column] = colour.2;
            }
        }
        buffer
    }

    /// BT.601 limited-range primaries.
    const RED: (u8, u8, u8) = (81, 90, 240);
    const BLUE: (u8, u8, u8) = (41, 240, 110);

    #[test]
    fn fitted_picture_respects_source_and_viewport() {
        assert_eq!(fit(1920, 1080, 360, 200), (355, 200));
        assert_eq!(fit(480, 640, 1920, 1080), (810, 1080));
        assert_eq!(fit(480, 640, 0, 0), (0, 0));
    }

    #[test]
    fn libyuv_scales_into_reusable_bgra_output() {
        let source = I420Buffer::new_black(64, 32);
        let mut target = I420Buffer::new(24, 12);
        assert!(scale_i420(&source, &mut target));
        let mut bytes = [0; 24 * 12 * 4];
        bgra(&target, &mut bytes, 24, 12);
        assert_eq!(&bytes[..4], &[0, 0, 0, 255]);
    }

    #[test]
    fn known_colours_keep_channel_order_and_orientation() {
        // 4x4: the top half is red, the bottom half blue. The output is the
        // BGRA surface GPUI uploads, so red must be `[0, 0, 255, 255]`.
        let source = synthetic(4, 4, RED, BLUE);
        let mut bytes = [0; 4 * 4 * 4];
        bgra(&source, &mut bytes, 4, 4);
        let pixel = |x: usize, y: usize| {
            let at = (y * 4 + x) * 4;
            [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]
        };
        let red = pixel(0, 0);
        assert!(
            red[0] < 4 && red[1] < 4,
            "top row has no blue/green: {red:?}"
        );
        assert!(red[2] > 250, "top row is red in the BGRA red byte: {red:?}");
        assert_eq!(red[3], 255);
        let blue = pixel(0, 3);
        assert!(
            blue[2] < 4 && blue[1] < 4,
            "bottom row has no red/green: {blue:?}"
        );
        assert!(
            blue[0] > 250,
            "bottom row is blue in the BGRA blue byte: {blue:?}"
        );
        assert_eq!(blue[3], 255);
        assert_eq!(pixel(3, 0), red);
    }

    #[test]
    fn scaling_stretches_without_flipping_or_swapping_colours() {
        let source = synthetic(8, 4, RED, BLUE);
        let mut target = I420Buffer::new(24, 24);
        assert!(scale_i420(&source, &mut target));
        let mut bytes = [0; 24 * 24 * 4];
        bgra(&target, &mut bytes, 24, 24);
        let pixel = |x: usize, y: usize| {
            let at = (y * 24 + x) * 4;
            [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]
        };
        // Stretch keeps the top row red and the bottom row blue at every x.
        for x in [0, 11, 23] {
            assert!(pixel(x, 1)[2] > 250, "red at x={x}: {:?}", pixel(x, 1));
            assert!(pixel(x, 1)[0] < 4);
            assert!(pixel(x, 22)[0] > 250, "blue at x={x}: {:?}", pixel(x, 22));
            assert!(pixel(x, 22)[2] < 4);
        }
    }

    #[test]
    fn reused_read_reports_the_same_frame_only_once() {
        let store = VideoStore::default();
        let frame = libwebrtc::video_frame::VideoFrame::new(
            libwebrtc::video_frame::VideoRotation::VideoRotation0,
            Box::new(synthetic(8, 8, RED, BLUE)) as Box<dyn VideoBuffer>,
        );
        assert!(store.publish(frame));
        let mut request = VideoRead {
            viewport_width: 8,
            viewport_height: 8,
            ..VideoRead::default()
        };
        let mut output = vec![0; 8 * 8 * 4];
        assert_eq!(store.read(&mut request, &mut output), 8 * 8 * 4);
        assert!(output[2] > 250 && output[0] < 4);
        assert_eq!(store.read(&mut request, &mut output), 0);
    }

    #[test]
    fn a_viewport_change_re_reads_the_static_frame_at_the_fitted_size() {
        let store = VideoStore::default();
        assert!(store.publish_i420(I420Buffer::new_black(320, 180)));
        let mut request = VideoRead {
            viewport_width: 160,
            viewport_height: 90,
            ..VideoRead::default()
        };
        let mut output = vec![0; 160 * 90 * 4];
        assert_eq!(
            store.read(&mut request, &mut output),
            (160 * 90 * 4) as isize
        );
        // Fullscreen changes only the player surface: no new RTP frame is
        // stored, yet the same picture must be scaled to the new size.
        request.viewport_width = 320;
        request.viewport_height = 278;
        output.resize(320 * 180 * 4, 0);
        assert_eq!(
            store.read(&mut request, &mut output),
            (320 * 180 * 4) as isize
        );
        assert_eq!((request.width, request.height), fit(320, 180, 320, 278));
        // A second read at the same viewport and sequence is a no-op, so the
        // UI settles after the repaint instead of forcing another frame.
        assert_eq!(store.read(&mut request, &mut output), 0);
    }
}
