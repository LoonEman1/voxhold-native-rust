//! Voice settings that cross the UI/media boundary: one snapshot per change,
//! endpoint-id selection, a hot-unplug through the meter tick and the debounced
//! atomic write.

use super::*;
use gpui::{TestAppContext, VisualTestContext};
use voxhold_media::runtime::VoiceUpdate;

fn device(id: &str, name: &str, input: bool) -> crate::voice_settings::Device {
    crate::voice_settings::Device {
        id: id.into(),
        name: name.into(),
        input,
    }
}

fn draw(visual: &mut VisualTestContext) {
    visual.update(|window, app| {
        window.refresh();
        window.draw(app).clear();
    });
    visual.run_until_parked();
}

/// The regular call fixture with the voice tab open and the panel able to
/// report the snapshot it hands to the media layer.
fn voice_call(
    cx: &mut TestAppContext,
) -> (
    &'static mut VisualTestContext,
    Entity<Voxhold>,
    Entity<voice_panel::VoicePanel>,
) {
    let (mut visual, view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.test_set_call(7, crate::voice_room::CallPhase::Connected, vec![], vec![]);
            cx.notify();
        });
        view.update(app, |v, cx| {
            v.active_settings = Some(SettingsKind::Personal);
            v.personal.as_mut().unwrap().dialog.active = 1;
            v.voice_settings.path = None;
            v.voice_settings.devices = vec![
                device("mic-a", "USB Mic", true),
                device("spk-a", "Speakers", false),
            ];
            cx.notify();
        });
        let _ = window;
    });
    draw(&mut visual);
    (visual, view, voice)
}

#[gpui::test]
async fn a_per_user_volume_reaches_the_snapshot_and_is_removed_at_one_hundred(
    cx: &mut TestAppContext,
) {
    let (visual, view, voice) = voice_call(cx);
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            use personal_settings::Action;
            assert!(v.voice_settings_action(Action::SetVoiceUserVolume(2, 150), cx));
            let snapshot = voice
                .update(cx, |panel, _| panel.test_take_audio_snapshot())
                .expect("the per-user volume is one State snapshot");
            assert_eq!(snapshot.0.user_volumes.get(&2), Some(&150));
            assert_eq!(v.voice_settings.prefs.user_volumes.get(&2), Some(&150));
            assert_eq!(
                voice.read(cx).user_volume(2),
                150,
                "the panel keeps the stable user key"
            );
            // 100 % is the default and is not persisted as an override.
            assert!(v.voice_settings_action(Action::SetVoiceUserVolume(2, 100), cx));
            let snapshot = voice
                .update(cx, |panel, _| panel.test_take_audio_snapshot())
                .expect("removing the override is also one snapshot");
            assert!(snapshot.0.user_volumes.get(&2).is_none());
            assert!(!v.voice_settings.prefs.user_volumes.contains_key(&2));
        });
    });
}

#[test]
fn the_mixer_line_disappears_when_the_software_mixer_stops() {
    use voxhold_media::mixer::{PlayoutPhase, PlayoutStats};
    let live = PlayoutStats {
        level: 0.4,
        running: true,
        phase: PlayoutPhase::Stable,
        ..PlayoutStats::default()
    };
    assert!(
        super::settings_voice_view::mixer_status_line(Some(live), true).is_some(),
        "a live mixer shows its measured line"
    );
    // After the death/ADM fallback the panel's `software_mixer()` is false;
    // the stale running snapshot must not keep the line alive.
    assert!(
        super::settings_voice_view::mixer_status_line(Some(live), false).is_none(),
        "the line is hidden once the applied path is no longer software"
    );
    let dead = PlayoutStats {
        running: false,
        ..live
    };
    assert!(
        super::settings_voice_view::mixer_status_line(Some(dead), true).is_none(),
        "a final running=false report hides the line as well"
    );
}

#[gpui::test]
async fn the_playout_report_is_kept_for_the_measured_mixer_line(cx: &mut TestAppContext) {
    let (visual, _view, voice) = voice_call(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_apply_update(
                VoiceUpdate::AudioMode(voxhold_media::controller::AppliedAudio {
                    capture: "software",
                    noise_mode: "auto",
                    echo_cancellation: false,
                    auto_gain_control: true,
                    software_mixer: true,
                }),
                window,
                cx,
            );
            panel.test_apply_update(
                VoiceUpdate::Playout(voxhold_media::mixer::PlayoutStats {
                    level: 0.42,
                    peak: 0.6,
                    underruns: 3,
                    dropped: 1,
                    p95_micros: 120,
                    running: true,
                    ..Default::default()
                }),
                window,
                cx,
            );
            let stats = panel.playout_stats().expect("the report is stored");
            assert_eq!(stats.p95_micros, 120);
            assert_eq!(stats.underruns, 3);
            assert!(
                panel.software_mixer(),
                "a live mixer report and the applied snapshot agree"
            );
            // A dead sink must stop the claim even though the start snapshot
            // still says software_mixer.
            panel.test_apply_update(
                VoiceUpdate::Playout(voxhold_media::mixer::PlayoutStats {
                    running: false,
                    ..stats
                }),
                window,
                cx,
            );
            assert!(
                !panel.software_mixer(),
                "the UI must not claim a mixer that is not running"
            );
            panel.test_apply_update(
                VoiceUpdate::Playout(voxhold_media::mixer::PlayoutStats {
                    running: true,
                    ..stats
                }),
                window,
                cx,
            );
            assert!(panel.software_mixer(), "a recovered sink is claimed again");
        });
    });
}

#[gpui::test]
async fn the_output_gain_follows_the_active_playback_path(cx: &mut TestAppContext) {
    let (visual, view, voice) = voice_call(cx);
    visual.update(|window, app| {
        // Before any path is reported the stored 150 % is already the value the
        // next call will use.
        view.update(app, |v, cx| {
            use personal_settings::Action;
            assert!(v.voice_settings_action(Action::SetVolume("output", 150), cx));
        });
        voice.update(app, |panel, _| {
            assert_eq!(
                panel.test_controls().output_gain,
                Some(1.5),
                "an unknown path keeps the stored percentage"
            );
            let snapshot = panel
                .test_take_audio_snapshot()
                .expect("the settings change is forwarded");
            assert_eq!(snapshot.0.output_gain, Some(1.5));
        });

        let adm = || voxhold_media::controller::AppliedAudio {
            capture: "platform",
            noise_mode: "auto",
            echo_cancellation: false,
            auto_gain_control: true,
            software_mixer: false,
        };
        voice.update(app, |panel, cx| {
            assert!(panel.test_apply_update(VoiceUpdate::AudioMode(adm()), window, cx));
            assert_eq!(
                panel.test_controls().output_gain,
                None,
                "the ADM has no numeric room gain"
            );
            let snapshot = panel
                .test_take_audio_snapshot()
                .expect("the path change forwards the updated controls");
            assert_eq!(snapshot.0.output_gain, None);
            // Deafen is the real room mute on the ADM path: it still reaches
            // the engine while the numeric gain stays absent.
            panel.deafen(cx);
            let snapshot = panel
                .test_take_audio_snapshot()
                .expect("deafen is forwarded");
            assert!(snapshot.0.deafened);
            assert_eq!(snapshot.0.output_gain, None);
            panel.deafen(cx);
            let _ = panel.test_take_audio_snapshot();
        });

        // The software mixer returns: the stored percentage is restored and
        // sent without reopening the settings tab.
        let software = voxhold_media::controller::AppliedAudio {
            capture: "software",
            noise_mode: "auto",
            echo_cancellation: false,
            auto_gain_control: true,
            software_mixer: true,
        };
        voice.update(app, |panel, cx| {
            assert!(panel.test_apply_update(VoiceUpdate::AudioMode(software), window, cx));
            assert_eq!(panel.test_controls().output_gain, Some(1.5));
            let snapshot = panel
                .test_take_audio_snapshot()
                .expect("the restored gain is forwarded");
            assert_eq!(snapshot.0.output_gain, Some(1.5));
        });

        // Leaving the call restores the stored value for the next join after
        // the ADM had cleared the numeric field.
        voice.update(app, |panel, cx| {
            panel.leave_call(cx);
            assert_eq!(panel.test_controls().output_gain, Some(1.5));
        });
    });
}

#[gpui::test]
async fn the_output_slider_keeps_its_value_but_is_disabled_on_the_adm(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = voice_call(cx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            use personal_settings::Action;
            assert!(v.voice_settings_action(Action::SetVolume("output", 150), cx));
        });
        voice.update(app, |panel, cx| {
            assert!(panel.test_apply_update(
                VoiceUpdate::AudioMode(voxhold_media::controller::AppliedAudio {
                    capture: "platform",
                    noise_mode: "auto",
                    echo_cancellation: false,
                    auto_gain_control: true,
                    software_mixer: false,
                }),
                window,
                cx,
            ));
        });
        let _ = window;
    });
    draw(&mut visual);
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.voice_settings.prefs.output_volume, 150,
            "the stored percentage stays visible in the form"
        );
        assert!(
            v.test_slider_focus("output-volume").is_none(),
            "the ADM slider is not a Tab stop"
        );
    });
    assert!(visual.debug_bounds("output-volume").is_some());
    assert!(
        visual.debug_bounds("output-volume-value").is_some(),
        "the percentage is painted, not hidden"
    );
    assert!(
        visual.debug_bounds("output-volume-adm-note").is_some(),
        "the form states that only mute is applied"
    );
    // A click on the track must not change the stored value.
    let track = view
        .read_with(&*visual, |v, _| v.test_slider_track("output-volume"))
        .expect("the disabled slider is still measured");
    let row = visual
        .debug_bounds("output-volume-track")
        .expect("the disabled track is painted");
    let y = f32::from(row.origin.y) + f32::from(row.size.height) / 2.;
    let at = gpui::point(gpui::px(track.origin_x + track.width * 0.25), gpui::px(y));
    visual.simulate_mouse_move(at, None, gpui::Modifiers::default());
    visual.simulate_click(at, gpui::Modifiers::default());
    draw(&mut visual);
    view.read_with(&*visual, |v, _| {
        assert_eq!(
            v.voice_settings.prefs.output_volume, 150,
            "a disabled ADM slider ignores the click"
        );
    });

    // The software mixer returns: the same stored percentage becomes editable.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            assert!(panel.test_apply_update(
                VoiceUpdate::AudioMode(voxhold_media::controller::AppliedAudio {
                    capture: "software",
                    noise_mode: "auto",
                    echo_cancellation: false,
                    auto_gain_control: true,
                    software_mixer: true,
                }),
                window,
                cx,
            ));
        });
    });
    draw(&mut visual);
    view.read_with(&*visual, |v, _| {
        assert!(
            v.test_slider_focus("output-volume").is_some(),
            "the software mixer makes the slider a Tab stop again"
        );
        assert_eq!(v.voice_settings.prefs.output_volume, 150);
    });

    // Outside a call the stored value is editable for the next join.
    visual.update(|_, app| {
        voice.update(app, |panel, cx| panel.leave_call(cx));
    });
    draw(&mut visual);
    view.read_with(&*visual, |v, _| {
        assert!(v.test_slider_focus("output-volume").is_some());
        assert_eq!(v.voice_settings.prefs.output_volume, 150);
    });
}

#[test]
fn the_output_slider_is_editable_outside_a_call_and_on_the_software_mixer() {
    use super::settings_voice_view::output_gain_editable;
    assert!(
        output_gain_editable(false, false),
        "outside a call it is stored for the next join"
    );
    assert!(output_gain_editable(false, true));
    assert!(
        output_gain_editable(true, true),
        "the software mixer applies it"
    );
    assert!(
        !output_gain_editable(true, false),
        "the ADM keeps the value but cannot apply it"
    );
}

/// Both cross cases: the output row follows the playback path, not the
/// capture chain. A software microphone can be played by the ADM, and a
/// platform microphone can coexist with the running software mixer.
#[gpui::test]
async fn the_output_row_follows_playback_not_the_capture_chain(cx: &mut TestAppContext) {
    use crate::audio_capabilities::OutputGainPath;
    use voxhold_media::mixer::{PlayoutPhase, PlayoutStats};
    let (mut visual, view, voice) = voice_call(cx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            use personal_settings::Action;
            assert!(v.voice_settings_action(Action::SetVolume("output", 150), cx));
            v.voice_settings.diagnostics_open = true;
            cx.notify();
        });
        voice.update(app, |panel, cx| {
            // Cross case 1: the microphone opened in software, but the session
            // has no software mixer, so the ADM plays the room.
            assert!(panel.test_apply_update(
                VoiceUpdate::AudioMode(voxhold_media::controller::AppliedAudio {
                    capture: "software",
                    noise_mode: "auto",
                    echo_cancellation: false,
                    auto_gain_control: true,
                    software_mixer: false,
                }),
                window,
                cx,
            ));
            assert_eq!(
                panel.output_gain_path(),
                OutputGainPath::Adm,
                "a software microphone does not imply a software room"
            );
            assert_eq!(
                panel.test_controls().output_gain,
                None,
                "the ADM receives no numeric gain"
            );
            let _ = window;
        });
    });
    draw(&mut visual);
    view.read_with(&*visual, |v, _| {
        assert!(
            v.test_slider_focus("output-volume").is_none(),
            "no running mixer means the slider is disabled"
        );
    });
    assert!(
        visual.debug_bounds("cap-output-tag-недоступно").is_some(),
        "«Подробнее» must not promise gain while the ADM plays"
    );
    assert!(
        visual.debug_bounds("output-volume-adm-note").is_some(),
        "the disabled slider explains that only mute applies"
    );

    // Cross case 2: the microphone fell back to the platform ADM, but the
    // software mixer is running and applies the numeric gain.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            assert!(panel.test_apply_update(
                VoiceUpdate::AudioMode(voxhold_media::controller::AppliedAudio {
                    capture: "platform",
                    noise_mode: "auto",
                    echo_cancellation: false,
                    auto_gain_control: true,
                    software_mixer: true,
                }),
                window,
                cx,
            ));
            assert!(panel.test_apply_update(
                VoiceUpdate::Playout(PlayoutStats {
                    running: true,
                    phase: PlayoutPhase::Stable,
                    ..PlayoutStats::default()
                }),
                window,
                cx,
            ));
            assert_eq!(
                panel.output_gain_path(),
                OutputGainPath::Software,
                "the running mixer owns playback even after a platform microphone"
            );
            let snapshot = panel
                .test_take_audio_snapshot()
                .expect("the restored numeric gain is forwarded");
            assert_eq!(snapshot.0.output_gain, Some(1.5));
        });
    });
    draw(&mut visual);
    view.read_with(&*visual, |v, _| {
        assert!(
            v.test_slider_focus("output-volume").is_some(),
            "the running mixer keeps the slider editable"
        );
    });
    assert!(
        visual.debug_bounds("cap-output-tag-применяется").is_some(),
        "the row follows the live playback path"
    );
}

/// A mixer that is opening, retrying or suspended is not the ADM: the slider
/// stays disabled, but the sentence and the row must not claim system playback.
#[gpui::test]
async fn a_retrying_or_suspended_mixer_is_not_reported_as_the_adm(cx: &mut TestAppContext) {
    use crate::audio_capabilities::OutputGainPath;
    use crate::voice_form_text::{
        output_gain_note, output_volume_note, OUTPUT_VOLUME_ADM_NOTE, OUTPUT_VOLUME_TEMP_NOTE,
    };
    use voxhold_media::mixer::{PlayoutPhase, PlayoutStats};
    let (mut visual, view, voice) = voice_call(cx);
    visual.update(|window, app| {
        view.update(app, |v, cx| {
            use personal_settings::Action;
            assert!(v.voice_settings_action(Action::SetVolume("output", 150), cx));
            v.voice_settings.diagnostics_open = true;
            cx.notify();
        });
        voice.update(app, |panel, cx| {
            assert!(panel.test_apply_update(
                VoiceUpdate::AudioMode(voxhold_media::controller::AppliedAudio {
                    capture: "software",
                    noise_mode: "auto",
                    echo_cancellation: false,
                    auto_gain_control: true,
                    software_mixer: true,
                }),
                window,
                cx,
            ));
            assert!(panel.test_apply_update(
                VoiceUpdate::Playout(PlayoutStats {
                    running: true,
                    phase: PlayoutPhase::Stable,
                    ..PlayoutStats::default()
                }),
                window,
                cx,
            ));
            assert_eq!(panel.output_gain_path(), OutputGainPath::Software);
            let _ = window;
        });
    });

    // The sink died and the bounded retry owns it now. There is no ADM in this
    // state, so the slider is disabled and the text must not name one.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            assert!(panel.test_apply_update(
                VoiceUpdate::Playout(PlayoutStats {
                    running: false,
                    phase: PlayoutPhase::Opening,
                    ..PlayoutStats::default()
                }),
                window,
                cx,
            ));
            assert_eq!(panel.output_gain_path(), OutputGainPath::Unavailable);
            assert_eq!(
                panel.test_controls().output_gain,
                Some(1.5),
                "the stored percentage is kept for the reopening mixer"
            );
            assert_eq!(
                output_volume_note(panel.output_gain_path()),
                OUTPUT_VOLUME_TEMP_NOTE,
                "a retry is described as temporarily unavailable"
            );
            // The engine follows the final `running=false` report with an
            // `Audio` snapshot whose live `software_mixer` flag is false. That
            // is not proof of the ADM: the retry still owns the room.
            assert!(panel.test_apply_update(
                VoiceUpdate::AudioMode(voxhold_media::controller::AppliedAudio {
                    capture: "software",
                    noise_mode: "auto",
                    echo_cancellation: false,
                    auto_gain_control: true,
                    software_mixer: false,
                }),
                window,
                cx,
            ));
            assert_eq!(panel.output_gain_path(), OutputGainPath::Unavailable);
            assert_eq!(panel.test_controls().output_gain, Some(1.5));

            // A suspended system has no sink and no ADM either.
            panel.power(false, cx);
            assert_eq!(panel.output_gain_path(), OutputGainPath::Unavailable);
            panel.power(true, cx);
        });
    });
    draw(&mut visual);
    view.read_with(&*visual, |v, _| {
        assert!(
            v.test_slider_focus("output-volume").is_none(),
            "a mixer that is not running disables the slider"
        );
    });
    assert!(
        visual.debug_bounds("cap-output-tag-недоступно").is_some(),
        "a transient outage is still unavailable, not applied"
    );

    // The budget is spent: now the ADM really owns the room.
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            assert!(panel.test_apply_update(
                VoiceUpdate::Playout(PlayoutStats {
                    running: false,
                    phase: PlayoutPhase::AdmFallback,
                    ..PlayoutStats::default()
                }),
                window,
                cx,
            ));
            assert_eq!(panel.output_gain_path(), OutputGainPath::Adm);
            assert_eq!(
                panel.test_controls().output_gain,
                None,
                "the ADM must not receive the software percentage"
            );
            assert_eq!(
                output_volume_note(panel.output_gain_path()),
                OUTPUT_VOLUME_ADM_NOTE
            );
            assert_eq!(
                output_gain_note(panel.output_gain_path()),
                OUTPUT_VOLUME_ADM_NOTE,
                "the capability row follows the ADM path"
            );
            let snapshot = panel
                .test_take_audio_snapshot()
                .expect("clearing the gain is forwarded");
            assert_eq!(snapshot.0.output_gain, None);
        });
    });
    draw(&mut visual);
    view.read_with(&*visual, |v, _| {
        assert!(v.test_slider_focus("output-volume").is_none());
    });
    // The ADM note selector was never painted before this draw (the earlier
    // frames showed the transient path note), so it proves the Adm branch.
    // The capability tag cannot: it was already `недоступно` for the
    // transient outage, so a stale frame would satisfy it. The fresh-frame tag
    // assertion lives in `the_first_adm_frame_paints...`; here the note
    // mapping above is the direct check.
    assert!(
        visual.debug_bounds("output-volume-adm-note").is_some(),
        "the spent budget paints the ADM sentence for the first time"
    );
}

#[gpui::test]
async fn closing_the_view_flushes_a_queued_voice_save(cx: &mut TestAppContext) {
    let (mut visual, view, _voice) = voice_call(cx);
    let directory = std::env::temp_dir().join(format!(
        "voxhold-voice-flush-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("prefs.voice.json");
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            use personal_settings::Action;
            v.voice_settings.path = Some(path.clone());
            assert!(v.voice_settings_action(Action::SetVolume("input", 175), cx));
            assert_eq!(
                v.voice_settings.writes, 0,
                "the 150 ms debounce has not fired yet"
            );
            assert!(!path.exists(), "nothing has reached the disk yet");
            // The view is going away: its settings (with the queued save) are
            // dropped before the timer can run, exactly as on window close.
            let settings = std::mem::take(&mut v.voice_settings);
            drop(settings);
        });
    });
    assert_eq!(
        crate::voice_settings::VoicePrefs::load(Some(&path)).input_volume,
        175,
        "the close path must flush the debounced value"
    );
    assert!(!path.with_file_name(".prefs.voice.json.tmp").exists());
    std::fs::remove_dir_all(&directory).unwrap();
}

#[gpui::test]
async fn the_in_call_meter_applies_the_input_gain_and_mute(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = voice_call(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_apply_update(VoiceUpdate::LocalLevel(0.5), window, cx);
        });
        view.update(app, |v, cx| {
            use personal_settings::Action;
            let tick = |v: &mut Voxhold, cx: &mut Context<Voxhold>| {
                let epoch = v.voice_settings.meter_epoch;
                let session = v.session_generation;
                v.voice_meter_tick(epoch, session, cx)
            };
            // The engine reports the pre-gate level of the chain (0.5 here);
            // 0 % gain must show the silence that is actually transmitted.
            assert!(v.voice_settings_action(Action::SetVolume("input", 0), cx));
            assert!(tick(v, cx));
            assert_eq!(
                v.voice_settings.preview_level, 0.0,
                "0 % gain shows silence"
            );
            assert!(v.voice_settings_action(Action::SetVolume("input", 200), cx));
            assert!(tick(v, cx));
            assert!(
                (v.voice_settings.preview_level - 1.0).abs() < 1e-6,
                "200 % doubles the level up to the meter ceiling"
            );
            assert!(v.voice_settings_action(Action::SetVolume("input", 50), cx));
            assert!(tick(v, cx));
            assert!(
                (v.voice_settings.preview_level - 0.25).abs() < 1e-6,
                "50 % halves the reported level"
            );
            // A closed microphone transmits silence, so the in-call bar is 0.
            voice.update(cx, |panel, cx| panel.mute(cx));
            assert!(v.voice_settings.preview_level > 0.0);
            assert!(tick(v, cx));
            assert_eq!(
                v.voice_settings.preview_level, 0.0,
                "a muted microphone shows no level even with gain 50 %"
            );
        });
    });
}

/// Every value of the snapshot, taken from the queue the media worker reads.
#[gpui::test]
async fn every_settings_change_queues_exactly_one_full_snapshot(cx: &mut TestAppContext) {
    let (visual, view, voice) = voice_call(cx);
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            use personal_settings::Action;
            // Each action is one settings change and must leave exactly one
            // snapshot in the media request slot.
            let steps = [
                (Action::SelectInput("mic-a".into()), "input"),
                (Action::SelectOutput("spk-a".into()), "output"),
                (Action::SetBitrate(96), "bitrate"),
                (Action::SetVolume("input", 140), "input gain"),
                (Action::SetVolume("output", 150), "output volume"),
                (Action::SetNoiseMode(1), "noise mode"),
                (Action::SetVolume("noise", 32), "gate"),
                (Action::ToggleAutoGain, "auto gain"),
            ];
            for (action, label) in steps {
                assert!(v.voice_settings_action(action, cx), "{label} is handled");
                let snapshot = voice
                    .update(cx, |panel, _| panel.test_take_audio_snapshot())
                    .unwrap_or_else(|| panic!("{label} must queue one snapshot"));
                // A second read finds nothing: the change did not send a
                // second State behind the first.
                assert!(
                    voice
                        .update(cx, |panel, _| panel.test_take_audio_snapshot())
                        .is_none(),
                    "{label} queued more than one snapshot"
                );
                assert_eq!(
                    snapshot.0.output_gain,
                    Some(v.voice_settings.prefs.output_volume as f64 / 100.0),
                    "{label} carries the current output volume"
                );
                assert!(
                    (snapshot.0.input_gain - v.voice_settings.prefs.input_volume as f32 / 100.0)
                        .abs()
                        < f32::EPSILON,
                    "{label} carries the current input gain"
                );
            }
            // One last change proves the delivered snapshot carries every
            // field of the form at once.
            assert!(v.voice_settings_action(Action::SetVolume("sound", 50), cx));
            let snapshot = voice
                .update(cx, |panel, _| panel.test_take_audio_snapshot())
                .expect("the last change queues its snapshot");
            let controls = &snapshot.0;
            assert_eq!(controls.input.as_deref(), Some("mic-a"));
            assert_eq!(controls.output.as_deref(), Some("spk-a"));
            assert_eq!(controls.bitrate_kbps, Some(96));
            assert!((controls.input_gain - 1.4).abs() < f32::EPSILON);
            assert_eq!(controls.output_gain, Some(1.5));
            assert_eq!(controls.noise_mode.as_str(), "threshold");
            assert_eq!(controls.noise_gate_percent, 32);
            assert!(!controls.auto_gain_control);
            assert!(!controls.echo_cancellation);
        });
    });
}

#[gpui::test]
async fn a_stale_endpoint_id_is_rejected_instead_of_selecting_another_device(
    cx: &mut TestAppContext,
) {
    let (mut visual, view, _voice) = voice_call(cx);
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            use personal_settings::Action;
            v.voice_settings.devices = vec![
                device("mic-a", "USB Mic", true),
                device("mic-b", "Headset", true),
                device("spk-a", "Speakers", false),
            ];
            // The row drawn for "mic-a" carried that endpoint id. The device is
            // then unplugged and the refreshed list starts with "mic-b", so the
            // same row position would now point at a different microphone.
            v.voice_settings.prefs.input = "mic-b".into();
            v.voice_settings
                .devices
                .retain(|device| device.id != "mic-a");
            assert!(v.voice_settings_action(Action::SelectInput("mic-a".into()), cx));
            assert_eq!(
                v.voice_settings.prefs.input, "mic-b",
                "a stale id must not select the device that took its row"
            );
            assert!(
                v.voice_settings
                    .device_notice
                    .contains("больше не подключено"),
                "the user is told why the click did nothing: {}",
                v.voice_settings.device_notice
            );
            assert!(
                v.voice_settings.devices_loading,
                "the stale id triggers a fresh device list"
            );

            // A current id and the system default still work.
            assert!(v.voice_settings_action(Action::SelectInput("mic-b".into()), cx));
            assert!(v.voice_settings_action(Action::SelectInput(String::new()), cx));
            assert_eq!(v.voice_settings.prefs.input, "");
            assert!(v.voice_settings.device_notice.is_empty());
        });
    });
    draw(&mut visual);
}

#[gpui::test]
async fn a_hot_unplug_and_recovery_go_through_the_meter_tick(cx: &mut TestAppContext) {
    let (mut visual, view, voice) = voice_call(cx);
    visual.update(|window, app| {
        view.update(app, |v, _| v.voice_settings.prefs.input = "mic-a".into());
        voice.update(app, |panel, cx| {
            panel.test_apply_update(
                VoiceUpdate::DeviceError("Выбранный микрофон недоступен.".into()),
                window,
                cx,
            );
        });
    });
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            let epoch = v.voice_settings.meter_epoch;
            let session = v.session_generation;
            assert!(v.voice_meter_tick(epoch, session, cx));
            assert_eq!(
                v.voice_settings.device_notice,
                "Выбранный микрофон недоступен."
            );
            // The saved choice is kept and the list is being re-read.
            assert_eq!(v.voice_settings.prefs.input, "mic-a");
            assert_eq!(
                v.voice_settings.device_error_shown,
                v.voice_settings.device_notice
            );
            assert!(v.voice_settings.devices_loading);
        });
    });
    visual.run_until_parked();
    visual.update(|window, app| {
        // The engine reports a working input again (generation 0 is an
        // engine-initiated result), which clears the typed device error.
        voice.update(app, |panel, cx| {
            panel.test_apply_update(
                VoiceUpdate::InputApplied {
                    generation: 0,
                    input: Some("mic-a".into()),
                    error: None,
                },
                window,
                cx,
            );
            assert_eq!(panel.device_error(), "");
            assert_eq!(panel.test_controls().input.as_deref(), Some("mic-a"));
        });
        view.update(app, |v, cx| {
            let epoch = v.voice_settings.meter_epoch;
            let session = v.session_generation;
            assert!(v.voice_meter_tick(epoch, session, cx));
            assert_eq!(
                v.voice_settings.device_notice, "",
                "a recovered device clears the stale notice"
            );
            assert_eq!(
                v.voice_settings.prefs.input, "mic-a",
                "recovery must not drop the saved device"
            );
            assert!(v.voice_settings.devices_loading);
        });
    });
}

#[gpui::test]
async fn rapid_slider_changes_collapse_into_one_atomic_write(cx: &mut TestAppContext) {
    let (mut visual, view, _voice) = voice_call(cx);
    let directory = std::env::temp_dir().join(format!(
        "voxhold-voice-save-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("prefs.voice.json");
    visual.update(|_, app| {
        view.update(app, |v, cx| {
            use personal_settings::Action;
            v.voice_settings.path = Some(path.clone());
            for value in [110, 120, 130, 140] {
                assert!(v.voice_settings_action(Action::SetVolume("input", value), cx));
            }
        });
    });
    // Four changes, one debounce window: exactly one write, and it carries the
    // last value. An older generation cannot win because it is no longer the
    // newest token when the timer fires.
    // `advance_clock` runs ready tasks, so it must run outside `app.update`.
    let executor = visual.update(|_, app| app.background_executor().clone());
    for _ in 0..40 {
        executor.advance_clock(std::time::Duration::from_millis(50));
        visual.run_until_parked();
        if view.read_with(&*visual, |v, _| v.voice_settings.writes) > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    view.read_with(&*visual, |v, _| {
        assert_eq!(v.voice_settings.writes, 1, "one coalesced write");
        assert!(v.voice_settings.saved_error.is_empty());
    });
    assert_eq!(
        crate::voice_settings::VoicePrefs::load(Some(&path)).input_volume,
        140,
        "the newest value wins"
    );
    assert!(!path.with_file_name(".prefs.voice.json.tmp").exists());
    std::fs::remove_dir_all(&directory).unwrap();
}
#[gpui::test]
async fn one_voice_snapshot_carries_devices_bitrate_and_both_gains(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    visual.update(|_, app| {
        voice.update(app, |panel, cx| {
            panel.mute(cx);
            assert!(panel.test_controls_muted(), "the local mute closes the mic");
            panel.apply_voice_prefs(
                Some("1:Mic".into()),
                Some("2:Spk".into()),
                96,
                140,
                0,
                "threshold",
                32,
                false,
                false,
                std::collections::BTreeMap::new(),
            );
            let controls = panel.test_controls();
            assert_eq!(controls.input.as_deref(), Some("1:Mic"));
            assert_eq!(controls.output.as_deref(), Some("2:Spk"));
            assert_eq!(
                controls.bitrate_kbps,
                Some(96),
                "one snapshot carries the selected preset"
            );
            assert_eq!(
                controls.output_gain,
                Some(0.0),
                "0 % closes the room tracks"
            );
            assert_eq!(controls.noise_mode.as_str(), "threshold");
            assert_eq!(controls.noise_gate_percent, 32);
            assert!((controls.input_gain - 1.4).abs() < 1e-6);
            assert!(!controls.echo_cancellation);
            assert!(!controls.auto_gain_control);
            assert!((controls.input_gain - 1.4).abs() < 1e-6);
            assert!(
                panel.test_controls_muted(),
                "a settings snapshot must not clear the local mute"
            );
            panel.apply_voice_prefs(
                None,
                None,
                64,
                0,
                100,
                "auto",
                15,
                true,
                true,
                std::collections::BTreeMap::new(),
            );
            assert_eq!(panel.test_controls().input_gain, 0.0);
            assert_eq!(panel.test_controls().bitrate_kbps, Some(64));
            assert_eq!(panel.test_controls().output_gain, Some(1.0));
            assert_eq!(panel.test_controls().noise_mode.as_str(), "auto");
            assert_eq!(panel.test_controls().input_gain, 0.0);
        });
    });
}
