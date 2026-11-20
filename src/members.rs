use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use gpui::{RenderImage, ScrollHandle, Task};

pub const AVATAR_CONCURRENCY: usize = 4;
const AVATAR_CAPACITY: usize = 64;

pub struct Roster {
    pub channel_tab: bool,
    pub channel_ids: Option<HashSet<i64>>,
    pub channel_error: Option<String>,
    pub channel_pending: bool,
    pub channel_request: u64,
    pub last_seen: HashMap<i64, i64>,
    pub scroll: ScrollHandle,
    pub avatars: HashMap<(i64, String), (Arc<RenderImage>, Instant)>,
    pub failures: HashMap<i64, (String, Instant)>,
    pub loading: HashMap<i64, Task<()>>,
}

impl Default for Roster {
    fn default() -> Self {
        Self {
            channel_tab: true,
            channel_ids: None,
            channel_error: None,
            channel_pending: false,
            channel_request: 0,
            last_seen: HashMap::new(),
            scroll: ScrollHandle::new(),
            avatars: HashMap::new(),
            failures: HashMap::new(),
            loading: HashMap::new(),
        }
    }
}

impl Roster {
    pub fn reset_channel(&mut self) {
        self.channel_request = self.channel_request.wrapping_add(1);
        self.channel_ids = None;
        self.channel_error = None;
        self.channel_pending = false;
        self.scroll = ScrollHandle::new();
    }

    pub fn cache_avatar(
        &mut self,
        key: (i64, String),
        avatar: Arc<RenderImage>,
    ) -> Option<Arc<RenderImage>> {
        self.avatars.insert(key, (avatar, Instant::now()));
        if self.avatars.len() > AVATAR_CAPACITY {
            let oldest = self
                .avatars
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(key, _)| key.clone());
            return oldest.and_then(|key| self.avatars.remove(&key).map(|(avatar, _)| avatar));
        }
        None
    }
}

pub fn avatar_frame(image: image::DynamicImage) -> image::Frame {
    let mut pixels = image.into_rgba8();
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    image::Frame::new(pixels)
}

pub fn initials(name: &str) -> String {
    let letters: String = name
        .trim()
        .chars()
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    if letters.is_empty() {
        "VX".into()
    } else {
        letters
    }
}

pub fn avatar_color(name: &str) -> u32 {
    // Same UTF-16 hash and palette order as the web client.
    let mut hash = 0_i64;
    for unit in name.encode_utf16() {
        let signed = hash as i32;
        hash = i64::from(signed.wrapping_shl(5)) - hash + i64::from(unit);
    }
    [0xa993ff, 0xe7ff5b, 0xff8c75, 0x63d8e6, 0xffc857][hash.unsigned_abs() as usize % 5]
}

pub fn role(role: &str, _light: bool) -> (&'static str, u32) {
    // Go `roleColor`/`roleLabel` use the same values in both themes.
    match role {
        "owner" => ("Владелец сервера", 0xffc45d),
        "admin" => ("Администратор", 0xa993ff),
        "moderator" => ("Модератор", 0x63d8e6),
        _ => ("Участник", 0x63d8e6),
    }
}

/// Go `formatSeen`: minutes below an hour, hours below a day, then days, with
/// the Russian plural forms. There is no separate "вчера" in the reference.
pub fn format_seen(value: i64, now: i64) -> String {
    let elapsed = now.saturating_sub(value).max(0);
    if elapsed < 60 {
        return "только что".into();
    }
    let (count, unit) = if elapsed < 3600 {
        (elapsed / 60, "минуту")
    } else if elapsed < 86_400 {
        (elapsed / 3600, "час")
    } else {
        (elapsed / 86_400, "день")
    };
    format!("{count} {} назад", russian_count(count, unit))
}

fn russian_count(count: i64, unit: &str) -> &'static str {
    let forms = match unit {
        "минуту" => ["минуту", "минуты", "минут"],
        "час" => ["час", "часа", "часов"],
        _ => ["день", "дня", "дней"],
    };
    let index = if (11..=14).contains(&(count % 100)) {
        2
    } else {
        match count % 10 {
            1 => 0,
            2..=4 => 1,
            _ => 2,
        }
    };
    forms[index]
}

pub fn presence_label(online: bool, last_seen: Option<i64>) -> String {
    if online {
        return "Сейчас в сети".into();
    }
    let Some(last_seen) = last_seen.filter(|at| *at > 0) else {
        return "Нет данных об активности".into();
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64;
    format!("Был в сети {}", format_seen(last_seen, now))
}

/// The Go model keeps the server roster sorted case-insensitively by username;
/// owners/admins are not pinned. Events and reloads use the same order.
pub fn sort_members(members: &mut [voxhold_api::Member]) {
    members.sort_by(|a, b| {
        a.username
            .to_lowercase()
            .cmp(&b.username.to_lowercase())
            .then_with(|| a.user_id.cmp(&b.user_id))
    });
}

/// Members of one channel in roster order, followed by ids that are not in the
/// server snapshot (Go renders them as `Участник <id>`).
pub fn channel_roster(
    members: &[voxhold_api::Member],
    ids: &HashSet<i64>,
) -> Vec<voxhold_api::Member> {
    let mut rows: Vec<voxhold_api::Member> = members
        .iter()
        .filter(|member| ids.contains(&member.user_id))
        .cloned()
        .collect();
    let mut unknown: Vec<i64> = ids
        .iter()
        .copied()
        .filter(|id| !members.iter().any(|member| member.user_id == *id))
        .collect();
    unknown.sort_unstable();
    for id in unknown {
        rows.push(voxhold_api::Member {
            user_id: id,
            username: format!("Участник {id}"),
            ..Default::default()
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avatar_preserves_color_channels_and_uses_two_initials() {
        let pixel = image::Rgba([201, 33, 7, 255]);
        let frame = avatar_frame(image::DynamicImage::ImageRgba8(
            image::RgbaImage::from_pixel(1, 1, pixel),
        ));
        assert_eq!(frame.buffer().as_raw(), &[7, 33, 201, 255]);
        assert_eq!(initials(" Инвалид "), "ИН");
        assert_eq!(initials("KOTONY"), "KO");
        assert_eq!(avatar_color("KOTONY"), 0xff8c75);
        assert_eq!(avatar_color("Инвалид"), 0xffc857);
    }

    #[test]
    fn presence_and_roles_match_the_go_strings() {
        assert_eq!(role("owner", false).1, 0xffc45d);
        assert_eq!(role("admin", true).1, 0xa993ff);
        assert_eq!(role("member", true).0, "Участник");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert_eq!(format_seen(now - 10, now), "только что");
        assert_eq!(format_seen(now - 60, now), "1 минуту назад");
        assert_eq!(format_seen(now - 125, now), "2 минуты назад");
        assert_eq!(format_seen(now - 300, now), "5 минут назад");
        assert_eq!(format_seen(now - 3600, now), "1 час назад");
        assert_eq!(format_seen(now - 7200, now), "2 часа назад");
        assert_eq!(format_seen(now - 86_400, now), "1 день назад");
        assert_eq!(format_seen(now - 172_800, now), "2 дня назад");
        assert_eq!(format_seen(now - 5 * 86_400, now), "5 дней назад");
        assert_eq!(presence_label(false, None), "Нет данных об активности");
        assert_eq!(
            presence_label(false, Some(now - 120)),
            "Был в сети 2 минуты назад"
        );
    }

    #[test]
    fn channel_roster_keeps_server_order_and_adds_unknown_ids() {
        let members = vec![
            voxhold_api::Member {
                user_id: 2,
                username: "beta".into(),
                ..Default::default()
            },
            voxhold_api::Member {
                user_id: 3,
                username: "Alpha".into(),
                ..Default::default()
            },
        ];
        let mut sorted = members.clone();
        sort_members(&mut sorted);
        assert_eq!(sorted[0].user_id, 3);
        let ids = HashSet::from([2, 3, 9]);
        let rows = channel_roster(&sorted, &ids);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].username, "Участник 9");
    }
}
