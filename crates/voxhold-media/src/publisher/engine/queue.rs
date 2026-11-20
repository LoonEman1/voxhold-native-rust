//! Bounded queue from the media worker to the runtime.
//!
//! Most events are replaceable diagnostics, but a `Bitrate` result decides
//! whether the dialog leaves `Applying`, and an `Error` decides whether the
//! attempt survives. An overflow may therefore only evict the oldest
//! unprotected event; a burst of protected events grows the queue instead of
//! dropping a result the state machine still needs.

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
                // Every slot holds a Bitrate/Error event. A replaceable sample
                // is not worth losing one of them.
                return;
            }
            // All slots are protected and so is the incoming event: grow rather
            // than drop a Bitrate result that the publisher still waits for.
        }
        items.push_back(event);
    }

    pub(super) fn pop(&self) -> Option<EngineEvent> {
        self.items.lock().ok()?.pop_front()
    }
}

/// A `Bitrate` result ends the `Applying` state, and an `Error` may end the
/// attempt: neither may be evicted by a diagnostics burst.
fn protected(event: &EngineEvent) -> bool {
    matches!(event, EngineEvent::Bitrate { .. } | EngineEvent::Error(_))
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
