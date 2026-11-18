//! UI gate of the screen publication: the «Экран» action must be unavailable
//! without a confirmed join, the Stream permission or a free publisher slot.

use crate::voice_room::CallPhase;
use gpui::TestAppContext;

fn publication(index: usize) -> voxhold_media::controller::Publication {
    voxhold_media::controller::Publication {
        server_id: 1,
        channel_id: 7,
        publisher_user_id: 100 + index as i64,
        publisher_connection_id: format!("publisher-{index}"),
        mode: "server".into(),
        codec: "vp8".into(),
        has_audio: false,
        viewer_count: 0,
        started_at_ms: 0,
    }
}

#[gpui::test]
async fn the_screen_action_needs_a_confirmed_join_permission_and_slot(cx: &mut TestAppContext) {
    let (visual, _view, voice) = super::voice_tests::fixture(cx);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            // Permission Stream on the channel is the first gate.
            panel.set_stream_permission(false);
            let error = panel.share_gate().expect_err("no Stream permission");
            assert!(error.contains("права"), "{error}");

            // The join must be confirmed in this room (the fixture starts
            // connected and then loses the call).
            panel.set_stream_permission(true);
            panel.test_set_call(0, CallPhase::Idle, Vec::new(), Vec::new());
            let error = panel.share_gate().expect_err("not in the call");
            assert!(error.contains("подключитесь"), "{error}");
            panel.test_set_call(7, CallPhase::Connected, Vec::new(), Vec::new());

            // A full channel has no publisher slot, independent of the host
            // capture stack.
            panel.test_set_streams((0..8).map(publication).collect());
            let error = panel.share_gate().expect_err("channel full");
            assert!(error.contains("8 трансляций"), "{error}");
            panel.test_set_streams(Vec::new());

            // With every server-side gate open the answer depends only on the
            // local capture/codec capability; the test states which it got
            // instead of pretending capture always exists.
            let capability = voxhold_media::screen_capture::capability_cached();
            let codec = voxhold_media::publisher::Codec::available();
            match panel.share_gate() {
                Ok(()) => {
                    assert!(capability.is_ok(), "gate passed without a capturer");
                    assert!(!codec.is_empty(), "gate passed without a codec");
                }
                Err(error) => {
                    assert!(
                        capability.is_err() || codec.is_empty(),
                        "gate failed for an unexpected reason: {error}"
                    );
                }
            }
        });
    });
}

#[gpui::test]
async fn an_unconfirmed_acl_disables_the_screen_action(cx: &mut TestAppContext) {
    let (visual, _view, voice) = super::voice_tests::fixture(cx);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.set_stream_permission(true);
            panel.set_access_confirmed(false);
            let error = panel.share_gate().expect_err("ACL is stale");
            assert!(error.contains("подтверждены"), "{error}");
            panel.set_access_confirmed(true);
        });
    });
}
