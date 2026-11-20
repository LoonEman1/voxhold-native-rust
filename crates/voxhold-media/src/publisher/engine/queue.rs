//! Bounded queue from the media worker to the runtime.
//!
//! Most events are replaceable diagnostics, but an `Answer` must still reach
//! the runtime after `set_local_description`, a `Bitrate` result decides
//! whether the dialog leaves `Applying`, and an `Error`/`Closed` decides
//! whether the attempt survives. An overflow may therefore only evict the
//! oldest unprotected event; a burst of protected events grows the queue
//! instead of dropping a result the state machine still needs.

use std::{collections::VecDeque, sync::Mutex};

use super::{ENGINE_EVENT_CAPACITY, EngineEvent};

#[derive(Default)]
pub(super) struct EventQueue {
    items: Mutex<VecDeque<EngineEvent>>,
}

impl EventQueue {
    pub(super) fn push(&self, event: EngineEvent) {
        let Ok(mut items) = self.items.lock() else {
            return;
        };
        if matches!(event, EngineEvent::Stats(_)) {
            // Only the latest counters matter.
            items.retain(|item| !matches!(item, EngineEvent::Stats(_)));
        }
        if items.len() >= ENGINE_EVENT_CAPACITY {
            if let Some(index) = items.iter().position(|item| !protected(item)) {
                items.remove(index);
            } else if !protected(&event) {
                // Every slot holds a protected event. A replaceable sample is
                // not worth losing one of them.
                return;
            }
            // All slots are protected and so is the incoming event: grow rather
            // than drop an answer, a Bitrate result or a terminal event that
            // the publisher still waits for.
        }
        items.push_back(event);
    }

    pub(super) fn pop(&self) -> Option<EngineEvent> {
        self.items.lock().ok()?.pop_front()
    }
}

/// An `Answer` is the SDP the runtime still has to send; a `Bitrate` result
/// ends the `Applying` state; an `Error`/`Closed` ends the attempt. None of
/// them may be evicted by a diagnostics or ICE burst.
fn protected(event: &EngineEvent) -> bool {
    matches!(
        event,
        EngineEvent::Answer(_)
            | EngineEvent::Bitrate { .. }
            | EngineEvent::Error(_)
            | EngineEvent::Closed
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publisher::{BitrateResult, Connection};

    fn state() -> EngineEvent {
        EngineEvent::State(Connection::Connected)
    }

    fn bitrate(op: u64) -> EngineEvent {
        EngineEvent::Bitrate {
            attempt: 1,
            op,
            result: BitrateResult::Applied,
        }
    }

    fn ice() -> EngineEvent {
        EngineEvent::Ice(crate::Candidate {
            candidate: "candidate:1 1 udp 1 127.0.0.1 5000 typ host".into(),
            sdp_mid: Some("0".into()),
            sdp_mline_index: Some(0),
            username_fragment: None,
        })
    }

    fn drain(queue: &EventQueue) -> Vec<EngineEvent> {
        let mut drained = Vec::new();
        while let Some(event) = queue.pop() {
            drained.push(event);
        }
        drained
    }

    /// A flood of replaceable state/stats samples evicts only itself. The
    /// `Bitrate` result and the `Error` survive in order.
    #[test]
    fn an_overflow_never_evicts_a_bitrate_result_or_an_error() {
        let queue = EventQueue::default();
        for _ in 0..ENGINE_EVENT_CAPACITY {
            queue.push(state());
        }
        let result = bitrate(7);
        let error = EngineEvent::Error("Соединение потеряно.".into());
        queue.push(result.clone());
        queue.push(error.clone());
        for _ in 0..ENGINE_EVENT_CAPACITY * 2 {
            queue.push(state());
            queue.push(EngineEvent::Stats(super::super::PublishStats::default()));
        }

        let drained = drain(&queue);
        assert!(
            drained.contains(&result),
            "the overflow must not swallow the Bitrate result: {} events",
            drained.len()
        );
        assert!(
            drained.contains(&error),
            "the overflow must not swallow the Error: {} events",
            drained.len()
        );
        assert!(
            drained
                .iter()
                .filter(|event| !protected(event))
                .all(|event| matches!(
                    event,
                    EngineEvent::State(Connection::Connected) | EngineEvent::Stats(_)
                )),
            "only replaceable samples may remain or be dropped"
        );
    }

    /// An answer sits behind an ICE burst in the same queue. `set_local_description`
    /// succeeds before the runtime drains it, so the burst may evict only its
    /// own samples: the SDP and the terminal `Closed` still reach the runtime.
    #[test]
    fn an_ice_burst_never_evicts_an_answer_or_a_closed_event() {
        let queue = EventQueue::default();
        for _ in 0..ENGINE_EVENT_CAPACITY / 2 {
            queue.push(ice());
        }
        let answer = EngineEvent::Answer("v=0\r\na=mid:0\r\n".into());
        let closed = EngineEvent::Closed;
        queue.push(answer.clone());
        queue.push(closed.clone());
        for _ in 0..ENGINE_EVENT_CAPACITY * 2 {
            queue.push(ice());
            queue.push(EngineEvent::Stats(super::super::PublishStats::default()));
        }

        let drained = drain(&queue);
        assert_eq!(
            drained.iter().filter(|event| **event == answer).count(),
            1,
            "the ICE burst must not swallow the answer: {} events",
            drained.len()
        );
        assert!(
            drained.contains(&closed),
            "the ICE burst must not swallow Closed: {} events",
            drained.len()
        );
        assert!(
            drained
                .iter()
                .filter(|event| !protected(event))
                .all(|event| matches!(event, EngineEvent::Ice(_) | EngineEvent::Stats(_))),
            "only replaceable samples may remain or be dropped"
        );
    }

    /// Protected events may outnumber the capacity; none of them may be lost,
    /// and the queue still drains all of them.
    #[test]
    fn more_protected_events_than_capacity_are_all_kept() {
        let queue = EventQueue::default();
        let total = ENGINE_EVENT_CAPACITY + 5;
        for op in 0..total {
            queue.push(bitrate(op as u64));
        }
        let drained = drain(&queue);
        assert_eq!(drained.len(), total, "a Bitrate result is never dropped");
        for (op, event) in (0..total).zip(drained) {
            assert_eq!(event, bitrate(op as u64), "the order is preserved");
        }
    }
}
