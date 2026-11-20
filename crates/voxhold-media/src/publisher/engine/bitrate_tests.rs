//! The engine side of the live-bitrate burst: a full bounded queue must not
//! look like a dead worker, and `Sent` only proves delivery.
//!
//! `PublisherEngine::set_bitrate` hands the quality to the same bounded queue
//! as ICE and offers. A burst can fill it for a tick; only a closed channel
//! proves that the worker is gone. The verified success or refusal arrives
//! later as `EngineEvent::Bitrate` with the same attempt/operation tags.

use tokio::sync::mpsc;

use super::{BitrateSend, Command, Quality, send_bitrate};

fn quality(bitrate_kbps: u32) -> Quality {
    Quality {
        bitrate_kbps,
        ..Quality::default()
    }
}

/// Pre-fill the command channel, so the queue is observably `Full`, then let
/// it drain and finally drop the worker side.
#[test]
fn a_full_command_queue_defers_the_bitrate_and_only_a_closed_worker_is_terminal() {
    let (commands, mut receiver) = mpsc::channel(1);
    // The slot is taken by an ordinary command of the same queue, exactly like
    // an ICE burst would.
    assert!(
        commands.try_send(Command::Offer(String::new())).is_ok(),
        "the first command must fit the empty queue"
    );

    assert_eq!(
        send_bitrate(Some(&commands), 4, 1, quality(1200)),
        BitrateSend::Full,
        "a full queue is a burst, not a dead worker"
    );
    assert_eq!(
        send_bitrate(Some(&commands), 4, 1, quality(1500)),
        BitrateSend::Full,
        "a second attempt while the burst lasts is still Full"
    );

    // Once the worker takes the queued command the retry is accepted.
    assert!(receiver.try_recv().is_ok(), "the burst drains");
    assert_eq!(
        send_bitrate(Some(&commands), 4, 1, quality(1500)),
        BitrateSend::Sent,
        "Sent only means the command reached the worker queue"
    );
    match receiver.try_recv() {
        Ok(Command::Bitrate {
            attempt,
            op,
            quality,
        }) => {
            assert_eq!(attempt, 4, "the attempt tag travels with the command");
            assert_eq!(op, 1, "the operation tag travels with the command");
            assert_eq!(quality.bitrate_kbps, 1500);
        }
        _ => panic!("the accepted command must carry the newest quality and both tags"),
    }

    // A dropped receiver (and a dropped handle) is the one terminal answer.
    drop(receiver);
    assert_eq!(
        send_bitrate(Some(&commands), 4, 1, quality(900)),
        BitrateSend::Closed
    );
    assert_eq!(
        send_bitrate(None, 4, 1, quality(900)),
        BitrateSend::Closed,
        "a released handle cannot accept a bitrate change"
    );
}
