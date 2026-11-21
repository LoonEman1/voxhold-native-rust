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

/// Deterministic BGRA picture whose channels change on every pixel. The row
/// padding is filled with garbage so a reader that follows `stride` wrong
/// changes the result.
fn gradient(width: u32, height: u32, stride: u32) -> Vec<u8> {
    let mut bytes = vec![0xCD_u8; stride as usize * height as usize];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let at = y * stride as usize + x * 4;
            bytes[at] = (x * 7 + y * 13) as u8; // blue
            bytes[at + 1] = (x * 11 + y * 3 + 40) as u8; // green
            bytes[at + 2] = (y * 17 + x * 5 + 80) as u8; // red
            bytes[at + 3] = 255;
        }
    }
    bytes
}

/// Compare the planes of two pictures, allowing fixed-point rounding noise.
fn assert_planes_match(expected: &I420Buffer, actual: &I420Buffer, tolerance: u8) {
    let (expected_y, expected_u, expected_v) = planes(expected);
    let (actual_y, actual_u, actual_v) = planes(actual);
    for (left, right) in [
        (&expected_y, &actual_y),
        (&expected_u, &actual_u),
        (&expected_v, &actual_v),
    ] {
        assert_eq!(left.len(), right.len(), "plane sizes differ");
        for (index, (want, got)) in left.iter().zip(right.iter()).enumerate() {
            assert!(
                want.abs_diff(*got) <= tolerance,
                "plane sample {index}: expected {want}, got {got}"
            );
        }
    }
}

#[test]
fn the_exact_size_path_matches_the_scalar_box_filter() {
    // An unchanged size now runs through libyuv. The scalar filter must give
    // the same picture up to one step of fixed-point rounding.
    let stride = 18 * 4 + 8;
    for source in [
        solid(18, 10, stride, [40, 200, 30, 255]),
        gradient(18, 10, stride),
    ] {
        let mut fast = I420Buffer::new(18, 10);
        assert!(convert_bgra_to_i420(&source, 18, 10, stride, &mut fast));
        let mut scalar = I420Buffer::new(18, 10);
        convert_bgra_to_i420_box(&source, 18, 10, stride, &mut scalar);
        assert_planes_match(&scalar, &fast, 1);
    }
}

#[test]
fn the_exact_size_path_ignores_the_row_padding() {
    // 4×4 picture inside 24-byte rows (16 data bytes plus 8 garbage bytes).
    let width = 4;
    let height = 4;
    let stride = width * 4 + 8;
    let source = gradient(width, height, stride);
    let mut padded = I420Buffer::new(width, height);
    assert!(convert_bgra_to_i420(
        &source,
        width,
        height,
        stride,
        &mut padded
    ));
    // The same pixels packed without padding.
    let packed_source = gradient(width, height, width * 4);
    let mut tight = I420Buffer::new(width, height);
    assert!(convert_bgra_to_i420(
        &packed_source,
        width,
        height,
        width * 4,
        &mut tight
    ));
    // Identical pixels, different stride: identical planes.
    assert_planes_match(&tight, &padded, 0);
}

#[test]
fn a_scaled_picture_ignores_the_row_padding() {
    // The rescaled scalar path must only read the width*4 bytes it samples;
    // the rest of each row is noise.
    let width = 8;
    let height = 4;
    let packed = width * 4;
    let stride = packed + 12;
    let clean = gradient(width, height, packed);
    let padded = gradient(width, height, stride);
    let mut a = I420Buffer::new(4, 2);
    let mut b = I420Buffer::new(4, 2);
    assert!(convert_bgra_to_i420(&clean, width, height, packed, &mut a));
    assert!(convert_bgra_to_i420(&padded, width, height, stride, &mut b));
    assert_planes_match(&a, &b, 0);
}

#[test]
fn scaled_solid_frames_keep_their_colour_at_even_and_odd_sizes() {
    // (source width, source height, output width, output height). The 6×4
    // case keeps the size and takes the libyuv path; the others rescale,
    // including an odd source and a one-row source.
    let cases = [
        (6, 4, 6, 4),
        (8, 6, 4, 2),
        (8, 6, 6, 4),
        (7, 5, 6, 4),
        (2, 1, 2, 2),
    ];
    for (width, height, out_w, out_h) in cases {
        let stride = width * 4 + 4;
        let source = solid(width, height, stride, [40, 200, 30, 255]);
        let mut output = I420Buffer::new(out_w, out_h);
        assert!(
            convert_bgra_to_i420(&source, width, height, stride, &mut output),
            "{width}×{height} into {out_w}×{out_h}"
        );
        let label = format!("{width}×{height} into {out_w}×{out_h}");
        let (luma, u, v) = planes(&output);
        // BGRA (40, 200, 30) is Y=128, U=83, V=65 in BT.601 limited range.
        assert!(
            luma.iter().all(|value| (126..=130).contains(value)),
            "Y {label}: {luma:?}"
        );
        assert!(
            u.iter().all(|value| (81..=85).contains(value)),
            "U {label}: {u:?}"
        );
        assert!(
            v.iter().all(|value| (63..=67).contains(value)),
            "V {label}: {v:?}"
        );
    }
}

#[test]
fn a_short_or_odd_input_is_refused_without_reading_past_it() {
    // One byte short of `stride * height`: the libyuv call would panic, so
    // the public entry point must refuse the buffer first.
    let short = vec![0_u8; 8 * 4 * 4 - 1];
    let mut output = I420Buffer::new(8, 4);
    assert!(!convert_bgra_to_i420(&short, 8, 4, 32, &mut output));

    // height = 1 is a usable source, but an odd destination is still refused.
    let one_row = solid(2, 1, 8, [40, 200, 30, 255]);
    let mut odd = I420Buffer::new(2, 1);
    assert!(!convert_bgra_to_i420(&one_row, 2, 1, 8, &mut odd));
    let mut even = I420Buffer::new(2, 2);
    assert!(convert_bgra_to_i420(&one_row, 2, 1, 8, &mut even));
    let (luma, _, _) = planes(&even);
    assert!(luma.iter().all(|value| (126..=130).contains(value)));
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
    for candidate in &sources {
        println!(
            "monitor {} · identity {:?} · {}×{}",
            candidate.id,
            candidate
                .monitor
                .as_ref()
                .map(|monitor| monitor.device.as_str()),
            candidate.width,
            candidate.height
        );
    }
    // Every screen source has an empty title on the pinned capturer; the
    // identity must therefore come from `EnumDisplayDevicesW`. Without a
    // bound display the picker cannot offer a selectable monitor at all.
    let source = sources
        .iter()
        .find(|source| source.monitor.is_some())
        .expect("no monitor source is bound to a Windows display")
        .clone();
    let identity = source.monitor.as_ref().expect("checked above");
    assert!(
        !identity.device.trim().is_empty(),
        "a bound monitor must name its Windows display"
    );
    assert!(identity.size().0 >= 2 && identity.size().1 >= 2);
    // The same display must resolve uniquely in a fresh enumeration.
    let other = sources
        .iter()
        .filter(|candidate| candidate.id != source.id)
        .filter(|candidate| {
            candidate
                .monitor
                .as_ref()
                .is_some_and(|m| m.device.eq_ignore_ascii_case(&identity.device))
        })
        .count();
    assert_eq!(other, 0, "one display cannot be offered under two ids");

    let latest = std::sync::Arc::new(LatestImage::default());
    let mut capture = ScreenCapture::start(source.clone(), (1280, 720), 30, latest.clone())
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
        "captured {}×{} from {}; first frame {}×{}",
        image.width, image.height, source.title, image.width, image.height
    );
    assert!(image.width >= 2 && image.height >= 2);
    assert!(image.i420.width() == image.width && image.i420.height() == image.height);
    capture.stop();
    assert!(!source.describe().is_empty());

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
    MonitorIdentity {
        device: device.into(),
        bounds: MonitorRect::new(0, 0, width, height),
        work: MonitorRect::new(0, 0, width, height),
        width,
        height,
    }
}

#[test]
fn a_monitor_source_identity_ignores_its_mutable_label() {
    let identity = descriptor(r"\\.\DISPLAY1", 1920, 1080);
    let source = ScreenSource {
        id: 7,
        kind: SourceKind::Monitor,
        title: "Экран 1".into(),
        display_id: -1,
        width: 1920,
        height: 1080,
        monitor: Some(identity.clone()),
    };
    let renamed = ScreenSource {
        title: "Другое имя".into(),
        width: 800,
        height: 600,
        ..source.clone()
    };
    assert_eq!(source.source_id(), renamed.source_id());
    assert_eq!(source.identity(), renamed.identity());
    assert_eq!(source.identity(), Some(SourceIdentity::Monitor(identity)));
    assert_ne!(source.describe(), renamed.describe());

    // The card/focus key follows the display, not the capturer number.
    let moved = ScreenSource {
        id: 9,
        ..source.clone()
    };
    assert_eq!(source.focus_key(), moved.focus_key());
    let other = ScreenSource {
        monitor: Some(descriptor(r"\\.\DISPLAY2", 2560, 1440)),
        ..source.clone()
    };
    assert_ne!(source.focus_key(), other.focus_key());
}
