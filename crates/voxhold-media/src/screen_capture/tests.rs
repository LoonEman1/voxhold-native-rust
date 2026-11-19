//! Tests of the pixel path, the bounded stores and the Windows capability.

use super::*;
use crate::screen_capture::SourceKind;
use libwebrtc::video_frame::VideoBuffer;

fn solid(width: u32, height: u32, stride: u32, bgra: [u8; 4]) -> Vec<u8> {
    let mut bytes = vec![0_u8; stride as usize * height as usize];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let at = y * stride as usize + x * 4;
            bytes[at..at + 4].copy_from_slice(&bgra);
        }
    }
    bytes
}

fn planes(buffer: &I420Buffer) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let (stride_y, stride_u, stride_v) = buffer.strides();
    let (data_y, data_u, data_v) = buffer.data();
    let luma: Vec<u8> = (0..buffer.height() as usize)
        .flat_map(|y| (0..buffer.width() as usize).map(move |x| (y, x)))
        .map(|(y, x)| data_y[y * stride_y as usize + x])
        .collect();
    let chroma = |data: &[u8], stride: u32| -> Vec<u8> {
        (0..buffer.chroma_height() as usize)
            .flat_map(|y| (0..buffer.chroma_width() as usize).map(move |x| (y, x)))
            .map(|(y, x)| data[y * stride as usize + x])
            .collect()
    };
    (luma, chroma(data_u, stride_u), chroma(data_v, stride_v))
}

#[test]
fn a_solid_colour_keeps_its_bt601_planes() {
    let source = solid(2, 2, 8, [0, 0, 255, 255]); // BGRA red
    let mut output = I420Buffer::new(2, 2);
    assert!(convert_bgra_to_i420(&source, 2, 2, 8, &mut output));
    let (luma, u, v) = planes(&output);
    assert_eq!(luma.len(), 4);
    assert!(
        luma.iter().all(|value| (80..=84).contains(value)),
        "Y {luma:?}"
    );
    assert!(u.iter().all(|value| (88..=92).contains(value)), "U {u:?}");
    assert!(v.iter().all(|value| (238..=242).contains(value)), "V {v:?}");
}

#[test]
fn the_top_and_bottom_halves_keep_their_colours_and_order() {
    // 16×16: red above, blue below, exactly like the viewer orientation test.
    let mut source = solid(16, 16, 16 * 4, [0, 0, 255, 255]);
    for y in 8..16 {
        for x in 0..16 {
            let at = y * 16 * 4 + x * 4;
            source[at..at + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
    }
    let mut output = I420Buffer::new(16, 16);
    assert!(convert_bgra_to_i420(&source, 16, 16, 16 * 4, &mut output));
    let (luma, u, v) = planes(&output);
    let at = |y: usize, x: usize| luma[y * 16 + x];
    assert!(at(0, 0) > 75 && at(0, 15) > 75, "top rows are red");
    assert!(at(15, 0) < 50 && at(15, 15) < 50, "bottom rows are blue");
    let chroma_at = |plane: &[u8], y: usize, x: usize| plane[y * 8 + x];
    assert!(chroma_at(&u, 0, 0) < 100, "red chroma U is low");
    assert!(chroma_at(&v, 0, 0) > 200, "red chroma V is high");
    assert!(chroma_at(&u, 7, 0) > 200, "blue chroma U is high");
    assert!(chroma_at(&v, 7, 0) < 130, "blue chroma V is low");
}

#[test]
fn a_padded_stride_never_reads_the_padding() {
    // 2×2 picture inside a 16-byte row: the padding is white noise and must
    // not affect the result.
    let mut source = solid(2, 2, 16, [0, 0, 255, 255]);
    for y in 0..2 {
        for x in 8..16 {
            source[y * 16 + x] = 0xFF;
        }
    }
    let mut output = I420Buffer::new(2, 2);
    assert!(convert_bgra_to_i420(&source, 2, 2, 16, &mut output));
    let (luma, u, _) = planes(&output);
    assert!(luma.iter().all(|value| *value < 90), "Y {luma:?}");
    assert!(u.iter().all(|value| *value < 100), "U {u:?}");
}

#[test]
fn odd_sources_convert_without_panicking() {
    let source = solid(7, 5, 7 * 4, [40, 200, 30, 255]);
    let mut output = I420Buffer::new(6, 4);
    assert!(convert_bgra_to_i420(&source, 7, 5, 7 * 4, &mut output));
    assert_eq!(output.width(), 6);
    assert_eq!(output.height(), 4);
    // Too small a buffer or an odd destination is a refusal, not a panic.
    let mut odd = I420Buffer::new(5, 3);
    assert!(!convert_bgra_to_i420(&source, 7, 5, 7 * 4, &mut odd));
    let mut zero = I420Buffer::new(6, 4);
    assert!(!convert_bgra_to_i420(&[], 7, 5, 7 * 4, &mut zero));
    assert!(!convert_bgra_to_i420(&source, 7, 5, 20, &mut zero));
}

#[test]
fn a_downscale_preserves_the_picture_geometry() {
    // 8×4 red/blue halves into 4×2: the top row stays red, the bottom blue.
    let mut source = solid(8, 4, 8 * 4, [0, 0, 255, 255]);
    for y in 2..4 {
        for x in 0..8 {
            let at = y * 8 * 4 + x * 4;
            source[at..at + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
    }
    let mut output = I420Buffer::new(4, 2);
    assert!(convert_bgra_to_i420(&source, 8, 4, 8 * 4, &mut output));
    let (luma, _, _) = planes(&output);
    assert!(
        (78..=86).contains(&luma[0]),
        "top-left is the red half: {}",
        luma[0]
    );
    assert!(
        (38..=46).contains(&luma[4]),
        "bottom-left is the blue half: {}",
        luma[4]
    );
}

#[test]
fn fitting_keeps_the_aspect_ratio_and_never_upscales() {
    assert_eq!(fit_even(2560, 1440, 1280, 720), (1280, 720));
    assert_eq!(fit_even(1920, 1080, 1280, 720), (1280, 720));
    assert_eq!(fit_even(2560, 1080, 1280, 720), (1280, 540));
    assert_eq!(fit_even(1920, 1080, 1920, 1080), (1920, 1080));
    assert_eq!(fit_even(800, 600, 1280, 720), (800, 600));
    assert_eq!(fit_even(7, 5, 1280, 720), (6, 4));
    assert_eq!(fit_even(0, 0, 1280, 720), (0, 0));
    assert_eq!(fit_even(10_000, 10_000, 1280, 720), (720, 720));
}

#[test]
fn the_preview_is_a_bounded_bgra_picture() {
    let mut source = I420Buffer::new_black(16, 16);
    {
        let (stride_y, _, _) = source.strides();
        let (data_y, _, _) = source.data_mut();
        for y in 0..8 {
            for x in 0..16 {
                data_y[y * stride_y as usize + x] = 235;
            }
        }
    }
    let preview = preview_from_i420(&source, 8).expect("preview");
    assert_eq!((preview.width, preview.height), (8, 8));
    assert_eq!(preview.bgra.len(), 8 * 8 * 4);
    // The top half is white-ish and the bottom half black.
    assert!(preview.bgra[2] > 200 && preview.bgra[1] > 200);
    let bottom = (7 * 8) * 4;
    assert!(preview.bgra[bottom + 2] < 40 && preview.bgra[bottom + 1] < 40);
    assert_eq!(preview.bgra[3], 255);
}

#[test]
fn only_the_newest_capture_survives_the_slot() {
    let slot = LatestImage::default();
    let image = |width| CapturedImage {
        width,
        height: 2,
        i420: I420Buffer::new_black(width, 2),
        preview: None,
    };
    slot.publish(image(2));
    slot.publish(image(4));
    assert_eq!(slot.published(), 2);
    assert_eq!(slot.dropped(), 1, "the unread frame is counted as a drop");
    assert_eq!(slot.take().expect("latest").width, 4);
    assert_eq!(slot.dropped(), 1, "taking a frame is not a drop");
    assert!(slot.take().is_none());
}

#[test]
fn the_preview_store_keeps_one_bounded_picture_and_does_not_grow() {
    let store = PreviewStore::default();
    let picture = |width, height| PreviewFrame {
        width,
        height,
        bgra: vec![7; width as usize * height as usize * 4],
    };
    assert!(store.publish(picture(4, 2)));
    assert!(store.publish(picture(8, 4)));
    let latest = store.take().expect("latest preview");
    assert_eq!((latest.width, latest.height), (8, 4));
    assert_eq!(store.published(), 2);
    // The slot is empty after the UI took the picture; an idle UI does not
    // keep growing the store.
    assert_eq!(store.bytes(), 0);
    assert!(store.take().is_none());
    for _ in 0..64 {
        assert!(store.publish(picture(8, 4)));
        let _ = store.take();
    }
    assert_eq!(store.bytes(), 0);
    // A picture above either bound is rejected and the slot stays empty.
    assert!(!store.publish(picture(MAX_PREVIEW_WIDTH + 2, 2)));
    assert!(!store.publish(PreviewFrame {
        width: 4,
        height: 4,
        bgra: vec![0; 8],
    }));
    assert_eq!(store.bytes(), 0);
    assert_eq!(store.rejected(), 2);
}

#[test]
fn an_empty_source_list_is_not_a_native_success() {
    // A locked-down desktop may hide every window; the caller gets an honest
    // capability answer or a list, never a fabricated source.
    let capability = capability();
    if let Ok(()) = capability {
        let monitors = list_sources(SourceKind::Monitor).expect("monitor listing");
        for source in &monitors {
            assert_eq!(source.kind, SourceKind::Monitor);
            assert!(!source.title.is_empty(), "every monitor has a label");
        }
    } else {
        assert!(list_sources(SourceKind::Monitor).is_err());
        assert!(list_sources(SourceKind::Window).is_err());
    }
}

/// Manual proof on a real desktop: the prebuilt capturer lists at least one
/// source and delivers one real frame. Run explicitly:
/// `cargo test -p voxhold-media -- --ignored screen_capture`.
#[cfg(windows)]
#[test]
#[ignore = "требует реальный рабочий стол; запускается вручную"]
fn a_real_desktop_capture_delivers_a_frame() {
    let sources = list_sources(SourceKind::Monitor).expect("monitor list");
    assert!(!sources.is_empty(), "no monitor sources on this host");
    let latest = std::sync::Arc::new(LatestImage::default());
    let mut capture = ScreenCapture::start(sources[0].clone(), (1280, 720), 30, latest.clone())
        .expect("capture start");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let image = loop {
        if let Some(image) = latest.take() {
            break image;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no frame in five seconds"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    println!(
        "captured {}×{} from {:?}; first frame {}×{}",
        image.width, image.height, sources[0], image.width, image.height
    );
    assert!(image.width >= 2 && image.height >= 2);
    assert!(image.i420.width() == image.width && image.i420.height() == image.height);
    capture.stop();
    assert!(!sources[0].describe().is_empty());

    // The same picker must list windows with real titles. This process has no
    // visible top-level window, so the own-process filter is a no-op here; it
    // still proves the window capturer is usable on this host.
    let windows = list_sources(SourceKind::Window).expect("window list");
    for window in windows.iter().take(5) {
        // No titles in test output: the log stays free of personal window
        // names, while id and size still prove the listing works.
        println!("window {} · {}×{}", window.id, window.width, window.height);
    }
    assert!(
        windows.iter().all(|window| !window.title.trim().is_empty()),
        "a window source without a title must not be offered"
    );
    assert!(
        windows
            .iter()
            .all(|window| window.kind == SourceKind::Window),
        "the window list must not mix monitor sources"
    );
}

#[test]
fn a_bgra_thumbnail_is_downscaled_to_a_bounded_picture() {
    // 16×8: red above, blue below. The preview is 8×4 and keeps the halves.
    let mut source = solid(16, 8, 16 * 4, [0, 0, 255, 255]);
    for y in 4..8 {
        for x in 0..16 {
            let at = y * 16 * 4 + x * 4;
            source[at..at + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
    }
    let preview = preview_from_bgra(&source, 16, 8, 16 * 4, 8).expect("thumbnail");
    assert_eq!((preview.width, preview.height), (8, 4));
    assert_eq!(preview.bgra.len(), 8 * 4 * 4);
    // Top row is red (B low, R high), bottom row is blue.
    assert!(preview.bgra[0] < 30 && preview.bgra[2] > 200, "top");
    let bottom = 3 * 8 * 4;
    assert!(
        preview.bgra[bottom] > 200 && preview.bgra[bottom + 2] < 30,
        "bottom"
    );
    assert!(preview.bgra.iter().skip(3).step_by(4).all(|a| *a == 255));

    // A caller cannot exceed the picker bound, and a small picture is never
    // upscaled.
    let wide = solid(400, 8, 400 * 4, [10, 20, 30, 255]);
    let bounded = preview_from_bgra(&wide, 400, 8, 400 * 4, 10_000).expect("bounded");
    assert_eq!(bounded.width, MAX_THUMBNAIL_WIDTH);
    let small = preview_from_bgra(&source, 16, 8, 16 * 4, 16).expect("small");
    assert_eq!((small.width, small.height), (16, 8));

    // A short buffer, a too-small stride or a zero side is refused.
    assert!(preview_from_bgra(&[], 16, 8, 16 * 4, 8).is_none());
    assert!(preview_from_bgra(&source, 16, 8, 8, 8).is_none());
    assert!(preview_from_bgra(&source, 0, 8, 16 * 4, 8).is_none());
}

/// Manual proof on a real desktop: one monitor or window delivers a thumbnail.
/// Only dimensions are printed, never a source title. Run explicitly:
/// `cargo test -p voxhold-media -- --ignored screen_capture`.
#[cfg(windows)]
#[test]
#[ignore = "требует реальный рабочий стол; запускается вручную"]
fn a_real_source_yields_a_bounded_thumbnail() {
    let source = list_sources(SourceKind::Monitor)
        .ok()
        .and_then(|sources| sources.into_iter().next())
        .or_else(|| {
            list_sources(SourceKind::Window)
                .ok()
                .and_then(|sources| sources.into_iter().next())
        })
        .expect("no monitor or window source on this host");
    let preview = capture_thumbnail(&source, MAX_THUMBNAIL_WIDTH).expect("thumbnail");
    println!(
        "thumbnail {}×{} ({:?} source kind)",
        preview.width, preview.height, source.kind
    );
    assert!(preview.width >= 2 && preview.height >= 2);
    assert!(preview.width <= MAX_THUMBNAIL_WIDTH);
    assert_eq!(
        preview.bgra.len(),
        preview.width as usize * preview.height as usize * 4
    );
}

/// Manual proof on a real desktop: the first window source yields a thumbnail
/// of the same bounded shape. Only dimensions are printed, never a title. Run
/// explicitly:
/// `cargo test -p voxhold-media -- --ignored a_real_window_yields_a_bounded_thumbnail`.
#[cfg(windows)]
#[test]
#[ignore = "требует реальный рабочий стол; запускается вручную"]
fn a_real_window_yields_a_bounded_thumbnail() {
    let source = list_sources(SourceKind::Window)
        .expect("window list")
        .into_iter()
        .next()
        .expect("no visible window source on this host");
    assert_eq!(source.kind, SourceKind::Window);
    let preview = capture_thumbnail(&source, MAX_THUMBNAIL_WIDTH).expect("window thumbnail");
    println!("window thumbnail {}×{}", preview.width, preview.height);
    assert!(preview.width >= 2 && preview.height >= 2);
    assert!(preview.width <= MAX_THUMBNAIL_WIDTH);
    assert_eq!(
        preview.bgra.len(),
        preview.width as usize * preview.height as usize * 4
    );
}

fn descriptor(device: &str, width: u32, height: u32) -> MonitorDescriptor {
    MonitorDescriptor {
        device: device.into(),
        width,
        height,
    }
}

#[test]
fn a_monitor_title_matches_its_own_device_name() {
    let monitors = [
        descriptor(r"\\.\DISPLAY1", 1920, 1080),
        descriptor(r"\\.\DISPLAY2", 2560, 1440),
    ];
    let matched = monitor_descriptor_for_title(r"Экран (\\.\DISPLAY2)", &monitors)
        .expect("the device name is in the title");
    assert_eq!((matched.width, matched.height), (2560, 1440));
}

#[test]
fn a_monitor_title_matches_the_display_number_not_the_list_order() {
    // The list starts with DISPLAY2, so position-based metadata would attach
    // the 2560×1440 size to the title that names DISPLAY1.
    let monitors = [
        descriptor(r"\\.\DISPLAY2", 2560, 1440),
        descriptor(r"\\.\DISPLAY1", 1920, 1080),
    ];
    let matched = monitor_descriptor_for_title("Screen 1", &monitors).expect("display number");
    assert_eq!((matched.width, matched.height), (1920, 1080));
}

#[test]
fn an_unmatched_or_ambiguous_monitor_title_never_borrows_a_size() {
    let monitors = [
        descriptor(r"\\.\DISPLAY1", 1920, 1080),
        descriptor(r"\\.\DISPLAY2", 2560, 1440),
    ];
    assert!(
        monitor_descriptor_for_title("Документ", &monitors).is_none(),
        "a title without a device name or number has no known size"
    );
    assert!(monitor_descriptor_for_title("   ", &monitors).is_none());

    // Two devices end in the same number: guessing either one would attach a
    // neighbour's size.
    let ambiguous = [
        descriptor(r"\\.\DISPLAY1", 1920, 1080),
        descriptor(r"\\.\MONITOR1", 1280, 720),
    ];
    assert!(monitor_descriptor_for_title("Screen 1", &ambiguous).is_none());
}

#[test]
fn a_monitor_source_identity_ignores_its_mutable_label() {
    let source = ScreenSource {
        id: 7,
        kind: SourceKind::Monitor,
        title: "Экран 1".into(),
        display_id: -1,
        width: 1920,
        height: 1080,
    };
    let renamed = ScreenSource {
        title: "Другое имя".into(),
        width: 800,
        height: 600,
        ..source.clone()
    };
    assert_eq!(source.source_id(), renamed.source_id());
    assert_ne!(source.describe(), renamed.describe());
}
