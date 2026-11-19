//! Output-gain regressions that need a session boundary or a fresh first
//! frame: a previous session's `Playout` report must not leak into the next
//! call, and an ADM frame must paint its own selectors rather than pass on a
//! stale `debug_bounds` from an earlier state.

use super::*;
use gpui::{TestAppContext, VisualTestContext};
use voxhold_media::runtime::VoiceUpdate;

fn draw(visual: &mut VisualTestContext) {
    visual.update(|window, app| {
        window.refresh();
        window.draw(app).clear();
    });
    visual.run_until_parked();
}

/// The regular call fixture with the voice tab and diagnostics open, but
/// without a frame. A test can make the very first draw the state under test,
/// so a selector assertion cannot be satisfied by an earlier paint.
fn voice_call_unpainted(
    cx: &mut TestAppContext,
) -> (
    &'static mut VisualTestContext,
    Entity<Voxhold>,
    Entity<voice_panel::VoicePanel>,
) {
    let (visual, view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.test_set_call(7, crate::voice_room::CallPhase::Connected, vec![], vec![]);
            cx.notify();
        });
        view.update(app, |v, cx| {
            use personal_settings::Action;
            v.active_settings = Some(SettingsKind::Personal);
            v.personal.as_mut().unwrap().dialog.active = 1;
            v.voice_settings.path = None;
            v.voice_settings.diagnostics_open = true;
            assert!(v.voice_settings_action(Action::SetVolume("output", 150), cx));
            cx.notify();
        });
        let _ = window;
    });
    (visual, view, voice)
}

/// A session boundary must drop the old sink's report. Otherwise a new attempt
/// whose software mixer never opened keeps the previous `running, Stable`
/// stats and the numeric gain stays on the software path forever.
#[gpui::test]
async fn a_stale_playout_report_does_not_survive_a_reconnect(cx: &mut TestAppContext) {
    use crate::audio_capabilities::OutputGainPath;
    use voxhold_media::controller::AppliedAudio;
    use voxhold_media::mixer::{PlayoutPhase, PlayoutStats};

    let (visual, _view, voice) = voice_call_unpainted(cx);
    let audio = |software_mixer: bool| AppliedAudio {
        capture: "software",
        noise_mode: "auto",
        echo_cancellation: false,
        auto_gain_control: true,
        software_mixer,
    };
    let stable = PlayoutStats {
        running: true,
        phase: PlayoutPhase::Stable,
        ..PlayoutStats::default()
    };
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            // Session 1: the mixer is open and applies the stored 150 %.
            assert!(panel.test_apply_update(VoiceUpdate::AudioMode(audio(true)), window, cx));
            assert!(panel.test_apply_update(VoiceUpdate::Playout(stable), window, cx));
            assert_eq!(panel.output_gain_path(), OutputGainPath::Software);
            assert_eq!(panel.test_controls().output_gain, Some(1.5));
            let _ = panel.test_take_audio_snapshot();

            // Session 2 starts; no report for its sink has arrived. The stored
            // value is kept until the path is known.
            assert!(panel.test_apply_update(VoiceUpdate::Reconnecting, window, cx));
            assert_eq!(
                panel.output_gain_path(),
                OutputGainPath::Software,
                "before the new path is known the stored value is kept"
            );
            assert_eq!(panel.test_controls().output_gain, Some(1.5));

            // The new mixer failed to open and the ADM plays. Without the
            // clear above, the stale `running, Stable` report still said
            // Software and the percentage stayed enabled.
            assert!(panel.test_apply_update(VoiceUpdate::AudioMode(audio(false)), window, cx));
            assert_eq!(panel.output_gain_path(), OutputGainPath::Adm);
            assert_eq!(panel.test_controls().output_gain, None);
            let snapshot = panel
                .test_take_audio_snapshot()
                .expect("clearing the gain is forwarded");
            assert_eq!(snapshot.0.output_gain, None);

            // A reconnect whose mixer does run restores the stored value.
            assert!(panel.test_apply_update(VoiceUpdate::Reconnecting, window, cx));
            assert_eq!(panel.test_controls().output_gain, Some(1.5));
            assert!(panel.test_apply_update(VoiceUpdate::AudioMode(audio(true)), window, cx));
            assert!(panel.test_apply_update(VoiceUpdate::Playout(stable), window, cx));
            assert_eq!(panel.output_gain_path(), OutputGainPath::Software);
            assert_eq!(panel.test_controls().output_gain, Some(1.5));
            let snapshot = panel
                .test_take_audio_snapshot()
                .expect("the restored gain is forwarded");
            assert_eq!(snapshot.0.output_gain, Some(1.5));

            // `Connecting` is the same boundary: without the clear, a stale
            // running report would survive into an ADM-only attempt.
            assert!(panel.test_apply_update(VoiceUpdate::Connecting, window, cx));
            assert!(panel.test_apply_update(VoiceUpdate::AudioMode(audio(false)), window, cx));
            assert_eq!(panel.output_gain_path(), OutputGainPath::Adm);
            assert_eq!(panel.test_controls().output_gain, None);
        });
    });
}

/// The first painted frame is already the ADM path. Both the ADM sentence and
/// the unavailable capability tag were never painted before, so asserting them
/// proves the Adm branch instead of re-reading a stale selector.
#[gpui::test]
async fn the_first_adm_frame_paints_the_unavailable_tag_and_adm_note(cx: &mut TestAppContext) {
    use crate::audio_capabilities::OutputGainPath;
    use voxhold_media::controller::AppliedAudio;

    let (mut visual, view, voice) = voice_call_unpainted(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            assert!(panel.test_apply_update(
                VoiceUpdate::AudioMode(AppliedAudio {
                    capture: "software",
                    noise_mode: "auto",
                    echo_cancellation: false,
                    auto_gain_control: true,
                    software_mixer: false,
                }),
                window,
                cx,
            ));
            assert_eq!(panel.output_gain_path(), OutputGainPath::Adm);
            assert_eq!(panel.test_controls().output_gain, None);
        });
    });
    draw(&mut visual);
    view.read_with(&*visual, |v, _| {
        assert!(
            v.test_slider_focus("output-volume").is_none(),
            "the ADM keeps the slider out of the Tab ring"
        );
    });
    assert!(
        visual.debug_bounds("output-volume-adm-note").is_some(),
        "the first frame explains that only mute applies"
    );
    assert!(
        visual.debug_bounds("output-volume-path-note").is_none(),
        "the transient-outage sentence is not the ADM one"
    );
    assert!(
        visual.debug_bounds("cap-output-tag-недоступно").is_some(),
        "the first frame already disables the capability row"
    );
    assert!(
        visual.debug_bounds("cap-output-tag-применяется").is_none(),
        "the applied tag was never painted for this fixture"
    );
}
