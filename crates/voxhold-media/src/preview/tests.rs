use super::*;

#[test]
fn rms_covers_silence_full_scale_and_a_known_amplitude() {
    assert_eq!(rms(&[]), 0.0, "an empty buffer is silence, not a panic");
    assert_eq!(rms(&[0, 0, 0, 0]), 0.0);
    let full = rms(&[32767, 32767, 32767, 32767]);
    assert!((full - 1.0).abs() < 1e-4);
    assert_eq!(level_from_rms(full), 1.0);
    let quarter = rms(&[8192, 8192, 8192, 8192]);
    assert!((quarter - 0.25).abs() < 1e-4);
    assert!((level_from_rms(quarter) - 1.0).abs() < 1e-6);
    let quiet = rms(&[1000, 1000]);
    let expected = (1000.0_f64 / 32768.0) * 4.5;
    assert!((level_from_rms(quiet) as f64 - expected).abs() < 1e-5);
}

#[test]
fn smoothing_matches_the_go_coefficients() {
    assert!((smooth_level(0.0, 1.0) - 0.55).abs() < 1e-9);
    assert!((smooth_level(1.0, 0.0) - 0.45).abs() < 1e-9);
}

#[test]
fn tone_has_the_go_length_envelope_and_peak() {
    let samples = tone_samples(48_000, 100);
    assert_eq!(samples.len(), 12_000, "rate / 4 like Go");
    assert_eq!(
        samples.first().copied(),
        Some(0),
        "the envelope starts at 0"
    );
    let peak = 16_000_u32;
    assert!(
        samples
            .iter()
            .all(|sample| sample.unsigned_abs() as u32 <= peak),
        "the amplitude is capped by 16000 * volume / 100"
    );
    let middle = &samples[1_000..11_000];
    let loud = middle
        .iter()
        .map(|sample| sample.unsigned_abs())
        .max()
        .unwrap();
    assert!(loud > 10_000, "the steady tone is audible: {loud}");
    let tail = samples[samples.len() - 120..]
        .iter()
        .map(|sample| sample.unsigned_abs())
        .max()
        .unwrap();
    assert!(tail as u32 <= peak / 8, "the tail fades out: {tail}");
    assert!(tone_samples(48_000, 0).iter().all(|sample| *sample == 0));
    assert!(tone_samples(0, 100).is_empty(), "rate / 4 is zero");
    let low = tone_samples(48_000, 30);
    let low_peak = low
        .iter()
        .map(|sample| sample.unsigned_abs())
        .max()
        .unwrap();
    assert!(low_peak <= 4_800, "volume scales the peak: {low_peak}");
}

#[test]
fn the_tone_sample_matches_the_go_formula() {
    let samples = tone_samples(48_000, 100);
    let index = 100;
    let t = index as f64 / 48_000.0;
    let envelope = (t / 0.012).min(1.0).min((0.25 - t) / 0.06);
    let tone = 0.55 * (2.0 * std::f64::consts::PI * 660.0 * t).sin()
        + 0.45 * (2.0 * std::f64::consts::PI * 880.0 * t).sin();
    let expected = (tone * envelope * 16_000.0) as i16;
    assert_eq!(samples[index], expected);
}

#[test]
fn device_ids_resolve_exactly_then_by_name() {
    let devices = vec![(1, "Microphone".to_owned()), (2, "Line In".to_owned())];
    assert_eq!(resolve_device("", &devices), Some(WAVE_MAPPER));
    assert_eq!(resolve_device("2:Line In", &devices), Some(2));
    assert_eq!(
        resolve_device("7:Microphone", &devices),
        Some(1),
        "a stale index falls back to the device name"
    );
    assert_eq!(
        resolve_device("1:Line In", &devices),
        Some(2),
        "a stale index falls back to the matching name, like Go"
    );
    assert_eq!(resolve_device("1:Missing", &devices), None);
    assert_eq!(resolve_device("9:Missing", &devices), None);
    assert_eq!(resolve_device("Microphone", &[]), None);
    assert_eq!(resolve_device("", &[]), Some(WAVE_MAPPER));
}

#[test]
fn a_pending_cue_is_replaced_and_drop_cannot_be_revived() {
    let player = CuePlayer::detached();
    let first = join_cue();
    player.play(first.clone(), 100, "");
    {
        let state = player.shared.state.lock().unwrap();
        let pending = state.pending.as_ref().expect("the first cue is pending");
        assert_eq!(pending.spec, first);
        assert_eq!(pending.volume, 100);
        assert_eq!(
            pending.device, "",
            "the empty id reaches the worker as the system endpoint"
        );
    }

    // One pending slot: an unstarted cue is replaced, not queued.
    let second = CueSpec {
        gain: 0.2,
        tones: vec![tone(440.0, 0.0, 0.1)],
    };
    player.play(second.clone(), 50, "2:Speakers");
    {
        let state = player.shared.state.lock().unwrap();
        let pending = state.pending.as_ref().expect("the newer cue is pending");
        assert_eq!(
            pending.spec, second,
            "a second unstarted cue replaces the first"
        );
        assert_eq!(pending.volume, 50);
        assert_eq!(pending.device, "2:Speakers");
    }

    let shared = player.shared.clone();
    drop(player);
    {
        let state = shared.state.lock().unwrap();
        assert!(state.stopping, "Drop stops the worker");
        assert!(state.pending.is_none(), "Drop clears the pending cue");
    }
    assert!(shared.stop.load(Ordering::Relaxed));

    // `play` shares this helper with the player; a stopped slot stays empty.
    queue_cue(&shared, join_cue(), 100, "");
    assert!(
        shared.state.lock().unwrap().pending.is_none(),
        "a dropped player cannot be revived by play"
    );
}

#[test]
fn an_endpoint_name_must_identify_one_input() {
    let devices = vec![(1, "Microphone".to_owned()), (2, "Line In".to_owned())];
    assert_eq!(unique_index_by_name("Line In", &devices), Some(2));
    assert_eq!(unique_index_by_name("Missing", &devices), None);
    assert_eq!(unique_index_by_name("", &devices), None);
    let duplicated = vec![(1, "USB Mic".to_owned()), (2, "USB Mic".to_owned())];
    assert_eq!(
        unique_index_by_name("USB Mic", &duplicated),
        None,
        "two identical names must not open an arbitrary microphone"
    );
}

fn tone(frequency: f64, at: f64, duration: f64) -> CueTone {
    CueTone {
        frequency,
        at,
        duration,
    }
}

fn join_cue() -> CueSpec {
    CueSpec {
        gain: 0.45,
        tones: vec![tone(587.33, 0.0, 0.12), tone(880.0, 0.1, 0.2)],
    }
}

#[test]
fn a_cue_is_a_short_enveloped_pair_limited_by_gain_and_volume() {
    let samples = cue_samples(&join_cue(), 48_000, 100);
    // The longest tone ends at 0.3 s: 587.33 Hz pair like the frontend.
    assert_eq!(samples.len(), 14_400);
    let peak = samples
        .iter()
        .map(|sample| sample.unsigned_abs())
        .max()
        .unwrap();
    assert!(peak > 8_000, "the pair is audible: {peak}");
    assert!(peak as f64 <= 0.45 * 2.0 * 32_767.0, "gain caps the mix");
    assert!(
        samples[..100].iter().all(|sample| sample.abs() < 1_000),
        "the attack starts from silence"
    );
    let tail = samples[samples.len() - 100..]
        .iter()
        .map(|sample| sample.unsigned_abs())
        .max()
        .unwrap();
    assert!(tail < peak / 8, "the release fades out: {tail}");

    let quiet = cue_samples(&join_cue(), 48_000, 50);
    let quiet_peak = quiet
        .iter()
        .map(|sample| sample.unsigned_abs())
        .max()
        .unwrap();
    assert!(
        quiet_peak as f64 <= peak as f64 / 2.0 + 1.0,
        "volume 50 halves the peak"
    );
    assert!(cue_samples(&join_cue(), 48_000, 0).is_empty());
    assert!(cue_samples(&join_cue(), 0, 100).is_empty());
    let empty = CueSpec {
        gain: 0.45,
        tones: Vec::new(),
    };
    assert!(cue_samples(&empty, 48_000, 100).is_empty());
}

#[test]
fn a_cue_sample_follows_the_frontend_oscillator_and_envelope() {
    let samples = cue_samples(&join_cue(), 48_000, 100);
    let index = 6_000; // 125 ms: inside the second tone, after the attack
    let time = index as f64 / 48_000.0;
    let local = time;
    let mixed = (2.0 * std::f64::consts::PI * 587.33 * local).sin() * cue_envelope(local, 0.12)
        + (2.0 * std::f64::consts::PI * 880.0 * (time - 0.1)).sin() * cue_envelope(time - 0.1, 0.2);
    let expected = (mixed * 0.45 * 32_767.0) as i16;
    assert!(
        (samples[index] as i32 - expected as i32).abs() <= 1,
        "{} vs {expected}",
        samples[index]
    );
}
