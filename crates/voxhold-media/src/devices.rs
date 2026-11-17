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
    /// Stable Windows endpoint interface id for a WinMM input index. WinMM
    /// indexes are reused after a USB change, so the saved preference must use
    /// this id whenever the driver exposes it.
    pub fn endpoint_id(index: u32) -> Option<String> {
        use windows::Win32::Media::Audio::{HWAVEIN, waveInMessage};
        const QUERY_SIZE: u32 = 2061;
        const QUERY_ID: u32 = 2060;
        let handle = HWAVEIN(index as usize as *mut core::ffi::c_void);
        let mut size = 0_u32;
        unsafe {
            if waveInMessage(
                Some(handle),
                QUERY_SIZE,
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
                QUERY_ID,
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

#[cfg(not(windows))]
pub(crate) fn endpoint_id(_index: u32) -> Option<String> {
    None
}
