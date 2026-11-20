//! Stable monitor identity for screen capture.
//!
//! The desktop capturer numbers its monitor sources per enumeration. That
//! number is only a lookup key for one list, never the display itself: after a
//! reordering, unplug or resolution change the same number can name another
//! monitor. Selection, preview, preparation and the real capture therefore
//! bind to the Windows display identity (`\\.\DISPLAYn`) plus its geometry,
//! and resolve the current capturer id through a confirmed, unique mapping.
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

/// The monitor configuration at one moment, sorted so two enumerations with
/// the same surfaces compare equal regardless of enumeration order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct TopologySnapshot {
    monitors: Vec<MonitorIdentity>,
}

impl TopologySnapshot {
    pub fn from_monitors(mut monitors: Vec<MonitorIdentity>) -> Self {
        monitors.sort();
        Self { monitors }
    }

    pub fn monitors(&self) -> &[MonitorIdentity] {
        &self.monitors
    }
}

/// One monitor source of the desktop capturer as the current enumeration
/// exposes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CapturerMonitor {
    pub id: u64,
    pub title: String,
}

/// Why the selected surface cannot be resolved in the current enumeration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResolveError {
    /// The display or its capturer row is gone.
    Missing,
    /// More than one row or monitor matches the confirmed identity.
    Ambiguous,
    /// The device still exists but its geometry (or the whole configuration)
    /// changed after the selection was confirmed.
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

/// A confirmed capturer row: the current numeric id and the Windows display it
/// names. Only unique matches are returned.
pub(crate) fn confirmed_mapping(
    monitors: &[MonitorIdentity],
    sources: &[CapturerMonitor],
) -> Vec<(u64, MonitorIdentity)> {
    let mut matched: Vec<(u64, MonitorIdentity)> = Vec::new();
    for source in sources {
        let Some(monitor) = monitor_for_title(&source.title, monitors) else {
            continue;
        };
        matched.push((source.id, monitor.clone()));
    }
    matched
        .iter()
        .filter(|(_, monitor)| {
            // A display with two capturer rows is ambiguous in both
            // directions: neither row may claim it. The same applies when the
            // Windows list itself repeats a monitor.
            let rows = matched
                .iter()
                .filter(|(_, candidate)| candidate == monitor)
                .count();
            let monitors = monitors
                .iter()
                .filter(|candidate| *candidate == monitor)
                .count();
            rows == 1 && monitors == 1
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
    let on_device: Vec<&MonitorIdentity> = monitors
        .iter()
        .filter(|monitor| monitor.device.eq_ignore_ascii_case(&identity.device))
        .collect();
    if on_device.is_empty() {
        return Err(ResolveError::Missing);
    }
    if !on_device.iter().any(|monitor| *monitor == identity) {
        return Err(ResolveError::TopologyChanged);
    }
    if on_device.len() > 1 {
        return Err(ResolveError::Ambiguous);
    }
    // Two rows naming the same display are ambiguous even though the mapping
    // helper discards both; that is a different answer from "gone".
    let ambiguous = sources
        .iter()
        .filter(|source| monitor_for_title(&source.title, monitors) == Some(identity))
        .count();
    if ambiguous > 1 {
        return Err(ResolveError::Ambiguous);
    }
    let mut ids = confirmed_mapping(monitors, sources)
        .into_iter()
        .filter(|(_, monitor)| monitor == identity)
        .map(|(id, _)| id);
    let Some(id) = ids.next() else {
        return Err(ResolveError::Missing);
    };
    if ids.next().is_some() {
        return Err(ResolveError::Ambiguous);
    }
    Ok(id)
}

/// Confirm that a capture prepared from `selected` still describes the same
/// display configuration when its first frame arrives. A moved, resized or
/// replaced monitor discards the preparation instead of publishing a picture
/// of another display.
pub(crate) fn confirm_prepared(
    selected: Option<&TopologySnapshot>,
    current: &TopologySnapshot,
) -> Result<(), ResolveError> {
    match selected {
        Some(snapshot) if snapshot != current => Err(ResolveError::TopologyChanged),
        _ => Ok(()),
    }
}

/// Match a capturer source title to the monitor it names.
///
/// An exact device substring wins; otherwise the display number at the end of
/// the device name is matched against a number in the title. An absent or
/// ambiguous match is `None`, so no metadata or identity is borrowed from a
/// neighbour.
pub(crate) fn monitor_for_title<'a>(
    title: &str,
    monitors: &'a [MonitorIdentity],
) -> Option<&'a MonitorIdentity> {
    let title = title.trim().to_lowercase();
    if title.is_empty() {
        return None;
    }
    let named: Vec<&MonitorIdentity> = monitors
        .iter()
        .filter(|monitor| {
            let device = monitor.device.trim().to_lowercase();
            !device.is_empty() && title.contains(&device)
        })
        .collect();
    if !named.is_empty() {
        // Only a unique longest device name is trusted, so a device that is a
        // prefix of another one cannot pick the wrong monitor.
        let longest = named
            .iter()
            .max_by_key(|monitor| monitor.device.len())
            .copied()?;
        let tied = named
            .iter()
            .filter(|monitor| monitor.device.len() == longest.device.len())
            .count();
        return (tied == 1).then_some(longest);
    }
    // Fall back to the display number, e.g. `\\.\DISPLAY2` -> 2.
    let numbers = title_numbers(&title);
    let mut matched = monitors.iter().filter(|monitor| {
        display_number(&monitor.device).is_some_and(|number| numbers.contains(&number))
    });
    let first = matched.next()?;
    matched.next().is_none().then_some(first)
}

/// Trailing number of a device name such as `\\.\DISPLAY2`.
fn display_number(device: &str) -> Option<u64> {
    let digits: String = device
        .chars()
        .rev()
        .take_while(|character| character.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// Every number that appears in a title, in order.
fn title_numbers(title: &str) -> Vec<u64> {
    let mut numbers = Vec::new();
    let mut current = String::new();
    for character in title.chars() {
        if character.is_ascii_digit() {
            current.push(character);
        } else if !current.is_empty() {
            if let Ok(number) = current.parse::<u64>() {
                numbers.push(number);
            }
            current.clear();
        }
    }
    if !current.is_empty() {
        if let Ok(number) = current.parse::<u64>() {
            numbers.push(number);
        }
    }
    numbers
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

    fn source(id: u64, title: &str) -> CapturerMonitor {
        CapturerMonitor {
            id,
            title: title.into(),
        }
    }

    fn snapshot(monitors: &[MonitorIdentity]) -> TopologySnapshot {
        TopologySnapshot::from_monitors(monitors.to_vec())
    }

    #[test]
    fn a_monitor_title_matches_its_own_device_name() {
        let monitors = [
            display("DISPLAY1", 0, 1920, 1080),
            display("DISPLAY2", 1920, 2560, 1440),
        ];
        let matched = monitor_for_title(r"Экран (\\.\DISPLAY2)", &monitors)
            .expect("the device name is in the title");
        assert_eq!(matched.size(), (2560, 1440));
    }

    #[test]
    fn a_monitor_title_matches_the_display_number_not_the_list_order() {
        // The list starts with DISPLAY2, so position-based metadata would
        // attach the 2560×1440 size to the title that names DISPLAY1.
        let monitors = [
            display("DISPLAY2", 1920, 2560, 1440),
            display("DISPLAY1", 0, 1920, 1080),
        ];
        let matched = monitor_for_title("Screen 1", &monitors).expect("display number");
        assert_eq!(matched.size(), (1920, 1080));
    }

    #[test]
    fn an_unmatched_or_ambiguous_monitor_title_never_borrows_a_size() {
        let monitors = [
            display("DISPLAY1", 0, 1920, 1080),
            display("DISPLAY2", 1920, 2560, 1440),
        ];
        assert!(monitor_for_title("Документ", &monitors).is_none());
        assert!(monitor_for_title("   ", &monitors).is_none());

        // Two devices end in the same number: guessing either one would attach
        // a neighbour's size.
        let ambiguous = [
            MonitorIdentity {
                device: "\\\\.\\DISPLAY1".into(),
                ..display("X", 0, 1920, 1080)
            },
            MonitorIdentity {
                device: "\\\\.\\MONITOR1".into(),
                ..display("Y", 1920, 1280, 720)
            },
        ];
        assert!(monitor_for_title("Screen 1", &ambiguous).is_none());
    }

    #[test]
    fn an_ordinary_reorder_resolves_the_same_display_to_its_new_id() {
        let a = display("DISPLAY1", 0, 1920, 1080);
        let b = display("DISPLAY2", 1920, 2560, 1440);
        let monitors = [a.clone(), b.clone()];

        // First enumeration: A is id 0, B is id 1.
        let first = [source(0, "Screen 1"), source(1, "Screen 2")];
        assert_eq!(resolve_monitor(&a, &monitors, &first), Ok(0));

        // The capturer reorders the list; the displays did not move.
        let reordered = [source(0, "Screen 2"), source(1, "Screen 1")];
        assert_eq!(resolve_monitor(&a, &monitors, &reordered), Ok(1));
        assert_eq!(resolve_monitor(&b, &monitors, &reordered), Ok(0));
    }

    #[test]
    fn a_capture_id_that_now_names_another_display_is_refused() {
        let a = display("DISPLAY1", 0, 1920, 1080);
        let b = display("DISPLAY2", 1920, 2560, 1440);
        // The user chose A when the capturer called it id 0. In the next
        // snapshot A is gone and id 0 belongs to B.
        let current = [b.clone()];
        let sources = [source(0, "Screen 2")];
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
        let sources = [source(0, "Screen 2")];
        assert_eq!(
            resolve_monitor(&a, &monitors, &sources),
            Err(ResolveError::Missing)
        );
    }

    #[test]
    fn an_ambiguous_capturer_list_is_rejected_not_guessed() {
        let a = display("DISPLAY1", 0, 1920, 1080);
        let monitors = [a.clone()];
        let sources = [source(0, "Screen 1"), source(2, "Screen 1")];
        assert_eq!(
            resolve_monitor(&a, &monitors, &sources),
            Err(ResolveError::Ambiguous)
        );
        assert!(confirmed_mapping(&monitors, &sources).is_empty());
    }

    #[test]
    fn a_changed_display_geometry_requires_a_new_selection() {
        let a = display("DISPLAY1", 0, 1920, 1080);
        let moved = display("DISPLAY1", 1920, 1920, 1080);
        let sources = [source(0, "Screen 1")];
        assert_eq!(
            resolve_monitor(&a, &[moved], &sources),
            Err(ResolveError::TopologyChanged)
        );
    }

    #[test]
    fn a_topology_change_between_selection_and_the_first_frame_discards_the_preparation() {
        let a = display("DISPLAY1", 0, 1920, 1080);
        let b = display("DISPLAY2", 1920, 2560, 1440);
        let selected = snapshot(&[a.clone()]);
        // The user plugged in a second monitor (or moved A) after the choice.
        let changed = snapshot(&[a, b]);
        assert_eq!(
            confirm_prepared(Some(&selected), &changed),
            Err(ResolveError::TopologyChanged)
        );
        // The unchanged configuration still confirms.
        assert_eq!(confirm_prepared(Some(&selected), &selected), Ok(()));
        // A source without a recorded snapshot has nothing to compare.
        assert_eq!(confirm_prepared(None, &changed), Ok(()));
    }

    #[test]
    fn the_confirmed_mapping_is_unique_in_both_directions() {
        let a = display("DISPLAY1", 0, 1920, 1080);
        let b = display("DISPLAY2", 1920, 2560, 1440);
        let mapping = confirmed_mapping(
            &[a.clone(), b.clone()],
            &[source(1, "Screen 1"), source(0, "Screen 2")],
        );
        assert_eq!(mapping, vec![(1, a), (0, b)]);
    }
}
