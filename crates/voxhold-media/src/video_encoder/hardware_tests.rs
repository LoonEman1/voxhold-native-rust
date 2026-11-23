//! G08: the manual hardware acceptance tests.
//!
//! They are not automatable in CI: each needs a real GPU driver and a real
//! encoder session. Run them locally with
//! `cargo test -p voxhold-media hardware_ -- --ignored --nocapture --test-threads=1`.
//!
//! Every test owns a [`WorkerPlatform`] on its own thread. Without it each
//! Media Foundation call fails with `CO_E_NOTINITIALIZED` (`0x800401f0`), and a
//! green "ok" would prove nothing.

use std::time::Duration;

use crate::publisher::SdrProfile;
use crate::video_encoder::windows::WorkerPlatform;
use crate::video_encoder::{
    AdapterInventory, AdapterSelection, Codec, EncodeQuery, EncoderProbeService,
    EncoderProbeTarget, EncoderRequest, HardwareProbe, PlatformProbe, ProbeCancel, ProbeOutcome,
    VideoEncoderPreference,
};

fn q720() -> EncodeQuery {
    EncodeQuery::new(1280, 720, 30)
}

/// COM apartment and Media Foundation of one test thread.
struct Platform(#[allow(dead_code)] WorkerPlatform);

fn platform() -> Platform {
    let platform = WorkerPlatform::new();
    assert!(
        platform.is_ready(),
        "COM/Media Foundation не инициализировались на тестовом потоке"
    );
    Platform(platform)
}

fn inventory(probe: &PlatformProbe) -> AdapterInventory {
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
    inventory
}

/// `None` means the machine has no hardware adapter at all: the test then
/// reports an explicit SKIP instead of a green run without proof.
fn probe_one(codec: Codec, query: EncodeQuery) -> Option<ProbeOutcome> {
    let _platform = platform();
    let probe = PlatformProbe;
    let inventory = inventory(&probe);
    if inventory.adapters().is_empty() {
        eprintln!("SKIP: на машине нет аппаратного адаптера, проверять нечего");
        return None;
    }
    let adapter = inventory
        .resolve(AdapterSelection::Auto)
        .expect("no suitable hardware adapter");
    assert_ne!(
        adapter.driver_version, 0,
        "версия драйвера {} не определена, кэш не заметит её смену",
        adapter.name
    );
    Some(probe.probe(adapter, codec, query, &ProbeCancel::never()))
}

#[test]
#[ignore = "needs real Windows hardware; G08 manual run"]
fn hardware_h264_mft_720p30() {
    let Some(outcome) = probe_one(Codec::H264, q720()) else {
        return;
    };
    match outcome {
        ProbeOutcome::Confirmed(SdrProfile::H264 { baseline, level }) => {
            eprintln!("confirmed codec=H264 baseline={baseline:?} level=0x{level:02x} 1280x720@30");
        }
        ProbeOutcome::Confirmed(other) => {
            panic!("H.264 подтверждён другим SDR-профилем: {other:?}");
        }
        ProbeOutcome::Unavailable(reason) => {
            panic!("H.264 720p30 не подтверждён: {} ({reason})", reason.kind())
        }
        ProbeOutcome::Cancelled => panic!("the never-cancelled probe reported cancel"),
    }
}

#[test]
#[ignore = "needs real Windows hardware; G08 manual run"]
fn hardware_av1_mft_720p30() {
    let Some(outcome) = probe_one(Codec::Av1, q720()) else {
        return;
    };
    match outcome {
        ProbeOutcome::Confirmed(profile) => {
            eprintln!("confirmed codec=AV1 profile={profile:?} 1280x720@30");
        }
        ProbeOutcome::Unavailable(reason) => {
            assert!(
                !reason.detail().contains("0x800401f0"),
                "COM не инициализирован, это не ответ драйвера: {reason}"
            );
            eprintln!(
                "not confirmed codec=AV1 reason={} ({reason})",
                reason.kind()
            );
        }
        ProbeOutcome::Cancelled => panic!("the never-cancelled probe reported cancel"),
    }
}

#[test]
#[ignore = "needs real Windows hardware; G08 manual run"]
fn hardware_inventory_is_stable_and_excludes_warp() {
    let _platform = platform();
    let probe = PlatformProbe;
    let first = inventory(&probe);
    let second = probe.inventory().expect("DXGI enumeration failed");
    assert_eq!(first.adapters(), second.adapters());
    assert!(
        first.adapters().iter().all(|adapter| !adapter.software),
        "software adapters must be filtered by the inventory"
    );
    for adapter in first.adapters() {
        assert_ne!(
            adapter.driver_version, 0,
            "версия драйвера {} не определена",
            adapter.name
        );
    }
}

/// The real end-to-end path: the service worker owns the platform, probes the
/// automatic adapter and publishes a snapshot for the dialog.
#[test]
#[ignore = "needs real Windows hardware; G08 manual run"]
fn hardware_service_confirms_h264() {
    let service = EncoderProbeService::platform().expect("probe service");
    let generation = service
        .set_video_encoder_backend(EncoderRequest::new(
            VideoEncoderPreference::gpu(AdapterSelection::Auto, false),
            q720(),
            EncoderProbeTarget::default(),
        ))
        .expect("request");
    let snapshot = service
        .wait_for(generation, Duration::from_secs(60))
        .expect("the probe did not finish");
    for adapter in snapshot.adapters() {
        eprintln!(
            "service adapter luid={} name={} driver=0x{:x}",
            adapter.luid, adapter.name, adapter.driver_version
        );
    }
    if !snapshot.has_hardware_adapter() {
        eprintln!("SKIP: на машине нет аппаратного адаптера, проверять нечего");
        service.shutdown();
        return;
    }
    let adapter = snapshot.adapter().expect("adapter");
    assert_ne!(
        adapter.driver_version, 0,
        "версия драйвера не определена через сервис"
    );
    for codec in Codec::ALL {
        let slot = snapshot.gpu(codec).expect("codec row");
        assert!(slot.is_checked(), "GPU-путь не проверялся");
        match (slot.profile(), slot.reason()) {
            (Some(profile), _) => {
                eprintln!("gpu codec={} confirmed profile={profile:?}", codec.label());
            }
            (None, Some(reason)) => eprintln!(
                "gpu codec={} reason={} ({reason})",
                codec.label(),
                reason.kind()
            ),
            (None, None) => eprintln!("gpu codec={} не проверялся", codec.label()),
        }
    }
    match snapshot.gpu(Codec::H264).and_then(|slot| slot.profile()) {
        Some(profile) => eprintln!(
            "confirmed codec=H264 adapter={} profile={profile:?} 1280x720@30",
            adapter.name
        ),
        None => panic!(
            "H.264 не подтверждён через сервис: {:?}",
            snapshot
                .gpu(Codec::H264)
                .and_then(|slot| slot.reason())
                .map(|reason| (reason.kind(), reason.to_string()))
        ),
    }
    service.shutdown();
}
