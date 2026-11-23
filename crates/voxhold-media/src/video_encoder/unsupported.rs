//! Placeholder probe for non-Windows builds. The model is still usable so the
//! CPU path and the picker tests compile on every CI host; hardware is simply
//! never offered.

use super::{
    AdapterInfo, AdapterInventory, Codec, EncodeQuery, EncoderReason, ProbeCancel, ProbeOutcome,
    probe::HardwareProbe,
};

pub struct UnsupportedHardwareProbe;

impl HardwareProbe for UnsupportedHardwareProbe {
    fn inventory(&self) -> Result<AdapterInventory, String> {
        Ok(AdapterInventory::new(Vec::new(), None))
    }

    fn probe(
        &self,
        _adapter: &AdapterInfo,
        _codec: Codec,
        _query: EncodeQuery,
        _cancel: &ProbeCancel,
    ) -> ProbeOutcome {
        ProbeOutcome::Unavailable(EncoderReason::DeviceDisabled(
            "аппаратное кодирование доступно только в Windows".into(),
        ))
    }
}
