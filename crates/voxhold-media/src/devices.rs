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
                    devices.push(AudioDevice {
                        id: format!("{index}:{name}"),
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
