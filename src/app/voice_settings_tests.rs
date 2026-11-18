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
