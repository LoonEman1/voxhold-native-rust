//! Bounded request/update behaviour of the voice session.

use super::*;
use crate::runtime::requests::ScreenCommand;
use crate::stream::EngineUpdate;
use std::collections::HashMap;

mod player;

fn publication(connection: &str) -> Publication {
    Publication {
        server_id: 1,
        channel_id: 10,
        publisher_user_id: 1,
        publisher_connection_id: connection.into(),
        mode: "server".into(),
        codec: "vp8".into(),
        has_audio: false,
        viewer_count: 1,
        started_at_ms: 1,
    }
}

#[test]
fn screen_commands_keep_their_order_and_listings_keep_the_newest_generation() {
    use crate::publisher::{Codec, PublishRequest, Quality};
    use crate::screen_capture::{ScreenSource, SourceKind};

    let requests = Requests::default();
    let request = PublishRequest {
        source: ScreenSource {
            id: 1,
            kind: SourceKind::Monitor,
            title: "Экран 1".into(),
            display_id: -1,
            width: 1920,
            height: 1080,
            monitor: None,
        },
        quality: Quality::default(),
        codec: Codec::Vp8,
        audio: false,
        audio_scope: crate::system_audio::AudioScope::Device,
    };
    // Opening the picker must reach the worker before the chosen source.
    crate::runtime::requests::push_screen(&requests, ScreenCommand::OpenPicker { picker: 1 });
    crate::runtime::requests::push_screen(
        &requests,
        ScreenCommand::Publish {
            picker: 1,
            request,
            stream_permission: true,
            slot_available: true,
        },
    );
    assert!(matches!(
        requests.take(),
        Some(Pending::Screen(ScreenCommand::OpenPicker { picker: 1 }))
    ));
    assert!(matches!(
        requests.take(),
        Some(Pending::Screen(ScreenCommand::Publish { picker: 1, .. }))
    ));

    // A late older listing cannot replace a newer one of the same kind.
    crate::runtime::requests::set_source_request(&requests, SourceKind::Monitor, 4);
    crate::runtime::requests::set_source_request(&requests, SourceKind::Monitor, 3);
    crate::runtime::requests::set_source_request(&requests, SourceKind::Window, 7);
    let first = requests.take();
    let second = requests.take();
    let generations: Vec<u64> = [first, second]
        .into_iter()
        .filter_map(|pending| match pending {
            Some(Pending::ScreenSources(request)) => Some(request.generation),
            _ => None,
        })
        .collect();
    assert_eq!(generations, vec![4, 7], "newest listing per kind only");
}

#[test]
fn control_slots_keep_only_the_latest_value() {
    let requests = Requests::default();
    *requests.controls.lock().unwrap() = Some((
        Controls {
            muted: false,
            ..Controls::default()
        },
        false,
    ));
    *requests.controls.lock().unwrap() = Some((
        Controls {
            muted: true,
            ..Controls::default()
        },
        true,
    ));
    match requests.take() {
        Some(Pending::Controls(controls, self_muted)) => {
            assert!(controls.muted);
            assert!(self_muted);
        }
        _ => panic!("the latest control state must win"),
    }
}

#[test]
fn watch_and_close_share_one_last_value_slot() {
    let requests = Requests::default();
    requests.stream.lock().unwrap().pending = Some(StreamCommand::Watch {
        selection: 1,
        publication: publication("b"),
    });
    requests.stream.lock().unwrap().pending = Some(StreamCommand::Watch {
        selection: 2,
        publication: publication("c"),
    });
    match requests.take() {
        Some(Pending::Stream(StreamCommand::Watch {
            selection,
            publication,
        })) => {
            assert_eq!(selection, 2);
            assert_eq!(publication.publisher_connection_id, "c");
        }
        _ => panic!("the last selection wins"),
    }
    requests.stream.lock().unwrap().pending = Some(StreamCommand::Watch {
        selection: 3,
        publication: publication("d"),
    });
    // A close from a stale caller must not erase the newer watch.
    assert!(!set_stream_command(
        &requests,
        StreamCommand::Close { selection: 1 }
    ));
    assert!(matches!(
        requests.take(),
        Some(Pending::Stream(StreamCommand::Watch { selection: 3, .. }))
    ));
    set_stream_command(&requests, StreamCommand::Close { selection: 3 });
    assert!(matches!(
        requests.take(),
        Some(Pending::Stream(StreamCommand::Close { selection: 3 }))
    ));
    assert!(requests.stream.lock().unwrap().pending.is_none());

    // A delayed close for the previous selection cannot erase B.
    assert!(set_stream_command(
        &requests,
        StreamCommand::Watch {
            selection: 5,
            publication: publication("b"),
        }
    ));
    assert!(!set_stream_command(
        &requests,
        StreamCommand::Close { selection: 4 }
    ));
    assert!(matches!(
        requests.take(),
        Some(Pending::Stream(StreamCommand::Watch { selection: 5, .. }))
    ));
}

#[test]
fn a_close_newer_than_a_displaced_watch_closes_the_running_view() {
    let requests = Requests::default();
    // A is already running and the worker has recorded its command number.
    set_stream_command(
        &requests,
        StreamCommand::Watch {
            selection: 1,
            publication: publication("a"),
        },
    );
    assert!(matches!(
        requests.take(),
        Some(Pending::Stream(StreamCommand::Watch { selection: 1, .. }))
    ));
    let mut viewer = Viewer::new(1, 10);
    let now = Instant::now();
    viewer.on_server_event(
        "stream.snapshot",
        "",
        &serde_json::json!({"streams": [publication("a")]}),
        now,
    );
    viewer.set_joined(true, now);
    let started = viewer.select(&publication("a"), 1, now);
    assert!(
        started
            .iter()
            .any(|outcome| matches!(outcome, ViewerOutcome::Start { selection: 1, .. }))
    );

    // Watch(B) and Close(B) both land in the slot before the worker reads it.
    set_stream_command(
        &requests,
        StreamCommand::Watch {
            selection: 2,
            publication: publication("b"),
        },
    );
    set_stream_command(&requests, StreamCommand::Close { selection: 2 });
    let command = match requests.take() {
        Some(Pending::Stream(command)) => command,
        _ => panic!("the close replaces the watch"),
    };
    assert!(matches!(command, StreamCommand::Close { selection: 2 }));

    let outcomes = viewer.leave_selection(2, now);
    assert!(
        outcomes.contains(&ViewerOutcome::Close),
        "A must stop: {outcomes:?}"
    );
    assert!(outcomes.contains(&ViewerOutcome::Clear { selection: 2 }));
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, ViewerOutcome::Send(signal) if signal.kind == "stream.leave"))
            .count(),
        1
    );
    assert!(
        !outcomes
            .iter()
            .any(|outcome| matches!(outcome, ViewerOutcome::Start { publication, .. } if publication.publisher_connection_id == "b")),
        "the displaced watch B must never start"
    );
    assert!(viewer.watching().is_none());

    // Once the worker has taken B, a delayed close for A is still rejected.
    assert!(set_stream_command(
        &requests,
        StreamCommand::Watch {
            selection: 4,
            publication: publication("b"),
        }
    ));
    assert!(matches!(
        requests.take(),
        Some(Pending::Stream(StreamCommand::Watch { selection: 4, .. }))
    ));
    assert!(!set_stream_command(
        &requests,
        StreamCommand::Close { selection: 3 }
    ));
    assert!(requests.stream.lock().unwrap().pending.is_none());
}

#[test]
fn explicit_output_selection_is_never_coalesced_into_controls() {
    let requests = Requests::default();
    *requests.output.lock().unwrap() = Some(Some("usb-a".into()));
    assert!(matches!(
        requests.take(),
        Some(Pending::Output(Some(output))) if output == "usb-a"
    ));
    // Selecting the system default is a real request, not an empty slot.
    *requests.output.lock().unwrap() = Some(None);
    assert!(matches!(requests.take(), Some(Pending::Output(None))));
}

#[test]
fn explicit_input_selection_outlives_a_later_control_update() {
    let requests = Requests::default();
    *requests.input.lock().unwrap() = Some(InputRequest {
        input: Some("usb".into()),
        generation: 7,
    });
    // The PTT/DSP update arrives after the explicit request but must not
    // replace it.
    *requests.controls.lock().unwrap() = Some((
        Controls {
            muted: true,
            ..Controls::default()
        },
        false,
    ));
    match requests.take() {
        Some(Pending::Input(request)) => {
            assert_eq!(request.generation, 7);
            assert_eq!(request.input.as_deref(), Some("usb"));
        }
        _ => panic!("the explicit input request must be drained first"),
    }
    assert!(matches!(requests.take(), Some(Pending::Controls(..))));
}

#[test]
fn a_newer_explicit_input_replaces_the_pending_one() {
    let requests = Requests::default();
    *requests.input.lock().unwrap() = Some(InputRequest {
        input: Some("first".into()),
        generation: 1,
    });
    *requests.input.lock().unwrap() = Some(InputRequest {
        input: Some("second".into()),
        generation: 2,
    });
    match requests.take() {
        Some(Pending::Input(request)) => {
            assert_eq!(request.generation, 2);
            assert_eq!(request.input.as_deref(), Some("second"));
        }
        _ => panic!("the last explicit selection wins"),
    }
}

#[test]
fn an_older_input_generation_cannot_replace_a_newer_pending_one() {
    let mut slot = Some(InputRequest {
        input: Some("newer".into()),
        generation: 5,
    });
    set_input_request(
        &mut slot,
        InputRequest {
            input: Some("older".into()),
            generation: 4,
        },
    );
    let request = slot
        .as_ref()
        .expect("the newer selection must stay pending");
    assert_eq!(request.generation, 5);
    assert_eq!(request.input.as_deref(), Some("newer"));
    set_input_request(
        &mut slot,
        InputRequest {
            input: Some("newest".into()),
            generation: 6,
        },
    );
    assert_eq!(slot.unwrap().input.as_deref(), Some("newest"));
}

#[test]
fn local_mutes_are_bounded() {
    let requests = Requests::default();
    for index in 0..(LOCAL_MUTE_CAPACITY + 20) {
        if let Ok(mut queue) = requests.local_mutes.lock() {
            if queue.len() >= LOCAL_MUTE_CAPACITY {
                queue.pop_front();
            }
            queue.push_back((format!("c{index}"), true));
        }
    }
    assert_eq!(
        requests.local_mutes.lock().unwrap().len(),
        LOCAL_MUTE_CAPACITY
    );
}

#[test]
fn updates_coalesce_high_frequency_values_and_keep_the_terminal_reason() {
    let updates = Updates::new();
    let mut speaking = HashMap::new();
    speaking.insert("a".to_string(), true);
    updates.push(VoiceUpdate::Speaking(speaking.clone()));
    speaking.insert("a".to_string(), false);
    updates.push(VoiceUpdate::Speaking(speaking));
    updates.push(VoiceUpdate::StreamStatus {
        selection: 4,
        status: "first".into(),
    });
    updates.push(VoiceUpdate::StreamStatus {
        selection: 4,
        status: "second".into(),
    });
    updates.push(VoiceUpdate::Ended(Some("failed".into())));
    let mut kinds = Vec::new();
    while let Some(update) = updates.poll() {
        kinds.push(match update {
            VoiceUpdate::Speaking(map) => {
                assert!(!map["a"]);
                "speaking"
            }
            VoiceUpdate::StreamStatus { status, .. } => {
                assert_eq!(status, "second");
                "status"
            }
            VoiceUpdate::Ended(Some(reason)) => {
                assert_eq!(reason, "failed");
                "ended"
            }
            _ => "other",
        });
    }
    assert_eq!(
        kinds,
        vec!["speaking", "status", "ended"],
        "the end must keep the error reason"
    );
}

#[test]
fn a_burst_of_device_notices_stays_bounded() {
    let updates = Updates::new();
    for index in 0..(super::updates::UPDATE_CAPACITY + 30) {
        updates.push(VoiceUpdate::DeviceError(format!("device {index}")));
    }
    let mut count = 0;
    while updates.poll().is_some() {
        count += 1;
    }
    assert_eq!(count, super::updates::UPDATE_CAPACITY);
}

fn screen_request() -> crate::publisher::PublishRequest {
    use crate::publisher::{Codec, Quality};
    use crate::screen_capture::{ScreenSource, SourceKind};
    crate::publisher::PublishRequest {
        source: ScreenSource {
            id: 1,
            kind: SourceKind::Monitor,
            title: "Экран 1".into(),
            display_id: -1,
            width: 1920,
            height: 1080,
            monitor: None,
        },
        quality: Quality::default(),
        codec: Codec::Vp8,
        audio: false,
        audio_scope: crate::system_audio::AudioScope::Device,
    }
}

fn screen_publisher() -> crate::publisher::Publisher {
    use crate::publisher::Access;
    let mut publisher = crate::publisher::Publisher::new(1, 10);
    publisher.set_connection(Some("me".into()));
    publisher.set_access(Access {
        joined: true,
        stream_permission: true,
        slots_available: true,
    });
    publisher
}

/// A publisher whose attempt reached the air: both engine confirmations.
fn live_screen_publisher(now: Instant) -> crate::publisher::Publisher {
    use crate::publisher::EngineEvent;
    let mut publisher = screen_publisher();
    publisher.begin_picker(1);
    publisher.publish(1, screen_request(), false, now);
    publisher.engine_ready(now, false, None);
    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::Connected),
        now,
    );
    publisher.on_engine(
        EngineEvent::State(crate::publisher::Connection::FirstFrame),
        now,
    );
    assert_eq!(publisher.phase(), crate::publisher::Phase::Live);
    publisher
}

#[test]
fn a_stale_deferred_bitrate_never_reaches_a_freed_worker_queue() {
    use crate::publisher::engine::BitrateSend;
    use crate::publisher::{BITRATE_CONFIRM_TIMEOUT, Outcome as PublisherOutcome, Phase};

    let now = Instant::now();
    let mut publisher = live_screen_publisher(now);
    let attempt = publisher.sharing().attempt;
    let previous = publisher.request().cloned().expect("live request");
    let previous_confirmed = publisher.sharing().confirmed.clone();
    let mut next = screen_request();
    next.quality.bitrate_kbps = 9000;
    let outcomes = publisher.apply_quality(7, next.clone(), now);
    let Some(PublisherOutcome::SetBitrate { quality, .. }) = outcomes.first() else {
        panic!("a live bitrate change is one command: {outcomes:?}");
    };
    let quality = *quality;
    let mut queue = super::screen::BitrateQueue::default();
    queue.request(attempt, 7, quality);
    // The bounded worker queue was full: the value stays deferred.
    assert!(
        super::screen::deliver_bitrate(&mut queue, &mut publisher, |_, _, _| BitrateSend::Full)
            .is_empty()
    );
    assert!(queue.is_pending(), "the deferred value survives a tick");

    // The confirmation deadline passes: the operation is refused and is no
    // longer awaited, while the attempt keeps running.
    let expired = publisher.tick(now + BITRATE_CONFIRM_TIMEOUT);
    assert!(matches!(expired.as_slice(), [PublisherOutcome::State(_)]));
    assert_eq!(publisher.phase(), Phase::Live);
    assert!(
        !publisher.awaits_bitrate(7),
        "the timeout ends the operation"
    );

    // The queue frees up only after the timeout. The next tick must drop the
    // deferred value instead of sending a bitrate the dialog gave up on.
    let late = super::screen::deliver_bitrate(&mut queue, &mut publisher, |_, _, _| {
        panic!("a stale bitrate must not reach the engine")
    });
    assert!(late.is_empty(), "a stale retry reports nothing: {late:?}");
    assert!(!queue.is_pending(), "the stale value is dropped");

    // The live attempt, the confirmed limits and the reported error stay.
    assert_eq!(publisher.phase(), Phase::Live);
    assert_eq!(publisher.request(), Some(&previous));
    assert_eq!(publisher.sharing().confirmed, previous_confirmed);
    assert!(
        publisher.sharing().error.is_some(),
        "the refused operation stays visible"
    );

    // A retry under a new operation is delivered as usual.
    let retried = publisher.apply_quality(8, next, now + BITRATE_CONFIRM_TIMEOUT);
    assert!(matches!(
        retried.first(),
        Some(PublisherOutcome::SetBitrate { op: 8, .. })
    ));
    queue.request(attempt, 8, quality);
    let mut sent = None;
    let delivered =
        super::screen::deliver_bitrate(&mut queue, &mut publisher, |attempt, op, value| {
            sent = Some((attempt, op, value));
            BitrateSend::Sent
        });
    assert!(delivered.is_empty());
    assert_eq!(
        sent,
        Some((attempt, 8, quality)),
        "the retry reaches the engine under its own operation"
    );
}

#[test]
fn a_slow_engine_drop_does_not_block_the_release_path() {
    use std::sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    /// A stand-in for `PublisherEngine` whose `Drop` takes as long as the gate
    /// stays closed, like the bounded join of a capture/publisher worker. The
    /// wait has its own timeout, so a release that ran on the caller's thread
    /// fails the elapsed assertion instead of hanging the suite.
    struct GatedEngine {
        gate: Arc<(Mutex<bool>, Condvar)>,
        released: Arc<AtomicBool>,
    }

    impl Drop for GatedEngine {
        fn drop(&mut self) {
            let (open, changed) = &*self.gate;
            let mut open = open.lock().unwrap();
            while !*open {
                let (next, timeout) = changed.wait_timeout(open, Duration::from_secs(2)).unwrap();
                open = next;
                if timeout.timed_out() {
                    break;
                }
            }
            self.released.store(true, Ordering::SeqCst);
        }
    }

    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let released = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    // `screen::release_off_loop` is the same primitive plus the live probe; an
    // engine that joins its worker for seconds must not hold the session loop.
    super::screen::release_on_worker(
        GatedEngine {
            gate: gate.clone(),
            released: released.clone(),
        },
        || {},
    );
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "the session path waited for the engine drop: {:?}",
        started.elapsed()
    );
    assert!(
        !released.load(Ordering::SeqCst),
        "the drop is still gated, so the caller must not have performed it"
    );

    // The engine is still released, just off the session path.
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !released.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < deadline,
            "the release thread never dropped the engine"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_failed_engine_start_cancels_the_attempt_without_stream_start() {
    use crate::publisher::{Outcome as PublisherOutcome, Phase};
    use crate::runtime::prep::{PrepBinding, PrepEvent, ScreenPrep, finish_prep};
    let now = Instant::now();
    let mut publisher = screen_publisher();
    publisher.begin_picker(1);
    let outcomes = publisher.publish(1, screen_request(), false, now);
    let PublisherOutcome::StartEngine { attempt, .. } = outcomes[0] else {
        unreachable!()
    };
    let binding = PrepBinding {
        attempt,
        server: 1,
        channel: 10,
        connection: Some("me".into()),
        access: super::screen::screen_access(true, true, true),
    };
    // The off-loop preparation fails: the attempt must be cancelled whole,
    // with neither `stream.start` nor a compensating `stream.stop`.
    let mut prep = ScreenPrep::<()>::start(binding.clone(), now, Duration::from_secs(5), |_| {
        Err("движок не создан".to_string())
    })
    .expect("the preparation thread starts");
    let event = loop {
        if let Some(event) = prep.poll(Instant::now()) {
            break event;
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    assert!(matches!(event, PrepEvent::Failed(_)));
    let finish = finish_prep(
        &mut publisher,
        &binding,
        &binding,
        super::screen::screen_access(true, true, true),
        event,
        now,
    );
    assert!(finish.engine.is_none());
    let outcomes = finish.outcomes;
    assert!(
        !outcomes
            .iter()
            .any(|outcome| matches!(outcome, PublisherOutcome::Send(signal) if signal.kind == "stream.start")),
        "a failed start must not reach the server: {outcomes:?}"
    );
    assert!(
        !outcomes
            .iter()
            .any(|outcome| matches!(outcome, PublisherOutcome::Send(signal) if signal.kind == "stream.stop")),
        "no start was sent, so no stop is sent: {outcomes:?}"
    );
    assert_eq!(publisher.sharing().phase, Phase::Failed);
    assert!(publisher.sharing().error.is_some());
    assert!(
        publisher
            .tick(now + crate::publisher::ANSWER_TIMEOUT)
            .is_empty(),
        "the failed start leaves no deadline"
    );
}

#[test]
fn taking_a_frame_requests_the_next_picture_from_the_worker() {
    let handle = VoiceHandle::detached_for_tests();
    assert!(handle.take_frame().is_none(), "no picture was prepared yet");
    assert!(
        handle.take_frame_request_for_tests(),
        "the UI must ask the background loop for the next picture"
    );
    // The flag is last-value: a repeat poll still leaves only one request, and
    // the worker consumes it exactly once.
    assert!(handle.take_frame().is_none());
    assert!(handle.take_frame_request_for_tests());
    assert!(
        !handle.take_frame_request_for_tests(),
        "the worker took the request once"
    );
}

#[test]
fn the_newest_screen_gate_is_a_last_value_request() {
    let requests = Requests::default();
    requests
        .screen_access
        .lock()
        .unwrap()
        .replace(ScreenAccess {
            stream_permission: false,
            slot_available: true,
        });
    requests
        .screen_access
        .lock()
        .unwrap()
        .replace(ScreenAccess {
            stream_permission: true,
            slot_available: false,
        });
    match requests.take() {
        Some(Pending::ScreenAccess(access)) => {
            assert!(access.stream_permission, "the newest value wins");
            assert!(!access.slot_available);
        }
        _ => panic!("the newest gate must reach the worker"),
    }
    assert!(requests.take().is_none());
}

#[test]
fn a_cached_repaint_does_not_confirm_a_recovering_stream() {
    let now = Instant::now();
    let a = publication("a");
    let mut viewer = Viewer::new(1, 10);
    viewer.on_server_event(
        "stream.snapshot",
        "",
        &serde_json::json!({"streams": [a.clone()]}),
        now,
    );
    viewer.set_joined(true, now);
    let started = viewer.select(&a, 1, now);
    assert!(
        started
            .iter()
            .any(|outcome| matches!(outcome, ViewerOutcome::Start { selection: 1, .. })),
        "the viewer must be watching before recovery: {started:?}"
    );
    // A working picture first, then the engine reports a broken transport and
    // arms the recovery deadline.
    assert!(viewer.on_frame().iter().any(|outcome| matches!(
        outcome,
        ViewerOutcome::Connected {
            selection: 1,
            connected: true
        }
    )));
    let recovering = viewer.on_engine_state("disconnected", now);
    assert!(recovering.iter().any(|outcome| matches!(
        outcome,
        ViewerOutcome::Connected {
            selection: 1,
            connected: false
        }
    )));
    assert!(viewer.watching().unwrap().recovering);

    // The last decoded frame is still in the store. A viewport-only re-read
    // must surface as a repaint, not as a fresh RTP frame.
    let (mut stream, _commands, events, video) = StreamViewer::detached_for_tests();
    assert!(video.publish_i420(libwebrtc::video_frame::I420Buffer::new_black(320, 180)));
    events.push(crate::Event::Frame);
    let mut render = crate::VideoFrame::default();
    let mut request = false;
    assert!(matches!(
        stream.poll(&mut render, 160, 90, &mut request),
        Some(EngineUpdate::Frame)
    ));
    // Only the picture surface changes now; no RTP frame is queued.
    assert!(matches!(
        stream.poll(&mut render, 320, 278, &mut request),
        Some(EngineUpdate::Repaint)
    ));
    // The static picture was converted once for the fresh frame and once for
    // the new surface, not once per poll.
    assert_eq!(video.conversions(), 2);

    // Exactly what `drain_viewer` does for the cached update: publish the
    // picture without any viewer transition.
    let slot = Mutex::new(None);
    let EngineEffect::Outcomes(outcomes) =
        apply_engine_update(&slot, &mut render, &mut viewer, EngineUpdate::Repaint, now)
    else {
        panic!("a repaint is a picture update, not an audio-output result");
    };
    assert!(
        outcomes.is_empty(),
        "a repaint must not emit status or Connected: {outcomes:?}"
    );
    let frame = slot
        .lock()
        .unwrap()
        .take()
        .expect("the repaint must reach the picture slot");
    assert_eq!((frame.selection, frame.width, frame.height), (1, 320, 180));

    let watching = viewer
        .watching()
        .expect("the recovering stream stays selected");
    assert!(watching.recovering, "a repaint must not clear recovery");
    assert!(
        !watching.connected,
        "a repaint must not report the stream as connected"
    );
    // The deadline armed by `on_engine_state` is still live: the runtime tick
    // still closes and restarts instead of leaving a frozen "connected" view.
    let expired = viewer.tick(now + crate::viewer::START_TIMEOUT + Duration::from_millis(1));
    assert!(!expired.is_empty(), "the recovery deadline must still fire");
    assert!(
        expired
            .iter()
            .any(|outcome| matches!(outcome, ViewerOutcome::Close)),
        "the failed attempt is closed: {expired:?}"
    );
}

#[test]
fn a_full_bitrate_queue_defers_the_newest_value_and_a_closed_worker_is_terminal() {
    use crate::publisher::Quality;
    use crate::publisher::engine::BitrateSend;

    let mut queue = super::screen::BitrateQueue::default();
    let first = Quality {
        bitrate_kbps: 1200,
        ..Quality::default()
    };
    queue.request(4, 1, first);
    assert_eq!(
        queue.flush(4, |_, _, _| BitrateSend::Full),
        super::screen::BitrateDelivery::Full,
        "a full command queue is deferred, not failed"
    );
    assert!(
        queue.is_pending(),
        "the deferred value must survive for the next tick"
    );

    // The newest dialog value replaces the deferred one; only it is sent.
    let newest = Quality {
        bitrate_kbps: 900,
        ..Quality::default()
    };
    queue.request(4, 2, newest);
    let mut sent = None;
    assert_eq!(
        queue.flush(4, |attempt, op, quality| {
            sent = Some((attempt, op, quality));
            BitrateSend::Sent
        }),
        super::screen::BitrateDelivery::Sent
    );
    assert_eq!(
        sent.map(|(attempt, op, quality)| (attempt, op, quality.bitrate_kbps)),
        Some((4, 2, 900)),
        "the retry carries the newest value and the same operation tag"
    );
    assert!(
        !queue.is_pending(),
        "an accepted value is no longer pending"
    );

    // A request that belongs to an older attempt never reaches a new sender.
    queue.request(3, 3, newest);
    assert_eq!(
        queue.flush(4, |_, _, _| panic!("a stale attempt must not be sent")),
        super::screen::BitrateDelivery::Idle
    );
    assert!(!queue.is_pending());

    // A closed worker is terminal: the value is dropped, not retried, and the
    // publisher ends the live attempt instead of claiming the new limit.
    queue.request(4, 4, newest);
    assert_eq!(
        queue.flush(4, |_, _, _| BitrateSend::Closed),
        super::screen::BitrateDelivery::Closed
    );
    assert!(
        !queue.is_pending(),
        "a dead worker must not be retried forever"
    );
    assert_eq!(
        queue.flush(4, |_, _, _| panic!("nothing may be retried")),
        super::screen::BitrateDelivery::Idle
    );
}

#[test]
fn a_pending_viewer_switch_is_left_before_the_screen_publication() {
    use crate::publisher::Outcome as PublisherOutcome;
    let now = Instant::now();
    let mut viewer = Viewer::new(1, 10);
    viewer.on_server_event(
        "stream.snapshot",
        "",
        &serde_json::json!({"streams": [publication("a"), publication("b")]}),
        now,
    );
    viewer.set_joined(true, now);
    let started = viewer.select(&publication("a"), 1, now);
    assert!(
        started
            .iter()
            .any(|outcome| matches!(outcome, ViewerOutcome::Start { selection: 1, .. })),
        "A starts: {started:?}"
    );

    // B is selected while A is still leaving: no `watching()` but a live
    // intent and an unacknowledged `stream.leave`.
    let switching = viewer.select(&publication("b"), 2, now);
    assert!(viewer.leaving(), "the switch waits for stream.left");
    assert!(viewer.watching().is_none());
    assert!(switching.iter().any(
        |outcome| matches!(outcome, ViewerOutcome::Send(signal) if signal.kind == "stream.leave")
    ));
    assert!(super::screen::viewer_conflicts(&viewer));

    let mut publisher = screen_publisher();
    publisher.begin_picker(1);
    let outcomes = publisher.publish(1, screen_request(), true, now);
    assert!(
        matches!(outcomes[0], PublisherOutcome::LeaveViewer),
        "the pending switch is a conflict: {outcomes:?}"
    );
    assert!(
        !outcomes
            .iter()
            .any(|outcome| matches!(outcome, PublisherOutcome::StartEngine { .. })),
        "the capture waits for the viewer to be gone: {outcomes:?}"
    );
    // Once the leave is acknowledged the publication starts.
    let left = publisher.viewer_left(now + Duration::from_millis(50));
    assert!(matches!(left[0], PublisherOutcome::StartEngine { .. }));
}
