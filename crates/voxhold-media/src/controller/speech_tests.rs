use super::*;

#[test]
fn local_speech_checks_the_software_gate_and_the_join_state() {
    let mut controller = VoiceController::new(
        1,
        2,
        Controls {
            muted: false,
            ..Default::default()
        },
        false,
    );
    controller.local_level = 0.2;
    assert!(
        !controller.local_speaking(),
        "no highlight before voice.joined"
    );
    controller.joined = true;
    assert!(
        controller.local_speaking(),
        "the post-gate ADM level qualifies"
    );
    controller.input_level_seen = true;
    controller.input_muted = true;
    assert!(
        !controller.local_speaking(),
        "the pre-gate meter is visible even when noise is blocked"
    );
    controller.input_muted = false;
    assert!(controller.local_speaking());
    controller.server_muted = true;
    assert!(!controller.local_speaking());
    controller.server_muted = false;
    controller.controls.muted = true;
    assert!(
        !controller.local_speaking(),
        "PTT or a local mute closes the indicator"
    );
}
