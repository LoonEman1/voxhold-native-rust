//! UI gate of the screen publication: the «Экран» action must be unavailable
//! without a confirmed join, the Stream permission or a free publisher slot.

use crate::voice_room::CallPhase;
use gpui::TestAppContext;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use voxhold_media::screen_capture::{
    MonitorIdentity, MonitorRect, ScreenSource, SourceKind, TopologySnapshot,
};
use voxhold_media::system_audio::AudioDeviceInfo;

pub(super) fn monitor_identity(display: u32) -> MonitorIdentity {
    let left = display.saturating_sub(1) as i32 * 1920;
    let bounds = MonitorRect::new(left, 0, 1920, 1080);
    MonitorIdentity {
        device: format!("\\\\.\\DISPLAY{display}"),
        bounds,
        work: bounds,
        width: bounds.width,
        height: bounds.height,
    }
}

/// A monitor row whose display is `display`, independent of the capturer id
/// so a test can model the same number moving to another display.
pub(super) fn monitor_on(
    id: u64,
    display: u32,
    title: &str,
    width: u32,
    height: u32,
) -> ScreenSource {
    let identity = monitor_identity(display);
    ScreenSource {
        id,
        kind: SourceKind::Monitor,
        title: title.into(),
        display_id: -1,
        width,
        height,
        topology: Some(TopologySnapshot::from_monitors(vec![identity.clone()])),
        monitor: Some(identity),
    }
}

pub(super) fn monitor(id: u64, title: &str, width: u32, height: u32) -> ScreenSource {
    monitor_on(id, id as u32 + 1, title, width, height)
}

pub(super) fn window(id: u64, title: &str) -> ScreenSource {
    ScreenSource {
        id,
        kind: SourceKind::Window,
        title: title.into(),
        display_id: -1,
        width: 1280,
        height: 720,
        monitor: None,
        topology: None,
    }
}

fn source() -> ScreenSource {
    monitor(3, "Экран 1", 1920, 1080)
}

pub(super) fn draw(visual: &mut gpui::VisualTestContext) {
    visual.update(|window, app| {
        window.refresh();
        window.draw(app).clear();
    });
    visual.run_until_parked();
}

/// Open the picker through the test seam and paint it once.
pub(super) fn open_picker(
    visual: &mut gpui::VisualTestContext,
    voice: &gpui::Entity<crate::voice_panel::VoicePanel>,
) {
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_attach_detached_handle();
            panel.test_open_screen_picker(source());
        });
    });
    // The focus request is deferred to the frame callback; the second draw
    // installs it on the rendered focus path.
    draw(visual);
    draw(visual);
}

/// Open the picker with a controlled list and paint it once.
pub(super) fn open_picker_with(
    visual: &mut gpui::VisualTestContext,
    voice: &gpui::Entity<crate::voice_panel::VoicePanel>,
    kind: SourceKind,
    sources: Vec<ScreenSource>,
) {
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_attach_detached_handle();
            panel.test_open_screen_picker_with(kind, sources, None);
        });
    });
    draw(visual);
    draw(visual);
}

pub(super) fn focused(
    visual: &mut gpui::VisualTestContext,
    voice: &gpui::Entity<crate::voice_panel::VoicePanel>,
) -> Option<String> {
    visual.update(|window, app| voice.read(app).test_focused_share(window))
}

/// Bounded wait for a background result on the GPUI test loop.
pub(super) fn wait_for(
    visual: &mut gpui::VisualTestContext,
    voice: &gpui::Entity<crate::voice_panel::VoicePanel>,
    mut done: impl FnMut(&crate::voice_panel::VoicePanel) -> bool,
) {
    for _ in 0..800 {
        visual.run_until_parked();
        if voice.read_with(&*visual, |panel, _| done(panel)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the background screen-audio probe did not finish");
}

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

#[gpui::test]
async fn the_picker_takes_focus_and_the_keyboard_selects_and_starts(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    open_picker(&mut visual, &voice);
    voice.read_with(&*visual, |panel, _| {
        assert!(panel.screen_share_open(), "the picker opened");
        assert!(
            panel.test_picker_focus_is_separate(),
            "the picker handles must not overlap the toolbar or the on-air card"
        );
    });
    assert_eq!(
        focused(&mut visual, &voice),
        Some("Monitors".to_owned()),
        "the modal owns the keyboard from the first frame"
    );
    assert!(
        visual.debug_bounds("screen-share-card").is_some(),
        "the picker must be painted"
    );

    // Tab to the only source row and select it with Enter.
    visual.simulate_keystrokes("tab");
    visual.simulate_keystrokes("tab");
    visual.simulate_keystrokes("tab");
    assert_eq!(
        focused(&mut visual, &voice),
        Some("Source(monitor:3)".to_owned())
    );
    visual.simulate_keystrokes("enter");

    // Walk to Start and run it.
    for _ in 0..24 {
        if focused(&mut visual, &voice).as_deref() == Some("Start") {
            break;
        }
        visual.simulate_keystrokes("tab");
    }
    assert_eq!(focused(&mut visual, &voice), Some("Start".to_owned()));
    visual.simulate_keystrokes("enter");

    let commands = visual
        .update(|_window, app| voice.update(app, |panel, _| panel.test_take_screen_commands()));
    assert_eq!(
        commands,
        vec![("publish", 7)],
        "Enter on Start publishes the selected source"
    );
    voice.read_with(&*visual, |panel, _| {
        assert!(panel.screen_share_open(), "the dialog waits for the worker");
    });
}

#[gpui::test]
async fn escape_before_start_only_cancels_the_picker(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    open_picker(&mut visual, &voice);
    visual.simulate_keystrokes("escape");
    draw(&mut visual);
    let commands = visual
        .update(|_window, app| voice.update(app, |panel, _| panel.test_take_screen_commands()));
    assert_eq!(
        commands,
        vec![("cancel", 7)],
        "closing before Start never touches the publication"
    );
    voice.read_with(&*visual, |panel, _| assert!(!panel.screen_share_open()));
}

#[gpui::test]
async fn escape_after_start_stops_the_committed_publication(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    open_picker(&mut visual, &voice);
    // Select the source and start it with the keyboard.
    for _ in 0..3 {
        visual.simulate_keystrokes("tab");
    }
    visual.simulate_keystrokes("enter");
    for _ in 0..24 {
        if focused(&mut visual, &voice).as_deref() == Some("Start") {
            break;
        }
        visual.simulate_keystrokes("tab");
    }
    visual.simulate_keystrokes("enter");
    let published = visual
        .update(|_window, app| voice.update(app, |panel, _| panel.test_take_screen_commands()));
    assert_eq!(published, vec![("publish", 7)]);

    // Escape while the attempt is committed is a stop, not a stale cancel.
    visual.simulate_keystrokes("escape");
    draw(&mut visual);
    let commands = visual
        .update(|_window, app| voice.update(app, |panel, _| panel.test_take_screen_commands()));
    assert_eq!(
        commands,
        vec![("stop", 0)],
        "Escape sends exactly one stop and no cancel: {commands:?}"
    );
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.screen_share_open());
        assert!(!panel.screen_share_busy());
    });
}

#[gpui::test]
async fn cancel_while_busy_never_enqueues_a_cancel_and_the_close_is_a_stop(
    cx: &mut TestAppContext,
) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    open_picker(&mut visual, &voice);

    // Select the source and commit the attempt with the keyboard.
    for _ in 0..3 {
        visual.simulate_keystrokes("tab");
    }
    assert_eq!(
        focused(&mut visual, &voice),
        Some("Source(monitor:3)".to_owned())
    );
    visual.simulate_keystrokes("enter");
    for _ in 0..24 {
        if focused(&mut visual, &voice).as_deref() == Some("Start") {
            break;
        }
        visual.simulate_keystrokes("tab");
    }
    assert_eq!(focused(&mut visual, &voice), Some("Start".to_owned()));
    visual.simulate_keystrokes("enter");
    let published = visual
        .update(|_window, app| voice.update(app, |panel, _| panel.test_take_screen_commands()));
    assert_eq!(published, vec![("publish", 7)]);
    voice.read_with(&*visual, |panel, _| {
        assert!(panel.screen_share_busy(), "the attempt is committed")
    });

    // Walk to the Cancel button and activate it while the attempt is busy.
    for _ in 0..24 {
        if focused(&mut visual, &voice).as_deref() == Some("Cancel") {
            break;
        }
        visual.simulate_keystrokes("tab");
    }
    assert_eq!(focused(&mut visual, &voice), Some("Cancel".to_owned()));
    visual.simulate_keystrokes("enter");
    let commands = visual
        .update(|_window, app| voice.update(app, |panel, _| panel.test_take_screen_commands()));
    assert!(
        commands.iter().all(|(kind, _)| *kind != "cancel"),
        "Cancel after Start must never reach the worker as a cancel: {commands:?}"
    );
    assert!(
        commands.is_empty(),
        "the disabled Cancel button is inert while the attempt is busy: {commands:?}"
    );

    // The dialog's close intent (Escape is the same path as the backdrop) is
    // the only way out while busy: exactly one stop and no cancel.
    visual.simulate_keystrokes("escape");
    draw(&mut visual);
    let commands = visual
        .update(|_window, app| voice.update(app, |panel, _| panel.test_take_screen_commands()));
    assert!(
        commands.iter().all(|(kind, _)| *kind != "cancel"),
        "closing a committed attempt is not a cancel: {commands:?}"
    );
    assert_eq!(
        commands,
        vec![("stop", 0)],
        "a committed close is exactly one stop: {commands:?}"
    );
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.screen_share_open());
        assert!(!panel.screen_share_busy());
    });
}

#[gpui::test]
async fn the_live_quality_dialog_warns_and_restarts_with_the_new_values(cx: &mut TestAppContext) {
    use voxhold_media::publisher::{Phase, Preset, Quality, Resolution, Sharing};
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            let sharing = Sharing {
                phase: Phase::Live,
                live: true,
                quality: Some(Quality {
                    resolution: Resolution::P720,
                    fps: 30,
                    bitrate_kbps: 4000,
                    audio_bitrate_kbps: 128,
                }),
                has_audio: true,
                ..Sharing::default()
            };
            panel.test_open_screen_settings(source(), sharing, cx);
            assert!(panel.screen_share_open(), "the live dialog opened");
            assert!(panel.test_screen_restart());
            assert!(
                panel.test_restart_warning().is_some(),
                "a live change must warn about the restart"
            );
            assert!(panel.test_sound_label().contains("128"));
            // The preset changes only the applied fps/bitrate; audio is a
            // separate field and can be turned off.
            panel.test_apply_preset(Preset::Games, cx);
            panel.test_toggle_system_audio(cx);
            let request = panel.test_screen_request();
            assert_eq!(request.quality.fps, 60);
            assert_eq!(request.quality.bitrate_kbps, 12_000);
            assert_eq!(request.quality.resolution, Resolution::P720);
            assert!(!request.audio);
            assert_eq!(request.source, source());
        });
    });
    for _ in 0..40 {
        if focused(&mut visual, &voice).as_deref() == Some("Start") {
            break;
        }
        visual.simulate_keystrokes("tab");
    }
    assert_eq!(focused(&mut visual, &voice).as_deref(), Some("Start"));
    visual.simulate_keystrokes("enter");
    let commands = visual
        .update(|_window, app| voice.update(app, |panel, _| panel.test_take_screen_commands()));
    assert_eq!(
        commands,
        vec![("restart", 0)],
        "Start in the live dialog queues exactly one restart"
    );
    voice.read_with(&*visual, |panel, _| {
        assert!(panel.screen_share_busy(), "the restart is committed");
    });
}

#[gpui::test]
async fn closing_the_live_quality_dialog_keeps_the_stream(cx: &mut TestAppContext) {
    use voxhold_media::publisher::{Phase, Quality, Sharing};
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.test_open_screen_settings(
                source(),
                Sharing {
                    phase: Phase::Live,
                    live: true,
                    quality: Some(Quality::default()),
                    has_audio: true,
                    ..Sharing::default()
                },
                cx,
            );
            assert!(panel.screen_share_open());
        });
    });
    visual.simulate_keystrokes("escape");
    draw(&mut visual);
    let commands = visual
        .update(|_window, app| voice.update(app, |panel, _| panel.test_take_screen_commands()));
    assert!(
        commands.is_empty(),
        "closing the live quality dialog must not stop or cancel: {commands:?}"
    );
    voice.read_with(&*visual, |panel, _| {
        assert!(!panel.screen_share_open());
        assert_eq!(panel.test_screen_phase(), Phase::Live);
    });
}

#[gpui::test]
async fn the_on_air_sound_line_reports_measured_and_missing_audio(cx: &mut TestAppContext) {
    use voxhold_media::publisher::{Phase, Quality, Sharing};
    let (visual, _view, voice) = super::voice_tests::fixture(cx);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            let quality = Quality::default();
            panel.test_set_screen_sharing(Sharing {
                phase: Phase::Live,
                live: true,
                quality: Some(quality),
                has_audio: true,
                stats: Some(voxhold_media::publisher::engine::PublishStats {
                    audio_active: true,
                    audio_kbps: 96.0,
                    ..Default::default()
                }),
                ..Sharing::default()
            });
            assert!(
                panel.test_sound_label().contains("96"),
                "the card must show the measured audio rate"
            );
            panel.test_set_screen_sharing(Sharing {
                phase: Phase::Live,
                live: true,
                quality: Some(quality),
                has_audio: true,
                audio_scope: Some("Только звук demo.exe (PID 42)".into()),
                stats: Some(voxhold_media::publisher::engine::PublishStats {
                    audio_active: true,
                    audio_kbps: 96.0,
                    ..Default::default()
                }),
                ..Sharing::default()
            });
            assert!(
                panel.test_sound_label().contains("demo.exe"),
                "the on-air line must name the actual scope: {}",
                panel.test_sound_label()
            );
            panel.test_set_screen_sharing(Sharing {
                phase: Phase::Live,
                live: true,
                quality: Some(quality),
                has_audio: false,
                ..Sharing::default()
            });
            assert_eq!(panel.test_sound_label(), "Без звука");
            assert!(
                !panel.test_screen_request().audio,
                "an unavailable track is never requested"
            );
        });
    });
}

#[gpui::test]
async fn the_disabled_reason_is_hidden_while_a_share_is_active(cx: &mut TestAppContext) {
    use voxhold_media::publisher::{Phase, Sharing};
    let (visual, _view, voice) = super::voice_tests::fixture(cx);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_share_gate(Err("Трансляция уже идёт."));
            panel.test_set_screen_sharing(Sharing {
                phase: Phase::Live,
                live: true,
                ..Sharing::default()
            });
            assert!(panel.share_gate().is_err(), "the action is really blocked");
            assert!(
                panel.test_screen_reason().is_none(),
                "an active share shows its card, not the permanent reason"
            );

            // A non-active failure of the gate still explains itself.
            panel.test_set_screen_sharing(Sharing::default());
            panel.test_set_share_gate(Err("Нет доступа к голосовому каналу."));
            assert_eq!(
                panel.test_screen_reason(),
                Some("Нет доступа к голосовому каналу.")
            );
        });
    });
}

#[gpui::test]
async fn the_endpoint_probe_never_blocks_the_screen_share_click(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    let entered = Arc::new(AtomicBool::new(false));
    let flag = entered.clone();
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.test_set_share_gate(Ok(()));
            panel.test_set_audio_probe(Arc::new(move || {
                flag.store(true, Ordering::SeqCst);
                // Stands in for a stalled COM/driver call. The real probe is
                // bounded at two seconds; the click must not wait even 350 ms.
                std::thread::sleep(Duration::from_millis(350));
                Ok(AudioDeviceInfo {
                    id: "slow".into(),
                    label: "Медленное устройство".into(),
                    format: "48000 Гц · 2 канала · 16 бит PCM".into(),
                })
            }));
            let started = Instant::now();
            panel.test_open_screen_share(cx);
            let elapsed = started.elapsed();
            assert!(panel.screen_share_open(), "the picker opened");
            assert!(
                elapsed < Duration::from_millis(150),
                "the click waited for the endpoint probe: {elapsed:?}"
            );
            assert!(
                panel.test_audio_pending(),
                "a synchronous probe would already have an answer here"
            );
            assert!(
                panel.test_audio_status().contains("Проверяем"),
                "the picker must show the pending state"
            );
        });
    });
    wait_for(&mut visual, &voice, |panel| !panel.test_audio_pending());
    assert!(
        entered.load(Ordering::SeqCst),
        "the injected probe did not run on a background task"
    );
    voice.read_with(&*visual, |panel, _| {
        assert!(
            panel.test_audio_status().contains("Медленное устройство"),
            "the background answer must reach the picker: {}",
            panel.test_audio_status()
        );
    });
}

#[gpui::test]
async fn a_pending_audio_probe_blocks_start_and_never_publishes_video_only(
    cx: &mut TestAppContext,
) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    let device = || AudioDeviceInfo {
        id: "slow".into(),
        label: "Медленное устройство".into(),
        format: "48000 Гц · 2 канала · 16 бит PCM".into(),
    };

    // The answer is still pending: a Start here would publish video with
    // `audio == false` although the user asked for system sound.
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.test_set_share_gate(Ok(()));
            panel.test_set_audio_probe(Arc::new(move || {
                std::thread::sleep(Duration::from_millis(250));
                Ok(device())
            }));
            panel.test_open_screen_share(cx);
            panel.test_select_screen_source(source(), cx);
            assert!(panel.test_audio_pending());
            let opened = panel.test_take_screen_commands();
            assert_eq!(opened.len(), 1, "one picker open: {opened:?}");
            assert_eq!(opened[0].0, "open");
            assert!(
                !panel.test_can_start(),
                "Start must wait for the endpoint probe"
            );
            panel.test_start_screen_share(cx);
            assert!(
                panel.test_take_screen_commands().is_empty(),
                "a pending probe must not commit a video-only publication"
            );
            assert!(!panel.screen_share_busy(), "no publish attempt started");
            assert_eq!(panel.test_audio_toggle_label(), "Выключен");
        });
    });

    wait_for(&mut visual, &voice, |panel| !panel.test_audio_pending());

    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            assert_eq!(panel.test_audio_toggle_label(), "Выключен");
            // Audio is off by default now; enabling it is an explicit choice.
            panel.test_toggle_system_audio(cx);
            assert_eq!(panel.test_audio_toggle_label(), "Включён");
            assert!(panel.test_can_start());
            panel.test_start_screen_share(cx);
            let commands = panel.test_take_screen_commands();
            assert_eq!(commands.len(), 1, "one publish intent: {commands:?}");
            assert_eq!(commands[0].0, "publish");
            assert!(
                panel.test_screen_request().audio,
                "the confirmed endpoint must be requested"
            );
        });
    });

    // Reopen with a probe that fails: the picker must stay honest about the
    // missing track and still publish without audio.
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_set_audio_probe(Arc::new(|| {
                std::thread::sleep(Duration::from_millis(250));
                Err("Нет устройства вывода".to_owned())
            }));
            panel.test_open_screen_share(cx);
            panel.test_select_screen_source(source(), cx);
            assert!(panel.test_audio_pending());
            let opened = panel.test_take_screen_commands();
            assert_eq!(opened.len(), 1, "one picker open: {opened:?}");
            assert_eq!(opened[0].0, "open");
            assert_eq!(panel.test_audio_toggle_label(), "Выключен");
            assert!(
                !panel.test_can_start(),
                "a repeated probe blocks Start until it answers"
            );
            panel.test_start_screen_share(cx);
            assert!(
                panel.test_take_screen_commands().is_empty(),
                "a pending re-probe must not commit"
            );
        });
    });

    wait_for(&mut visual, &voice, |panel| !panel.test_audio_pending());

    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            assert!(
                panel.test_audio_status().contains("Нет устройства вывода"),
                "the failure reason stays visible: {}",
                panel.test_audio_status()
            );
            assert_eq!(panel.test_audio_toggle_label(), "Выключен");
            assert!(
                panel.test_can_start(),
                "Start is allowed without audio after a failed probe"
            );
            panel.test_start_screen_share(cx);
            let commands = panel.test_take_screen_commands();
            assert_eq!(commands.len(), 1, "one publish intent: {commands:?}");
            assert_eq!(commands[0].0, "publish");
            assert!(
                !panel.test_screen_request().audio,
                "a failed endpoint must not be requested"
            );
        });
    });
}

#[gpui::test]
async fn the_quality_action_is_rendered_only_while_the_stream_is_live(cx: &mut TestAppContext) {
    use voxhold_media::publisher::{Phase, Sharing};
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_screen_sharing(Sharing {
                phase: Phase::Publishing,
                live: false,
                ..Sharing::default()
            });
        });
    });
    draw(&mut visual);
    assert!(
        visual.debug_bounds("screen-stop").is_some(),
        "the publishing card is on screen"
    );
    assert!(
        visual.debug_bounds("screen-quality").is_none(),
        "Publishing must not show a quality action that would do nothing"
    );

    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_set_screen_sharing(Sharing {
                phase: Phase::Live,
                live: true,
                ..Sharing::default()
            });
        });
    });
    draw(&mut visual);
    assert!(
        visual.debug_bounds("screen-quality").is_some(),
        "Live must offer the restart dialog"
    );
}

#[gpui::test]
async fn sharing_and_stopping_leave_the_microphone_gate_and_ptt_untouched(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.set_stream_permission(true);
            panel.test_set_share_gate(Ok(()));
            // The microphone is open through push-to-talk: the sharing path
            // must not close it, latch the gate or emit a mic control update.
            panel.set_push_to_talk(true);
            panel.set_push_to_talk_held(true);
            assert!(panel.test_ptt_held(), "the microphone is transmitting");
            panel.test_prepare_screen_publish(source());
            assert!(
                panel.test_ptt_held(),
                "preparing the source must not release push-to-talk"
            );
            // Drain the control snapshot caused by enabling push-to-talk.
            let _ = panel.test_take_audio_snapshot();
            panel.test_start_screen_share(cx);
            assert!(
                panel.test_ptt_held(),
                "the publish handler itself must not release push-to-talk"
            );
        });
    });
    let commands = visual
        .update(|_window, app| voice.update(app, |panel, _| panel.test_take_screen_commands()));
    assert_eq!(commands, vec![("publish", 7)]);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            assert!(
                panel.test_ptt_held(),
                "starting the share must not release push-to-talk"
            );
            assert!(
                panel.test_take_audio_snapshot().is_none(),
                "starting the share must not push new mic/gate controls"
            );
        });
    });

    visual.update(|_window, app| {
        voice.update(app, |panel, cx| panel.test_stop_screen_share(cx));
    });
    let commands = visual
        .update(|_window, app| voice.update(app, |panel, _| panel.test_take_screen_commands()));
    assert_eq!(commands, vec![("stop", 0)]);
    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            assert!(
                panel.test_ptt_held(),
                "stopping the share must not release push-to-talk"
            );
            assert!(
                panel.test_take_audio_snapshot().is_none(),
                "stopping the share must not push new mic/gate controls"
            );
        });
    });
}

#[gpui::test]
async fn switching_tabs_drops_a_selection_from_the_hidden_category(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    open_picker(&mut visual, &voice);
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            assert_eq!(
                panel.test_selected_source_id(),
                Some(source().source_id()),
                "the only monitor is selected"
            );
            assert!(panel.test_can_start());

            // A monitor hidden by the «Окна» tab must not stay startable.
            panel.test_switch_share_kind(SourceKind::Window, cx);
            assert_eq!(
                panel.test_selected_source_id(),
                None,
                "a hidden-category selection must be dropped"
            );
            assert!(!panel.test_can_start(), "Start is disabled without a row");
            panel.test_start_screen_share(cx);
            assert!(
                panel.test_take_screen_commands().is_empty(),
                "no publish command for an invisible selection"
            );

            // Switching back must not silently restore the old monitor.
            panel.test_switch_share_kind(SourceKind::Monitor, cx);
            assert_eq!(panel.test_selected_source_id(), None);
            assert!(!panel.test_can_start());
            panel.test_start_screen_share(cx);
            assert!(
                panel.test_take_screen_commands().is_empty(),
                "the old selection must stay explicit"
            );
        });
    });
}

#[gpui::test]
async fn a_reordered_snapshot_keeps_the_id_and_uses_the_current_row(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let first = monitor(3, "Экран A", 1920, 1080);
    let second = monitor(5, "Экран B", 2560, 1440);
    let renamed = monitor(3, "Экран A · переименован", 1280, 720);
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![first.clone(), second.clone()],
    );
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_select_screen_source(first.clone(), cx);
            // The capturer reorders and renames the entry; only the id binds.
            panel.test_apply_screen_sources(
                SourceKind::Monitor,
                vec![second.clone(), renamed.clone()],
            );
            assert_eq!(
                panel.test_selected_source_id(),
                Some(first.source_id()),
                "reordering must not move the selection to another source"
            );
            let selected = panel
                .test_selected_source()
                .expect("the selection is still present");
            assert_eq!(
                selected.describe(),
                renamed.describe(),
                "the label comes from the current snapshot"
            );
            assert_eq!((selected.width, selected.height), (1280, 720));

            panel.test_apply_screen_sources(
                SourceKind::Monitor,
                vec![renamed.clone(), second.clone()],
            );
            assert!(panel.test_can_start());
            panel.test_start_screen_share(cx);
            let commands = panel.test_take_screen_commands();
            assert_eq!(commands, vec![("publish", 7)]);
            let request = panel.test_screen_request();
            assert_eq!(
                request.source, renamed,
                "the request is built from the current row with that id"
            );
        });
    });
}

#[gpui::test]
async fn a_closed_window_or_unplugged_monitor_drops_the_selection_on_refresh(
    cx: &mut TestAppContext,
) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    open_picker(&mut visual, &voice);
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_apply_screen_sources(SourceKind::Monitor, Vec::new());
            assert_eq!(panel.test_selected_source_id(), None, "the monitor is gone");
            assert!(!panel.test_can_start());
            assert!(
                panel.test_screen_error().contains("больше недоступен"),
                "the refresh explains the loss: {}",
                panel.test_screen_error()
            );
            panel.test_start_screen_share(cx);
            assert!(panel.test_take_screen_commands().is_empty());
        });
    });

    let visible = window(11, "Документ");
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Window,
        vec![visible.clone()],
    );
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_select_screen_source(visible.clone(), cx);
            panel.test_apply_screen_sources(SourceKind::Window, Vec::new());
            assert_eq!(panel.test_selected_source_id(), None, "the window closed");
            assert!(!panel.test_can_start());
            panel.test_start_screen_share(cx);
            assert!(panel.test_take_screen_commands().is_empty());
        });
    });
}

#[gpui::test]
async fn start_with_a_removed_source_is_refused_before_the_refresh(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let selected = monitor(3, "Экран A", 1920, 1080);
    let replacement = monitor(5, "Экран B", 2560, 1440);
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![selected.clone()],
    );
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_select_screen_source(selected.clone(), cx);
            // The list changed but the refresh answer is not applied yet.
            panel.test_replace_screen_list_without_refresh(SourceKind::Monitor, vec![replacement]);
            assert_eq!(panel.test_selected_source_id(), Some(selected.source_id()));
            assert!(!panel.test_can_start(), "the id is not in the list");
            panel.test_start_screen_share(cx);
            assert!(
                panel.test_take_screen_commands().is_empty(),
                "a stale selection must never reach the worker"
            );
            assert!(
                panel.test_screen_error().contains("больше недоступен"),
                "the refusal is visible: {}",
                panel.test_screen_error()
            );
        });
    });
}

#[gpui::test]
async fn the_accepted_publish_update_closes_the_committed_picker(cx: &mut TestAppContext) {
    use voxhold_media::publisher::{Phase, Sharing};
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    open_picker(&mut visual, &voice);
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            assert!(panel.test_can_start());
            panel.test_start_screen_share(cx);
            assert!(panel.screen_share_busy(), "the attempt is committed");
            let commands = panel.test_take_screen_commands();
            assert_eq!(commands, vec![("publish", 7)]);

            assert!(panel.test_apply_screen_sharing(Sharing {
                phase: Phase::Publishing,
                live: false,
                ..Sharing::default()
            }));
            assert!(
                !panel.screen_share_open(),
                "the accepted publish closes the committed picker"
            );
            assert!(!panel.screen_share_busy());
        });
    });
}

#[gpui::test]
async fn a_duplicate_source_id_is_never_selected_or_published(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let first = monitor(3, "Экран A", 1920, 1080);
    let duplicate = monitor(3, "Экран A · дубль", 1920, 1080);
    let id = first.source_id();
    visual.update(|_window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            // A normal worker answer may describe the same (kind,id) twice.
            // The selection is still held by identity, as after a click on the
            // row before the duplicated snapshot arrived.
            panel.test_open_screen_picker_with(
                SourceKind::Monitor,
                vec![first.clone(), duplicate.clone()],
                Some(id),
            );
            assert_eq!(panel.test_selected_source_id(), Some(id));
            assert!(
                !panel.test_can_start(),
                "an ambiguous identity cannot be started"
            );
            panel.test_start_screen_share(cx);
            assert!(
                panel.test_take_screen_commands().is_empty(),
                "a duplicated id must never reach the worker"
            );
            assert_eq!(
                panel.test_screen_error(),
                panel.test_source_gone_reason(),
                "the duplicate refusal is visible"
            );

            // The worker repeats the same duplicated answer: the selection is
            // invalidated through `retain_selection`, not silently repaired.
            panel.test_apply_screen_sources(SourceKind::Monitor, vec![first, duplicate]);
            assert_eq!(
                panel.test_selected_source_id(),
                None,
                "a duplicated snapshot clears the ambiguous selection"
            );
            assert!(!panel.test_can_start());
            assert_eq!(panel.test_screen_error(), panel.test_source_gone_reason());
        });
    });
}

#[gpui::test]
async fn the_panel_pushes_gate_changes_while_the_picker_is_open(cx: &mut TestAppContext) {
    let (visual, _view, voice) = super::voice_tests::fixture(cx);
    visual.update(|window, app| {
        voice.update(app, |panel, cx| {
            panel.test_attach_detached_handle();
            panel.test_set_share_gate(Ok(()));
            panel.test_open_screen_share(cx);
            assert_eq!(
                panel.test_take_screen_access(),
                Some((false, true)),
                "opening the picker binds the worker to the current gate"
            );
            panel.set_stream_permission(true);
            assert_eq!(
                panel.test_take_screen_access(),
                Some((true, true)),
                "a granted permission reaches the worker while the picker is open"
            );
            // The channel fills while the picker stays open. The worker must
            // see the slot loss before it accepts a prepared capture.
            panel.test_apply_update(
                voxhold_media::runtime::VoiceUpdate::Streams((0..8).map(publication).collect()),
                window,
                cx,
            );
            assert_eq!(
                panel.test_take_screen_access(),
                Some((true, false)),
                "the slot loss reaches the worker without a new Start"
            );
            panel.set_stream_permission(false);
            assert_eq!(panel.test_take_screen_access(), Some((false, false)));
        });
    });
}

#[gpui::test]
async fn keyboard_focus_follows_the_source_id_across_a_reorder(cx: &mut TestAppContext) {
    let (mut visual, _view, voice) = super::voice_tests::fixture(cx);
    draw(&mut visual);
    let first = monitor(3, "Экран A", 1920, 1080);
    let second = monitor(5, "Экран B", 2560, 1440);
    open_picker_with(
        &mut visual,
        &voice,
        SourceKind::Monitor,
        vec![first.clone(), second.clone()],
    );
    // Monitors -> Windows -> Refresh -> first source row.
    for _ in 0..3 {
        visual.simulate_keystrokes("tab");
    }
    assert_eq!(
        focused(&mut visual, &voice),
        Some("Source(monitor:3)".to_owned())
    );

    visual.update(|_window, app| {
        voice.update(app, |panel, _| {
            panel.test_apply_screen_sources(
                SourceKind::Monitor,
                vec![second.clone(), first.clone()],
            );
        });
    });
    draw(&mut visual);
    // Focus must follow the source, not the row position.
    assert_eq!(
        focused(&mut visual, &voice),
        Some("Source(monitor:3)".to_owned()),
        "the same kind+id stays focused"
    );

    // Enter still selects the focused identity, not the new row position.
    visual.simulate_keystrokes("enter");
    voice.read_with(&*visual, |panel, _| {
        assert_eq!(panel.test_selected_source_id(), Some(first.source_id()));
        assert_eq!(panel.test_selected_source(), Some(first));
    });
}
