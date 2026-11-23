//! Bounded probe worker and the immutable snapshot it publishes.

use std::{
    collections::VecDeque,
    fmt,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use super::{
    AdapterInfo, AdapterInventory, AdapterKey, Codec, CodecEncoderState, CpuEncoderSource,
    EncodeQuery, EncoderBackend, EncoderProbeState, EncoderProbeTarget, EncoderReason, EncoderSlot,
    HardwareProbe, ProbeCancel, ProbeOutcome, ResolvedEncoder, SdrProfile, VideoEncoderPreference,
    VideoEncoderSnapshot,
};

/// One dialog request. A newer value of any field produces a new generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncoderRequest {
    pub preference: VideoEncoderPreference,
    pub query: EncodeQuery,
    pub target: EncoderProbeTarget,
}

impl EncoderRequest {
    pub fn new(
        preference: VideoEncoderPreference,
        query: EncodeQuery,
        target: EncoderProbeTarget,
    ) -> Self {
        Self {
            preference,
            query,
            target,
        }
    }

    fn validated(self) -> Result<Self, EncoderRequestError> {
        Ok(Self {
            query: self.query.validate()?,
            ..self
        })
    }
}

/// A request the model cannot probe. The dialog never starts a publication on
/// this answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EncoderRequestError {
    ZeroSize,
    OddSize,
    TooLarge,
    Fps,
    Stopped,
}

impl fmt::Display for EncoderRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ZeroSize => "Размер кадра не может быть нулевым.",
            Self::OddSize => "Для NV12 нужны чётные ширина и высота.",
            Self::TooLarge => "Размер кадра превышает 4096 по одной из сторон.",
            Self::Fps => "Частота кадров вне диапазона 1–120.",
            Self::Stopped => "Проверка кодировщика уже остановлена.",
        })
    }
}

impl std::error::Error for EncoderRequestError {}

/// Why a live-publication revalidation did not confirm the pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RevalidateError {
    /// The service was stopped before or during the check.
    Stopped,
    /// The bounded wait expired; the caller must not start the publication.
    Timeout,
    /// A newer revalidation replaced this one.
    Stale,
    /// The adapter disappeared, the driver changed or activation failed.
    Rejected(EncoderReason),
}

impl fmt::Display for RevalidateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopped => formatter.write_str("Проверка кодировщика остановлена."),
            Self::Timeout => formatter.write_str("Проверка кодировщика не завершилась вовремя."),
            Self::Stale => formatter.write_str("Проверка кодировщика устарела."),
            Self::Rejected(reason) => write!(formatter, "{reason}"),
        }
    }
}

impl std::error::Error for RevalidateError {}

/// One bounded worker owns the adapter inventory, the confirmation cache and
/// every COM/MFT object. The UI only submits intents and polls the snapshot.
pub struct EncoderProbeService {
    inner: Arc<Inner>,
}

struct Inner {
    probe: Arc<dyn HardwareProbe>,
    cpu: Arc<dyn CpuEncoderSource>,
    current: Arc<AtomicU64>,
    state: Mutex<ServiceState>,
    cache: Mutex<ProbeCache>,
    active: AtomicBool,
    wake: Condvar,
    worker: Mutex<Option<JoinHandle<()>>>,
}

struct ServiceState {
    generation: u64,
    pending: Option<EncoderRequest>,
    revalidate: Option<RevalidateJob>,
    snapshot: Arc<VideoEncoderSnapshot>,
    stop: bool,
}

struct RevalidateJob {
    selected: ResolvedEncoder,
    reply: mpsc::SyncSender<RevalidateReply>,
}

enum RevalidateReply {
    Confirmed(SdrProfile),
    Rejected(EncoderReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProbeCacheKey {
    adapter: AdapterKey,
    codec: Codec,
    query: EncodeQuery,
}

#[derive(Default)]
struct ProbeCache {
    inventory: u64,
    entries: Vec<(ProbeCacheKey, SdrProfile)>,
}

impl ProbeCache {
    fn reconcile(&mut self, inventory: u64) {
        if self.inventory != inventory {
            self.entries.clear();
            self.inventory = inventory;
        }
    }

    fn get(&self, key: ProbeCacheKey) -> Option<SdrProfile> {
        self.entries
            .iter()
            .find(|(candidate, _)| *candidate == key)
            .map(|(_, profile)| *profile)
    }

    fn insert(&mut self, key: ProbeCacheKey, profile: SdrProfile) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|(candidate, _)| *candidate == key)
        {
            entry.1 = profile;
        } else {
            self.entries.push((key, profile));
        }
    }

    fn remove(&mut self, key: ProbeCacheKey) {
        self.entries.retain(|(candidate, _)| *candidate != key);
    }

    fn clear(&mut self) {
        self.entries.clear();
    }
}

impl EncoderProbeService {
    pub fn new(
        probe: Arc<dyn HardwareProbe>,
        cpu: Arc<dyn CpuEncoderSource>,
    ) -> Result<Self, std::io::Error> {
        let inner = Arc::new(Inner {
            probe,
            cpu,
            current: Arc::new(AtomicU64::new(0)),
            state: Mutex::new(ServiceState {
                generation: 0,
                pending: None,
                revalidate: None,
                snapshot: Arc::new(VideoEncoderSnapshot::checking(
                    0,
                    VideoEncoderPreference::default(),
                    EncodeQuery::default(),
                    EncoderProbeTarget::default(),
                )),
                stop: false,
            }),
            cache: Mutex::new(ProbeCache::default()),
            active: AtomicBool::new(false),
            wake: Condvar::new(),
            worker: Mutex::new(None),
        });
        let worker_inner = inner.clone();
        let worker = thread::Builder::new()
            .name("voxhold-encoder-probe".into())
            .spawn(move || worker(worker_inner))?;
        *inner.worker.lock().unwrap() = Some(worker);
        Ok(Self { inner })
    }

    /// The production probe: real DXGI/MFT on Windows, an empty inventory
    /// elsewhere, with the pinned WebRTC CPU catalog.
    pub fn platform() -> Result<Self, std::io::Error> {
        Self::new(
            Arc::new(super::PlatformProbe),
            Arc::new(super::WebRtcCpuEncoders),
        )
    }

    /// Submit a new preference/query/target. This never blocks on hardware:
    /// the returned generation is already the checking snapshot generation.
    pub fn set_video_encoder_backend(
        &self,
        request: EncoderRequest,
    ) -> Result<u64, EncoderRequestError> {
        let request = request.validated()?;
        let mut state = self.inner.state.lock().unwrap();
        if state.stop {
            return Err(EncoderRequestError::Stopped);
        }
        state.generation = state.generation.wrapping_add(1);
        let generation = state.generation;
        state.snapshot = Arc::new(VideoEncoderSnapshot::checking(
            generation,
            request.preference,
            request.query,
            request.target.clone(),
        ));
        state.pending = Some(request);
        self.inner.current.store(generation, Ordering::Release);
        drop(state);
        self.inner.wake.notify_all();
        Ok(generation)
    }

    /// Invalidate the current attempt without starting another one.
    ///
    /// The snapshot deliberately becomes a `Checking` value of the new
    /// generation: neither the old "available" pair nor an "unavailable"
    /// reason of the discarded attempt may be shown as the answer to the new
    /// intent. A caller that does want a result must submit a request.
    pub fn cancel(&self) -> u64 {
        let mut state = self.inner.state.lock().unwrap();
        state.generation = state.generation.wrapping_add(1);
        let generation = state.generation;
        state.pending = None;
        let previous = state.snapshot.clone();
        state.snapshot = Arc::new(VideoEncoderSnapshot::checking(
            generation,
            previous.preference(),
            previous.query(),
            previous.target().clone(),
        ));
        self.inner.current.store(generation, Ordering::Release);
        drop(state);
        self.inner.wake.notify_all();
        generation
    }

    pub fn snapshot(&self) -> Arc<VideoEncoderSnapshot> {
        self.inner.state.lock().unwrap().snapshot.clone()
    }

    pub fn generation(&self) -> u64 {
        self.inner.state.lock().unwrap().generation
    }

    /// Wait for the first ready snapshot at `generation` or newer. Intended for
    /// tests and for the next slice's preflight; the UI polls instead.
    pub fn wait_for(
        &self,
        generation: u64,
        timeout: Duration,
    ) -> Option<Arc<VideoEncoderSnapshot>> {
        let deadline = Instant::now() + timeout;
        let mut state = self.inner.state.lock().unwrap();
        loop {
            if state.stop {
                return None;
            }
            let snapshot = state.snapshot.clone();
            if snapshot.generation() >= generation && snapshot.state() == EncoderProbeState::Ready {
                return Some(snapshot);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            state = self.inner.wake.wait_timeout(state, deadline - now).ok()?.0;
        }
    }

    /// True when no probe or revalidation is queued or running.
    pub fn wait_until_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.inner.state.lock().unwrap();
        loop {
            if state.stop {
                return true;
            }
            if state.pending.is_none()
                && state.revalidate.is_none()
                && !self.inner.active.load(Ordering::Acquire)
            {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            state = match self.inner.wake.wait_timeout(state, deadline - now) {
                Ok((guard, _)) => guard,
                Err(_) => return false,
            };
        }
    }

    /// Re-check one confirmed pair on the worker before a real publication.
    /// This always activates the transform again and bypasses the cache. The
    /// call belongs to the media worker thread, not the UI.
    pub fn revalidate(
        &self,
        selected: &ResolvedEncoder,
        timeout: Duration,
    ) -> Result<ResolvedEncoder, RevalidateError> {
        if selected.backend == EncoderBackend::Cpu {
            return Ok(selected.clone());
        }
        let (reply, wait) = mpsc::sync_channel(1);
        {
            let mut state = self.inner.state.lock().unwrap();
            if state.stop {
                return Err(RevalidateError::Stopped);
            }
            if let Some(previous) = state.revalidate.replace(RevalidateJob {
                selected: selected.clone(),
                reply,
            }) {
                let _ = previous
                    .reply
                    .send(RevalidateReply::Rejected(EncoderReason::ProbeFailed(
                        "проверка заменена новой".into(),
                    )));
            }
        }
        self.inner.wake.notify_all();
        match wait.recv_timeout(timeout) {
            Ok(RevalidateReply::Confirmed(profile)) => Ok(ResolvedEncoder {
                profile,
                ..selected.clone()
            }),
            Ok(RevalidateReply::Rejected(reason)) => Err(RevalidateError::Rejected(reason)),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(RevalidateError::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(RevalidateError::Stopped),
        }
    }

    /// Stop the worker and join it.
    ///
    /// This blocks for the duration of one in-flight driver call: the worker
    /// is never detached, so COM objects, activations and any residual encoded
    /// frame are released on the thread that owns them. Call it from a
    /// non-UI teardown thread, not from a paint or input handler; [`Drop`]
    /// signals the stop without joining, so closing a dialog cannot block on
    /// an encoder.
    pub fn shutdown(&self) {
        self.signal_stop();
        let worker = self.inner.worker.lock().unwrap().take();
        if let Some(worker) = worker
            && worker.thread().id() != thread::current().id()
        {
            let _ = worker.join();
        }
    }

    fn signal_stop(&self) {
        {
            let mut state = self.inner.state.lock().unwrap();
            state.stop = true;
            state.pending = None;
            if let Some(job) = state.revalidate.take() {
                let _ = job
                    .reply
                    .send(RevalidateReply::Rejected(EncoderReason::DeviceDisabled(
                        "проверка остановлена".into(),
                    )));
            }
        }
        // Invalidate any cooperative probe before waiting for the worker.
        let generation = self.inner.state.lock().unwrap().generation;
        self.inner
            .current
            .store(generation.wrapping_add(1), Ordering::Release);
        self.inner.wake.notify_all();
    }
}

impl Drop for EncoderProbeService {
    fn drop(&mut self) {
        // The worker owns all hardware objects and observes the stop flag on
        // its next checkpoint. Drop never joins, so closing settings cannot
        // block the UI on an in-flight driver call.
        self.signal_stop();
    }
}

enum Job {
    Probe(EncoderRequest, u64),
    Revalidate(RevalidateJob),
}

fn worker(inner: Arc<Inner>) {
    #[cfg(windows)]
    let _platform = super::windows::WorkerPlatform::new();
    loop {
        let job = {
            let mut state = inner.state.lock().unwrap();
            loop {
                if state.stop {
                    return;
                }
                if let Some(job) = state.revalidate.take() {
                    inner.active.store(true, Ordering::Release);
                    break Job::Revalidate(job);
                }
                if let Some(request) = state.pending.take() {
                    let generation = state.generation;
                    inner.active.store(true, Ordering::Release);
                    break Job::Probe(request, generation);
                }
                state = inner.wake.wait(state).unwrap();
            }
        };
        match job {
            Job::Probe(request, generation) => {
                let snapshot = run_probe(&inner, &request, generation);
                let mut state = inner.state.lock().unwrap();
                if !state.stop
                    && state.generation == generation
                    && let Some(snapshot) = snapshot
                {
                    state.snapshot = Arc::new(snapshot);
                }
                inner.active.store(false, Ordering::Release);
                inner.wake.notify_all();
            }
            Job::Revalidate(job) => {
                let reply = run_revalidate(&inner, &job.selected);
                let state = inner.state.lock().unwrap();
                if state.stop {
                    let _ =
                        job.reply
                            .send(RevalidateReply::Rejected(EncoderReason::DeviceDisabled(
                                "проверка остановлена".into(),
                            )));
                } else {
                    let _ = job.reply.send(reply);
                }
                drop(state);
                inner.active.store(false, Ordering::Release);
                inner.wake.notify_all();
            }
        }
    }
}

fn run_probe(
    inner: &Inner,
    request: &EncoderRequest,
    generation: u64,
) -> Option<VideoEncoderSnapshot> {
    let cancel = ProbeCancel::new(inner.current.clone(), generation);
    let cpu = inner.cpu.catalog();
    let inventory = match inner.probe.inventory() {
        Ok(inventory) => {
            inner
                .cache
                .lock()
                .unwrap()
                .reconcile(inventory.generation());
            inventory
        }
        Err(detail) => {
            let inventory = AdapterInventory::new(Vec::new(), None);
            let reason = EncoderReason::ProbeFailed(detail);
            let codecs = super::ENCODER_CODECS
                .into_iter()
                .map(|codec| {
                    let gpu = match request.preference.backend {
                        EncoderBackend::Gpu => EncoderSlot::unavailable(reason.clone()),
                        EncoderBackend::Cpu => EncoderSlot::unchecked(),
                    };
                    CodecEncoderState::new(codec, cpu.slot(codec), gpu)
                })
                .collect();
            return Some(VideoEncoderSnapshot::ready(
                generation,
                request.preference,
                request.query,
                request.target.clone(),
                inventory,
                None,
                codecs,
            ));
        }
    };
    if cancel.is_cancelled() {
        return None;
    }
    let selected = inventory.resolve(request.preference.adapter).cloned();
    let mut codecs = VecDeque::with_capacity(super::ENCODER_CODECS.len());
    for codec in super::ENCODER_CODECS {
        let cpu_slot = cpu.slot(codec);
        let gpu_slot = match request.preference.backend {
            EncoderBackend::Cpu => EncoderSlot::unchecked(),
            EncoderBackend::Gpu => match &selected {
                Err(reason) => EncoderSlot::unavailable(reason.clone()),
                Ok(adapter) => match probe_codec(inner, adapter, codec, request.query, &cancel) {
                    Some(slot) => slot,
                    None => return None,
                },
            },
        };
        codecs.push_back(CodecEncoderState::new(codec, cpu_slot, gpu_slot));
    }
    Some(VideoEncoderSnapshot::ready(
        generation,
        request.preference,
        request.query,
        request.target.clone(),
        inventory,
        selected.ok(),
        codecs.into(),
    ))
}

fn probe_codec(
    inner: &Inner,
    adapter: &AdapterInfo,
    codec: Codec,
    query: EncodeQuery,
    cancel: &ProbeCancel,
) -> Option<EncoderSlot> {
    let key = ProbeCacheKey {
        adapter: adapter.key(),
        codec,
        query,
    };
    if let Some(profile) = inner.cache.lock().unwrap().get(key) {
        return Some(EncoderSlot::confirmed(profile));
    }
    if cancel.is_cancelled() {
        return None;
    }
    match inner.probe.probe(adapter, codec, query, cancel) {
        ProbeOutcome::Confirmed(profile) => {
            inner.cache.lock().unwrap().insert(key, profile);
            Some(EncoderSlot::confirmed(profile))
        }
        ProbeOutcome::Unavailable(reason) => {
            inner.cache.lock().unwrap().remove(key);
            Some(EncoderSlot::unavailable(reason))
        }
        ProbeOutcome::Cancelled => None,
    }
}

fn run_revalidate(inner: &Inner, selected: &ResolvedEncoder) -> RevalidateReply {
    let Some(adapter) = selected.adapter.as_ref() else {
        return RevalidateReply::Rejected(EncoderReason::DeviceDisabled(
            "у проверки нет аппаратного адаптера".into(),
        ));
    };
    let inventory = match inner.probe.inventory() {
        Ok(inventory) => inventory,
        Err(detail) => return RevalidateReply::Rejected(EncoderReason::ProbeFailed(detail)),
    };
    inner
        .cache
        .lock()
        .unwrap()
        .reconcile(inventory.generation());
    let Some(current) = inventory.find(adapter.luid) else {
        inner.cache.lock().unwrap().clear();
        return RevalidateReply::Rejected(EncoderReason::DeviceDisabled(format!(
            "видеокарта {} отключена",
            adapter.luid
        )));
    };
    if current.driver_version != adapter.driver_version {
        inner.cache.lock().unwrap().clear();
        return RevalidateReply::Rejected(EncoderReason::DeviceDisabled(
            "драйвер видеокарты изменился".into(),
        ));
    }
    let key = ProbeCacheKey {
        adapter: current.key(),
        codec: selected.codec,
        query: selected.query,
    };
    match inner.probe.probe(
        current,
        selected.codec,
        selected.query,
        &ProbeCancel::never(),
    ) {
        ProbeOutcome::Confirmed(profile) => {
            if !profile.compatible_with(selected.profile) {
                inner.cache.lock().unwrap().remove(key);
                return RevalidateReply::Rejected(EncoderReason::ProfileUnsupported(
                    "подтверждённый профиль не совпадает с выбранным SDR-профилем".into(),
                ));
            }
            inner.cache.lock().unwrap().insert(key, profile);
            RevalidateReply::Confirmed(profile)
        }
        ProbeOutcome::Unavailable(reason) => {
            inner.cache.lock().unwrap().remove(key);
            RevalidateReply::Rejected(reason)
        }
        ProbeOutcome::Cancelled => {
            RevalidateReply::Rejected(EncoderReason::ProbeFailed("проверка отменена".into()))
        }
    }
}
