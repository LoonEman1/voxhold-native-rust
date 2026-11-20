//! Deterministic unit tests for the software capture path.
//!
//! Kept out of `capture.rs` so the WinMM/policy implementation stays under
//! the handwritten-file budget; the tests still use the parent module's
//! private helpers through `use super::*`.

use super::timeline::FADE_SAMPLES;
use super::*;
use std::time::Duration;

#[test]
fn source_and_wait_outcomes_reach_distinct_counters() {
    let mut stats = CaptureStats::default();
    record_source(&mut stats, classify_source(Some(Ok::<(), ()>(()))));
    record_source(&mut stats, classify_source(Some(Err::<(), ()>(()))));
    record_source(&mut stats, classify_source(None::<Result<(), ()>>));
    assert_eq!(stats.source_ok, 1);
    assert_eq!(stats.source_errors, 1);
    assert_eq!(stats.source_pending, 1);

    assert!(record_wait(&mut stats, WaitOutcome::Signaled));
    assert!(record_wait(&mut stats, WaitOutcome::Timeout));
    assert!(!record_wait(&mut stats, WaitOutcome::Error));
    assert_eq!(stats.wait_timeouts, 1);
    assert_eq!(stats.wait_errors, 1);
    assert_eq!(stats.source_errors, 1, "wait errors are separate");
}

#[test]
fn the_underrun_fade_starts_from_the_last_delivered_amplitude() {
    fn fade_after_sends(send: SourceSend) -> [i16; BLOCK] {
        let base = Instant::now();
        let mut jitter = PcmJitter::new();
        let mut block = [0_i16; BLOCK];
        jitter.push(&vec![5_000_i16; BLOCK * 2], 0);
        jitter.push(&vec![5_000_i16; BLOCK * 2], 0);
        for step in 0..4 {
            assert_eq!(
                jitter.tick(base + Duration::from_millis(step * 10), &mut block),
                Tick::Send
            );
            // Same ordering as `capture_loop`: note only what was accepted.
            note_delivered(&mut jitter, &block, send);
        }
        assert_eq!(
            jitter.tick(base + Duration::from_millis(40), &mut block),
            Tick::Fade
        );
        block
    }

    let delivered = fade_after_sends(SourceSend::Ok);
    assert!(
        delivered[0] > 4_000,
        "the fade must start from the delivered 5 000 amplitude: {}",
        delivered[0]
    );
    assert_eq!(delivered[BLOCK - 1], 0, "the fade reaches silence");

    let failed = fade_after_sends(SourceSend::Error);
    assert_eq!(
        failed[0], 0,
        "a block that never reached the source must not seed the fade"
    );
    assert!(failed.iter().all(|sample| *sample == 0));
}

#[test]
fn short_late_sends_are_published_in_shared_stats() {
    let base = Instant::now();
    let mut jitter = PcmJitter::new();
    let mut block = [0_i16; BLOCK];
    jitter.push(&vec![222_i16; BLOCK * 2], 0);
    jitter.push(&vec![222_i16; BLOCK * 2], 0);
    assert_eq!(jitter.tick(base, &mut block), Tick::Send);
    assert_eq!(
        jitter.tick(base + Duration::from_millis(12), &mut block),
        Tick::Send,
        "a 2 ms late wake still sends the due block"
    );
    let shared = SharedStats::default();
    publish_stats(&shared, &CaptureStats::default(), &jitter);
    let snapshot = shared.snapshot();
    assert_eq!(snapshot.late_sends, 1);
    assert_eq!(snapshot.late_ticks, 0);
    assert_eq!(snapshot.underruns, 0);
    assert_eq!(snapshot.overflow_blocks, 0);
}

#[test]
fn input_level_event_carries_the_capture_aggregates() {
    let base = Instant::now();
    let mut jitter = PcmJitter::new();
    let mut block = [0_i16; BLOCK];
    // Ten 20 ms chunks exceed the 80 ms ceiling, so overflow is non-zero.
    for index in 0..10_i16 {
        jitter.push(&vec![index; BLOCK * 2], 0);
    }
    assert_eq!(
        jitter.tick(base, &mut block),
        Tick::Send,
        "the ceiling still leaves blocks to send"
    );
    // 2 ms late: a short late send that keeps the grid.
    assert_eq!(
        jitter.tick(base + Duration::from_millis(12), &mut block),
        Tick::Send
    );
    // 13 ms late against the 20 ms deadline: one whole period is skipped.
    assert_eq!(
        jitter.tick(base + Duration::from_millis(33), &mut block),
        Tick::Send
    );
    let mut counters = CaptureStats::default();
    counters.source_errors = 3;
    let events = Events::default();
    let mut ticks = LEVEL_INTERVAL - 1;
    report_level(&events, &mut ticks, 0.5, false, &counters, &jitter);
    let Some(Event::InputLevel {
        underruns,
        overflow_blocks,
        late_ticks,
        late_sends,
        source_errors,
        ..
    }) = events.poll(Duration::ZERO)
    else {
        panic!("the level interval did not publish an input level");
    };
    assert_eq!(underruns, jitter.underruns());
    assert_eq!(overflow_blocks, jitter.overflow_blocks());
    assert!(overflow_blocks > 0, "the queue ceiling was exceeded");
    assert_eq!(late_ticks, 1, "one slot was skipped");
    assert_eq!(late_sends, 1, "one short late send was counted");
    assert_eq!(source_errors, 3);
}

#[test]
fn mute_silences_the_next_block_and_does_not_replay_the_tail() {
    let speech = vec![8_000_i16; BLOCK];
    let background = vec![20_i16; BLOCK];
    let mut processor = BlockProcessor::default();
    let params = AudioParams {
        mode: NoiseMode::Threshold,
        gate_percent: 15,
        auto_gain_control: false,
        ..AudioParams::default()
    };
    for _ in 0..10 {
        let mut block = speech.clone();
        processor.process(&mut block, &params);
    }
    let muted = AudioParams {
        muted: true,
        ..params.clone()
    };
    let mut block = background.clone();
    let (_, silenced) = processor.process(&mut block, &muted);
    assert!(silenced);
    assert!(block.iter().all(|sample| *sample == 0));
    // Unmuting starts from a closed gate, even though the previous block
    // was speech: the background must not leak old energy.
    let mut block = background;
    processor.process(&mut block, &params);
    assert!(block.iter().all(|sample| *sample == 0));
}

#[test]
fn processing_key_rebuilds_the_apm_only_when_processing_changes() {
    let mut processor = BlockProcessor::default();
    let mut block = [0_i16; BLOCK];
    processor.process(&mut block, &AudioParams::default());
    let key = processor.key.clone();
    processor.process(
        &mut block,
        &AudioParams {
            gate_percent: 42,
            ..AudioParams::default()
        },
    );
    assert_eq!(
        processor.key, key,
        "a gate change must keep the APM instance"
    );
    processor.process(
        &mut block,
        &AudioParams {
            mode: NoiseMode::Threshold,
            ..AudioParams::default()
        },
    );
    assert!(!processor.key.as_ref().unwrap().noise_suppression);
    assert!(
        !ECHO_CANCELLATION_AVAILABLE,
        "AEC is only reported when a render reference exists"
    );
}

#[test]
fn every_supported_gain_level_is_clamped_and_bounded() {
    for percent in [0_u32, 50, 100, 200] {
        let mut block = vec![10_000_i16; BLOCK];
        let peak = dsp::apply_gain(&mut block, percent as f32 / 100.0);
        let expected = 10_000.0 / 32_768.0 * percent as f32 / 100.0;
        assert!((peak - expected).abs() < 0.001, "{percent}%: {peak}");
        assert!(
            block
                .iter()
                .all(|sample| (-32_768..=32_767).contains(sample))
        );
    }
    let mut saturated = vec![30_000_i16; BLOCK];
    dsp::apply_gain(&mut saturated, 2.0);
    assert!(
        saturated.iter().all(|sample| *sample == 32_767),
        "200% must clamp at full scale"
    );
}

#[test]
fn synthetic_capture_matrix_matches_the_report() {
    let input = crate::dsp::fixture(SAMPLE_RATE);
    let rms = |samples: &[i16]| crate::dsp::dbfs(samples).unwrap_or(-120.0);
    let section = |from: f32, to: f32| {
        let start = (SAMPLE_RATE as f32 * from) as usize;
        let end = (SAMPLE_RATE as f32 * to) as usize;
        input[start..end].to_vec()
    };
    let run = |params: &AudioParams, muted_from: Option<f32>| {
        let mut processor = BlockProcessor::default();
        let mut output = Vec::with_capacity(input.len());
        let mut clipped = 0_usize;
        let mut block = [0_i16; BLOCK];
        for (index, chunk) in input.chunks(BLOCK).enumerate() {
            if chunk.len() < BLOCK {
                break;
            }
            block.copy_from_slice(chunk);
            let mut params = params.clone();
            if muted_from.is_some_and(|from| index as f32 * 0.01 >= from) {
                params.muted = true;
            }
            processor.process(&mut block, &params);
            clipped += block.iter().filter(|sample| **sample == 32_767).count();
            output.extend_from_slice(&block);
        }
        (output, clipped)
    };
    let report = |name: &str, params: &AudioParams| {
        let (output, clipped) = run(params, None);
        let out = |from, to| rms(&section_of(&output, from, to, SAMPLE_RATE));
        println!(
            "{name:10} dry {:6.1} / out noise1 {:6.1}, tone {:6.1}, noise2 {:6.1}, clipped {clipped}",
            rms(&section(0.5, 1.5)),
            out(0.6, 1.4),
            out(1.6, 2.4),
            out(2.9, 3.4)
        );
        output
    };
    let auto = report(
        "auto",
        &AudioParams {
            auto_gain_control: false,
            ..AudioParams::default()
        },
    );
    assert!(rms(&section_of(&auto, 0.6, 1.4, SAMPLE_RATE)) < rms(&section(0.5, 1.5)) - 3.0);
    let bypass = report(
        "threshold0",
        &AudioParams {
            mode: NoiseMode::Threshold,
            gate_percent: 0,
            auto_gain_control: false,
            ..AudioParams::default()
        },
    );
    assert!(
        (rms(&section_of(&bypass, 0.6, 1.4, SAMPLE_RATE)) - rms(&section(0.5, 1.5))).abs() < 4.0,
        "threshold=0 bypasses the gate; only the always-on HPF may change the noise level"
    );
    let gated = report(
        "threshold15",
        &AudioParams {
            mode: NoiseMode::Threshold,
            gate_percent: 15,
            auto_gain_control: false,
            ..AudioParams::default()
        },
    );
    assert!(rms(&section_of(&gated, 0.6, 1.4, SAMPLE_RATE)) < -80.0);
    assert!(rms(&section_of(&gated, 1.6, 2.4, SAMPLE_RATE)) > -20.0);
    assert!(
        rms(&section_of(&gated, 2.9, 3.4, SAMPLE_RATE)) < -80.0,
        "the gate must close after the 180 ms hold and 60 ms release"
    );
    for percent in [0_u32, 50, 100, 200] {
        let (output, _) = run(
            &AudioParams {
                mode: NoiseMode::Threshold,
                gate_percent: 0,
                input_gain: percent as f32 / 100.0,
                auto_gain_control: false,
                ..AudioParams::default()
            },
            None,
        );
        let tone = rms(&section_of(&output, 1.6, 2.4, SAMPLE_RATE));
        println!("gain {percent:3}% tone {tone:6.1} dBFS");
        let expected = -15.1 + 20.0 * (percent as f32 / 100.0).max(0.000_1).log10();
        if percent == 0 {
            assert!(tone < -100.0, "0 % must be silence");
        } else {
            assert!(
                (tone - expected).abs() < 1.0,
                "gain {percent}%: {tone} vs {expected}"
            );
        }
    }
    let (muted, clipped) = run(&AudioParams::default(), Some(1.0));
    assert!(clipped == 0);
    assert!(
        muted
            .iter()
            .skip(SAMPLE_RATE as usize)
            .all(|sample| *sample == 0),
        "PTT release, deafen and server mute must be silent after the last block"
    );
}

fn section_of(samples: &[i16], from: f32, to: f32, rate: u32) -> Vec<i16> {
    let start = (rate as f32 * from) as usize;
    let end = (rate as f32 * to) as usize;
    samples[start..end].to_vec()
}

#[test]
fn dsp_timing_report() {
    use std::time::Instant;
    let mut processor = BlockProcessor::default();
    let params = AudioParams {
        mode: NoiseMode::Auto,
        auto_gain_control: false,
        ..AudioParams::default()
    };
    let mut timings = Vec::with_capacity(2_000);
    let mut block = [0_i16; BLOCK];
    for _ in 0..2_100 {
        // Warm-up is included in the first 100 samples and discarded.
        for sample in block.iter_mut() {
            *sample = 1_000;
        }
        let started = Instant::now();
        processor.process(&mut block, &params);
        timings.push(started.elapsed().as_secs_f64() * 1e6);
    }
    timings.drain(..100);
    timings.sort_by(f64::total_cmp);
    let pick = |p: f64| timings[((timings.len() - 1) as f64 * p) as usize];
    println!(
        "DSP block 10 ms: p50 {:.1} us, p95 {:.1} us, p99 {:.1} us, max {:.1} us",
        pick(0.50),
        pick(0.95),
        pick(0.99),
        timings.last().unwrap()
    );
    println!(
        "Input latency: 30 ms prefill reserve + WinMM 20 ms driver buffers + one 10 ms block; the 80 ms ceiling is a stall guard, not steady-state latency, and NativeAudioSource keeps queue_size_ms = 0"
    );
    assert!(pick(0.99) < 5_000.0, "DSP block must fit in real time");
}

#[test]
#[cfg(windows)]
#[ignore = "opens the default Windows microphone; run manually"]
fn software_capture_reaches_the_native_source() {
    let params = Arc::new(Mutex::new(AudioParams {
        muted: false,
        ..AudioParams::default()
    }));
    let events = Arc::new(Events::default());
    let source = NativeAudioSource::new(source_options(), SAMPLE_RATE, CHANNELS, 0);
    let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut mic = MicCapture::new(source, params, epoch, None, events.clone());
    mic.start(None).expect("default microphone");
    thread::sleep(Duration::from_millis(600));
    mic.stop();
    let mut levels = 0;
    let mut dropped = 0;
    while let Some(event) = events.poll(Duration::ZERO) {
        if let Event::InputLevel { dropped: count, .. } = event {
            levels += 1;
            dropped = dropped.max(count);
        }
    }
    assert!(levels > 0, "capture produced no input_level events");
    let progress = mic.progress();
    let stats = mic.stats();
    assert!(progress > 0, "capture produced no real PCM blocks");
    assert_eq!(
        stats.captured_blocks, progress,
        "progress must count only real captured blocks"
    );
    // Counters only: no PCM or personal content is printed.
    println!(
        "input levels {levels}, dropped blocks {dropped}, progress {progress}, stats {stats:?}"
    );
}

#[test]
fn permission_epoch_discards_queued_audio_across_fast_ptt() {
    let epoch = std::sync::atomic::AtomicU64::new(0);
    let mut permission = PermissionEpoch::new(&epoch);
    let mut jitter = PcmJitter::new();
    jitter.push(&vec![9_000_i16; BLOCK * 2], 0);
    // Press and release between two polls restores the boolean state but
    // still advances the epoch twice.
    epoch.fetch_add(1, Ordering::SeqCst);
    epoch.fetch_add(1, Ordering::SeqCst);
    assert!(permission.changed(epoch.load(Ordering::Acquire)));
    jitter.reset(2);
    assert_eq!(jitter.len(), 0, "old audio was discarded");
    jitter.push(&vec![222_i16; BLOCK * 2], 2);
    let mut block = [0_i16; BLOCK];
    assert_eq!(
        jitter.tick(Instant::now(), &mut block),
        Tick::None,
        "the new epoch must rebuild the 30 ms reserve first"
    );
    jitter.push(&vec![222_i16; BLOCK * 2], 2);
    assert_eq!(jitter.tick(Instant::now(), &mut block), Tick::Send);
    assert_eq!(block[0], 0, "the first post-epoch block ramps in");
    assert!(block[FADE_SAMPLES..].iter().all(|sample| *sample == 222));
    // End-to-end view of the same transition: the first processed block
    // after the flush comes from the post-transition buffer, not from the
    // 9 000-amplitude audio captured before unmute.
    let mut processor = BlockProcessor::default();
    processor.reset();
    for (index, sample) in block.iter_mut().enumerate() {
        *sample = if index % 2 == 0 { 222 } else { -222 };
    }
    let params = AudioParams {
        mode: NoiseMode::Threshold,
        gate_percent: 0,
        auto_gain_control: false,
        ..AudioParams::default()
    };
    processor.process(&mut block, &params);
    assert!(
        block.iter().all(|sample| sample.abs() < 400),
        "stale pre-unmute audio reached the output: {:?}",
        &block[..4]
    );
    assert!(!permission.changed(epoch.load(Ordering::Acquire)));
}

#[test]
fn a_block_read_before_a_generation_change_is_not_sent_after_unmute() {
    let mut jitter = PcmJitter::new();
    // The driver buffer was captured while muted, so the loop tagged it
    // with generation 7 before calling `completed()`. The mute is released
    // while that call is still running; the generation reaches 8.
    jitter.push(&vec![12_345_i16; BLOCK * 2], 7);
    let mut block = [0_i16; BLOCK];
    jitter.reset(8);
    assert_eq!(jitter.len(), 0, "the old epoch queue was cleared");
    assert_eq!(jitter.tick(Instant::now(), &mut block), Tick::None);
    // After the queue is reset, only blocks captured under the new
    // generation are eligible and they still wait for the reserve.
    jitter.push(&vec![222_i16; BLOCK * 2], 8);
    assert_eq!(jitter.tick(Instant::now(), &mut block), Tick::None);
    jitter.push(&vec![222_i16; BLOCK * 2], 8);
    assert_eq!(jitter.tick(Instant::now(), &mut block), Tick::Send);
    assert_eq!(block[0], 0, "the first post-epoch block ramps in");
    assert!(block[FADE_SAMPLES..].iter().all(|sample| *sample == 222));
}

#[test]
#[cfg(windows)]
fn saved_winmm_device_is_resolved_by_endpoint_or_unique_name() {
    let devices = vec![
        winmm::DeviceInfo {
            index: 0,
            name: "Speakers".into(),
            endpoint: Some("endpoint-0".into()),
            legacy_interface: Some("interface-0".into()),
        },
        winmm::DeviceInfo {
            index: 1,
            name: "USB Headset".into(),
            endpoint: Some("endpoint-1".into()),
            legacy_interface: Some("interface-1".into()),
        },
    ];
    assert_eq!(
        winmm::choose_device(Some("0:USB Headset"), &devices)
            .unwrap()
            .index,
        1,
        "indexes shift; the saved name still identifies the device"
    );
    assert_eq!(
        winmm::choose_device(Some("endpoint-1"), &devices)
            .unwrap()
            .index,
        1,
        "new preferences use the stable endpoint id"
    );
    assert_eq!(
        winmm::choose_device(Some("interface-1"), &devices)
            .unwrap()
            .index,
        1,
        "a legacy PnP interface id saved by an older build still migrates"
    );
    assert_eq!(
        winmm::choose_device(Some("1:USB"), &devices).unwrap().index,
        1,
        "legacy truncated names are resolved by a unique prefix"
    );
    assert_eq!(
        winmm::choose_device(None, &devices).unwrap().index,
        winmm::WAVE_MAPPER,
        "an empty preference is the Windows default"
    );
    assert!(winmm::choose_device(Some("0:Gone"), &devices).is_err());
    assert!(
        winmm::choose_device(Some("0:"), &devices).is_err(),
        "a legacy id without a name must not open a reused index"
    );
    let same_name = vec![
        winmm::DeviceInfo {
            index: 3,
            name: "USB Mic".into(),
            endpoint: Some("endpoint-3".into()),
            legacy_interface: Some("interface-3".into()),
        },
        winmm::DeviceInfo {
            index: 0,
            name: "USB Mic".into(),
            endpoint: Some("endpoint-0".into()),
            legacy_interface: Some("interface-0".into()),
        },
    ];
    assert!(
        winmm::choose_device(Some("3:USB Mic"), &same_name).is_err(),
        "a legacy duplicate name must ask the user to choose again"
    );
    assert_eq!(
        winmm::choose_device(Some("endpoint-0"), &same_name)
            .unwrap()
            .index,
        0,
        "the endpoint id distinguishes identical names"
    );
}

#[test]
#[cfg(windows)]
#[ignore = "opens the default Windows microphone; run manually"]
fn winmm_flush_restarts_the_driver_queue() {
    let mut device = WinIn::open(None).expect("default microphone");
    thread::sleep(Duration::from_millis(120));
    let mut before = 0;
    device.completed(|samples| before += samples.len());
    assert!(before > 0, "the driver returned no samples before flush");
    assert!(device.flush(), "flush must restart the capture queue");
    thread::sleep(Duration::from_millis(120));
    let mut after = 0;
    device.completed(|samples| after += samples.len());
    assert!(after > 0, "the driver returned no samples after flush");
    device.close();
    assert!(!device.failed(), "a normal close must succeed");
}

#[test]
#[cfg(windows)]
#[ignore = "opens the default Windows microphone; run manually"]
fn winmm_capture_delivers_blocks() {
    let mut device = WinIn::open(None).expect("default microphone");
    let started = std::time::Instant::now();
    let mut samples = 0;
    while started.elapsed() < Duration::from_millis(600) {
        device.completed(|chunk| samples += chunk.len());
        thread::sleep(Duration::from_millis(5));
    }
    device.close();
    println!("captured {samples} samples in 600 ms");
    assert!(
        samples >= (SAMPLE_RATE as usize / 5),
        "captured only {samples} samples"
    );
    assert!(
        samples <= SAMPLE_RATE as usize,
        "captured duplicate buffers: {samples} samples"
    );
}
