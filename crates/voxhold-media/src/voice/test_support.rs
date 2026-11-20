//! Headless session for the recovery tests.
//!
//! The real `Session::new` opens platform audio and a capture device, so tests
//! that exercise `Session::tick_playout` need a session without devices. Every
//! field is the same one the production constructor sets; only the ADM/capture
//! paths are left unopened.

use super::*;

impl Session {
    pub(super) fn test_headless(open: Arc<winmm_out::OpenSink>) -> Self {
        let factory = PeerConnectionFactory::default();
        let peer = factory
            .create_peer_connection(RtcConfiguration::default())
            .expect("a headless peer connection");
        let now = std::time::Instant::now();
        Self {
            peer,
            sender: None,
            viewer: None,
            capture: Arc::new(CaptureGate::default()),
            events: Arc::new(Events::default()),
            // A factory that never acquired the platform ADM: the software
            // branch never calls it, and an unexpected ADM call fails instead
            // of opening a device.
            audio: PlatformAudio(PeerConnectionFactory::default()),
            mic: None,
            controls: Controls::default(),
            sources: HashMap::new(),
            playout: None,
            software_playout: false,
            playout_mode: PlayoutMode::AdmFallback,
            playout_attempts: SinkRecovery::default(),
            adm_retry: RetryAt::default(),
            adm_playout_duration: None,
            adm_playout_progress_at: now,
            adm_playout_started_at: now,
            #[cfg(test)]
            test_no_adm: true,
            playout_suspended: false,
            playout_reconnects: 0,
            playout_open: open,
            playout_tracks: Arc::new(Mutex::new(TrackSet::default())),
            runtime: tokio::runtime::Handle::current(),
            last_playout_report: now,
            speaking: HashMap::new(),
            local_speaking: false,
            remote_sdp: None,
            completed_ufrags: Vec::new(),
            pending_ice: VecDeque::new(),
            input_index: None,
            output_index: None,
            input_opened: None,
            input_progress: 0,
            input_progress_at: now,
            input_started_at: now,
            input_duration: None,
            input_connected: false,
            input_recovery_failed: false,
            input_reopen_attempted: false,
            input_system_attempts: 0,
            input_stable_since: None,
            output: OutputState::new(None),
            device_check: now,
        }
    }
}
