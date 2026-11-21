//! Stable monitor identity for screen capture.
//!
//! The desktop capturer numbers its monitor sources per enumeration. On the
//! pinned `libwebrtc 0.4.0` a Windows screen source is `(device_index, "")`:
//! the capturer id is exactly the index of
//! `EnumDisplayDevicesW(NULL, index, ...)` and the title is always empty. That
//! number is only a lookup key for one list, never the display itself: after a
//! reordering, unplug or resolution change the same number can name another
//! monitor.
//!
//! Selection, preview, preparation and the real capture therefore bind to the
//! Windows display identity (`\\.\DISPLAYn`) plus its geometry, and resolve the
//! current capturer id through the device name that the same index reports.
//!
//! Everything here is pure so the reordering rules are tested without a
//! desktop. The Windows enumeration in [`super::windows`] is a thin wrapper.

/// A rectangle in the Windows virtual-desktop coordinate space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MonitorRect {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

impl MonitorRect {
    pub fn new(left: i32, top: i32, width: u32, height: u32) -> Self {
        Self {
            left,
            top,
            width,
            height,
        }
    }
}

/// Stable identity of one display surface.
///
/// The device name is the only durable part Windows reports. The geometry is
/// part of the identity so a monitor that was moved, resized or had its work
/// area changed requires a fresh confirmation instead of silently matching a
/// neighbour.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MonitorIdentity {
    pub device: String,
    pub bounds: MonitorRect,
    pub work: MonitorRect,
    /// `0` when the enumeration did not report a size.
    pub width: u32,
    pub height: u32,
}

impl MonitorIdentity {
    /// Size as the picker shows it; `0` when unknown.
    pub fn size(&self) -> (u32, u32) {
        if self.width > 0 && self.height > 0 {
            (self.width, self.height)
        } else {
            (self.bounds.width, self.bounds.height)
        }
    }

    /// Deterministic key for a UI element of this display. It is stable across
    /// a capturer reorder and changes with the confirmed geometry.
    pub fn focus_key(&self) -> u64 {
        fn feed(hash: &mut u64, bytes: &[u8]) {
            for byte in bytes {
                *hash ^= u64::from(*byte);
                *hash = hash.wrapping_mul(0x100_0000_01b3);
            }
        }
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        feed(&mut hash, self.device.as_bytes());
        for value in [
            self.bounds.left,
            self.bounds.top,
            self.work.left,
            self.work.top,
        ] {
            feed(&mut hash, &value.to_le_bytes());
        }
        for value in [
            self.bounds.width,
            self.bounds.height,
            self.work.width,
            self.work.height,
            self.width,
            self.height,
        ] {
            feed(&mut hash, &value.to_le_bytes());
        }
        hash
    }
}

/// One monitor source of the desktop capturer, translated from its numeric id
/// to the Windows device name that id names in this enumeration.
///
/// The device is empty when the id has no display device behind it (a
/// synthetic DirectX row beyond `EnumDisplayDevicesW`); such a row stays
/// identity-less and can neither be selected nor captured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CapturerMonitor {
    pub id: u64,
    pub device: String,
}

/// Why the selected surface cannot be resolved in the current enumeration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResolveError {
    /// The display or its capturer row is gone.
    Missing,
    /// More than one row or monitor matches the confirmed identity.
    Ambiguous,
    /// The device still exists but its geometry changed after the selection
    /// was confirmed.
    TopologyChanged,
}

impl ResolveError {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Missing => {
                "Выбранный экран больше недоступен. Обновите список и выберите экран снова."
            }
            Self::Ambiguous => {
                "Не удалось однозначно определить выбранный экран. Обновите список и выберите экран снова."
            }
            Self::TopologyChanged => "Конфигурация экранов изменилась. Выберите экран заново.",
        }
    }
}

/// Whether two device names name the same Windows display.
///
/// Device names are ASCII (`\\.\DISPLAYn`) and the comparison is case
/// insensitive; an absent or blank name never matches, so an identity-less
/// capturer row cannot claim a display.
fn same_device(left: &str, right: &str) -> bool {
    let left = left.trim();
    let right = right.trim();
    !left.is_empty() && left.eq_ignore_ascii_case(right)
}

/// Whether two identities describe the same confirmed surface. The device is
/// compared separately and case insensitively.
fn same_geometry(left: &MonitorIdentity, right: &MonitorIdentity) -> bool {
    left.bounds == right.bounds
        && left.work == right.work
        && left.width == right.width
        && left.height == right.height
}

/// The single monitor whose device is `device`; `None` when absent or doubled.
fn monitor_for_device<'a>(
    device: &str,
    monitors: &'a [MonitorIdentity],
) -> Option<&'a MonitorIdentity> {
    if device.trim().is_empty() {
        return None;
    }
    let mut matched = monitors
        .iter()
        .filter(|monitor| same_device(&monitor.device, device));
    let first = matched.next()?;
    matched.next().is_none().then_some(first)
}

/// A confirmed capturer row: the current numeric id and the Windows display it
/// names. Only unique matches are returned.
pub(crate) fn confirmed_mapping(
    monitors: &[MonitorIdentity],
    sources: &[CapturerMonitor],
) -> Vec<(u64, MonitorIdentity)> {
    let mut matched: Vec<(u64, MonitorIdentity)> = Vec::new();
    for source in sources {
        let Some(monitor) = monitor_for_device(&source.device, monitors) else {
            continue;
        };
        matched.push((source.id, monitor.clone()));
    }
    matched
        .iter()
        .filter(|(_, monitor)| {
            // A display with two capturer rows is ambiguous in both
            // directions: neither row may claim it.
            let rows = matched
                .iter()
                .filter(|(_, candidate)| same_device(&candidate.device, &monitor.device))
                .count();
            rows == 1
        })
        .cloned()
        .collect()
}

/// Resolve the current capturer id of `identity` from one snapshot.
///
/// This is the only bridge between a confirmed choice and the capturer's
/// numeric id. It never falls back to the old number: a missing, duplicated or
/// re-geometried display is a typed refusal.
pub(crate) fn resolve_monitor(
    identity: &MonitorIdentity,
    monitors: &[MonitorIdentity],
    sources: &[CapturerMonitor],
) -> Result<u64, ResolveError> {
    if identity.device.trim().is_empty() {
        return Err(ResolveError::Missing);
    }
    let on_device: Vec<&MonitorIdentity> = monitors
        .iter()
        .filter(|monitor| same_device(&monitor.device, &identity.device))
        .collect();
    match on_device.len() {
        0 => return Err(ResolveError::Missing),
        1 => {}
        _ => return Err(ResolveError::Ambiguous),
    }
    if !same_geometry(on_device[0], identity) {
        return Err(ResolveError::TopologyChanged);
    }
    let mut rows = sources
        .iter()
        .filter(|source| same_device(&source.device, &identity.device));
    let Some(row) = rows.next() else {
        return Err(ResolveError::Missing);
    };
    if rows.next().is_some() {
        return Err(ResolveError::Ambiguous);
    }
    Ok(row.id)
}

/// Confirm that a capture prepared from `selected` still describes the same
/// display when its first frame arrives.
///
/// Only the selected surface is checked: its device must still exist exactly
/// once and keep the same geometry. Another monitor being plugged in, a
/// taskbar/work-area change on a different display or a DPI change elsewhere
/// does not invalidate the prepared picture.
pub(crate) fn confirm_prepared(
    selected: Option<&MonitorIdentity>,
    current: &[MonitorIdentity],
) -> Result<(), ResolveError> {
    let Some(identity) = selected else {
        return Ok(());
    };
    if identity.device.trim().is_empty() {
        return Err(ResolveError::Missing);
    }
    let on_device: Vec<&MonitorIdentity> = current
        .iter()
        .filter(|monitor| same_device(&monitor.device, &identity.device))
        .collect();
    match on_device.len() {
        0 => Err(ResolveError::Missing),
        1 if same_geometry(on_device[0], identity) => Ok(()),
        1 => Err(ResolveError::TopologyChanged),
        _ => Err(ResolveError::Ambiguous),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, width: u32, height: u32) -> MonitorRect {
        MonitorRect::new(left, top, width, height)
    }

    fn display(device: &str, left: i32, width: u32, height: u32) -> MonitorIdentity {
        MonitorIdentity {
            device: format!("\\\\.\\{device}"),
            bounds: rect(left, 0, width, height),
            work: rect(left, 0, width, height),
            width,
            height,
        }
    }

    fn capturer(id: u64, device: &str) -> CapturerMonitor {
        CapturerMonitor {
            id,
            device: if device.is_empty() {
                String::new()
            } else {
                format!("\\\\.\\{device}")
            },
        }
    }

    #[test]
    fn a_capturer_id_resolves_through_the_display_device_at_that_index() {
        let a = display("DISPLAY1", 0, 1920, 1080);
        let b = display("DISPLAY2", 1920, 2560, 1440);
        let monitors = [a.clone(), b.clone()];
        let sources = [capturer(0, "DISPLAY1"), capturer(1, "DISPLAY2")];

        assert_eq!(resolve_monitor(&a, &monitors, &sources), Ok(0));
        assert_eq!(resolve_monitor(&b, &monitors, &sources), Ok(1));
        assert_eq!(confirmed_mapping(&monitors, &sources), vec![(0, a), (1, b)]);
    }

    #[test]
    fn an_ordinary_reorder_resolves_the_same_display_to_its_new_id() {
        let a = display("DISPLAY1", 0, 1920, 1080);
        let b = display("DISPLAY2", 1920, 2560, 1440);
        let monitors = [a.clone(), b.clone()];

        // The capturer's numeric ids follow the adapter index, so a reorder
        // means the index of a device changed.
        let reordered = [capturer(0, "DISPLAY2"), capturer(1, "DISPLAY1")];
        assert_eq!(resolve_monitor(&a, &monitors, &reordered), Ok(1));
        assert_eq!(resolve_monitor(&b, &monitors, &reordered), Ok(0));
    }

    #[test]
    fn a_synthetic_id_without_a_display_device_is_identity_less() {
        let a = display("DISPLAY1", 0, 1920, 1080);
        let monitors = [a.clone()];
        // A DirectX-only row beyond the adapter list has no device name.
        let sources = [capturer(0, ""), capturer(7, "")];
        assert!(confirmed_mapping(&monitors, &sources).is_empty());
        assert_eq!(
            resolve_monitor(&a, &monitors, &sources),
            Err(ResolveError::Missing),
            "an id without a device must never borrow the only display"
        );
    }

    #[test]
    fn a_capture_id_that_now_names_another_display_is_refused() {
        let a = display("DISPLAY1", 0, 1920, 1080);
        let b = display("DISPLAY2", 1920, 2560, 1440);
        // The user chose A when the capturer called it id 0. In the next
        // snapshot A is gone and id 0 belongs to B.
        let current = [b.clone()];
        let sources = [capturer(0, "DISPLAY2")];
        assert_eq!(
            resolve_monitor(&a, &current, &sources),
            Err(ResolveError::Missing),
            "a numeric id alone must never confirm a display"
        );
        assert_eq!(resolve_monitor(&b, &current, &sources), Ok(0));
    }

    #[test]
    fn a_vanished_display_is_rejected() {
        let a = display("DISPLAY1", 0, 1920, 1080);
        let b = display("DISPLAY2", 1920, 2560, 1440);
        // Windows still lists both displays but the capturer lost A's row.
        let monitors = [a.clone(), b.clone()];
        let sources = [capturer(0, "DISPLAY2")];
        assert_eq!(
            resolve_monitor(&a, &monitors, &sources),
            Err(ResolveError::Missing)
        );
    }

    #[test]
    fn an_ambiguous_capturer_list_is_rejected_not_guessed() {
        let a = display("DISPLAY1", 0, 1920, 1080);
        let monitors = [a.clone()];
        let sources = [capturer(0, "DISPLAY1"), capturer(2, "DISPLAY1")];
        assert_eq!(
            resolve_monitor(&a, &monitors, &sources),
            Err(ResolveError::Ambiguous)
        );
        assert!(confirmed_mapping(&monitors, &sources).is_empty());
    }

    /// P06: a display left of or above the primary monitor has negative
    /// coordinates. Its identity still resolves and keeps the same positive
    /// size, and moving it changes the focus key.
    #[test]
    fn a_display_with_negative_origin_resolves_with_its_full_size() {
        let left = display("DISPLAY2", -1920, 1920, 1080);
        let above = display("DISPLAY3", 0, 1920, 1080);
        let above = MonitorIdentity {
            bounds: rect(0, -1080, 1920, 1080),
            work: rect(0, -1080, 1920, 1080),
            ..above
        };
        let sources = [capturer(0, "DISPLAY2"), capturer(2, "DISPLAY3")];
        let primary = display("DISPLAY1", 0, 1920, 1080);
        assert_eq!(
            resolve_monitor(
                &left,
                &[primary.clone(), left.clone(), above.clone()],
                &sources
            ),
            Ok(0)
        );
        assert_eq!(
            resolve_monitor(&above, &[primary, left.clone(), above.clone()], &sources),
            Ok(2)
        );
        assert_eq!(left.size(), (1920, 1080));
        assert_eq!(above.size(), (1920, 1080));
        assert_ne!(left.focus_key(), above.focus_key());
        assert_ne!(
            left.focus_key(),
            display("DISPLAY2", 0, 1920, 1080).focus_key()
        );
    }

    #[test]
    fn a_changed_display_geometry_requires_a_new_selection() {
        let a = display("DISPLAY1", 0, 1920, 1080);
        let moved = display("DISPLAY1", 1920, 1920, 1080);
        let sources = [capturer(0, "DISPLAY1")];
        assert_eq!(
            resolve_monitor(&a, &[moved], &sources),
            Err(ResolveError::TopologyChanged)
        );
    }

    #[test]
    fn a_device_case_change_alone_is_not_a_topology_change() {
        let a = display("DISPLAY1", 0, 1920, 1080);
        let upper = MonitorIdentity {
            device: r"\\.\display1".into(),
            ..a.clone()
        };
        let sources = [capturer(0, "DISPLAY1")];
        assert_eq!(resolve_monitor(&a, &[upper.clone()], &sources), Ok(0));
        assert_eq!(confirm_prepared(Some(&a), &[upper]), Ok(()));
    }

    #[test]
    fn confirming_the_selected_display_ignores_the_rest_of_the_configuration() {
        let a = display("DISPLAY1", 0, 1920, 1080);
        let b = display("DISPLAY2", 1920, 2560, 1440);

        // A second monitor appears: the selected surface did not change.
        assert_eq!(confirm_prepared(Some(&a), &[a.clone(), b.clone()]), Ok(()));
        // A third monitor appears and the other display's work area changes.
        let mut moved_b = b.clone();
        moved_b.work = rect(1920, 0, 2560, 1400);
        let c = display("DISPLAY3", 4480, 1280, 720);
        assert_eq!(confirm_prepared(Some(&a), &[a.clone(), moved_b, c]), Ok(()));

        // The selected display itself moved.
        assert_eq!(
            confirm_prepared(Some(&a), &[display("DISPLAY1", 1920, 1920, 1080)]),
            Err(ResolveError::TopologyChanged)
        );
        // Its work area changed (taskbar moved).
        let mut work_moved = a.clone();
        work_moved.work = rect(0, 0, 1920, 1040);
        assert_eq!(
            confirm_prepared(Some(&a), &[work_moved]),
            Err(ResolveError::TopologyChanged)
        );
        // It vanished, duplicated, or was never recorded.
        assert_eq!(
            confirm_prepared(Some(&a), &[b.clone()]),
            Err(ResolveError::Missing)
        );
        assert_eq!(
            confirm_prepared(Some(&a), &[a.clone(), a.clone()]),
            Err(ResolveError::Ambiguous)
        );
        assert_eq!(confirm_prepared(None, &[b]), Ok(()));
    }

    #[test]
    fn the_confirmed_mapping_is_unique_in_both_directions() {
        let a = display("DISPLAY1", 0, 1920, 1080);
        let b = display("DISPLAY2", 1920, 2560, 1440);
        let mapping = confirmed_mapping(
            &[a.clone(), b.clone()],
            &[capturer(1, "DISPLAY1"), capturer(0, "DISPLAY2")],
        );
        assert_eq!(mapping, vec![(1, a.clone()), (0, b)]);

        // Two Windows monitors with the same device name claim nothing.
        let duplicated = confirmed_mapping(&[a.clone(), a.clone()], &[capturer(0, "DISPLAY1")]);
        assert!(duplicated.is_empty());
    }
}
