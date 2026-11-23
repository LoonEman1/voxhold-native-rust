//! The driver version that identifies a cached probe.
//!
//! Windows reports the same packed `major.minor.build.revision` quadruple in
//! three places: `Win32_VideoController.DriverVersion`, the display-class
//! registry entry of the device, and (for a UMD that implements the D3D10
//! interface) `IDXGIAdapter::CheckInterfaceSupport(IID_ID3D10Device)`. The probe
//! prefers the DXGI query and falls back to this module, which reads the
//! registry without COM or WMI.
//!
//! The KMT `KMTQAITYPE_DRIVERVERSION` query is not used: it answers the WDDM
//! model version (a 4-byte value such as 3200 for WDDM 3.2), which does not
//! change when a driver is updated and therefore cannot identify a driver
//! build.

use windows::{
    Win32::{
        Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS},
        System::Registry::{
            HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW,
            RegGetValueW, RegOpenKeyExW,
        },
    },
    core::{PCWSTR, PWSTR},
};

/// The driver version of the adapter with this PCI identity, packed like
/// [`crate::video_encoder::AdapterInfo::driver_version`]. Several registry
/// entries can match the same device (a stale package or the same card model
/// installed twice); the highest version wins so the result is deterministic
/// across enumerations.
pub(crate) fn registry(vendor_id: u32, device_id: u32) -> Option<u64> {
    let device = format!("PCI\\VEN_{vendor_id:04X}&DEV_{device_id:04X}");
    unsafe { display_entries() }
        .into_iter()
        .filter(|(matching, _)| matching_device_matches(matching, &device))
        .filter_map(|(_, version)| parse_dotted(&version))
        .max()
}

/// `major.minor.build.revision` in 16-bit lanes, little-endian in the number:
/// `32.0.16.1692` is `0x0020_0000_0010_069c`, the value
/// `CheckInterfaceSupport` returns for the same driver.
pub(crate) const fn pack(major: u32, minor: u32, build: u32, revision: u32) -> u64 {
    ((major as u64) << 48) | ((minor as u64) << 32) | ((build as u64) << 16) | revision as u64
}

/// A dotted driver version from the registry, or `None` when it is not one to
/// four numeric components of 16 bits each.
pub(crate) fn parse_dotted(version: &str) -> Option<u64> {
    let mut parts = [0u32; 4];
    let mut count = 0;
    for part in version.trim().split('.') {
        if count == parts.len() {
            return None;
        }
        let value: u32 = part.parse().ok()?;
        if value > u32::from(u16::MAX) {
            return None;
        }
        parts[count] = value;
        count += 1;
    }
    if count == 0 {
        return None;
    }
    Some(pack(parts[0], parts[1], parts[2], parts[3]))
}

/// `MatchingDeviceId` is `PCI\VEN_10DE&DEV_2782`, sometimes with
/// `&SUBSYS_...&REV_...` appended, and the registry stores it in lower case.
fn matching_device_matches(matching: &str, device: &str) -> bool {
    let matching = matching.trim().to_ascii_uppercase();
    let device = device.to_ascii_uppercase();
    if matching == device {
        return true;
    }
    matching
        .strip_prefix(&device)
        .is_some_and(|rest| rest.starts_with('&'))
}

/// `(MatchingDeviceId, DriverVersion)` of every display class subkey.
unsafe fn display_entries() -> Vec<(String, String)> {
    let mut entries = Vec::new();
    let mut class = HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            windows::core::w!(
                r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}"
            ),
            None,
            KEY_READ,
            &mut class,
        )
    };
    if status != ERROR_SUCCESS {
        return entries;
    }
    let mut index = 0u32;
    loop {
        let mut name = [0u16; 64];
        let mut length = name.len() as u32;
        let status = unsafe {
            RegEnumKeyExW(
                class,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut length,
                None,
                None,
                None,
                None,
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            break;
        }
        if status != ERROR_SUCCESS {
            break;
        }
        index += 1;
        let mut subkey = HKEY::default();
        let status =
            unsafe { RegOpenKeyExW(class, PCWSTR(name.as_ptr()), None, KEY_READ, &mut subkey) };
        if status != ERROR_SUCCESS {
            continue;
        }
        let matching = unsafe { read_string(subkey, windows::core::w!("MatchingDeviceId")) };
        let version = unsafe { read_string(subkey, windows::core::w!("DriverVersion")) };
        let _ = unsafe { RegCloseKey(subkey) };
        if let (Some(matching), Some(version)) = (matching, version) {
            entries.push((matching, version));
        }
    }
    let _ = unsafe { RegCloseKey(class) };
    entries
}

unsafe fn read_string(key: HKEY, name: PCWSTR) -> Option<String> {
    let mut size = 0u32;
    let status = unsafe {
        RegGetValueW(
            key,
            PCWSTR::null(),
            name,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS || !(2..=4096).contains(&size) {
        return None;
    }
    let mut buffer = vec![0u16; size as usize / 2];
    let status = unsafe {
        RegGetValueW(
            key,
            PCWSTR::null(),
            name,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let length = (size as usize / 2).min(buffer.len());
    let text = String::from_utf16_lossy(&buffer[..length]);
    let text = text.trim_end_matches('\0').trim();
    (!text.is_empty()).then(|| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_packed_form_matches_check_interface_support() {
        // The value the review measured on the RTX 4070 Ti (driver
        // 32.0.16.1692) through CheckInterfaceSupport(IID_ID3D10Device).
        assert_eq!(parse_dotted("32.0.16.1692"), Some(0x0020_0000_0010_069c));
        assert_eq!(pack(32, 0, 16, 1692), 0x0020_0000_0010_069c);
    }

    #[test]
    fn only_one_to_four_16_bit_components_are_a_version() {
        assert_eq!(parse_dotted("31.0.15.5222"), Some(0x001f_0000_000f_1466));
        assert_eq!(parse_dotted(" 1.2 "), Some(0x0001_0002_0000_0000));
        assert_eq!(parse_dotted("1"), Some(0x0001_0000_0000_0000));
        for rejected in ["", "  ", "1.2.3.4.5", "a.b.c.d", "1..2", "1.70000.0.0"] {
            assert_eq!(parse_dotted(rejected), None, "{rejected:?}");
        }
    }

    #[test]
    fn the_device_id_matches_with_and_without_a_subsystem() {
        let device = "PCI\\VEN_10DE&DEV_2782";
        assert!(matching_device_matches("pci\\ven_10de&dev_2782", device));
        assert!(matching_device_matches(
            "PCI\\VEN_10DE&DEV_2782&SUBSYS_88881043&REV_A1",
            device
        ));
        assert!(matching_device_matches(
            "  PCI\\VEN_10DE&DEV_2782  ",
            device
        ));
        assert!(!matching_device_matches("PCI\\VEN_10DE&DEV_27820", device));
        assert!(!matching_device_matches("PCI\\VEN_1002&DEV_2782", device));
        assert!(!matching_device_matches("", device));
    }
}
