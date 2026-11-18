use super::*;

use std::sync::atomic::AtomicUsize;

struct FakeSink {
    writes: Arc<Mutex<Vec<Vec<i16>>>>,
}

impl Sink for FakeSink {
    fn writable(&self) -> bool {
        true
    }

    fn write(&mut self, block: &[i16]) -> bool {
        if let Ok(mut writes) = self.writes.lock() {
            writes.push(block.to_vec());
        }
        true
    }

    fn failed(&self) -> bool {
        false
    }

    fn buffered_blocks(&self) -> u16 {
        0
    }

    fn close(&mut self) {}
}

fn failing_then_open(
    failures: usize,
    writes: Arc<Mutex<Vec<Vec<i16>>>>,
) -> (Arc<OpenSink>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let worker_calls = calls.clone();
    let open: OpenSink = Box::new(move |_index, _rate| {
        if worker_calls.fetch_add(1, Ordering::SeqCst) < failures {
            Err("fake device unavailable".into())
        } else {
            Ok(Box::new(FakeSink {
                writes: writes.clone(),
            }) as Box<dyn Sink>)
        }
    });
    (Arc::new(open), calls)
}

fn test_clip(capacity: usize) -> (PlayoutClip, Receiver<Msg>, Arc<Metrics>) {
    let (sender, receiver) = sync_channel(capacity);
    let metrics = Arc::new(Metrics::default());
    let clip = PlayoutClip {
        sender,
        metrics: metrics.clone(),
    };
    (clip, receiver, metrics)
}

#[test]
fn a_failed_sink_is_retried_after_the_backoff_and_then_restores_audio() {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let (open, calls) = failing_then_open(2, writes.clone());
    let now = Instant::now();
    let mut attempts = OpenAttempts::default();

    assert!(
        open_sink(&open, None, &mut attempts, now).is_err(),
        "the first open fails"
    );
    assert_eq!(attempts.attempts(), 1);
    assert!(
        !attempts.due(now + Duration::from_millis(100)),
        "a retry must wait for the backoff"
    );
    assert!(
        open_sink(&open, None, &mut attempts, now + Duration::from_millis(600)).is_err(),
        "the second open fails"
    );
    assert_eq!(attempts.attempts(), 2);
    let later = now + Duration::from_millis(600) + Duration::from_secs(1);
    assert!(attempts.due(later), "the later tick is allowed to retry");

    let playout = open_sink(&open, None, &mut attempts, later).expect("the third open succeeds");
    assert!(playout.running(), "the restored sink is live");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(attempts.attempts(), 0, "a success resets the ladder");
    // The recovered mixer really reaches the sink with the attached frame.
    playout
        .clip()
        .push_frame("track", vec![2_000_i16; BLOCK_SAMPLES]);
    let deadline = Instant::now() + Duration::from_millis(400);
    while writes.lock().map(|w| w.is_empty()).unwrap_or(false) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !writes.lock().unwrap().is_empty(),
        "the restored sink receives a mixed block"
    );
}

#[test]
fn the_fifth_failure_requests_the_adm_fallback_instead_of_retrying() {
    let now = Instant::now();
    let mut attempts = OpenAttempts::default();
    for expected in 1..MAX_OPEN_ATTEMPTS {
        assert!(matches!(
            attempts.record_failure(now),
            FailureOutcome::Retry(_)
        ));
        assert_eq!(attempts.attempts(), expected);
    }
    assert_eq!(
        attempts.record_failure(now),
        FailureOutcome::AdmFallback,
        "the budget is bounded at {MAX_OPEN_ATTEMPTS}"
    );
    attempts.reset(now);
    assert_eq!(attempts.attempts(), 0);
    assert!(attempts.due(now));
}

#[test]
fn a_full_source_channel_drops_the_frame_but_keeps_the_source() {
    let (clip, receiver, metrics) = test_clip(1);
    assert!(clip.push_frame("track", vec![1_i16; BLOCK_SAMPLES]));
    assert!(
        clip.push_frame("track", vec![2_i16; BLOCK_SAMPLES]),
        "a full channel is a dropped frame, not a dead source"
    );
    assert_eq!(metrics.queue_dropped.load(Ordering::Relaxed), 1);
    assert!(receiver.try_recv().is_ok());
    // Only a closed playout ends the task.
    drop(receiver);
    assert!(!clip.push_frame("track", vec![3_i16; BLOCK_SAMPLES]));
}

#[test]
fn the_state_slot_keeps_only_the_newest_gain_snapshot() {
    let slot = StateSlot::default();
    let first = MixState {
        bus: 1.0,
        sources: vec![("a".into(), Source::default())],
    };
    let second = MixState {
        bus: 0.5,
        sources: vec![(
            "b".into(),
            Source {
                gain: 1.5,
                muted: true,
            },
        )],
    };
    slot.store(first);
    slot.store(second.clone());
    let taken = slot.take().expect("the newest snapshot is kept");
    assert_eq!(taken.bus, 0.5);
    assert_eq!(taken.sources[0].0, "b");
    assert!(slot.take().is_none(), "taking consumes the slot");
}

#[test]
fn the_tap_fans_out_post_mix_blocks_and_drops_a_stalled_subscriber() {
    let tap = Tap::default();
    let live = tap.subscribe();
    let stalled = tap.subscribe();
    tap.emit(&[7_i16; BLOCK_SAMPLES]);
    assert_eq!(
        live.try_recv().map(|block| block.len()),
        Some(BLOCK_SAMPLES)
    );
    assert!(stalled.try_recv().is_some());
    for _ in 0..20 {
        tap.emit(&[9_i16; BLOCK_SAMPLES]);
        // The live subscriber keeps consuming, so only the stalled one is
        // removed when its bounded queue fills.
        while live.try_recv().is_some() {}
    }
    // The stalled receiver's 16-slot queue is full, so it is removed while
    // the live subscriber keeps receiving: a recorder that stops reading
    // cannot block the room mixer.
    assert_eq!(tap.subscribers(), 1);
    tap.emit(&[11_i16; BLOCK_SAMPLES]);
    assert!(live.try_recv().is_some());
}
