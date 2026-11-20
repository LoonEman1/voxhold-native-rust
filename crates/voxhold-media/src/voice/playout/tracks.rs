//! Receiver-track attachments for the software room mixer.
//!
//! `on_track` can fire at any time: at call entry, when a participant joins
//! later, or again for a track restored by a renegotiation. The WebRTC
//! callback only queues the receiver here. The media worker attaches it to the
//! `Playout` that is current at that moment, so a late track is never bound to
//! the mixer that existed when the call started.
//!
//! One track id owns at most one attachment task. A re-offered id replaces its
//! previous task instead of mixing the same receiver twice, and replacing the
//! mixer closes every task of the old one before attaching each known track to
//! the new one.

use super::*;
use tokio::task::JoinHandle;

/// Same bound as the per-participant source table; a misbehaving peer cannot
/// grow the pending queue without limit.
const MAX_TRACKS: usize = 64;

struct Attachment {
    task: JoinHandle<()>,
    /// Mixer generation the task pushes frames into. Tests read it to prove
    /// that a task follows a replacement instead of a stale mixer.
    #[cfg_attr(not(test), allow(dead_code))]
    mixer: u64,
}

/// Receivers announced by `on_track`, keyed by track id.
#[derive(Default)]
pub(crate) struct TrackSet {
    /// Arrivals not attached yet: no playout was open, or the worker has not
    /// drained them. Never touched from the UI thread.
    pending: VecDeque<(String, RtcAudioTrack)>,
    /// Tracks that are still alive, used to rebuild the mixer after a swap.
    known: HashMap<String, RtcAudioTrack>,
    tasks: HashMap<String, Attachment>,
    /// Tasks closed by a mixer swap. Tests wait for these to finish, which is
    /// only true when they were really aborted.
    #[cfg(test)]
    retired: Vec<JoinHandle<()>>,
}

impl TrackSet {
    /// Queue a receiver announced by `on_track`.
    pub(crate) fn offer(&mut self, track_id: String, track: RtcAudioTrack) {
        if self.pending.len() >= MAX_TRACKS {
            self.pending.pop_front();
        }
        self.pending.push_back((track_id, track));
    }

    /// Drop handles whose stream ended on its own. The task already removed
    /// the source from the mixer, so the id must not be reattached later.
    pub(crate) fn prune(&mut self) {
        let ended: Vec<String> = self
            .tasks
            .iter()
            .filter(|(_, attachment)| attachment.task.is_finished())
            .map(|(id, _)| id.clone())
            .collect();
        for id in ended {
            self.tasks.remove(&id);
            self.known.remove(&id);
        }
    }

    /// Attach every queued receiver to `playout`. A re-offered id closes its
    /// previous task first, so both tracks are never mixed at the same time.
    pub(crate) fn pump(&mut self, playout: &Playout, runtime: &tokio::runtime::Handle) {
        self.prune();
        while let Some((id, track)) = self.pending.pop_front() {
            self.attach_one(id, track, playout, runtime);
        }
    }

    /// Rebuild all attachments after the mixer was replaced: close the tasks
    /// of the old mixer first, then attach each active track exactly once.
    pub(crate) fn reattach(&mut self, playout: &Playout, runtime: &tokio::runtime::Handle) {
        self.prune();
        let ids: Vec<String> = self.tasks.keys().cloned().collect();
        for id in ids {
            self.close(&id);
        }
        // A queued arrival is newer than the handle that was known before.
        while let Some((id, track)) = self.pending.pop_front() {
            self.known.insert(id, track);
        }
        let active: Vec<(String, RtcAudioTrack)> = self
            .known
            .iter()
            .map(|(id, track)| (id.clone(), track.clone()))
            .collect();
        for (id, track) in active {
            self.attach_one(id, track, playout, runtime);
        }
    }

    /// Close every task but remember the tracks: a resume attaches them again
    /// to the next mixer instead of waiting for another `on_track`.
    pub(crate) fn detach(&mut self) {
        let ids: Vec<String> = self.tasks.keys().cloned().collect();
        for id in ids {
            self.close(&id);
        }
    }

    /// The single attachment path shared by new arrivals and restored tracks.
    fn attach_one(
        &mut self,
        id: String,
        track: RtcAudioTrack,
        playout: &Playout,
        runtime: &tokio::runtime::Handle,
    ) {
        self.close(&id);
        self.known.insert(id.clone(), track.clone());
        let task = playout.clip().attach(id.clone(), track, runtime.clone());
        self.tasks.insert(
            id,
            Attachment {
                task,
                mixer: playout.serial(),
            },
        );
    }

    fn close(&mut self, track_id: &str) {
        if let Some(attachment) = self.tasks.remove(track_id) {
            attachment.task.abort();
            #[cfg(test)]
            self.retired.push(attachment.task);
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.tasks.len()
    }

    #[cfg(test)]
    pub(crate) fn known_len(&self) -> usize {
        self.known.len()
    }

    #[cfg(test)]
    pub(crate) fn attached_to(&self, track_id: &str) -> Option<u64> {
        self.tasks.get(track_id).map(|attachment| attachment.mixer)
    }

    #[cfg(test)]
    pub(crate) fn retired(&self) -> &[JoinHandle<()>] {
        &self.retired
    }
}
