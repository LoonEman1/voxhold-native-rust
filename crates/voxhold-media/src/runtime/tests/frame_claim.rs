//! Picture-request regressions: a claim is spent only by a read that really
//! converted a frame, so a batch that skips the decoder keeps the request.

use super::*;

/// The request-preserving shell of `Live::drain_viewer`: one bounded batch
/// over the current engine. Applying viewer updates is left out because only
/// the claim on `frame_request` is under test here.
fn drain_batch_for_tests(
    stream: &mut Option<StreamViewer>,
    render: &mut VideoFrame,
    requests: &Requests,
) -> Vec<EngineUpdate> {
    let mut request = FrameClaim::take(requests.frame_request.clone());
    let mut updates = Vec::new();
    for _ in 0..FRAMES_PER_TICK {
        let Some(engine) = stream.as_mut() else {
            break;
        };
        let Some(update) = engine.poll(render, 160, 90, request.pending()) else {
            break;
        };
        updates.push(update);
    }
    updates
}

#[test]
fn a_batch_that_ends_without_a_read_keeps_the_picture_request() {
    let requests = Requests::default();
    let (engine, _commands, events, video) = StreamViewer::detached_for_tests();
    let mut stream = Some(engine);
    let mut render = VideoFrame::default();

    // A signaling or audio update ends the batch without touching the
    // decoder. The request must survive for the next round.
    requests.request_frame();
    events.push(Event::Track {
        track_id: "audio".into(),
        stream_id: None,
        mid: None,
    });
    assert!(drain_batch_for_tests(&mut stream, &mut render, &requests).is_empty());
    assert!(
        requests.frame_request.load(Ordering::SeqCst),
        "a non-video update must not consume the request"
    );

    // A batch with no engine is the same: the player's request is still
    // pending, not spent.
    requests.frame_request.store(false, Ordering::SeqCst);
    requests.request_frame();
    let mut closed: Option<StreamViewer> = None;
    assert!(drain_batch_for_tests(&mut closed, &mut render, &requests).is_empty());
    assert!(
        requests.frame_request.load(Ordering::SeqCst),
        "a closed player keeps the request for its next engine"
    );

    // A completed read consumes exactly this request.
    requests.frame_request.store(false, Ordering::SeqCst);
    assert!(video.publish_i420(libwebrtc::video_frame::I420Buffer::new_black(320, 180)));
    events.push(Event::Frame);
    requests.request_frame();
    assert!(matches!(
        drain_batch_for_tests(&mut stream, &mut render, &requests).as_slice(),
        [EngineUpdate::Frame]
    ));
    assert!(
        !requests.frame_request.load(Ordering::SeqCst),
        "a successful read consumed the request"
    );
}
