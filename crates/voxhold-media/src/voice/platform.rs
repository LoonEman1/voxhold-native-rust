//! Platform (WebRTC ADM) audio endpoints. Playout always uses this path; the
//! recording side is only the fallback when the software WinMM capture cannot
//! open the selected device.

use super::*;

pub(super) struct PlatformAudio(pub(super) PeerConnectionFactory);

/// Core Audio names may be longer than the WinMM label, so either side being
/// a prefix counts. Ambiguity returns `None` instead of opening another mic.
pub(super) fn unique_name_match(
    saved: &str,
    names: impl Iterator<Item = (u16, String)>,
) -> Option<u16> {
    let matches: Vec<u16> = names
        .filter(|(_, name)| name.starts_with(saved) || saved.starts_with(name))
        .map(|(index, _)| index)
        .collect();
    match matches.as_slice() {
        [index] => Some(*index),
        _ => None,
    }
}

impl PlatformAudio {
    pub(super) fn new(recording: bool) -> Result<Self, &'static str> {
        let factory = PeerConnectionFactory::default();
        if !factory.acquire_platform_adm() {
            return Err("Не удалось открыть аудиосистему Windows.");
        }
        factory.set_adm_recording_enabled(recording);
        factory.set_adm_playout_enabled(true);
        Ok(Self(factory))
    }

    /// Resolve a persisted device id to a current Core Audio index. The legacy
    /// WinMM UI stores `index:name`, so a unique name prefix is accepted; a GUID
    /// from the engine is matched exactly. An empty id means the Windows
    /// default endpoint, resolved to its current GUID rather than assuming
    /// index 0 after a device-order change.
    pub(super) fn resolve(&self, id: &str, input: bool) -> Option<u16> {
        let factory = &self.0;
        let count = if input {
            factory.recording_devices()
        } else {
            factory.playout_devices()
        };
        let guid = |index| {
            if input {
                factory.recording_device_guid(index)
            } else {
                factory.playout_device_guid(index)
            }
        };
        let name = |index| {
            if input {
                factory.recording_device_name(index)
            } else {
                factory.playout_device_name(index)
            }
        };
        if id.is_empty() {
            let default = default_endpoint_guid(input);
            return default
                .and_then(|default| (0..count.max(0) as u16).find(|&index| guid(index) == default))
                .or_else(|| (count > 0).then_some(0));
        }
        // New settings store the Core Audio endpoint id from
        // `DRV_QUERYFUNCTIONINSTANCEID`; match it exactly before falling back
        // to a display name, which cannot distinguish duplicate microphones.
        if let Some(index) = (0..count.max(0) as u16).find(|&index| guid(index) == id) {
            return Some(index);
        }
        if input {
            // Settings written by older builds hold the WinMM PnP interface
            // id. It is not the ADM id, so migrate it by its stable device name
            // and accept only an unambiguous match.
            if let Some(saved) = crate::devices::endpoint_name(id) {
                let matched = unique_name_match(
                    &saved,
                    (0..count.max(0) as u16).map(|index| (index, name(index))),
                );
                if let Some(index) = matched {
                    return Some(index);
                }
            }
        }
        if let Some((_, old_name)) = id
            .split_once(':')
            .filter(|(index, _)| index.parse::<u32>().is_ok())
        {
            let mut matches =
                (0..count.max(0) as u16).filter(|&index| name(index).starts_with(old_name));
            let first = matches.next();
            first.filter(|_| matches.next().is_none())
        } else {
            None
        }
    }

    pub(super) fn running(&self, input: bool) -> bool {
        if input {
            self.0.recording_is_initialized()
        } else {
            self.0.playout_is_initialized()
        }
    }

    pub(super) fn stop(&self, input: bool) -> bool {
        if input {
            self.0.stop_recording()
        } else {
            self.0.stop_playout()
        }
    }

    pub(super) fn start(&self, input: bool) -> bool {
        if input {
            self.0.init_recording() && self.0.start_recording()
        } else {
            self.0.init_playout() && self.0.start_playout()
        }
    }

    /// Point the ADM at one index and restore the requested running state.
    /// The previous device is only released after the new station is set; a
    /// failed set or start leaves the caller able to roll back explicitly.
    pub(super) fn apply(&self, index: u16, input: bool, running: bool) -> bool {
        let factory = &self.0;
        let was_running = self.running(input);
        if was_running && !self.stop(input) {
            return false;
        }
        let selected = if input {
            factory.set_recording_device(index)
        } else {
            factory.set_playout_device(index)
        };
        if !selected {
            return false;
        }
        !running || self.start(input)
    }

    /// Initial join: choose a device and start the path only if the platform
    /// path is expected to run. Used by `Session::new`.
    pub(super) fn activate(&self, index: u16, input: bool) -> bool {
        let running = self.running(input);
        self.apply(index, input, running)
    }

    /// The platform capture path is only used when the software capture cannot
    /// open the device; enabling it after construction avoids opening the
    /// microphone twice when our own capture works.
    pub(super) fn set_recording(&self, enabled: bool) {
        self.0.set_adm_recording_enabled(enabled);
    }
}

impl Drop for PlatformAudio {
    fn drop(&mut self) {
        self.0.release_platform_adm();
    }
}

#[cfg(windows)]
pub(super) fn default_endpoint_guid(input: bool) -> Option<String> {
    use windows::Win32::Media::Audio::{
        IMMDeviceEnumerator, MMDeviceEnumerator, eCapture, eConsole, eRender,
    };
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
        CoUninitialize,
    };
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let result = (|| -> windows::core::Result<String> {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let flow = if input { eCapture } else { eRender };
            let device = enumerator.GetDefaultAudioEndpoint(flow, eConsole)?;
            let id = device.GetId()?;
            let value = id.to_string().unwrap_or_default();
            CoTaskMemFree(Some(id.0 as *const core::ffi::c_void));
            Ok(value)
        })();
        if initialized {
            CoUninitialize();
        }
        result.ok().filter(|value| !value.is_empty())
    }
}

#[cfg(not(windows))]
pub(super) fn default_endpoint_guid(_input: bool) -> Option<String> {
    None
}
