//! Deterministic fake adapters for G01–G04. No DXGI, no MFT, no hardware.

use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use super::{
    AdapterInfo, AdapterInventory, AdapterLuid, Codec, CpuEncoderSource, CpuEncoders, EncodeQuery,
    EncoderReason, ProbeCancel, ProbeOutcome, codec_profile, probe::HardwareProbe,
};

/// A one-shot delay. A cooperative gate returns as soon as the probe is
/// cancelled; an uncooperative one models an in-flight driver call that only
/// returns later.
pub(crate) struct Gate {
    cooperative: bool,
    started: Mutex<bool>,
    started_cv: Condvar,
    released: Mutex<bool>,
    released_cv: Condvar,
}

impl Gate {
    pub(crate) fn new(cooperative: bool) -> Arc<Self> {
        Arc::new(Self {
            cooperative,
            started: Mutex::new(false),
            started_cv: Condvar::new(),
            released: Mutex::new(false),
            released_cv: Condvar::new(),
        })
    }

    pub(crate) fn wait_started(&self) {
        let mut started = self.started.lock().unwrap();
        while !*started {
            started = self
                .started_cv
                .wait_timeout(started, Duration::from_millis(5))
                .unwrap()
                .0;
        }
    }

    pub(crate) fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.released_cv.notify_all();
    }

    fn mark_started(&self) {
        *self.started.lock().unwrap() = true;
        self.started_cv.notify_all();
    }

    /// `true` means the probe may continue; `false` means it observed cancel.
    fn wait(&self, cancel: &ProbeCancel) -> bool {
        let mut released = self.released.lock().unwrap();
        while !*released {
            if self.cooperative && cancel.is_cancelled() {
                return false;
            }
            let (guard, _) = self
                .released_cv
                .wait_timeout(released, Duration::from_millis(2))
                .unwrap();
            released = guard;
        }
        true
    }
}

#[derive(Clone)]
pub(crate) struct FakeAdapter {
    pub info: AdapterInfo,
    pub primary: bool,
    pub codecs: Vec<Codec>,
    pub decode_only: Vec<Codec>,
    pub only_query: Option<EncodeQuery>,
    pub failure: Option<EncoderReason>,
    pub delay: Option<Arc<Gate>>,
    /// A gate that only blocks one codec, so a probe of the last codec of the
    /// row can be held back without holding the earlier ones.
    pub codec_delay: Option<(Codec, Arc<Gate>)>,
}

impl FakeAdapter {
    pub(crate) fn new(luid: u64, name: &str, codecs: &[Codec]) -> Self {
        Self {
            info: AdapterInfo {
                luid: AdapterLuid::new(luid as u32, (luid >> 32) as u32),
                name: name.into(),
                vendor_id: 0x10de,
                device_id: 0x2204,
                driver_version: 1,
                dedicated_video_memory: 8 << 30,
                software: false,
            },
            primary: false,
            codecs: codecs.to_vec(),
            decode_only: Vec::new(),
            only_query: None,
            failure: None,
            delay: None,
            codec_delay: None,
        }
    }

    pub(crate) fn primary(mut self) -> Self {
        self.primary = true;
        self
    }

    pub(crate) fn decode_only(mut self, codecs: &[Codec]) -> Self {
        self.decode_only = codecs.to_vec();
        self
    }

    pub(crate) fn only_query(mut self, query: EncodeQuery) -> Self {
        self.only_query = Some(query);
        self
    }

    pub(crate) fn failure(mut self, reason: EncoderReason) -> Self {
        self.failure = Some(reason);
        self
    }

    pub(crate) fn delay(mut self, gate: Arc<Gate>) -> Self {
        self.delay = Some(gate);
        self
    }

    pub(crate) fn delay_codec(mut self, codec: Codec, gate: Arc<Gate>) -> Self {
        self.codec_delay = Some((codec, gate));
        self
    }

    pub(crate) fn software(mut self) -> Self {
        self.info.software = true;
        self
    }

    pub(crate) fn driver(mut self, driver_version: u64) -> Self {
        self.info.driver_version = driver_version;
        self
    }
}

pub(crate) struct FakeProbe {
    adapters: Mutex<Vec<FakeAdapter>>,
    inventory_error: Mutex<Option<String>>,
    probes: AtomicUsize,
    activations: AtomicUsize,
    aborted: AtomicUsize,
    active: AtomicUsize,
    max_active: AtomicUsize,
}

impl FakeProbe {
    pub(crate) fn new(adapters: Vec<FakeAdapter>) -> Arc<Self> {
        Arc::new(Self {
            adapters: Mutex::new(adapters),
            inventory_error: Mutex::new(None),
            probes: AtomicUsize::new(0),
            activations: AtomicUsize::new(0),
            aborted: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        })
    }

    pub(crate) fn set_adapters(&self, adapters: Vec<FakeAdapter>) {
        *self.adapters.lock().unwrap() = adapters;
    }

    pub(crate) fn set_inventory_error(&self, error: Option<&str>) {
        *self.inventory_error.lock().unwrap() = error.map(str::to_owned);
    }

    pub(crate) fn remove(&self, luid: AdapterLuid) {
        self.adapters
            .lock()
            .unwrap()
            .retain(|adapter| adapter.info.luid != luid);
    }

    pub(crate) fn set_failure(&self, luid: AdapterLuid, failure: Option<EncoderReason>) {
        if let Some(adapter) = self
            .adapters
            .lock()
            .unwrap()
            .iter_mut()
            .find(|adapter| adapter.info.luid == luid)
        {
            adapter.failure = failure;
        }
    }

    pub(crate) fn set_delay(&self, luid: AdapterLuid, delay: Option<Arc<Gate>>) {
        if let Some(adapter) = self
            .adapters
            .lock()
            .unwrap()
            .iter_mut()
            .find(|adapter| adapter.info.luid == luid)
        {
            adapter.delay = delay;
        }
    }

    pub(crate) fn probes(&self) -> usize {
        self.probes.load(Ordering::SeqCst)
    }

    pub(crate) fn activations(&self) -> usize {
        self.activations.load(Ordering::SeqCst)
    }

    pub(crate) fn aborted(&self) -> usize {
        self.aborted.load(Ordering::SeqCst)
    }

    pub(crate) fn max_active(&self) -> usize {
        self.max_active.load(Ordering::SeqCst)
    }

    fn evaluate(&self, spec: &FakeAdapter, codec: Codec, query: EncodeQuery) -> ProbeOutcome {
        if let Some(reason) = &spec.failure {
            return ProbeOutcome::Unavailable(reason.clone());
        }
        if spec.decode_only.contains(&codec) {
            return ProbeOutcome::Unavailable(EncoderReason::EncoderNotProvided(
                "драйвер предоставляет только декодер".into(),
            ));
        }
        if !spec.codecs.contains(&codec) {
            return ProbeOutcome::Unavailable(EncoderReason::EncoderNotProvided(
                "нет аппаратного MFT для этого формата".into(),
            ));
        }
        if let Some(only) = spec.only_query
            && only != query
        {
            return ProbeOutcome::Unavailable(EncoderReason::ProfileUnsupported(format!(
                "кодировщик подтверждён только для {}×{}@{}",
                only.width, only.height, only.fps
            )));
        }
        ProbeOutcome::Confirmed(codec_profile(codec, query))
    }
}

impl HardwareProbe for FakeProbe {
    fn inventory(&self) -> Result<AdapterInventory, String> {
        if let Some(error) = self.inventory_error.lock().unwrap().as_ref() {
            return Err(error.clone());
        }
        let adapters = self.adapters.lock().unwrap().clone();
        let primary = adapters
            .iter()
            .find(|adapter| adapter.primary)
            .map(|adapter| adapter.info.luid);
        Ok(AdapterInventory::new(
            adapters.into_iter().map(|adapter| adapter.info).collect(),
            primary,
        ))
    }

    fn probe(
        &self,
        adapter: &AdapterInfo,
        codec: Codec,
        query: EncodeQuery,
        cancel: &ProbeCancel,
    ) -> ProbeOutcome {
        self.probes.fetch_add(1, Ordering::SeqCst);
        self.activations.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        let spec = self
            .adapters
            .lock()
            .unwrap()
            .iter()
            .find(|candidate| candidate.info.luid == adapter.luid)
            .cloned();
        let result = match spec {
            None => ProbeOutcome::Unavailable(EncoderReason::DeviceDisabled(
                "адаптер исчез во время проверки".into(),
            )),
            Some(spec) => {
                let gate = spec
                    .codec_delay
                    .as_ref()
                    .filter(|(delayed, _)| *delayed == codec)
                    .map(|(_, gate)| gate)
                    .or(spec.delay.as_ref());
                if let Some(gate) = gate {
                    gate.mark_started();
                    if !gate.wait(cancel) {
                        self.aborted.fetch_add(1, Ordering::SeqCst);
                        ProbeOutcome::Cancelled
                    } else {
                        self.evaluate(&spec, codec, query)
                    }
                } else {
                    self.evaluate(&spec, codec, query)
                }
            }
        };
        self.active.fetch_sub(1, Ordering::SeqCst);
        result
    }
}

pub(crate) struct FakeCpu {
    catalog: CpuEncoders,
}

impl FakeCpu {
    pub(crate) fn all() -> Arc<Self> {
        Arc::new(Self {
            catalog: CpuEncoders::all_available(),
        })
    }

    pub(crate) fn without(codecs: &[Codec]) -> Arc<Self> {
        let mut catalog = CpuEncoders::all_available();
        for codec in codecs {
            catalog.remove(*codec);
        }
        Arc::new(Self { catalog })
    }
}

impl CpuEncoderSource for FakeCpu {
    fn catalog(&self) -> CpuEncoders {
        self.catalog.clone()
    }
}
