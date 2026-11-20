//! Bounded reopen of the system-audio capture.
//!
//! A failure is either an open that produced no client or a client that stopped
//! before proving itself. Both consume the same retry budget, so a device that
//! opens and is invalidated immediately cannot loop forever. The budget resets
//! only after a session kept polling the client for [`STABLE_WINDOW`] without a
//! gap above [`MAX_POLL_GAP`]; a silent endpoint still answers every poll, so
//! silence counts as healthy.

use std::{
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use crate::system_audio::{AudioDeviceInfo, CaptureScope, Shared};

use super::{Com, Failure, Opened, capture, open_scope};

pub(super) const MAX_REOPEN: u8 = 3;
pub(super) const REOPEN_DELAY: Duration = Duration::from_millis(300);
/// A session must keep answering for this long before a later failure resets
/// the budget again.
pub(super) const STABLE_WINDOW: Duration = Duration::from_secs(2);
/// A gap between answered polls means the thread was blocked, and time it did
/// not observe must not count as healthy. The capture loop polls a few
/// milliseconds apart (`POLL` is 5 ms, blocks are 10 ms) and a blocking WASAPI
/// call normally returns far below this bound, so 500 ms is conservative: a
/// client that answers as rarely as every 900 ms cannot accumulate a streak,
/// while a silent endpoint still answers `GetNextPacketSize() == 0` on every
/// poll and stays healthy.
pub(super) const MAX_POLL_GAP: Duration = Duration::from_millis(500);

/// How one successfully opened session ended.
pub(super) enum SessionEnd {
    /// The publication asked the capture to stop; no reopen and no error.
    Stopped,
    /// The device is gone or changed. `stable` says whether the session had
    /// already earned a fresh retry budget.
    Reopen { stable: bool, message: String },
    /// A terminal failure; the publication is told once and stops.
    Fatal(String),
}

/// The only thing the retry loop needs from an opened client. Production uses
/// [`Opened`]; tests use a small fake carrying an [`AudioDeviceInfo`].
pub(super) trait SessionDevice {
    fn info(&self) -> &AudioDeviceInfo;
}

impl SessionDevice for Opened {
    fn info(&self) -> &AudioDeviceInfo {
        &self.info
    }
}

/// Continuous responsive polling of one session. Every answered WASAPI call
/// records a beat; an idle poll of a silent endpoint records the same beat as
/// a full packet.
pub(super) struct Health {
    last: Instant,
    streak: Duration,
}

impl Health {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            last: now,
            streak: Duration::ZERO,
        }
    }

    pub(super) fn record(&mut self, now: Instant) {
        let gap = now.saturating_duration_since(self.last);
        self.last = now;
        self.streak = if gap > MAX_POLL_GAP {
            Duration::ZERO
        } else {
            self.streak + gap
        };
    }

    pub(super) fn stable(&self) -> bool {
        self.streak >= STABLE_WINDOW
    }
}

/// Open and capture until the publication stops or the retry budget is spent.
pub(super) fn run(
    shared: &Arc<Shared>,
    ready: &mpsc::Sender<Result<AudioDeviceInfo, String>>,
    scope: CaptureScope,
) {
    let _com = match Com::init() {
        Ok(com) => com,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    run_with(
        shared,
        ready,
        scope,
        MAX_REOPEN,
        REOPEN_DELAY,
        open_scope,
        |shared, opened: &Opened| capture(shared, opened),
        thread::sleep,
    );
}

/// The retry state machine with the opener, the session and the pause
/// injectable, so every branch is testable without a device or a real wait.
fn run_with<D, O, C, P>(
    shared: &Arc<Shared>,
    ready: &mpsc::Sender<Result<AudioDeviceInfo, String>>,
    scope: CaptureScope,
    max_reopen: u8,
    delay: Duration,
    mut open: O,
    mut session: C,
    mut pause: P,
) where
    D: SessionDevice,
    O: FnMut(CaptureScope) -> Result<D, Failure>,
    C: FnMut(&Shared, &D) -> SessionEnd,
    P: FnMut(Duration),
{
    let mut first = true;
    let mut reopens = 0_u8;
    // The endpoint whose PCM queue is still valid. A transient failure on the
    // same device must not insert a silent gap; only a real device change
    // discards audio that belongs to the old endpoint.
    let mut last_device: Option<String> = None;
    loop {
        if shared.stopping() {
            return;
        }
        let device = match open(scope) {
            Ok(device) => device,
            Err(Failure::Reopen(message) | Failure::Fatal(message)) => {
                if first {
                    // The publication never started, so this is the startup
                    // error rather than a system-audio runtime error.
                    let _ = ready.send(Err(message));
                    return;
                }
                if !spend_retry(&mut reopens, max_reopen) {
                    shared.fail(stop_message(&message));
                    return;
                }
                pause(delay);
                continue;
            }
        };
        let info = device.info().clone();
        if !first && last_device.as_deref() != Some(info.id.as_str()) {
            // A reopen creates a fresh converter; audio queued by the previous
            // client belongs to the old endpoint and must not leak into the
            // new one. A same-device retry keeps its queue to avoid a gap.
            shared.clear_audio();
        }
        last_device = Some(info.id.clone());
        shared.set_device(info.clone());
        if first {
            first = false;
            if ready.send(Ok(info)).is_err() {
                return;
            }
        }
        match session(shared, &device) {
            SessionEnd::Stopped => return,
            SessionEnd::Fatal(message) => {
                shared.fail(message);
                return;
            }
            SessionEnd::Reopen { stable, message } => {
                if shared.stopping() {
                    return;
                }
                if stable {
                    // The session answered polls for the whole window, so its
                    // failure starts a new budget. Silence qualifies too.
                    reopens = 0;
                }
                if !spend_retry(&mut reopens, max_reopen) {
                    shared.fail(stop_message(&message));
                    return;
                }
                pause(delay);
            }
        }
    }
}

/// Count one consecutive failure. Returns `false` once the budget is spent.
fn spend_retry(reopens: &mut u8, max_reopen: u8) -> bool {
    *reopens = reopens.saturating_add(1);
    *reopens <= max_reopen
}

fn stop_message(message: &str) -> String {
    format!("Системный звук остановлен: {message} Повторное открытие не удалось.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system_audio::{BLOCK_SAMPLES, next_block, push_block};
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    struct FakeDevice(AudioDeviceInfo);

    impl SessionDevice for FakeDevice {
        fn info(&self) -> &AudioDeviceInfo {
            &self.0
        }
    }

    fn info(id: &str) -> AudioDeviceInfo {
        AudioDeviceInfo {
            id: id.into(),
            label: id.into(),
            format: "test".into(),
        }
    }

    fn ready() -> (
        mpsc::Sender<Result<AudioDeviceInfo, String>>,
        mpsc::Receiver<Result<AudioDeviceInfo, String>>,
    ) {
        mpsc::channel()
    }

    #[test]
    fn silence_and_a_stall_are_told_apart_by_the_health_streak() {
        let start = Instant::now();
        let mut health = Health::new(start);
        // Idle polls of a silent endpoint arrive every POLL interval. Nothing
        // distinguishes them from polls that return audio.
        for beat in 0..=400 {
            health.record(start + Duration::from_millis(5) * beat);
        }
        assert!(health.stable(), "2 s of answered polls is a stable session");

        // A blocked thread records no beats; the next beat cannot claim the
        // missing time as healthy.
        health.record(start + Duration::from_secs(10));
        assert!(!health.stable(), "a long gap must restart the streak");
        health.record(start + Duration::from_secs(11));
        assert!(
            !health.stable(),
            "the full window is needed again after a stall"
        );
    }

    #[test]
    fn answered_polls_as_slow_as_every_900ms_never_earn_a_stable_session() {
        let start = Instant::now();
        let mut health = Health::new(start);
        // A client that returns from every call only every 900 ms is blocked
        // most of the time. Summing those gaps used to reach STABLE_WINDOW
        // after three polls, which let a later failure reset the retry budget
        // forever.
        for beat in 1..=12 {
            health.record(start + Duration::from_millis(900) * beat);
            assert!(
                !health.stable(),
                "a 900 ms poll gap must reset the streak (beat {beat})"
            );
        }
        assert!(
            Duration::from_millis(900) * 12 > STABLE_WINDOW,
            "the beats span more than the window; only the gaps keep it unstable"
        );

        // The invalidation itself is recorded before the stability question, so
        // this session must spend the budget rather than refresh it.
        health.record(start + Duration::from_millis(900) * 13);
        assert!(!health.stable(), "the invalidation spends the budget");
    }

    #[test]
    fn an_immediately_invalidated_client_spends_the_budget_and_reports_once() {
        let shared = Arc::new(Shared::default());
        let (ready_tx, ready_rx) = ready();
        let opens = Arc::new(AtomicUsize::new(0));
        let scopes = Arc::new(Mutex::new(Vec::new()));
        let opens_in = opens.clone();
        let scopes_in = scopes.clone();
        run_with(
            &shared,
            &ready_tx,
            CaptureScope::Process { pid: 4_242 },
            3,
            Duration::ZERO,
            move |scope| {
                opens_in.fetch_add(1, Ordering::SeqCst);
                scopes_in.lock().expect("scope log").push(scope);
                Ok(FakeDevice(info("endpoint-a")))
            },
            |_shared, _device| SessionEnd::Reopen {
                stable: false,
                message: "Устройство вывода изменилось.".into(),
            },
            |_| {},
        );

        assert!(
            matches!(ready_rx.try_recv(), Ok(Ok(device)) if device.id == "endpoint-a"),
            "the first open is published before the invalidation"
        );
        assert_eq!(
            opens.load(Ordering::SeqCst),
            4,
            "the first attempt plus three retries, then the budget is spent"
        );
        assert!(
            scopes
                .lock()
                .expect("scope log")
                .iter()
                .all(|scope| *scope == CaptureScope::Process { pid: 4_242 }),
            "a retry never leaves the chosen capture scope"
        );
        let failure = shared.failure().expect("the publisher is told once");
        assert!(failure.contains("остановлен"), "{failure}");
        assert_eq!(
            failure.matches("Устройство вывода изменилось").count(),
            1,
            "exactly one error message: {failure}"
        );
    }

    #[test]
    fn an_open_failure_also_spends_the_same_budget() {
        let shared = Arc::new(Shared::default());
        let (ready_tx, _ready_rx) = ready();
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_in = attempts.clone();
        run_with(
            &shared,
            &ready_tx,
            CaptureScope::Device,
            2,
            Duration::ZERO,
            move |_scope| {
                if attempts_in.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok(FakeDevice(info("endpoint-a")))
                } else {
                    Err(Failure::Reopen("Устройство вывода отключено.".into()))
                }
            },
            |_shared, _device| SessionEnd::Reopen {
                stable: false,
                message: "Устройство вывода изменилось.".into(),
            },
            |_| {},
        );

        assert_eq!(
            attempts.load(Ordering::SeqCst),
            3,
            "one successful open, one session failure and two failed reopens"
        );
        let failure = shared.failure().expect("exhaustion is reported");
        assert!(failure.contains("Устройство вывода отключено"), "{failure}");
    }

    #[test]
    fn a_stable_session_refreshes_the_retry_budget() {
        let shared = Arc::new(Shared::default());
        let (ready_tx, _ready_rx) = ready();
        let sessions = Arc::new(AtomicUsize::new(0));
        let sessions_in = sessions.clone();
        run_with(
            &shared,
            &ready_tx,
            CaptureScope::Device,
            2,
            Duration::ZERO,
            |_scope| Ok(FakeDevice(info("endpoint-a"))),
            move |_shared, _device| {
                if sessions_in.fetch_add(1, Ordering::SeqCst) < 5 {
                    SessionEnd::Reopen {
                        stable: true,
                        message: "пауза".into(),
                    }
                } else {
                    SessionEnd::Stopped
                }
            },
            |_| {},
        );

        assert!(
            shared.failure().is_none(),
            "stable sessions keep earning a fresh budget"
        );
        assert_eq!(
            sessions.load(Ordering::SeqCst),
            6,
            "five stable sessions plus the final stop, far beyond a budget of 2"
        );
    }

    #[test]
    fn a_device_change_discards_the_old_queue_and_publishes_the_new_device() {
        let shared = Arc::new(Shared::default());
        let (ready_tx, _ready_rx) = ready();
        let opens = Arc::new(AtomicUsize::new(0));
        let sessions = Arc::new(AtomicUsize::new(0));
        let opens_in = opens.clone();
        let sessions_in = sessions.clone();
        run_with(
            &shared,
            &ready_tx,
            CaptureScope::Device,
            3,
            Duration::ZERO,
            move |_scope| {
                let index = opens_in.fetch_add(1, Ordering::SeqCst);
                Ok(FakeDevice(info(if index == 0 {
                    "endpoint-a"
                } else {
                    "endpoint-b"
                })))
            },
            move |shared, _device| {
                if sessions_in.fetch_add(1, Ordering::SeqCst) == 0 {
                    push_block(shared, vec![7_i16; BLOCK_SAMPLES]);
                    SessionEnd::Reopen {
                        stable: false,
                        message: "Устройство вывода изменилось.".into(),
                    }
                } else {
                    // The new client must start with no audio from the old one.
                    assert!(next_block(shared).is_none());
                    assert!(
                        shared
                            .device()
                            .is_some_and(|device| device.id == "endpoint-b")
                    );
                    shared.stop.store(true, Ordering::SeqCst);
                    SessionEnd::Stopped
                }
            },
            |_| {},
        );

        assert!(shared.failure().is_none());
        assert_eq!(opens.load(Ordering::SeqCst), 2);
        assert_eq!(
            shared.device().map(|device| device.id).as_deref(),
            Some("endpoint-b")
        );
        assert!(
            next_block(&shared).is_none(),
            "the queue of the old endpoint must be cleared"
        );
    }

    #[test]
    fn a_same_device_reopen_keeps_the_queued_audio() {
        let shared = Arc::new(Shared::default());
        let (ready_tx, _ready_rx) = ready();
        let sessions = Arc::new(AtomicUsize::new(0));
        let sessions_in = sessions.clone();
        run_with(
            &shared,
            &ready_tx,
            CaptureScope::Device,
            3,
            Duration::ZERO,
            |_scope| Ok(FakeDevice(info("endpoint-a"))),
            move |shared, _device| {
                if sessions_in.fetch_add(1, Ordering::SeqCst) == 0 {
                    push_block(shared, vec![7_i16; BLOCK_SAMPLES]);
                    SessionEnd::Reopen {
                        stable: false,
                        message: "Устройство вывода изменилось.".into(),
                    }
                } else {
                    // A transient failure on the same endpoint must continue
                    // from the queued audio instead of inserting a silence.
                    assert!(
                        next_block(shared).is_some(),
                        "a same-device retry keeps the PCM queue"
                    );
                    shared.stop.store(true, Ordering::SeqCst);
                    SessionEnd::Stopped
                }
            },
            |_| {},
        );

        assert!(shared.failure().is_none());
        assert_eq!(sessions.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_headset_disconnect_while_live_is_detected_and_reported_once() {
        let shared = Arc::new(Shared::default());
        let (ready_tx, ready_rx) = ready();
        let sessions = Arc::new(AtomicUsize::new(0));
        let sessions_in = sessions.clone();
        run_with(
            &shared,
            &ready_tx,
            CaptureScope::Device,
            2,
            Duration::ZERO,
            |_scope| Ok(FakeDevice(info("headset"))),
            move |shared, _device| {
                let index = sessions_in.fetch_add(1, Ordering::SeqCst);
                push_block(shared, vec![5_i16; BLOCK_SAMPLES]);
                if index == 0 {
                    // The live session was healthy, then the device vanished.
                    SessionEnd::Reopen {
                        stable: true,
                        message: "Устройство вывода изменилось.".into(),
                    }
                } else {
                    SessionEnd::Reopen {
                        stable: false,
                        message: "Устройство вывода изменилось.".into(),
                    }
                }
            },
            |_| {},
        );

        assert!(matches!(ready_rx.try_recv(), Ok(Ok(_))));
        let failure = shared.failure().expect("the disconnect is reported");
        assert!(
            failure.contains("Устройство вывода изменилось"),
            "{failure}"
        );
        assert_eq!(
            failure.matches("Устройство вывода изменилось").count(),
            1,
            "one message for the publication"
        );
    }
}
