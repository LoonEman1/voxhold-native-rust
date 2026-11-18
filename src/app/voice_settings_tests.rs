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
