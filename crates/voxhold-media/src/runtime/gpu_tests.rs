//! G04/G06: the GPU preflight gate and the release of a refused attempt.
//!
//! The probe double answers the same trait the Windows probe implements, so
//! the exact `revalidate_gpu` gate of the preparation thread is exercised:
//! an activation failure, a busy encoder (session limit) or a missing adapter
//! must stop the attempt before `stream.start` while a live stream stays.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use super::preflight::PendingRestart;
use super::preflight::PreflightEvent;
use super::preflight::finish_preflight;
use super::prep::PrepEvent;
use super::prep::finish_prep;
use super::screen::{launch_restart_preflight, launch_screen, revalidate_gpu};
use crate::publisher::engine::PublisherEngine;
use crate::publisher::{
    Access, Codec, Confirmed, Connection, EngineEvent, Outcome as PublisherOutcome, PublishRequest,
    Publisher, Quality, SdrProfile,
};
use crate::screen_capture::{PreviewDemand, PreviewStore, ScreenSource, SourceKind};
use crate::video_encoder::{
    AdapterEvidence, AdapterInfo, AdapterInventory, AdapterLuid, AdapterSelection,
    CpuEncoderSource, CpuEncoders, EncodeQuery, EncoderBackend, EncoderPlan, EncoderProbeService,
    EncoderReason, HardwareProbe, ProbeCancel, ProbeOutcome, VideoEncoderPreference,
};
use std::sync::atomic::AtomicBool;

fn adapter() -> AdapterInfo {
    AdapterInfo {
        luid: AdapterLuid::new(0x4d2, 0),
        name: "NVIDIA GeForce RTX 4070 Ti".into(),
        vendor_id: 0x10de,
        device_id: 0x2782,
        driver_version: 0x0020_0000_0010_069c,
        dedicated_video_memory: 12 << 30,
        software: false,
    }
}

fn source() -> ScreenSource {
    ScreenSource {
        id: 1,
        kind: SourceKind::Monitor,
        title: "Экран 1".into(),
        display_id: -1,
        width: 1920,
        height: 1080,
        monitor: None,
    }
}

fn cpu_request() -> PublishRequest {
    PublishRequest {
        source: source(),
        quality: Quality::default(),
        codec: Codec::H264,
        audio: false,
        audio_scope: crate::system_audio::AudioScope::Device,
        encoder: EncoderPlan::cpu(),
    }
}

fn gpu_request() -> PublishRequest {
    PublishRequest {
        encoder: EncoderPlan::gpu(
            adapter(),
            SdrProfile::H264 {
                baseline: crate::publisher::H264Baseline::Constrained,
                level: 0x1f,
            },
            9,
        ),
        ..cpu_request()
    }
}

/// A probe that never confirms: an activation failure, a busy encoder
/// (session limit) or a removed adapter.
struct RefusingProbe(EncoderReason);

impl HardwareProbe for RefusingProbe {
    fn inventory(&self) -> Result<AdapterInventory, String> {
        Ok(AdapterInventory::new(vec![adapter()], None))
    }

    fn probe(
        &self,
        _adapter: &AdapterInfo,
        _codec: Codec,
        _query: EncodeQuery,
        _cancel: &ProbeCancel,
    ) -> ProbeOutcome {
        ProbeOutcome::Unavailable(self.0.clone())
    }
}

struct Cpu;

impl CpuEncoderSource for Cpu {
    fn catalog(&self) -> CpuEncoders {
        CpuEncoders::all_available()
    }
}

fn refusing_service(reason: EncoderReason) -> Arc<EncoderProbeService> {
    Arc::new(
        EncoderProbeService::new(Arc::new(RefusingProbe(reason)), Arc::new(Cpu))
            .expect("the probe worker starts"),
    )
}

fn allowed() -> Access {
    Access {
        joined: true,
        stream_permission: true,
        slots_available: true,
    }
}

fn publishing(now: Instant) -> Publisher {
    let mut publisher = Publisher::new(1, 10);
    publisher.set_connection(Some("me".into()));
    publisher.set_access(allowed());
    publisher.begin_picker(1);
    publisher.publish(1, gpu_request(), false, now);
    assert_eq!(publisher.phase(), crate::publisher::Phase::Publishing);
    publisher
}

fn live_publisher(now: Instant) -> Publisher {
    let mut publisher = publishing(now);
    let outcomes = publisher.engine_ready(now, false, None);
    assert!(
        outcomes
            .iter()
            .any(|outcome| matches!(outcome, PublisherOutcome::Send(signal) if signal.kind == "stream.start")),
        "the attempt is publishing"
    );
    publisher.on_engine(
        EngineEvent::EncoderOpened(crate::video_encoder::HardwareEncoderIdentity {
            luid: adapter().luid,
            adapter: adapter().name,
            driver_version: adapter().driver_version,
            codec: Codec::H264,
            generation: 9,
            adapter_evidence: AdapterEvidence::MftAttribute,
        }),
        now,
    );
    publisher.on_engine(EngineEvent::State(Connection::Connected), now);
    publisher.on_engine(EngineEvent::State(Connection::FirstFrame), now);
    assert_eq!(publisher.phase(), crate::publisher::Phase::Live);
    publisher
}

fn binding(publisher: &Publisher) -> crate::runtime::prep::PrepBinding {
    crate::runtime::prep::PrepBinding {
        attempt: publisher.sharing().attempt,
        server: 1,
        channel: 10,
        connection: Some("me".into()),
        access: allowed(),
    }
}

/// Every refusal category of the probe is a refused launch: activation error,
/// a busy encoder at the session limit, a removed adapter. The server never
/// learns about the attempt.
#[test]
fn a_refused_gpu_revalidation_creates_no_publication() {
    let now = Instant::now();
    for reason in [
        EncoderReason::ProbeFailed("активация MFT не удалась".into()),
        EncoderReason::EncoderBusy("лимит сессий NVENC".into()),
        EncoderReason::DeviceDisabled("видеокарта отключена".into()),
    ] {
        let service = refusing_service(reason);
        let error = revalidate_gpu(Some(&service), &gpu_request())
            .expect_err("a refusing probe must refuse the launch");
        assert!(
            error.contains("не подтвердила кодировщик"),
            "the message names the failed stage: {error}"
        );

        let mut publisher = publishing(now);
        let binding = binding(&publisher);
        let outcomes = finish_prep(
            &mut publisher,
            &binding,
            &binding,
            allowed(),
            PrepEvent::<PublisherEngine>::Failed(error),
            now,
        );
        assert!(outcomes.engine.is_none(), "no engine is installed");
        assert!(
            !outcomes.outcomes.iter().any(|outcome| matches!(
                outcome,
                PublisherOutcome::Send(_) | PublisherOutcome::StartEngine { .. }
            )),
            "a refused preflight must not reach the server: {:?}",
            outcomes.outcomes
        );
        assert_eq!(publisher.phase(), crate::publisher::Phase::Failed);
        service.shutdown();
    }
}

/// A live attempt is not touched by a refused preflight of new settings.
#[test]
fn a_refused_gpu_revalidation_keeps_the_live_stream() {
    let now = Instant::now();
    let service = refusing_service(EncoderReason::EncoderBusy("занят".into()));
    let mut publisher = live_publisher(now);
    let binding = binding(&publisher);
    let attempt = publisher.sharing().attempt;
    let confirmed = publisher.sharing().confirmed.clone();
    let error = revalidate_gpu(Some(&service), &gpu_request()).expect_err("refused");
    publisher.preflight_started(1);
    let outcomes = finish_preflight(
        &mut publisher,
        &binding,
        &binding,
        allowed(),
        PreflightEvent::Failed(error),
        1,
        gpu_request(),
        now,
    );
    assert!(
        !outcomes.iter().any(|outcome| matches!(
            outcome,
            PublisherOutcome::Send(_)
                | PublisherOutcome::StartEngine { .. }
                | PublisherOutcome::StopEngine
        )),
        "the refused preflight stopped the live stream: {outcomes:?}"
    );
    let sharing = publisher.sharing();
    assert_eq!(sharing.phase, crate::publisher::Phase::Live);
    assert_eq!(sharing.attempt, attempt);
    assert_eq!(sharing.confirmed, confirmed);
    assert!(sharing.error.is_some());
    service.shutdown();
}

/// The CPU path never activates a transform before `stream.start`: the gate
/// passes without a probe, exactly like the pre-GPU build.
#[test]
fn a_cpu_attempt_skips_the_hardware_revalidation() {
    assert!(revalidate_gpu(None, &cpu_request()).is_ok());
    let service = refusing_service(EncoderReason::DeviceDisabled("нет".into()));
    assert!(
        revalidate_gpu(Some(&service), &cpu_request()).is_ok(),
        "a CPU attempt must not depend on the GPU probe"
    );
    assert!(
        revalidate_gpu(Some(&service), &gpu_request()).is_err(),
        "the same probe refuses the GPU attempt"
    );
    service.shutdown();
}

/// A GPU request without a probe worker in this session is refused instead of
/// being published on CPU.
#[test]
fn a_gpu_request_without_a_probe_is_refused() {
    let error = revalidate_gpu(None, &gpu_request()).expect_err("no probe");
    assert!(error.contains("Процессор"), "{error}");
}

/// The plan the request carries is the adapter the probe was asked about; a
/// generation mismatch is covered by the publisher identity test.
#[test]
fn the_gpu_plan_resolves_the_confirmed_pair() {
    let request = gpu_request();
    let resolved = request
        .encoder
        .resolved(request.codec, EncodeQuery::new(1280, 720, 30))
        .expect("the plan resolves");
    assert_eq!(resolved.backend, EncoderBackend::Gpu);
    assert_eq!(resolved.adapter_luid(), Some(adapter().luid));
    assert_eq!(resolved.profile, request.encoder.profile.expect("profile"));
    assert_eq!(
        request.encoder.generation, 9,
        "the snapshot generation travels with the request"
    );
}

/// A service snapshot is not a launch permit: `revalidate` activates again.
#[test]
fn the_probe_activation_count_grows_on_revalidation() {
    let service = refusing_service(EncoderReason::EncoderBusy("занят".into()));
    let generation = service
        .set_video_encoder_backend(crate::video_encoder::EncoderRequest::new(
            VideoEncoderPreference::gpu(AdapterSelection::Explicit(adapter().luid), false),
            EncodeQuery::new(1280, 720, 30),
            crate::video_encoder::EncoderProbeTarget::default(),
        ))
        .expect("request");
    let _ = service.wait_for(generation, Duration::from_secs(2));
    // The first probe of the request is answered; the live check is another
    // activation and must reach the driver again.
    let before = service.snapshot().generation();
    let _ = revalidate_gpu(Some(&service), &gpu_request());
    assert_eq!(service.snapshot().generation(), before);
    service.shutdown();
}

/// The live attempt keeps its confirmed configuration object; this pins that a
/// GPU identity is part of it rather than a lookalike of the request.
#[test]
fn the_confirmed_encoder_survives_a_preflight() {
    let now = Instant::now();
    let mut publisher = live_publisher(now);
    let confirmed: Confirmed = publisher.sharing().confirmed.clone().expect("confirmed");
    assert_eq!(confirmed.encoder.backend, EncoderBackend::Gpu);
    assert_eq!(
        confirmed
            .encoder
            .adapter
            .as_ref()
            .map(|adapter| adapter.luid),
        Some(adapter().luid)
    );
    publisher.preflight_started(1);
    assert_eq!(publisher.sharing().confirmed, Some(confirmed));
}

/// The preparation closure of `start_screen_prep` itself, not a hand-made
/// failure: a refusing revalidation has to stop the launch before any capture,
/// loopback or peer exists, so `stream.start` is never sent.
#[test]
fn the_real_preparation_launch_stops_before_stream_start_after_a_refused_pair() {
    let now = Instant::now();
    let service = refusing_service(EncoderReason::EncoderBusy("лимит сессий NVENC".into()));
    let error = match launch_screen(
        Some(service.clone()),
        gpu_request(),
        Vec::new(),
        "all".into(),
        Arc::new(PreviewStore::default()),
        Arc::new(PreviewDemand::default()),
        &AtomicBool::new(false),
    ) {
        Ok(_) => panic!("the launch must not create an engine for a refused pair"),
        Err(error) => error,
    };
    assert!(
        error.contains("не подтвердила кодировщик"),
        "the launch reported another stage: {error}"
    );

    // The session applies exactly this result of its preparation worker.
    let mut publisher = publishing(now);
    let binding = binding(&publisher);
    let outcomes = finish_prep(
        &mut publisher,
        &binding,
        &binding,
        allowed(),
        PrepEvent::<PublisherEngine>::Failed(error),
        now,
    );
    assert!(outcomes.engine.is_none(), "no engine is installed");
    assert!(
        !outcomes.outcomes.iter().any(|outcome| matches!(
            outcome,
            PublisherOutcome::Send(_) | PublisherOutcome::StartEngine { .. }
        )),
        "the refused launch reached the server: {:?}",
        outcomes.outcomes
    );
    assert_eq!(publisher.phase(), crate::publisher::Phase::Failed);
    service.shutdown();
}

/// The restart closure of `start_restart_prep`: the live attempt is not
/// stopped when the new hardware pair is refused.
#[test]
fn the_real_restart_preflight_keeps_the_live_stream_after_a_refused_pair() {
    let now = Instant::now();
    let service = refusing_service(EncoderReason::ProbeFailed(
        "активация MFT не удалась".into(),
    ));
    let mut publisher = live_publisher(now);
    let binding = binding(&publisher);
    let attempt = publisher.sharing().attempt;
    let confirmed = publisher.sharing().confirmed.clone();
    let plan = gpu_request();
    let request = plan.clone();
    publisher.preflight_started(1);
    let probe = Some(service.clone());
    let mut pending = PendingRestart::start_with(binding.clone(), now, 1, request.clone(), {
        move |cancelled| launch_restart_preflight(probe, &plan, cancelled)
    })
    .expect("the preflight thread starts");

    let deadline = Instant::now() + Duration::from_secs(5);
    let event = loop {
        if let Some(event) = pending.poll(Instant::now()) {
            break event;
        }
        assert!(
            Instant::now() < deadline,
            "the preflight worker never reported"
        );
        std::thread::sleep(Duration::from_millis(2));
    };
    match &event {
        PreflightEvent::Failed(message) => assert!(
            message.contains("не подтвердила кодировщик"),
            "the preflight failed in another stage: {message}"
        ),
        PreflightEvent::Ready => panic!("a refused pair must not report a ready capture"),
        PreflightEvent::Timeout => panic!("the test probe refused instead of timing out"),
    }
    let outcomes = finish_preflight(
        &mut publisher,
        &binding,
        &binding,
        allowed(),
        event,
        1,
        request,
        now,
    );
    assert!(
        !outcomes.iter().any(|outcome| matches!(
            outcome,
            PublisherOutcome::Send(_)
                | PublisherOutcome::StartEngine { .. }
                | PublisherOutcome::StopEngine
        )),
        "the refused restart touched the live stream: {outcomes:?}"
    );
    let sharing = publisher.sharing();
    assert_eq!(sharing.phase, crate::publisher::Phase::Live);
    assert_eq!(sharing.attempt, attempt);
    assert_eq!(sharing.confirmed, confirmed);
    service.shutdown();
}
