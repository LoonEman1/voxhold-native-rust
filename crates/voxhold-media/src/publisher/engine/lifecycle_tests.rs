//! Engine lifecycle: a full command queue is a counted ICE loss, and `Drop`
//! closes the channel before joining so it never waits on a live `recv()`.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::channel,
    },
    time::{Duration, Instant},
};

use tokio::sync::mpsc::{self, error::TryRecvError};

use super::{Command, ENGINE_COMMAND_CAPACITY, EventQueue, PublisherEngine};
use crate::screen_capture::PreviewStore;

fn candidate(index: u32) -> crate::Candidate {
    crate::Candidate {
        candidate: format!("candidate:{index} 1 udp 2130706431 192.0.2.1 5000 typ host"),
        sdp_mid: Some("0".into()),
        sdp_mline_index: Some(0),
        username_fragment: Some("abcd".into()),
    }
}

/// A handle with a synthetic worker and no WebRTC, so the queue contract is
/// tested without a capture or a peer connection.
fn fake_engine(
    worker: impl FnOnce(mpsc::Receiver<Command>, Arc<AtomicBool>) + Send + 'static,
) -> (PublisherEngine, Arc<AtomicBool>) {
    let (commands, receiver) = mpsc::channel(ENGINE_COMMAND_CAPACITY);
    let abort = Arc::new(AtomicBool::new(false));
    let worker_abort = abort.clone();
    let thread = std::thread::spawn(move || worker(receiver, worker_abort));
    (
        PublisherEngine {
            commands: Some(commands),
            events: Arc::new(EventQueue::default()),
            preview: Arc::new(PreviewStore::default()),
            ice_dropped: AtomicU64::new(0),
            thread: Some(thread),
        },
        abort,
    )
}

#[test]
fn drop_closes_a_full_command_queue_and_joins_within_the_budget() {
    let (engine, abort) = fake_engine(|mut receiver, abort| {
        loop {
            match receiver.try_recv() {
                Ok(Command::Stop) | Err(TryRecvError::Disconnected) => return,
                Ok(_) => {}
                Err(TryRecvError::Empty) => {
                    if abort.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    });
    for index in 0..ENGINE_COMMAND_CAPACITY {
        engine
            .commands
            .as_ref()
            .expect("open commands")
            .try_send(Command::Ice(candidate(index as u32)))
            .expect("the queue holds a full server ICE burst");
    }

    let (done_tx, done_rx) = channel();
    let dropper = std::thread::spawn(move || {
        drop(engine);
        let _ = done_tx.send(());
    });
    let joined = done_rx.recv_timeout(Duration::from_secs(5)).is_ok();
    // Release a worker of the old behaviour so a failing run does not leak it.
    abort.store(true, Ordering::SeqCst);
    let _ = dropper.join();
    assert!(
        joined,
        "Drop must close the command channel and join within five seconds"
    );
}

#[test]
fn an_ice_burst_over_the_queue_is_a_counted_drop_not_a_fatal_error() {
    // This worker never reads, so the queue stays full for the whole test.
    let (engine, abort) = fake_engine(|_receiver, abort| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !abort.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    for index in 0..ENGINE_COMMAND_CAPACITY {
        assert!(
            engine
                .commands
                .as_ref()
                .expect("open commands")
                .try_send(Command::Ice(candidate(index as u32)))
                .is_ok()
        );
    }
    for _ in 0..40 {
        assert!(
            !engine.ice(candidate(99)),
            "an ICE overflow is a counted loss, not a queued command"
        );
    }
    assert_eq!(engine.ice_dropped(), 40);
    abort.store(true, Ordering::SeqCst);
    drop(engine);
}
