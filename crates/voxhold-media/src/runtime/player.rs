//! Lifetime of the player engine of the current viewing attempt.
//!
//! Closing a view must never wait for the WebRTC peer while `stream.leave` or
//! the next voice event is still queued. The engine and selection leave the
//! session immediately, the published frame is cleared, and the engine is
//! dropped on the shared release thread (the same path as a publication
//! engine). `Media` also signals its worker ahead of the SDP/ICE queue.

use super::*;
use crate::stream::StreamViewer;

impl Live {
    /// The one close path of the player. Switching, an error, leaving and a
    /// lost transport all call this before their viewer outcomes (including
    /// `stream.leave`) are executed.
    pub(super) fn close_stream(&mut self) {
        close_player(&mut self.stream, &mut self.stream_selection, &self.frame);
    }
}

/// Remove the engine and its selection from the session and clear the picture
/// immediately. The peer is released outside the signaling loop: its `Drop`
/// signals the worker stop, asks the voice peer to close and joins the media
/// worker on the release thread.
pub(super) fn close_player(
    stream: &mut Option<StreamViewer>,
    selection: &mut Option<u64>,
    frame: &Mutex<Option<StreamFrame>>,
) {
    *selection = None;
    if let Some(stream) = stream.take() {
        screen::release_on_worker(stream, || {});
    }
    // A frame decoded just before the close must not be published by the UI
    // after the viewer is gone, even when no engine was installed yet.
    if let Ok(mut frame) = frame.lock() {
        frame.take();
    }
}
