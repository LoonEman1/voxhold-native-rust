//! Off-loop preparation of one screen publication.
//!
//! Opening the capture, the system-audio loopback and the peer worker can each
//! block for seconds. The voice session loop only starts this work, observes a
//! bounded result and cancels it; it never opens a device itself. A result is
//! bound to the attempt, the room and the connection that requested it, and its
//! engine is released on a short-lived thread when the session no longer wants
//! it, so a stale capture cannot hold the session worker either.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::publisher::{Access, Outcome as PublisherOutcome, Phase, Publisher};

/// Upper bound of the whole preparation. The per-stage bounds stay in force;
/// this one covers a capture or loopback call that never returns.
pub(super) const PREP_BUDGET: Duration = Duration::from_secs(10);

pub(super) const PREP_TIMEOUT_MESSAGE: &str = "Подготовка трансляции не завершилась за 10 секунд.";

/// Identity a preparation result must still match when it completes.
#[derive(Clone, Debug)]
pub(super) struct PrepBinding {
    pub(super) attempt: u64,
    pub(super) server: i64,
    pub(super) channel: i64,
    pub(super) connection: Option<String>,
    /// The gate that was confirmed when the capture was requested. A later
    /// revocation is re-checked against the current session before
    /// `stream.start` is allowed to leave.
    pub(super) access: Access,
}

impl PrepBinding {
    /// Everything except the live gate. A revoked gate is a terminal error of
    /// the same attempt, not a stale result of another one.
    fn same_attempt(&self, current: &Self) -> bool {
        self.attempt == current.attempt
            && self.server == current.server
            && self.channel == current.channel
            && self.connection == current.connection
    }
}

/// One bounded result of the preparation worker.
pub(super) enum PrepEvent<E> {
    Ready { engine: E, has_audio: bool },
    Failed(String),
    Timeout,
}

/// An engine that belongs to the current attempt and one that must be released
/// without touching the publisher. Keeping them apart lets the session hand a
/// stale engine to a release thread instead of joining a capture on its loop.
pub(super) struct PrepFinish<E> {
    pub(super) engine: Option<E>,
    pub(super) discard: Option<E>,
    pub(super) outcomes: Vec<PublisherOutcome>,
}

impl<E> Default for PrepFinish<E> {
    fn default() -> Self {
        Self {
            engine: None,
            discard: None,
            outcomes: Vec::new(),
        }
    }
}

/// One preparation in flight. At most one exists per session. The engine bound
/// lets a cancel hand a receiver that already holds an engine to a release
/// thread instead of joining a capture on the session loop.
pub(super) struct ScreenPrep<E: Send + 'static> {
    binding: PrepBinding,
    cancel: Arc<AtomicBool>,
    result: Option<Receiver<Result<(E, bool), String>>>,
    deadline: Instant,
    thread: Option<JoinHandle<()>>,
}

impl<E: Send + 'static> ScreenPrep<E> {
    pub(super) fn binding(&self) -> &PrepBinding {
        &self.binding
    }

    /// Abandon the preparation. The token is checked at every stage boundary
    /// and the receiver is dropped, so a late engine is released instead of
    /// being handed to a session that no longer wants it. The receiver is
    /// dropped on a short-lived thread: if the worker already queued an engine,
    /// its capture is not joined on the session loop.
    pub(super) fn cancel(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(result) = self.result.take() {
            let _ = thread::Builder::new()
                .name("voxhold-screen-cancel".into())
                .spawn(move || drop(result));
        }
        self.thread.take();
    }

    /// Spawn the preparation on its own thread. `launch` gets the cancellation
    /// token and must check it around each blocking stage; an engine that is
    /// created after a cancel is dropped by the worker below.
    pub(super) fn start<F>(
        binding: PrepBinding,
        now: Instant,
        budget: Duration,
        launch: F,
    ) -> Result<Self, String>
    where
        F: FnOnce(&AtomicBool) -> Result<(E, bool), String> + Send + 'static,
    {
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let (sender, result) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("voxhold-screen-prep".into())
            .spawn(move || {
                let outcome = launch(&worker_cancel);
                // A cancelled or replaced preparation has already dropped the
                // receiver. This send then fails and the engine (with its
                // capture, loopback and peer) is dropped on this thread.
                let _ = sender.send(outcome);
            })
            .map_err(|error| format!("Не удалось начать подготовку трансляции: {error}"))?;
        Ok(Self {
            binding,
            cancel,
            result: Some(result),
            deadline: now + budget,
            thread: Some(thread),
        })
    }

    /// Non-blocking observation. The result channel holds at most one value;
    /// the deadline is reported once, after the receiver was dropped.
    pub(super) fn poll(&mut self, now: Instant) -> Option<PrepEvent<E>> {
        let received = self.result.as_ref()?.try_recv();
        match received {
            Ok(Ok((engine, has_audio))) => {
                self.result.take();
                self.thread.take();
                Some(PrepEvent::Ready { engine, has_audio })
            }
            Ok(Err(message)) => {
                self.result.take();
                self.thread.take();
                Some(PrepEvent::Failed(message))
            }
            Err(TryRecvError::Empty) if now >= self.deadline => {
                self.cancel();
                Some(PrepEvent::Timeout)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.result.take();
                self.thread.take();
                Some(PrepEvent::Failed(
                    "Не удалось подготовить трансляцию.".into(),
                ))
            }
        }
    }
}

impl<E: Send + 'static> Drop for ScreenPrep<E> {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// The first gate that refuses a publication. The same messages are used when
/// the gate is re-checked at completion.
pub(super) fn access_violation(access: Access) -> Option<&'static str> {
    if !access.joined {
        Some("Сначала подключитесь к голосовому каналу.")
    } else if !access.stream_permission {
        Some("Нет права запускать трансляцию.")
    } else if !access.slots_available {
        Some("В канале уже 8 трансляций. Дождитесь завершения одной из них.")
    } else {
        None
    }
}

/// Cancel a pending preparation. A message fails the attempt visibly (the
/// engine never reached `stream.start`, so no compensating stop is emitted);
/// `None` is used when the publisher transition itself already reset the
/// attempt, such as a user Stop or a leave.
pub(super) fn cancel_prep<E: Send + 'static>(
    prep: &mut Option<ScreenPrep<E>>,
    publisher: &mut Publisher,
    message: Option<&'static str>,
    now: Instant,
) -> Vec<PublisherOutcome> {
    if let Some(mut prep) = prep.take() {
        prep.cancel();
    }
    match message {
        Some(message) if publisher.phase() == Phase::Publishing => {
            publisher.on_start_failed(message, now)
        }
        _ => Vec::new(),
    }
}

/// Apply one preparation event to the publisher. `stream.start` is released
/// only for a result that still matches the current attempt, room, connection
/// and gate; everything else is handed back as `discard` for an off-loop
/// release and never touches the publisher.
pub(super) fn finish_prep<E>(
    publisher: &mut Publisher,
    binding: &PrepBinding,
    current: &PrepBinding,
    access: Access,
    event: PrepEvent<E>,
    now: Instant,
) -> PrepFinish<E> {
    match event {
        PrepEvent::Ready { engine, has_audio } => {
            if !binding.same_attempt(current)
                || publisher.sharing().attempt != binding.attempt
                || publisher.phase() != Phase::Publishing
            {
                return PrepFinish {
                    discard: Some(engine),
                    ..PrepFinish::default()
                };
            }
            // The gate that allowed the capture must still have been granted
            // when the capture began, and it must still be granted now.
            if let Some(message) = access_violation(binding.access) {
                return PrepFinish {
                    discard: Some(engine),
                    outcomes: publisher.on_start_failed(message, now),
                    ..PrepFinish::default()
                };
            }
            if let Some(message) = access_violation(access) {
                return PrepFinish {
                    discard: Some(engine),
                    outcomes: publisher.on_start_failed(message, now),
                    ..PrepFinish::default()
                };
            }
            PrepFinish {
                outcomes: publisher.engine_ready(now, has_audio),
                engine: Some(engine),
                ..PrepFinish::default()
            }
        }
        PrepEvent::Failed(message) => PrepFinish {
            outcomes: fail_if_current(publisher, binding, current, &message, now),
            ..PrepFinish::default()
        },
        PrepEvent::Timeout => PrepFinish {
            outcomes: fail_if_current(publisher, binding, current, PREP_TIMEOUT_MESSAGE, now),
            ..PrepFinish::default()
        },
    }
}

fn fail_if_current(
    publisher: &mut Publisher,
    binding: &PrepBinding,
    current: &PrepBinding,
    message: &str,
    now: Instant,
) -> Vec<PublisherOutcome> {
    if binding.same_attempt(current)
        && publisher.sharing().attempt == binding.attempt
        && publisher.phase() == Phase::Publishing
    {
        publisher.on_start_failed(message, now)
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests;
