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

pub fn role(role: &str, light: bool) -> (&'static str, u32) {
    match role {
        "owner" => ("Владелец сервера", if light { 0x805011 } else { 0xffc857 }),
        "admin" => ("Администратор", if light { 0x5b3ebb } else { 0xa993ff }),
        _ => ("Участник", if light { 0x0b6878 } else { 0x63d8e6 }),
    }
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
    let elapsed = now.saturating_sub(last_seen).max(0);
    let relative = match elapsed {
        0..=59 => "только что".into(),
        60..=3599 => format!("{} мин назад", elapsed / 60),
        3600..=86399 => format!("{} ч назад", elapsed / 3600),
        86400..=172799 => "вчера".into(),
        _ => {
            let days = elapsed / 86400;
            let suffix = match (days % 100, days % 10) {
                (11..=14, _) => "дней",
                (_, 1) => "день",
                (_, 2..=4) => "дня",
                _ => "дней",
            };
            format!("{days} {suffix} назад")
        }
    };
    format!("Был в сети {relative}")
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
}
