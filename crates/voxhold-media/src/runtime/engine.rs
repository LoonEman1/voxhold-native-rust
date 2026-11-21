//! Interpretation of one engine update. Publishing the decoded picture lives
//! here so a prepared picture and a transport confirmation stay visibly
//! distinct: `Frame` and `Decoded` confirm the connection, `Repaint` never
//! does, and `Decoded` publishes no picture at all.

use crate::{
    VideoFrame,
    stream::EngineUpdate,
    viewer::{Outcome as ViewerOutcome, Viewer},
};
use std::{sync::Mutex, time::Instant};

use tokio::sync::Notify;

use super::StreamFrame;

/// Move the freshly decoded picture into the last-value slot, reclaiming the
/// buffer the UI has not taken yet so steady state does not allocate. A ready
/// picture is an event: it wakes the panel, which then takes it at its frame
/// deadline. A decode that prepared no picture never reaches here.
fn publish_frame(
    slot: &Mutex<Option<StreamFrame>>,
    render: &mut VideoFrame,
    selection: u64,
    wake: &Notify,
) {
    let mut next = slot
        .lock()
        .ok()
        .and_then(|mut slot| {
            // A frame from another selection is not a buffer to reclaim.
            slot.take()
                .filter(|frame| frame.selection == selection)
                .map(|frame| frame.bgra)
        })
        .unwrap_or_default();
    if next.len() < render.bgra.len() {
        next.resize(render.bgra.len(), 0);
    }
    let (width, height) = (render.width(), render.height());
    std::mem::swap(&mut next, &mut render.bgra);
    if let Ok(mut slot) = slot.lock() {
        *slot = Some(StreamFrame {
            selection,
            width,
            height,
            bgra: next,
        });
    }
    wake.notify_one();
}

/// Publish the decoded picture for the current selection. `fresh` marks a new
/// RTP frame, which also confirms the connection; a cached repaint at a new
/// viewport size must leave recovery and its deadline alone, otherwise a
/// failed ICE restart would never time out behind a frozen picture. `Decoded`
/// confirms the connection in `apply_engine_update` without reaching here.
fn picture_outcomes(
    slot: &Mutex<Option<StreamFrame>>,
    render: &mut VideoFrame,
    viewer: &mut Viewer,
    fresh: bool,
    wake: &Notify,
) -> Vec<ViewerOutcome> {
    if let Some(selection) = viewer.watching().map(|watching| watching.selection) {
        publish_frame(slot, render, selection, wake);
    }
    if fresh { viewer.on_frame() } else { Vec::new() }
}

/// Runtime effect of one engine update. Audio output results are separate
/// because they update the session caches and notify the panel instead of
/// running the viewer state machine.
pub(super) enum EngineEffect {
    Outcomes(Vec<ViewerOutcome>),
    Output {
        applied: Option<String>,
        working: bool,
        error: Option<String>,
    },
}

/// Interpret one engine update exactly once, so a cached repaint can never be
/// routed through the fresh-frame confirmation path.
pub(super) fn apply_engine_update(
    slot: &Mutex<Option<StreamFrame>>,
    render: &mut VideoFrame,
    viewer: &mut Viewer,
    update: EngineUpdate,
    now: Instant,
    wake: &Notify,
) -> EngineEffect {
    match update {
        EngineUpdate::Frame => {
            EngineEffect::Outcomes(picture_outcomes(slot, render, viewer, true, wake))
        }
        // The decode still confirms the transport; only its picture was not
        // prepared because the player had not asked for one. Publishing the
        // previous surface here would redraw a stale size after a switch.
        EngineUpdate::Decoded => EngineEffect::Outcomes(viewer.on_frame()),
        EngineUpdate::Repaint => {
            EngineEffect::Outcomes(picture_outcomes(slot, render, viewer, false, wake))
        }
        EngineUpdate::Answer(sdp) => EngineEffect::Outcomes(viewer.on_answer(sdp)),
        EngineUpdate::Ice(candidate) => EngineEffect::Outcomes(viewer.on_ice(candidate)),
        EngineUpdate::State(state) => EngineEffect::Outcomes(viewer.on_engine_state(state, now)),
        EngineUpdate::Stats {
            attempt,
            bytes_received,
            frames_decoded,
        } => EngineEffect::Outcomes(viewer.on_video_stats(
            attempt,
            bytes_received,
            frames_decoded,
            now,
        )),
        EngineUpdate::Error(message) => {
            EngineEffect::Outcomes(viewer.on_engine_error(&message, now))
        }
        EngineUpdate::Output {
            applied,
            audible: _,
            working,
            error,
        } => EngineEffect::Output {
            applied,
            working,
            error,
        },
    }
}
