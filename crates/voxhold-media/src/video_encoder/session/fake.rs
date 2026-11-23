//! Deterministic encoder double of the GPU path.
//!
//! It reproduces exactly the contract the engine relies on: a bounded input
//! queue with a drop before encoding, keyframe recovery, rate-control
//! accounting, a bounded output queue and an injectable terminal failure. The
//! encoded payload is synthetic; nothing here claims a driver ran.

use std::collections::VecDeque;

use super::{
    AdapterInfo, EncodeSubmit, EncodedAccessUnit, EncoderCounters, GpuEncoder, GpuEncoderFailure,
    Nv12Frame,
};

pub(crate) struct FakeGpuEncoder {
    input: VecDeque<Nv12Frame>,
    input_capacity: usize,
    output: VecDeque<EncodedAccessUnit>,
    output_capacity: usize,
    counters: EncoderCounters,
    adapter: Option<AdapterInfo>,
    force_key: bool,
    failure: Option<GpuEncoderFailure>,
    /// Report this failure after the next submit or poll.
    inject: Option<GpuEncoderFailure>,
    /// Fail after this many accepted submissions (a device removed mid-stream).
    fail_after: Option<u32>,
    stopped: bool,
}

impl FakeGpuEncoder {
    pub(crate) fn new(input_capacity: usize) -> Self {
        Self {
            input: VecDeque::new(),
            input_capacity: input_capacity.max(1),
            output: VecDeque::new(),
            output_capacity: 8,
            counters: EncoderCounters {
                bitrate_bps: 2_500_000,
                ..EncoderCounters::default()
            },
            adapter: None,
            force_key: false,
            failure: None,
            inject: None,
            fail_after: None,
            stopped: false,
        }
    }

    pub(crate) fn failing(message: &str, device_lost: bool) -> Self {
        let mut encoder = Self::new(2);
        encoder.inject = Some(GpuEncoderFailure {
            message: message.to_owned(),
            device_lost,
        });
        encoder
    }

    pub(crate) fn on(mut self, adapter: AdapterInfo) -> Self {
        self.adapter = Some(adapter);
        self
    }

    /// The device disappears after `frames` accepted frames.
    pub(crate) fn fail_after(mut self, frames: u32) -> Self {
        self.fail_after = Some(frames);
        self
    }

    /// Encode one queued frame, exactly like a worker iteration would.
    pub(crate) fn pump(&mut self) {
        let Some(frame) = self.input.pop_front() else {
            return;
        };
        let keyframe = self.force_key || self.counters.encoded == 0;
        self.force_key = false;
        self.counters.encoded += 1;
        if keyframe {
            self.counters.keyframes += 1;
        }
        let packet = EncodedAccessUnit {
            payload: frame.timestamp_us.to_le_bytes().to_vec(),
            keyframe,
            timestamp_us: frame.timestamp_us,
        };
        if self.output.len() >= self.output_capacity {
            self.counters.dropped_output += 1;
            self.force_key = true;
            return;
        }
        self.output.push_back(packet);
    }
}

impl GpuEncoder for FakeGpuEncoder {
    fn submit(&mut self, frame: Nv12Frame) -> EncodeSubmit {
        if self.stopped {
            return EncodeSubmit::Closed;
        }
        if let Some(failure) = self.inject.take() {
            self.failure = Some(failure);
            return EncodeSubmit::Closed;
        }
        if let Some(remaining) = self.fail_after {
            if remaining == 0 {
                self.failure = Some(GpuEncoderFailure {
                    message: "Аппаратный кодировщик остановился: устройство отключено".into(),
                    device_lost: true,
                });
                self.fail_after = None;
                return EncodeSubmit::Closed;
            }
            self.fail_after = Some(remaining - 1);
        }
        self.counters.submitted += 1;
        if self.input.len() >= self.input_capacity {
            self.counters.dropped_input += 1;
            return EncodeSubmit::Dropped;
        }
        self.input.push_back(frame);
        EncodeSubmit::Queued
    }

    fn next_packet(&mut self) -> Option<EncodedAccessUnit> {
        self.pump();
        self.output.pop_front()
    }

    fn request_keyframe(&mut self) {
        self.counters.keyframe_requests += 1;
        self.force_key = true;
    }

    fn set_bitrate(&mut self, target_bps: u32) {
        self.counters.bitrate_bps = target_bps;
        self.counters.bitrate_applied = true;
    }

    fn counters(&self) -> EncoderCounters {
        self.counters
    }

    fn adapter(&self) -> Option<AdapterInfo> {
        self.adapter.clone()
    }

    fn failure(&self) -> Option<GpuEncoderFailure> {
        self.failure.clone()
    }

    fn shutdown(&mut self) {
        self.stopped = true;
        self.input.clear();
        self.output.clear();
    }
}
