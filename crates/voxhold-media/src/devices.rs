//! Windows WinMM device labels, resolved by the engine to Core Audio GUIDs.
#[derive(Clone, Debug)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub input: bool,
}
#[cfg(windows)]
mod windows {
    use super::AudioDevice;
    use std::mem::{size_of, zeroed};
    #[repr(C)]
    struct WaveInCaps {
        manufacturer: u16,
        product: u16,
        version: u32,
        name: [u16; 32],
        formats: u32,
        channels: u16,
        reserved: u16,
    }
    #[repr(C)]
    struct WaveOutCaps {
        manufacturer: u16,
        product: u16,
        version: u32,
        name: [u16; 32],
        formats: u32,
        channels: u16,
        reserved: u16,
        support: u32,
    }
    #[link(name = "winmm")]
    unsafe extern "system" {
        fn waveInGetNumDevs() -> u32;
        fn waveOutGetNumDevs() -> u32;
        fn waveInGetDevCapsW(id: usize, caps: *mut WaveInCaps, size: u32) -> u32;
        fn waveOutGetDevCapsW(id: usize, caps: *mut WaveOutCaps, size: u32) -> u32;
    }
    /// Query one WinMM device id string. The size and id messages come in
    /// pairs; `DRV_QUERYFUNCTIONINSTANCEID` returns the Core Audio endpoint id,
    /// while the older `DRV_QUERYDEVICEINTERFACE` returns a PnP interface path.
    fn query_id(index: u32, size_message: u32, id_message: u32) -> Option<String> {
        use windows::Win32::Media::Audio::{HWAVEIN, waveInMessage};
        let handle = HWAVEIN(index as usize as *mut core::ffi::c_void);
        let mut size = 0_u32;
        unsafe {
            if waveInMessage(
                Some(handle),
                size_message,
                Some(&mut size as *mut u32 as usize),
                None,
            ) != 0
                || size == 0
            {
                return None;
            }
        }
        let mut buffer = vec![0_u16; (size as usize).div_ceil(2)];
        unsafe {
            if waveInMessage(
                Some(handle),
                id_message,
                Some(buffer.as_mut_ptr() as usize),
                Some(size as usize),
            ) != 0
            {
                return None;
            }
        }
        let end = buffer
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(buffer.len());
        let id = String::from_utf16_lossy(&buffer[..end]);
        (!id.is_empty()).then_some(id)
    }

    /// Stable Core Audio endpoint id for a WinMM input index. This is the
    /// value the ADM exposes as `recording_device_guid`. WinMM indexes are
    /// reused after a USB change, so the saved preference must use this id
    /// whenever the driver exposes it.
    pub fn endpoint_id(index: u32) -> Option<String> {
        use windows::Win32::Media::Multimedia::{
            DRV_QUERYFUNCTIONINSTANCEID, DRV_QUERYFUNCTIONINSTANCEIDSIZE,
        };
        query_id(
            index,
            DRV_QUERYFUNCTIONINSTANCEIDSIZE,
            DRV_QUERYFUNCTIONINSTANCEID,
        )
    }

    /// Legacy id written by builds that queried `DRV_QUERYDEVICEINTERFACE`.
    /// It is kept only to migrate already saved preferences; new choices use
    /// `endpoint_id`.
    pub fn legacy_interface_id(index: u32) -> Option<String> {
        use windows::Win32::Media::Multimedia::{
            DRV_QUERYDEVICEINTERFACE, DRV_QUERYDEVICEINTERFACESIZE,
        };
        query_id(
            index,
            DRV_QUERYDEVICEINTERFACESIZE,
            DRV_QUERYDEVICEINTERFACE,
        )
    }

    /// Rewrite a saved input id to the current Core Audio endpoint id. New ids
    /// pass through unchanged; a legacy PnP interface id is migrated only when
    /// its display name identifies one current microphone.
    pub fn canonical_input_id(id: &str) -> Option<String> {
        if id.is_empty() {
            return None;
        }
        let devices = list();
        if let Some(device) = devices
            .iter()
            .find(|device| device.input && device.id == id)
        {
            return Some(device.id.clone());
        }
        let name = endpoint_name(id)?;
        let mut matches = devices
            .into_iter()
            .filter(|device| device.input && device.name == name);
        let first = matches.next()?;
        matches.next().is_none().then_some(first.id)
    }

    /// WinMM display name for a saved endpoint id, including a legacy PnP
    /// interface id from settings written before the switch to the endpoint
    /// id.
    pub fn endpoint_name(id: &str) -> Option<String> {
        for index in 0..unsafe { waveInGetNumDevs() } {
            if endpoint_id(index).as_deref() == Some(id)
                || legacy_interface_id(index).as_deref() == Some(id)
            {
                let mut caps: WaveInCaps = unsafe { zeroed() };
                if unsafe {
                    waveInGetDevCapsW(index as usize, &mut caps, size_of::<WaveInCaps>() as u32)
                } == 0
                {
                    return Some(label(&caps.name));
                }
            }
        }
        None
    }
    fn label(name: &[u16]) -> String {
        String::from_utf16_lossy(&name[..name.iter().position(|c| *c == 0).unwrap_or(name.len())])
    }
    pub fn list() -> Vec<AudioDevice> {
        let mut devices = Vec::new();
        unsafe {
            for index in 0..waveInGetNumDevs() {
                let mut caps: WaveInCaps = zeroed();
                if waveInGetDevCapsW(index as usize, &mut caps, size_of::<WaveInCaps>() as u32) == 0
                {
                    let name = label(&caps.name);
                    let id = endpoint_id(index).unwrap_or_else(|| format!("{index}:{name}"));
                    devices.push(AudioDevice {
                        id,
                        name,
                        input: true,
                    });
                }
            }
            for index in 0..waveOutGetNumDevs() {
                let mut caps: WaveOutCaps = zeroed();
                if waveOutGetDevCapsW(index as usize, &mut caps, size_of::<WaveOutCaps>() as u32)
                    == 0
                {
                    let name = label(&caps.name);
                    devices.push(AudioDevice {
                        id: format!("{index}:{name}"),
                        name,
                        input: false,
                    });
                }
            }
        }
        devices
    }
}
pub fn list_devices() -> Vec<AudioDevice> {
    #[cfg(windows)]
    {
        windows::list()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Stable endpoint interface id for a WinMM input index. The software capture
/// path uses it to distinguish microphones with identical display names.
#[cfg(windows)]
pub(crate) fn endpoint_id(index: u32) -> Option<String> {
    windows::endpoint_id(index)
}

#[cfg(windows)]
pub(crate) fn legacy_interface_id(index: u32) -> Option<String> {
    windows::legacy_interface_id(index)
}

#[cfg(windows)]
pub(crate) fn endpoint_name(id: &str) -> Option<String> {
    windows::endpoint_name(id)
}

/// Public migration helper for the settings tab: turn an old WinMM interface
/// id into the Core Audio endpoint id that new preferences store.
pub fn canonical_input_id(id: &str) -> Option<String> {
    #[cfg(windows)]
    {
        windows::canonical_input_id(id)
    }
    #[cfg(not(windows))]
    {
        let _ = id;
        None
    }
}

#[cfg(not(windows))]
pub(crate) fn endpoint_id(_index: u32) -> Option<String> {
    None
}

#[cfg(not(windows))]
pub(crate) fn legacy_interface_id(_index: u32) -> Option<String> {
    None
}

#[cfg(not(windows))]
pub(crate) fn endpoint_name(_id: &str) -> Option<String> {
    None
}
