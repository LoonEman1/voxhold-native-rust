//! Numeric playout level for the voice session.
//!
//! WebRTC's platform ADM plays the decoded remote audio itself and the
//! `libwebrtc` wrapper exposes no per-track volume. Windows does expose a
//! per-process audio session, so the configured room gain is applied to this
//! process's render session instead of being collapsed into a boolean. Local
//! mutes stay per-track in the voice engine; this stage is only the shared
//! room coefficient (0–200 %).

/// Map a 0–200 % room gain to the endpoint's 0–1 amplitude range. Mapping the
/// full range (rather than clamping at 100 %) is what makes 1, 50, 100 and
/// 200 % distinct at the playback endpoint.
pub fn level_for_gain(gain: f64) -> f32 {
    (gain.clamp(0.0, 2.0) / 2.0) as f32
}

#[cfg(windows)]
pub fn set_process_gain(gain: f64) -> bool {
    use windows::Win32::Media::Audio::{
        IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume,
        MMDeviceEnumerator, eConsole, eRender,
    };
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
    };
    use windows::core::Interface;

    // 200 % is the maximum the session endpoint accepts; 100 % is half of the
    // endpoint range so 200 % is actually louder than 100 %.
    let level = level_for_gain(gain);
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let result = (|| -> windows::core::Result<()> {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
            let sessions = manager.GetSessionEnumerator()?;
            for index in 0..sessions.GetCount()? {
                let control = sessions.GetSession(index)?;
                let Ok(control) = control.cast::<IAudioSessionControl2>() else {
                    continue;
                };
                if control.GetProcessId().ok() != Some(std::process::id()) {
                    continue;
                }
                let Ok(volume) = control.cast::<ISimpleAudioVolume>() else {
                    continue;
                };
                volume.SetMasterVolume(level, std::ptr::null())?;
                return Ok(());
            }
            Err(windows::core::Error::from(
                windows::Win32::Foundation::E_FAIL,
            ))
        })();
        if initialized {
            CoUninitialize();
        }
        result.is_ok()
    }
}

#[cfg(not(windows))]
pub fn set_process_gain(_gain: f64) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_to_two_hundred_percent_maps_to_distinct_levels() {
        let levels: Vec<f32> = [0.0, 0.01, 0.5, 1.0, 2.0]
            .into_iter()
            .map(level_for_gain)
            .collect();
        assert_eq!(levels[0], 0.0);
        assert_eq!(levels[4], 1.0);
        assert!(levels[1] < levels[2]);
        assert!(levels[2] < levels[3]);
        assert!(levels[3] < levels[4]);
        assert_eq!(level_for_gain(9.0), 1.0, "above 200 % stays bounded");
        assert_eq!(level_for_gain(-1.0), 0.0);
    }
}
