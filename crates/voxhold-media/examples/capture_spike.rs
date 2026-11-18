//! W03 spike: do the prebuilt libwebrtc desktop capturers work on this host?
//!
//! Run with `cargo run -p voxhold-media --example capture_spike`. It lists the
//! screen and window sources, captures a couple of frames from the first
//! source of each kind and prints the real dimensions, stride and byte length.
//! The final implementation must not depend on a spike that never ran.

#[cfg(windows)]
fn main() {
    use libwebrtc::desktop_capturer::{
        DesktopCaptureSourceType, DesktopCapturer, DesktopCapturerOptions,
    };
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    fn probe(kind: DesktopCaptureSourceType, label: &str) {
        let mut options = DesktopCapturerOptions::new(kind);
        options.set_include_cursor(true);
        let Some(mut capturer) = DesktopCapturer::new(options) else {
            println!("{label}: DesktopCapturer::new -> None");
            return;
        };
        let sources = capturer.get_source_list();
        println!("{label}: {} sources", sources.len());
        for source in sources.iter().take(8) {
            println!(
                "  id={} display_id={} title={:?}",
                source.id(),
                source.display_id(),
                source.title()
            );
        }
        let Some(first) = sources.first().cloned() else {
            println!("{label}: empty source list");
            return;
        };
        let frames: Arc<Mutex<Vec<(i32, i32, u32, usize)>>> = Arc::new(Mutex::new(Vec::new()));
        let errors: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = frames.clone();
        let failures = errors.clone();
        capturer.start_capture(Some(first), move |result| match result {
            Ok(frame) => sink.lock().unwrap().push((
                frame.width(),
                frame.height(),
                frame.stride(),
                frame.data().len(),
            )),
            Err(error) => failures.lock().unwrap().push(match error {
                libwebrtc::desktop_capturer::CaptureError::Temporary => "temporary",
                libwebrtc::desktop_capturer::CaptureError::Permanent => "permanent",
            }),
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        while frames.lock().unwrap().len() < 2 && Instant::now() < deadline {
            capturer.capture_frame();
            std::thread::sleep(Duration::from_millis(120));
        }
        println!(
            "{label}: frames={:?} errors={:?}",
            frames.lock().unwrap(),
            errors.lock().unwrap()
        );
    }

    probe(DesktopCaptureSourceType::Screen, "screen");
    probe(DesktopCaptureSourceType::Window, "window");
}

#[cfg(not(windows))]
fn main() {
    println!("desktop capture is only implemented on Windows");
}
