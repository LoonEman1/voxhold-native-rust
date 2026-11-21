//! G01–G04 on deterministic fake adapters. No test in this module touches
//! DXGI, Media Foundation or a real encoder.

use std::{sync::Arc, time::Duration};

use super::fake::{FakeAdapter, FakeCpu, FakeProbe, Gate};
use super::{
    AdapterLuid, AdapterSelection, Codec, CodecEncoderState, CpuEncoderSource, EncodeQuery,
    EncoderBackend, EncoderProbeService, EncoderProbeState, EncoderProbeTarget, EncoderReason,
    EncoderRejection, EncoderRequest, VideoEncoderPreference, VideoEncoderSnapshot,
};
use crate::publisher::CodecIntent;
use crate::screen_capture::SourceIdentity;

const A: u64 = 0x0000_0001_0000_0001;
const B: u64 = 0x0000_0002_0000_0002;
const C: u64 = 0x0000_0003_0000_0003;

fn luid(id: u64) -> AdapterLuid {
    AdapterLuid::new(id as u32, (id >> 32) as u32)
}

fn q720() -> EncodeQuery {
    EncodeQuery::new(1280, 720, 30)
}

fn q1080_60() -> EncodeQuery {
    EncodeQuery::new(1920, 1080, 60)
}

fn gpu(id: u64, allow_fallback: bool) -> VideoEncoderPreference {
    VideoEncoderPreference::gpu(AdapterSelection::Explicit(luid(id)), allow_fallback)
}

fn request(
    preference: VideoEncoderPreference,
    query: EncodeQuery,
    intent: u64,
    source: Option<u64>,
) -> EncoderRequest {
    EncoderRequest::new(
        preference,
        query,
        EncoderProbeTarget {
            intent,
            source: source.map(SourceIdentity::Window),
        },
    )
}

fn service(fake: &Arc<FakeProbe>) -> EncoderProbeService {
    EncoderProbeService::new(fake.clone(), FakeCpu::all() as Arc<dyn CpuEncoderSource>).unwrap()
}

fn ready(service: &EncoderProbeService, generation: u64) -> Arc<VideoEncoderSnapshot> {
    service
        .wait_for(generation, Duration::from_secs(5))
        .unwrap_or_else(|| {
            panic!(
                "probe generation {generation} did not finish; snapshot = {:?}",
                service.snapshot()
            )
        })
}

fn submit(service: &EncoderProbeService, request: EncoderRequest) -> Arc<VideoEncoderSnapshot> {
    let generation = service.set_video_encoder_backend(request).unwrap();
    ready(service, generation)
}

fn reason_kind(snapshot: &VideoEncoderSnapshot, codec: Codec) -> &'static str {
    snapshot
        .gpu(codec)
        .and_then(|slot| slot.reason())
        .map(EncoderReason::kind)
        .unwrap_or("available")
}

#[test]
fn g01_explicit_av1_is_blocked_on_a_and_auto_uses_h264() {
    let fake = FakeProbe::new(vec![
        FakeAdapter::new(A, "A", &[Codec::H264]).primary(),
        FakeAdapter::new(B, "B", &[Codec::H264, Codec::Av1]),
    ]);
    let service = service(&fake);

    // CPU AV1 is available and explicit.
    let cpu = submit(
        &service,
        request(VideoEncoderPreference::CPU, q1080_60(), 1, Some(1)),
    );
    let av1 = cpu.resolve(CodecIntent::Explicit(Codec::Av1)).unwrap();
    assert_eq!((av1.backend, av1.codec), (EncoderBackend::Cpu, Codec::Av1));

    // Adapter A has H.264 only.
    let a = submit(&service, request(gpu(A, false), q1080_60(), 2, Some(1)));
    assert_eq!(a.adapter().map(|adapter| adapter.name.as_str()), Some("A"));
    assert_eq!(reason_kind(&a, Codec::Av1), "encoder_not_provided");
    let blocked = a.resolve(CodecIntent::Explicit(Codec::Av1)).unwrap_err();
    assert!(matches!(
        blocked,
        EncoderRejection::Unavailable {
            codec: Some(Codec::Av1),
            backend: EncoderBackend::Gpu,
            ..
        }
    ));
    assert!(
        a.resolve(CodecIntent::Explicit(Codec::Av1)).is_err(),
        "an explicit unavailable choice stays blocked, it is never replaced"
    );
    let auto = a.resolve(CodecIntent::Auto).unwrap();
    assert_eq!(
        (auto.backend, auto.codec),
        (EncoderBackend::Gpu, Codec::H264)
    );
    assert_eq!(auto.adapter_luid(), Some(luid(A)));

    // Even with the fallback allowed, a working GPU with a missing codec is
    // not silently bypassed.
    let a_fallback = submit(&service, request(gpu(A, true), q1080_60(), 21, Some(1)));
    assert!(matches!(
        a_fallback.resolve(CodecIntent::Explicit(Codec::Av1)),
        Err(EncoderRejection::Unavailable {
            codec: Some(Codec::Av1),
            backend: EncoderBackend::Gpu,
            ..
        })
    ));

    // Adapter B has AV1.
    let b = submit(&service, request(gpu(B, false), q1080_60(), 3, Some(1)));
    let av1 = b.resolve(CodecIntent::Explicit(Codec::Av1)).unwrap();
    assert_eq!((av1.backend, av1.codec), (EncoderBackend::Gpu, Codec::Av1));
    assert_eq!(av1.adapter_luid(), Some(luid(B)));

    // Back to CPU restores the CPU AV1 choice.
    let cpu = submit(
        &service,
        request(VideoEncoderPreference::CPU, q1080_60(), 4, Some(1)),
    );
    assert_eq!(
        cpu.resolve(CodecIntent::Explicit(Codec::Av1))
            .unwrap()
            .backend,
        EncoderBackend::Cpu
    );
    service.shutdown();
}

#[test]
fn g02_warp_only_and_no_adapters_leave_cpu_working() {
    let fake = FakeProbe::new(vec![
        FakeAdapter::new(C, "Microsoft Basic Render Driver", &[Codec::Av1]).software(),
    ]);
    let service = service(&fake);
    let snapshot = submit(&service, request(gpu(0, false), q720(), 1, Some(4)));
    assert!(snapshot.adapters().is_empty(), "WARP is not a candidate");
    assert!(snapshot.adapter().is_none());
    assert!(!snapshot.gpu_available());
    for codec in Codec::ALL {
        assert_eq!(reason_kind(&snapshot, codec), "device_disabled");
    }
    assert!(snapshot.resolve(CodecIntent::Auto).is_err());
    assert!(matches!(
        snapshot.resolve(CodecIntent::Auto),
        Err(EncoderRejection::Unavailable { .. })
    ));

    let cpu = submit(
        &service,
        request(VideoEncoderPreference::CPU, q720(), 2, Some(4)),
    );
    assert_eq!(
        cpu.resolve(CodecIntent::Auto).unwrap().codec,
        Codec::H264,
        "CPU remains the working path"
    );
    service.shutdown();
}

#[test]
fn g02_decode_only_av1_is_not_an_encode_pair() {
    let fake = FakeProbe::new(vec![
        FakeAdapter::new(A, "A", &[Codec::H264])
            .primary()
            .decode_only(&[Codec::Av1]),
    ]);
    let service = service(&fake);
    let snapshot = submit(&service, request(gpu(A, false), q720(), 1, Some(5)));
    assert_eq!(reason_kind(&snapshot, Codec::Av1), "encoder_not_provided");
    assert!(
        snapshot
            .gpu(Codec::Av1)
            .and_then(|slot| slot.reason())
            .is_some_and(|reason| reason.detail().contains("декодер"))
    );
    assert!(snapshot.resolve(CodecIntent::Explicit(Codec::Av1)).is_err());
    assert_eq!(
        snapshot.resolve(CodecIntent::Auto).unwrap().codec,
        Codec::H264
    );
    service.shutdown();
}

#[test]
fn g02_profile_limited_to_720p30_reprobes_for_1080p60() {
    let fake = FakeProbe::new(vec![
        FakeAdapter::new(A, "A", &[Codec::H264])
            .primary()
            .only_query(q720()),
    ]);
    let service = service(&fake);
    let first = submit(&service, request(gpu(A, false), q720(), 1, Some(6)));
    assert_eq!(
        first
            .resolve(CodecIntent::Explicit(Codec::H264))
            .unwrap()
            .codec,
        Codec::H264
    );
    let probes = fake.probes();

    let second = submit(&service, request(gpu(A, false), q1080_60(), 2, Some(6)));
    assert_eq!(reason_kind(&second, Codec::H264), "profile_unsupported");
    assert!(second.resolve(CodecIntent::Explicit(Codec::H264)).is_err());
    assert!(
        second.resolve(CodecIntent::Auto).is_err(),
        "no codec is confirmed for 1080p60"
    );
    assert!(
        fake.probes() > probes,
        "a new resolution/FPS starts a new probe instead of reusing 720p30"
    );

    let third = submit(&service, request(gpu(A, false), q720(), 3, Some(6)));
    assert_eq!(
        third
            .resolve(CodecIntent::Explicit(Codec::H264))
            .unwrap()
            .codec,
        Codec::H264
    );
    service.shutdown();
}

#[test]
fn g03_late_a_after_switch_to_b_does_not_change_b() {
    let gate = Gate::new(true);
    let fake = FakeProbe::new(vec![
        FakeAdapter::new(A, "A", &[Codec::H264])
            .primary()
            .delay(gate.clone()),
        FakeAdapter::new(B, "B", &[Codec::H264, Codec::Av1]),
    ]);
    let service = service(&fake);
    let _a = service
        .set_video_encoder_backend(request(gpu(A, false), q720(), 1, Some(10)))
        .unwrap();
    gate.wait_started();
    let b = submit(&service, request(gpu(B, false), q720(), 2, Some(11)));
    gate.release();
    assert!(service.wait_until_idle(Duration::from_secs(5)));
    assert_eq!(fake.aborted(), 1, "the cancelled A probe is released");
    assert_eq!(
        service.snapshot().generation(),
        b.generation(),
        "a late A result never replaces B"
    );
    assert_eq!(
        service
            .snapshot()
            .adapter()
            .map(|adapter| adapter.name.as_str()),
        Some("B")
    );
    assert_eq!(service.snapshot().target().intent, 2);
    assert_eq!(
        service.snapshot().target().source,
        Some(SourceIdentity::Window(11))
    );
    assert_eq!(
        service
            .snapshot()
            .resolve(CodecIntent::Explicit(Codec::Av1))
            .unwrap()
            .backend,
        EncoderBackend::Gpu
    );
    service.shutdown();
}

#[test]
fn g03_uncooperative_late_a_is_discarded_and_only_one_probe_runs() {
    let gate = Gate::new(false);
    let b_gate = Gate::new(false);
    let fake = FakeProbe::new(vec![
        FakeAdapter::new(A, "A", &[Codec::H264])
            .primary()
            .delay(gate.clone()),
        FakeAdapter::new(B, "B", &[Codec::H264, Codec::Av1]).delay(b_gate.clone()),
    ]);
    let service = service(&fake);
    let a_generation = service
        .set_video_encoder_backend(request(gpu(A, false), q720(), 1, Some(20)))
        .unwrap();
    gate.wait_started();
    let b_generation = service
        .set_video_encoder_backend(request(gpu(B, false), q1080_60(), 2, Some(21)))
        .unwrap();
    gate.release();
    b_gate.wait_started();
    let during_b = service.snapshot();
    assert_ne!(
        during_b.generation(),
        a_generation,
        "a late A result must never become the ready snapshot"
    );
    assert_eq!(
        during_b.generation(),
        b_generation,
        "the working snapshot stays the new B generation"
    );
    b_gate.release();
    let b = ready(&service, b_generation);
    assert!(service.wait_until_idle(Duration::from_secs(5)));
    assert_eq!(fake.aborted(), 0, "the driver call was already in flight");
    assert!(fake.probes() >= 2, "B was probed after the late A returned");
    assert_eq!(fake.max_active(), 1, "the worker is bounded to one probe");
    let snapshot = service.snapshot();
    assert_ne!(snapshot.generation(), a_generation);
    assert_eq!(snapshot.generation(), b.generation());
    assert_eq!(
        snapshot.adapter().map(|adapter| adapter.name.as_str()),
        Some("B")
    );
    assert_eq!(snapshot.query(), q1080_60());
    service.shutdown();
}

#[test]
fn g03_close_and_reopen_settings_cancels_the_old_probe() {
    let gate = Gate::new(true);
    let fake = FakeProbe::new(vec![
        FakeAdapter::new(A, "A", &[Codec::H264])
            .primary()
            .delay(gate.clone()),
    ]);
    let service = service(&fake);
    service
        .set_video_encoder_backend(request(gpu(A, false), q720(), 1, Some(30)))
        .unwrap();
    gate.wait_started();
    let cancelled = service.cancel();
    assert!(service.wait_until_idle(Duration::from_secs(5)));
    assert_eq!(fake.aborted(), 1);
    let snapshot = service.snapshot();
    assert_eq!(snapshot.generation(), cancelled);
    assert_eq!(snapshot.state(), EncoderProbeState::Checking);
    assert_eq!(
        snapshot.codecs().len(),
        4,
        "the four codec rows stay visible while checking"
    );
    assert!(
        snapshot
            .resolve(CodecIntent::Auto)
            .is_err_and(|error| error.is_checking())
    );
    gate.release();

    fake.set_delay(luid(A), None);
    let reopened = submit(&service, request(gpu(A, false), q720(), 2, Some(30)));
    assert_eq!(
        reopened.resolve(CodecIntent::Auto).unwrap().codec,
        Codec::H264
    );
    service.shutdown();
}

#[test]
fn g04_activation_failure_never_yields_a_resolved_pair() {
    let fake = FakeProbe::new(vec![
        FakeAdapter::new(A, "A", &Codec::ALL)
            .primary()
            .failure(EncoderReason::EncoderBusy("исчерпан лимит сессий".into())),
    ]);
    let service = service(&fake);
    let snapshot = submit(&service, request(gpu(A, false), q720(), 1, Some(40)));
    assert!(!snapshot.gpu_available());
    for codec in Codec::ALL {
        assert_eq!(reason_kind(&snapshot, codec), "encoder_busy");
        assert!(snapshot.resolve(CodecIntent::Explicit(codec)).is_err());
    }
    assert!(snapshot.resolve(CodecIntent::Auto).is_err());
    // The explicit fallback only replaces the unusable GPU backend; it never
    // turns a failed GPU check into a silent GPU success.
    let fallback = submit(&service, request(gpu(A, true), q720(), 2, Some(40)));
    let cpu = fallback
        .resolve(CodecIntent::Explicit(Codec::H264))
        .unwrap();
    assert_eq!(cpu.backend, EncoderBackend::Cpu);
    service.shutdown();
}

#[test]
fn g04_error_reasons_are_distinct() {
    let cases = [
        EncoderReason::ProfileUnsupported("x".into()),
        EncoderReason::EncoderNotProvided("x".into()),
        EncoderReason::EncoderBusy("x".into()),
        EncoderReason::DeviceDisabled("x".into()),
        EncoderReason::ProbeFailed("x".into()),
    ];
    let kinds: std::collections::BTreeSet<_> = cases.iter().map(EncoderReason::kind).collect();
    assert_eq!(kinds.len(), 5);
    for reason in cases {
        assert!(!reason.to_string().is_empty());
    }
}

#[test]
fn g04_device_removal_invalidates_a_cached_confirmation() {
    let fake = FakeProbe::new(vec![FakeAdapter::new(A, "A", &[Codec::H264]).primary()]);
    let service = service(&fake);
    let first = submit(&service, request(gpu(A, false), q720(), 1, Some(41)));
    assert_eq!(
        first
            .resolve(CodecIntent::Explicit(Codec::H264))
            .unwrap()
            .codec,
        Codec::H264
    );
    fake.remove(luid(A));
    let probes = fake.probes();
    let removed = submit(&service, request(gpu(A, false), q720(), 2, Some(41)));
    assert!(removed.adapters().is_empty());
    assert!(!removed.has_hardware_adapter());
    assert_eq!(reason_kind(&removed, Codec::H264), "device_disabled");
    assert!(removed.resolve(CodecIntent::Explicit(Codec::H264)).is_err());
    assert_eq!(
        fake.probes(),
        probes,
        "a removed adapter is not probed through the stale cache"
    );
    service.shutdown();
}

#[test]
fn g04_driver_change_invalidates_the_confirmation() {
    let fake = FakeProbe::new(vec![FakeAdapter::new(A, "A", &[Codec::H264]).primary()]);
    let service = service(&fake);
    submit(&service, request(gpu(A, false), q720(), 1, Some(42)));
    let before = fake.activations();
    fake.set_adapters(vec![
        FakeAdapter::new(A, "A", &[Codec::H264]).primary().driver(2),
    ]);
    let after = submit(&service, request(gpu(A, false), q720(), 2, Some(42)));
    assert!(after.resolve(CodecIntent::Explicit(Codec::H264)).is_ok());
    assert!(
        fake.activations() > before,
        "a driver generation change forces activation again"
    );
    service.shutdown();
}

#[test]
fn g04_revalidate_activates_again_and_rejects_a_removed_device() {
    let fake = FakeProbe::new(vec![FakeAdapter::new(A, "A", &[Codec::H264]).primary()]);
    let service = service(&fake);
    let snapshot = submit(&service, request(gpu(A, false), q720(), 1, Some(43)));
    let resolved = snapshot
        .resolve(CodecIntent::Explicit(Codec::H264))
        .unwrap();
    let before = fake.activations();
    let confirmed = service
        .revalidate(&resolved, Duration::from_secs(5))
        .unwrap();
    assert_eq!(confirmed.adapter_luid(), Some(luid(A)));
    assert_eq!(confirmed.codec, Codec::H264);
    assert!(
        fake.activations() > before,
        "revalidation always activates the transform again"
    );

    fake.remove(luid(A));
    let error = service
        .revalidate(&confirmed, Duration::from_secs(5))
        .unwrap_err();
    assert!(matches!(
        error,
        super::RevalidateError::Rejected(EncoderReason::DeviceDisabled(_))
    ));
    service.shutdown();
}

#[test]
fn g04_repeated_start_after_a_failure_probes_again() {
    let fake = FakeProbe::new(vec![
        FakeAdapter::new(A, "A", &[Codec::H264])
            .primary()
            .failure(EncoderReason::ProbeFailed("активация".into())),
    ]);
    let service = service(&fake);
    let failed = submit(&service, request(gpu(A, false), q720(), 1, Some(44)));
    assert_eq!(reason_kind(&failed, Codec::H264), "probe_failed");
    let probes = fake.probes();
    fake.set_failure(luid(A), None);
    let retried = submit(&service, request(gpu(A, false), q720(), 2, Some(44)));
    assert_eq!(
        retried
            .resolve(CodecIntent::Explicit(Codec::H264))
            .unwrap()
            .codec,
        Codec::H264
    );
    assert!(
        fake.probes() > probes,
        "a failed probe is not cached as available"
    );
    service.shutdown();
}

#[test]
fn auto_uses_the_windows_primary_adapter_then_the_first_dxgi_adapter() {
    let fake = FakeProbe::new(vec![
        FakeAdapter::new(A, "A", &[Codec::H264]),
        FakeAdapter::new(B, "B", &[Codec::H264]).primary(),
        FakeAdapter::new(C, "C", &[Codec::H264]),
    ]);
    let service = service(&fake);
    let snapshot = submit(
        &service,
        request(
            VideoEncoderPreference::gpu(AdapterSelection::Auto, false),
            q720(),
            1,
            None,
        ),
    );
    assert_eq!(
        snapshot.adapter().map(|adapter| adapter.name.as_str()),
        Some("B")
    );
    assert_eq!(
        snapshot
            .adapters()
            .iter()
            .map(|adapter| adapter.name.as_str())
            .collect::<Vec<_>>(),
        ["A", "B", "C"]
    );

    fake.set_adapters(vec![
        FakeAdapter::new(A, "A", &[Codec::H264]),
        FakeAdapter::new(C, "C", &[Codec::H264]),
    ]);
    let snapshot = submit(
        &service,
        request(
            VideoEncoderPreference::gpu(AdapterSelection::Auto, false),
            q720(),
            2,
            None,
        ),
    );
    assert_eq!(
        snapshot.adapter().map(|adapter| adapter.name.as_str()),
        Some("A"),
        "without a primary the first suitable DXGI adapter is used"
    );
    service.shutdown();
}

#[test]
fn adapter_identity_is_the_luid_not_the_display_name() {
    let fake = FakeProbe::new(vec![
        FakeAdapter::new(A, "Same name", &[Codec::H264]),
        FakeAdapter::new(B, "Same name", &[Codec::H264, Codec::Av1]),
    ]);
    let service = service(&fake);
    let snapshot = submit(&service, request(gpu(B, false), q720(), 1, None));
    assert_eq!(snapshot.adapter().unwrap().luid, luid(B));
    assert_eq!(
        snapshot
            .resolve(CodecIntent::Explicit(Codec::Av1))
            .unwrap()
            .adapter_luid(),
        Some(luid(B))
    );
    let missing = snapshot.resolve(CodecIntent::Explicit(Codec::H264));
    assert!(missing.is_ok());
    let unknown = AdapterSelection::Explicit(AdapterLuid::new(9, 9));
    let generation = service
        .set_video_encoder_backend(EncoderRequest::new(
            VideoEncoderPreference::gpu(unknown, false),
            q720(),
            EncoderProbeTarget::default(),
        ))
        .unwrap();
    let snapshot = ready(&service, generation);
    assert_eq!(reason_kind(&snapshot, Codec::H264), "device_disabled");
    service.shutdown();
}

#[test]
fn snapshots_are_immutable_and_generations_increase() {
    let fake = FakeProbe::new(vec![FakeAdapter::new(A, "A", &[Codec::H264]).primary()]);
    let service = service(&fake);
    let first = submit(&service, request(gpu(A, false), q720(), 1, None));
    let held = first.clone();
    let second = submit(&service, request(gpu(A, false), q1080_60(), 2, None));
    assert!(second.generation() > first.generation());
    assert_eq!(held.generation(), first.generation());
    assert_eq!(held.query(), q720());
    assert_eq!(held.target().intent, 1);
    assert_eq!(second.target().intent, 2);
    service.shutdown();
}

#[test]
fn invalid_requests_are_rejected_before_the_worker() {
    let fake = FakeProbe::new(vec![FakeAdapter::new(A, "A", &[Codec::H264]).primary()]);
    let service = service(&fake);
    for (query, expected) in [
        (
            EncodeQuery::new(0, 720, 30),
            super::EncoderRequestError::ZeroSize,
        ),
        (
            EncodeQuery::new(1279, 720, 30),
            super::EncoderRequestError::OddSize,
        ),
        (
            EncodeQuery::new(5000, 720, 30),
            super::EncoderRequestError::TooLarge,
        ),
        (
            EncodeQuery::new(1280, 720, 0),
            super::EncoderRequestError::Fps,
        ),
    ] {
        let error = service
            .set_video_encoder_backend(request(gpu(A, false), query, 1, None))
            .unwrap_err();
        assert_eq!(error, expected);
    }
    service.shutdown();
}

#[test]
fn cpu_unavailability_is_reported_per_codec() {
    let fake = FakeProbe::new(Vec::new());
    let cpu = FakeCpu::without(&[Codec::Av1, Codec::Vp9]);
    let service = EncoderProbeService::new(fake, cpu as Arc<dyn CpuEncoderSource>).unwrap();
    let snapshot = submit(
        &service,
        request(VideoEncoderPreference::CPU, q720(), 1, None),
    );
    assert_eq!(
        snapshot.resolve(CodecIntent::Auto).unwrap().codec,
        Codec::H264
    );
    assert_eq!(
        snapshot
            .cpu(Codec::Av1)
            .and_then(|slot| slot.reason())
            .map(EncoderReason::kind),
        Some("encoder_not_provided")
    );
    assert!(snapshot.resolve(CodecIntent::Explicit(Codec::Av1)).is_err());
    // The GPU side is not checked while the dialog is on CPU.
    assert!(
        snapshot
            .codecs()
            .iter()
            .all(|state: &CodecEncoderState| !state.gpu().is_checked())
    );
    service.shutdown();
}

#[test]
fn g04_inventory_failure_is_explicit_and_cpu_stays_available() {
    let fake = FakeProbe::new(vec![FakeAdapter::new(A, "A", &[Codec::H264]).primary()]);
    fake.set_inventory_error(Some("MFTEnum2 0x80004005"));
    let service = service(&fake);
    let gpu = submit(&service, request(gpu(A, false), q720(), 1, Some(45)));
    assert!(gpu.adapters().is_empty());
    assert_eq!(reason_kind(&gpu, Codec::H264), "probe_failed");
    assert!(gpu.resolve(CodecIntent::Auto).is_err());
    let cpu = submit(
        &service,
        request(VideoEncoderPreference::CPU, q720(), 2, Some(45)),
    );
    assert_eq!(
        cpu.resolve(CodecIntent::Explicit(Codec::H264))
            .unwrap()
            .backend,
        EncoderBackend::Cpu
    );
    service.shutdown();
}

#[test]
fn h264_level_and_quality_mapping_are_deterministic() {
    assert_eq!(q720().h264_level(), 0x1f, "720p30 is level 3.1");
    assert_eq!(
        EncodeQuery::new(1920, 1080, 30).h264_level(),
        0x28,
        "1080p30 is level 4.0"
    );
    assert_eq!(q1080_60().h264_level(), 0x2a, "1080p60 is level 4.2");
    let quality = crate::publisher::Quality {
        resolution: crate::publisher::Resolution::P720,
        fps: 60,
        ..crate::publisher::Quality::default()
    };
    assert_eq!(
        EncodeQuery::from_quality(quality),
        EncodeQuery::new(1280, 720, 60)
    );
    assert!(EncodeQuery::new(1280, 720, 60).validate().is_ok());
}

/// G08 is not automatable in CI: it needs a real GPU driver and a real
/// encoder session. Run this locally with
/// `cargo test -p voxhold-media hardware_ -- --ignored --nocapture`.
#[cfg(windows)]
mod hardware {
    use super::*;
    use crate::video_encoder::{HardwareProbe, PlatformProbe, ProbeCancel, ProbeOutcome};

    fn probe_one(codec: Codec, query: EncodeQuery) {
        let probe = PlatformProbe;
        let inventory = probe.inventory().expect("DXGI enumeration failed");
        for adapter in inventory.adapters() {
            eprintln!(
                "adapter luid={} name={} vendor=0x{:04x} device=0x{:04x} driver=0x{:x}",
                adapter.luid,
                adapter.name,
                adapter.vendor_id,
                adapter.device_id,
                adapter.driver_version
            );
        }
        let Ok(adapter) = inventory.resolve(AdapterSelection::Auto) else {
            eprintln!("no suitable hardware adapter; nothing to prove");
            return;
        };
        match probe.probe(adapter, codec, query, &ProbeCancel::never()) {
            ProbeOutcome::Confirmed(profile) => {
                eprintln!(
                    "confirmed codec={} adapter={} profile={:?} {}x{}@{}",
                    codec.label(),
                    adapter.name,
                    profile,
                    query.width,
                    query.height,
                    query.fps
                );
            }
            ProbeOutcome::Unavailable(reason) => {
                eprintln!(
                    "not confirmed codec={} adapter={} reason={} ({})",
                    codec.label(),
                    adapter.name,
                    reason.kind(),
                    reason
                );
            }
            ProbeOutcome::Cancelled => panic!("the never-cancelled probe reported cancel"),
        }
    }

    #[test]
    #[ignore = "needs real Windows hardware; G08 manual run"]
    fn hardware_h264_mft_720p30() {
        probe_one(Codec::H264, q720());
    }

    #[test]
    #[ignore = "needs real Windows hardware; G08 manual run"]
    fn hardware_av1_mft_720p30() {
        probe_one(Codec::Av1, q720());
    }

    #[test]
    #[ignore = "needs real Windows hardware; G08 manual run"]
    fn hardware_inventory_is_stable_and_excludes_warp() {
        let probe = PlatformProbe;
        let first = probe.inventory().expect("DXGI enumeration failed");
        let second = probe.inventory().expect("DXGI enumeration failed");
        assert_eq!(first.adapters(), second.adapters());
        assert!(
            first.adapters().iter().all(|adapter| !adapter.software),
            "software adapters must be filtered by the inventory"
        );
    }
}
