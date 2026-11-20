use super::*;

use std::sync::atomic::AtomicUsize;

use super::tap::TapRead;
use crate::voice::Session;
use crate::{Event, Events};

struct FakeSink {
    writes: Arc<Mutex<Vec<Vec<i16>>>>,
    failed: bool,
}

impl Sink for FakeSink {
    fn writable(&self) -> bool {
        !self.failed
    }

    fn write(&mut self, block: &[i16]) -> bool {
        if self.failed {
            return false;
        }
        if let Ok(mut writes) = self.writes.lock() {
            writes.push(block.to_vec());
        }
        true
    }

    fn failed(&self) -> bool {
        self.failed
    }

    fn buffered_blocks(&self) -> u16 {
        0
    }

    fn close(&mut self) {}
}

fn good_sink(writes: Arc<Mutex<Vec<Vec<i16>>>>) -> Box<dyn Sink> {
    Box::new(FakeSink {
        writes,
        failed: false,
    })
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
            Ok(good_sink(writes.clone()))
        }
    });
    (Arc::new(open), calls)
}

/// A factory that always opens and whose sink reports failure immediately: it
/// simulates a driver that accepts the handle and then dies.
fn always_open_broken_sink() -> (Arc<OpenSink>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let worker_calls = calls.clone();
    let open: OpenSink = Box::new(move |_index, _rate| {
        worker_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeSink {
            writes: Arc::new(Mutex::new(Vec::new())),
            failed: true,
        }) as Box<dyn Sink>)
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

/// The same decision sequence as `Session::tick_playout`, without devices. It
/// is the test driver for the shipped `SinkRecovery` and `open_sink`.
struct TickHarness {
    open: Arc<OpenSink>,
    recovery: SinkRecovery,
    playout: Option<Playout>,
    now: Instant,
    fallbacks: u32,
}

impl TickHarness {
    fn new(open: Arc<OpenSink>) -> Self {
        Self {
            open,
            recovery: SinkRecovery::default(),
            playout: None,
            now: Instant::now(),
            fallbacks: 0,
        }
    }

    fn tick(&mut self) {
        // The playout thread is real: give it a chance to publish a death.
        std::thread::sleep(Duration::from_millis(3));
        self.now += Duration::from_millis(250);
        if self.recovery.phase() == PlayoutPhase::AdmFallback {
            return;
        }
        if let Some(playout) = &self.playout {
            if playout.running() {
                if self.recovery.phase() == PlayoutPhase::Unstable && playout.stable(self.now) {
                    self.recovery.mark_stable();
                }
                return;
            }
            // The sink opened but died before proving stability.
            self.playout = None;
            if self.recovery.sink_died(self.now) == FailureOutcome::AdmFallback {
                self.fallbacks += 1;
                return;
            }
        }
        if !self.recovery.due(self.now) {
            return;
        }
        match open_sink(&self.open, None, &mut self.recovery, self.now) {
            Ok(playout) => self.playout = Some(playout),
            Err(FailureOutcome::AdmFallback) => self.fallbacks += 1,
            Err(FailureOutcome::Retry(_)) => {}
        }
    }
}

#[test]
fn a_failed_open_is_retried_after_the_backoff_and_then_restores_audio() {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let (open, calls) = failing_then_open(2, writes.clone());
    let now = Instant::now();
    let mut recovery = SinkRecovery::default();

    assert!(
        open_sink(&open, None, &mut recovery, now).is_err(),
        "the first open fails"
    );
    assert_eq!(recovery.failures(), 1);
    assert!(
        !recovery.due(now + Duration::from_millis(100)),
        "a retry must wait for the backoff"
    );
    assert!(
        open_sink(&open, None, &mut recovery, now + Duration::from_millis(600)).is_err(),
        "the second open fails"
    );
    assert_eq!(recovery.failures(), 2);
    let later = now + Duration::from_millis(600) + Duration::from_secs(1);
    assert!(recovery.due(later), "the later tick is allowed to retry");

    let playout = open_sink(&open, None, &mut recovery, later).expect("the third open succeeds");
    assert!(playout.running(), "the opened sink is live");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        recovery.phase(),
        PlayoutPhase::Unstable,
        "an open alone is not a recovery"
    );
    assert_eq!(
        recovery.failures(),
        2,
        "the budget is kept until stability is proven"
    );
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
        "the opened sink receives a mixed block"
    );
}

#[test]
fn a_sink_that_opens_and_dies_still_spends_the_shared_budget() {
    let (open, calls) = always_open_broken_sink();
    let mut harness = TickHarness::new(open);
    // 40 ticks are 10 s of simulated time: enough for the whole ladder and
    // several would-be reopen periods.
    for _ in 0..40 {
        harness.tick();
    }
    assert_eq!(
        harness.fallbacks,
        1,
        "exactly one transition to the ADM fallback (fallbacks={}, opens={}, failures={}, phase={:?})",
        harness.fallbacks,
        calls.load(Ordering::SeqCst),
        harness.recovery.failures(),
        harness.recovery.phase()
    );
    assert_eq!(harness.recovery.phase(), PlayoutPhase::AdmFallback);
    assert_eq!(
        harness.recovery.failures(),
        MAX_OPEN_ATTEMPTS,
        "an opened-but-dead sink counts as a failure"
    );
    let opened = calls.load(Ordering::SeqCst);
    assert_eq!(opened as u8, MAX_OPEN_ATTEMPTS);
    // After the fallback there are no more software reopens.
    for _ in 0..20 {
        harness.tick();
    }
    assert_eq!(calls.load(Ordering::SeqCst), opened, "no reopen after ADM");
    assert_eq!(harness.fallbacks, 1, "the fallback is not repeated");
}

#[test]
fn a_stable_sink_clears_the_budget_so_a_single_later_failure_is_retried() {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let (open, _calls) = failing_then_open(3, writes.clone());
    let mut harness = TickHarness::new(open);
    // Three failed opens, then the fourth open succeeds and stays alive.
    for _ in 0..20 {
        harness.tick();
        if harness.playout.is_some() {
            break;
        }
    }
    let playout = harness.playout.as_ref().expect("the fourth open succeeded");
    assert_eq!(harness.recovery.failures(), 3);
    let deadline = Instant::now() + Duration::from_millis(500);
    while playout.written_blocks() < STABLE_BLOCKS && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        playout.written_blocks() >= STABLE_BLOCKS,
        "the fake sink accepted real blocks"
    );
    harness.now += STABLE_AFTER + Duration::from_millis(10);
    assert!(playout.stable(harness.now));
    harness.recovery.mark_stable();
    assert_eq!(harness.recovery.phase(), PlayoutPhase::Stable);
    assert_eq!(harness.recovery.failures(), 0, "proof clears the budget");
    // A single later failure starts from zero and is retried, not escalated.
    assert_eq!(
        harness.recovery.record_failure(harness.now),
        FailureOutcome::Retry(Duration::from_millis(500))
    );
    assert_eq!(harness.recovery.failures(), 1);
}

#[test]
fn the_fifth_failure_requests_the_adm_fallback_instead_of_retrying() {
    let now = Instant::now();
    let mut recovery = SinkRecovery::default();
    for expected in 1..MAX_OPEN_ATTEMPTS {
        assert!(matches!(
            recovery.record_failure(now),
            FailureOutcome::Retry(_)
        ));
        assert_eq!(recovery.failures(), expected);
    }
    assert_eq!(
        recovery.record_failure(now),
        FailureOutcome::AdmFallback,
        "the budget is bounded at {MAX_OPEN_ATTEMPTS}"
    );
    assert_eq!(recovery.phase(), PlayoutPhase::AdmFallback);
    // A resume after the fallback must not hand out a fresh ladder.
    recovery.reset_for_resume(now);
    assert_eq!(recovery.phase(), PlayoutPhase::Opening);
    assert_eq!(recovery.failures(), MAX_OPEN_ATTEMPTS);
    assert_eq!(recovery.record_failure(now), FailureOutcome::AdmFallback);
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
fn a_stalled_tap_subscriber_counts_drops_instead_of_being_silently_removed() {
    let tap = Tap::default();
    let live = tap.subscribe();
    let stalled = tap.subscribe();
    tap.emit(&[7_i16; BLOCK_SAMPLES]);
    assert!(matches!(live.try_recv(), TapRead::Block(block) if block.len() == BLOCK_SAMPLES));
    assert!(matches!(stalled.try_recv(), TapRead::Block(_)));
    // The live subscriber drains; the stalled one fills its 16-slot queue.
    for _ in 0..20 {
        tap.emit(&[9_i16; BLOCK_SAMPLES]);
        while matches!(live.try_recv(), TapRead::Block(_)) {}
    }
    assert!(
        stalled.dropped() > 0,
        "a full queue is reported, not hidden"
    );
    assert_eq!(
        tap.subscribers(),
        2,
        "a slow subscriber is kept so recording can resume"
    );
    // Draining lets the same subscriber continue; no permanent truncation.
    tap.emit(&[11_i16; BLOCK_SAMPLES]);
    assert!(matches!(live.try_recv(), TapRead::Block(_)));
    // A dropped receiver is removed, and its replacement starts clean.
    drop(stalled);
    tap.emit(&[12_i16; BLOCK_SAMPLES]);
    assert_eq!(tap.subscribers(), 1);
}

#[test]
fn adm_health_requires_real_pcm_only_while_audio_is_expected() {
    let grace = INPUT_CAPTURE_GRACE;
    assert!(!adm_playout_ok(
        false,
        true,
        grace * 2,
        true,
        Duration::ZERO
    ));
    assert!(
        adm_playout_ok(true, false, grace + Duration::from_secs(1), false, grace),
        "an empty room has no inbound audio: unknown is not a failure"
    );
    assert!(
        adm_playout_ok(true, true, Duration::from_secs(1), false, grace * 2),
        "startup grace"
    );
    assert!(
        adm_playout_ok(true, true, grace + Duration::from_secs(1), false, grace),
        "no stats yet is unknown, not a failure"
    );
    assert!(adm_playout_ok(
        true,
        true,
        grace + Duration::from_secs(1),
        true,
        Duration::from_millis(500)
    ));
    assert!(
        !adm_playout_ok(
            true,
            true,
            grace + Duration::from_secs(1),
            true,
            Duration::from_secs(3)
        ),
        "stale observed progress is a real failure"
    );
}

#[test]
fn a_closed_tap_is_distinguishable_from_an_empty_one() {
    let tap = Tap::default();
    let receiver = tap.subscribe();
    assert!(matches!(receiver.try_recv(), TapRead::Empty));
    drop(tap);
    assert!(matches!(receiver.try_recv(), TapRead::Closed));
}

#[test]
fn finished_track_tasks_are_pruned() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut tasks = vec![runtime.spawn(async move {
        let _ = tx.send(());
    })];
    runtime.block_on(async {
        let _ = rx.await;
        tokio::task::yield_now().await;
    });
    prune_finished(&mut tasks);
    assert!(tasks.is_empty(), "a finished handle is not kept forever");
}

fn wait_for_blocks(playout: &Playout, min: u64) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while playout.written_blocks() < min && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        playout.written_blocks() >= min,
        "the fake sink must write {min} blocks"
    );
}

fn wait_until_dead(playout: &Playout) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while playout.running() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!playout.running(), "the sink thread must stop");
}

fn drain(events: &Events) -> (u32, Option<PlayoutStats>) {
    let mut errors = 0;
    let mut last = None;
    while let Some(event) = events.poll(Duration::ZERO) {
        match event {
            Event::DeviceError { .. } => errors += 1,
            Event::Playout(stats) => last = Some(stats),
            _ => {}
        }
    }
    (errors, last)
}

/// A synthetic receiver for the attachment tests: no device and no network,
/// but the same `RtcAudioTrack` the WebRTC callback hands to the worker.
fn synthetic_track(id: &str) -> RtcAudioTrack {
    let factory = PeerConnectionFactory::default();
    let source = libwebrtc::audio_source::native::NativeAudioSource::new(
        capture::source_options(),
        capture::SAMPLE_RATE,
        capture::CHANNELS,
        0,
    );
    factory.create_audio_track(id, source)
}

/// Builds a headless session whose first mixer uses the fake sink factory.
fn session_with_mixer(open: Arc<OpenSink>) -> Session {
    let playout = Playout::start_with(None, open.clone()).expect("the fake mixer opens");
    Session::test_headless(open).adopt_initial_playout(Some(playout))
}

/// Aborted tasks finish on the runtime; a dropped-but-not-aborted handle would
/// keep streaming into the replaced mixer and stay unfinished.
async fn wait_for_retired(session: &Session, expected: usize) {
    for _ in 0..50 {
        let done = session
            .playout_tracks
            .lock()
            .map(|tracks| {
                tracks.retired().len() == expected
                    && tracks.retired().iter().all(|task| task.is_finished())
            })
            .unwrap_or(false);
        if done {
            return;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the replaced mixer's tasks were not closed");
}

/// A participant who joins after the call entry must not be bound to the
/// entry-time clip: the arrival is attached to the mixer that is current when
/// the worker drains it, and a mixer replacement reattaches the earlier track
/// instead of dropping it.
#[tokio::test]
async fn a_late_track_attaches_to_the_current_mixer() {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let (open, _) = failing_then_open(0, writes);
    let mut session = session_with_mixer(open);
    let events = session.events.clone();
    let first_serial = session.playout.as_ref().unwrap().serial();

    session.test_offer_track("early", synthetic_track("early"));
    session.pump_track_arrivals();
    assert_eq!(
        session.playout_tracks.lock().unwrap().attached_to("early"),
        Some(first_serial)
    );

    assert!(
        session.attempt_software_playout(&events, RecoveryCause::Sink),
        "the fake factory opens the replacement mixer"
    );
    let current = session.playout.as_ref().unwrap().serial();
    assert_ne!(current, first_serial, "the mixer really was replaced");

    session.test_offer_track("late", synthetic_track("late"));
    session.pump_track_arrivals();
    let tracks = session.playout_tracks.lock().unwrap();
    assert_eq!(
        tracks.attached_to("late"),
        Some(current),
        "a track that arrives after entry follows the current mixer"
    );
    assert_eq!(
        tracks.attached_to("early"),
        Some(current),
        "the mixer swap must not drop the track that was already mixed"
    );
    assert_eq!(tracks.len(), 2, "one task per track id");
}

/// A re-offered track id replaces its task: the same receiver is never mixed
/// twice, and the old task is closed.
#[tokio::test]
async fn a_reoffered_track_is_attached_once() {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let (open, _) = failing_then_open(0, writes);
    let mut session = session_with_mixer(open);
    let current = session.playout.as_ref().unwrap().serial();

    session.test_offer_track("dup", synthetic_track("dup"));
    session.pump_track_arrivals();
    session.test_offer_track("dup", synthetic_track("dup"));
    session.pump_track_arrivals();

    let tracks = session.playout_tracks.lock().unwrap();
    assert_eq!(tracks.len(), 1, "a duplicate id owns exactly one task");
    assert_eq!(tracks.known_len(), 1);
    assert_eq!(tracks.attached_to("dup"), Some(current));
    drop(tracks);
    wait_for_retired(&session, 1).await;
}

/// Replacing the mixer closes every task of the old one and attaches each
/// active track once to the new one.
#[tokio::test]
async fn replacing_the_mixer_reattaches_each_active_track_once() {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let (open, _) = failing_then_open(0, writes);
    let mut session = session_with_mixer(open);
    let events = session.events.clone();
    let first_serial = session.playout.as_ref().unwrap().serial();

    session.test_offer_track("a", synthetic_track("a"));
    session.test_offer_track("b", synthetic_track("b"));
    session.pump_track_arrivals();
    {
        let tracks = session.playout_tracks.lock().unwrap();
        assert_eq!(tracks.attached_to("a"), Some(first_serial));
        assert_eq!(tracks.attached_to("b"), Some(first_serial));
    }

    assert!(session.attempt_software_playout(&events, RecoveryCause::Sink));
    let current = session.playout.as_ref().unwrap().serial();
    assert_ne!(current, first_serial);

    let tracks = session.playout_tracks.lock().unwrap();
    assert_eq!(
        tracks.len(),
        2,
        "each active track is attached exactly once"
    );
    assert_eq!(tracks.known_len(), 2, "no active track is forgotten");
    assert_eq!(tracks.attached_to("a"), Some(current));
    assert_eq!(tracks.attached_to("b"), Some(current));
    drop(tracks);
    wait_for_retired(&session, 2).await;
}

/// The initial sink is opened by `Session::new`; this drives the same
/// `adopt_initial_playout` constructor tail and `tick_playout` without devices,
/// so a session that forgets to enter the proof window fails here.
#[tokio::test]
async fn the_first_sink_proves_stability_through_tick_playout() {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let (open, calls) = failing_then_open(0, writes);
    let playout = Playout::start_with(None, open.clone()).expect("the first sink opens");
    let mut session = Session::test_headless(open).adopt_initial_playout(Some(playout));
    let events = session.events.clone();
    assert_eq!(session.playout_attempts.phase(), PlayoutPhase::Unstable);
    wait_for_blocks(session.playout.as_ref().unwrap(), STABLE_BLOCKS);
    session
        .playout
        .as_ref()
        .unwrap()
        .test_rewind_start(STABLE_AFTER + Duration::from_millis(10));
    session.tick_playout(&events);
    assert_eq!(
        session.playout_attempts.phase(),
        PlayoutPhase::Stable,
        "a normal call must leave the Opening/Unstable state"
    );
    assert_eq!(session.playout_attempts.failures(), 0);
    // A single later failure starts the ladder at Retry(0.5 s).
    session.playout.as_ref().unwrap().test_stop();
    wait_until_dead(session.playout.as_ref().unwrap());
    session.tick_playout(&events);
    assert_eq!(session.playout_attempts.phase(), PlayoutPhase::Opening);
    assert_eq!(session.playout_attempts.failures(), 1);
    assert!(
        !session
            .playout_attempts
            .due(Instant::now() + Duration::from_millis(100))
    );
    assert!(
        session
            .playout_attempts
            .due(Instant::now() + Duration::from_millis(600))
    );
    let (errors, _) = drain(&events);
    assert_eq!(errors, 1, "one message for the death transition");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "no reopen before the due time"
    );
}

#[tokio::test]
async fn the_fifth_failure_emits_one_message_and_a_final_report() {
    let (open, calls) = always_open_broken_sink();
    let playout = Playout::start_with(None, open.clone()).expect("opens");
    let mut session = Session::test_headless(open).adopt_initial_playout(Some(playout));
    let events = session.events.clone();
    let now = Instant::now();
    for _ in 0..4 {
        session.playout_attempts.record_failure(now);
    }
    assert_eq!(session.playout_attempts.phase(), PlayoutPhase::Opening);
    session.playout.as_ref().unwrap().test_stop();
    wait_until_dead(session.playout.as_ref().unwrap());
    session.tick_playout(&events);
    assert_eq!(session.playout_attempts.phase(), PlayoutPhase::AdmFallback);
    assert_eq!(session.playout_attempts.failures(), MAX_OPEN_ATTEMPTS);
    let (errors, last) = drain(&events);
    assert_eq!(errors, 1, "exactly one message on the transition to ADM");
    let report = last.expect("a final playout report");
    assert!(!report.running);
    assert_eq!(report.phase, PlayoutPhase::AdmFallback);
    // The fallback branch must not reopen the software sink.
    let opened = calls.load(Ordering::SeqCst);
    for _ in 0..4 {
        session.tick_playout(&events);
    }
    assert_eq!(calls.load(Ordering::SeqCst), opened);
}

/// A device switch must restart the recovery proof, not just the sink's own
/// window: the UI would otherwise keep "stable mixer" for a device that has not
/// lived two seconds and written 50 blocks yet.
#[tokio::test]
async fn switching_the_device_reports_an_unstable_sink() {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let (open, _calls) = failing_then_open(0, writes);
    let playout = Playout::start_with(None, open.clone()).expect("the first sink opens");
    let mut session = Session::test_headless(open).adopt_initial_playout(Some(playout));
    let events = session.events.clone();
    wait_for_blocks(session.playout.as_ref().unwrap(), STABLE_BLOCKS);
    session
        .playout
        .as_ref()
        .unwrap()
        .test_rewind_start(STABLE_AFTER + Duration::from_millis(10));
    session.tick_playout(&events);
    assert_eq!(session.playout_attempts.phase(), PlayoutPhase::Stable);
    let _ = drain(&events);

    let result = session.switch_playout("", &events);
    assert!(result.switched, "the system device opens");
    assert_eq!(
        session.playout_attempts.phase(),
        PlayoutPhase::Unstable,
        "a fresh device must prove itself again"
    );
    let (_, last) = drain(&events);
    let report = last.expect("the switch reports the new sink immediately");
    assert_eq!(report.phase, PlayoutPhase::Unstable);
    assert!(report.running, "the new sink is open but unproven");
    // The old proof must not carry over to the new device.
    session.tick_playout(&events);
    assert_eq!(session.playout_attempts.phase(), PlayoutPhase::Unstable);
}

#[test]
fn switching_the_sink_restarts_the_stability_window() {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let (open, _) = failing_then_open(0, writes);
    let playout = Playout::start_with(None, open).expect("opens");
    wait_for_blocks(&playout, STABLE_BLOCKS);
    playout.test_rewind_start(STABLE_AFTER + Duration::from_millis(10));
    assert!(playout.stable(Instant::now()));
    playout.switch_sink(None).expect("a second device opens");
    assert!(
        !playout.stable(Instant::now()),
        "the new sink must prove itself from scratch"
    );
    let baseline = playout.written_blocks();
    wait_for_blocks(&playout, baseline + STABLE_BLOCKS);
    playout.test_rewind_start(STABLE_AFTER + Duration::from_millis(10));
    assert!(playout.stable(Instant::now()));
}
