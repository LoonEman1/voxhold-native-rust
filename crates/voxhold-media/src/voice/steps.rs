//! One step of the media worker.
//!
//! A player close is a separate signal, not a queued command: an SDP/ICE burst
//! must never delay it. The select is biased so a ready stop wins even when
//! offers are already waiting in the bounded channel.

use crate::Command;
use tokio::{
    sync::{Notify, mpsc},
    time::Interval,
};

pub(super) enum Step {
    Command(Command),
    Stats,
    Tracks,
    Stopped,
    Closed,
}

pub(super) async fn next_step(
    commands: &mut mpsc::Receiver<Command>,
    stop: &Notify,
    tracks: &Notify,
    stats: &mut Interval,
    stats_enabled: bool,
) -> Step {
    tokio::select! {
        biased;
        _ = stop.notified() => Step::Stopped,
        _ = tracks.notified() => Step::Tracks,
        command = commands.recv() => match command {
            Some(command) => Step::Command(command),
            None => Step::Closed,
        },
        _ = stats.tick(), if stats_enabled => Step::Stats,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
    }

    fn idle_stats() -> Interval {
        let mut stats = tokio::time::interval(Duration::from_secs(3600));
        stats.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        stats
    }

    #[test]
    fn a_stop_signal_preempts_queued_offers() {
        runtime().block_on(async {
            let (commands, mut receiver) = mpsc::channel(8);
            for index in 0..8 {
                commands
                    .send(Command::Offer {
                        sdp: format!("offer-{index}"),
                    })
                    .await
                    .unwrap();
            }
            let stop = Notify::new();
            let tracks = Notify::new();
            stop.notify_one();
            let mut stats = idle_stats();
            // The offers are still queued, yet the close is processed first.
            assert!(matches!(
                next_step(&mut receiver, &stop, &tracks, &mut stats, false).await,
                Step::Stopped
            ));
            assert_eq!(receiver.len(), 8, "the stop must not drain the queue");
        });
    }

    #[test]
    fn without_a_stop_the_oldest_command_is_taken_first() {
        runtime().block_on(async {
            let (commands, mut receiver) = mpsc::channel(8);
            commands
                .send(Command::Offer {
                    sdp: "first".into(),
                })
                .await
                .unwrap();
            commands
                .send(Command::Ice(crate::Candidate {
                    candidate: "candidate:1".into(),
                    sdp_mid: None,
                    sdp_mline_index: None,
                    username_fragment: None,
                }))
                .await
                .unwrap();
            let stop = Notify::new();
            let tracks = Notify::new();
            let mut stats = idle_stats();
            assert!(matches!(
                next_step(&mut receiver, &stop, &tracks, &mut stats, false).await,
                Step::Command(Command::Offer { sdp }) if sdp == "first"
            ));
            drop(commands);
            assert!(matches!(
                next_step(&mut receiver, &stop, &tracks, &mut stats, false).await,
                Step::Command(Command::Ice(_))
            ));
            assert!(matches!(
                next_step(&mut receiver, &stop, &tracks, &mut stats, false).await,
                Step::Closed
            ));
        });
    }
}
