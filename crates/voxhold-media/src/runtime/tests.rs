//! Bounded request/update behaviour of the voice session.

use super::*;

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
    *requests.stream.lock().unwrap() = Some(StreamCommand::Watch {
        selection: 1,
        publication: publication("b"),
    });
    *requests.stream.lock().unwrap() = Some(StreamCommand::Watch {
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
    *requests.stream.lock().unwrap() = Some(StreamCommand::Watch {
        selection: 3,
        publication: publication("d"),
    });
    // A close from a stale caller must not erase the newer watch.
    *requests.stream.lock().unwrap() = Some(StreamCommand::Close { selection: 1 });
    *requests.stream.lock().unwrap() = Some(StreamCommand::Watch {
        selection: 3,
        publication: publication("d"),
    });
    assert!(matches!(
        requests.take(),
        Some(Pending::Stream(StreamCommand::Watch { selection: 3, .. }))
    ));
    *requests.stream.lock().unwrap() = Some(StreamCommand::Close { selection: 3 });
    assert!(matches!(
        requests.take(),
        Some(Pending::Stream(StreamCommand::Close { selection: 3 }))
    ));
    assert!(requests.stream.lock().unwrap().is_none());

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
    for index in 0..(UPDATE_CAPACITY + 30) {
        updates.push(VoiceUpdate::DeviceError(format!("device {index}")));
    }
    let mut count = 0;
    while updates.poll().is_some() {
        count += 1;
    }
    assert_eq!(count, UPDATE_CAPACITY);
}
