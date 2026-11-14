use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use gpui::{
    div, prelude::*, px, rgba, AnyElement, Context, RenderImage, ScrollHandle, Task, Window,
};

use crate::{icon, ink, Voxhold};

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

impl Voxhold {
    pub(crate) fn reset_roster(&mut self, window: &mut Window) {
        for (_, (image, _)) in self.roster.avatars.drain() {
            let _ = window.drop_image(image);
        }
        self.roster = Roster::default();
    }

    pub(crate) fn load_channel_members(&mut self, cx: &mut Context<Self>) {
        if !self.show_members
            || !self.roster.channel_tab
            || self.roster.channel_pending
            || self.roster.channel_ids.is_some()
            || self.access_pending
            || !self
                .channels
                .iter()
                .any(|channel| channel.id == self.channel && channel.readable())
        {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let (server, channel, access) = (self.server, self.channel, self.access_generation);
        self.roster.channel_request = self.roster.channel_request.wrapping_add(1);
        let request = self.roster.channel_request;
        self.roster.channel_pending = true;
        self.roster.channel_error = None;
        let runtime = self.runtime.clone();
        let task = cx.background_spawn(async move {
            runtime.block_on(client.channel_members(server, channel))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(view) = this.upgrade() {
                let _ = view.update(cx, |s, cx| {
                    if (s.server, s.channel, s.access_generation) != (server, channel, access)
                        || s.roster.channel_request != request
                    {
                        return;
                    }
                    s.roster.channel_pending = false;
                    match result {
                        Ok(ids) => s.roster.channel_ids = Some(ids.into_iter().collect()),
                        Err(error) => s.roster.channel_error = Some(error.to_string()),
                    }
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn request_avatar(
        &mut self,
        user_id: i64,
        version: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = (user_id, version);
        if self.roster.avatars.contains_key(&key)
            || self.roster.loading.contains_key(&user_id)
            || self.roster.loading.len() >= AVATAR_CONCURRENCY
            || self
                .roster
                .failures
                .get(&user_id)
                .is_some_and(|(version, until)| {
                    *version == key.1 && *until > std::time::Instant::now()
                })
        {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let access = self.access_generation;
        let runtime = self.runtime.clone();
        let version = key.1.clone();
        let task = cx.background_spawn(async move {
            runtime
                .block_on(client.avatar(user_id, &version))
                .map(avatar_frame)
        });
        let fetch = cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                if s.access_generation != access {
                    return;
                }
                // Keep the running task in a local until its update completes.
                let _finished = s.roster.loading.remove(&user_id);
                if !s.members.iter().any(|member| {
                    member.user_id == user_id && member.avatar_version.as_ref() == Some(&key.1)
                }) {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(frame) => {
                        let render =
                            std::sync::Arc::new(gpui::RenderImage::new(smallvec::smallvec![frame]));
                        if let Some(evicted) = s.roster.cache_avatar(key, render) {
                            let _ = window.drop_image(evicted);
                        }
                    }
                    Err(_) => {
                        s.roster.failures.insert(
                            user_id,
                            (
                                key.1,
                                std::time::Instant::now() + std::time::Duration::from_secs(20),
                            ),
                        );
                    }
                }
                cx.notify();
            });
        });
        self.roster.loading.insert(user_id, fetch);
    }

    pub(crate) fn members_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette;
        let channel = self
            .channels
            .iter()
            .find(|ch| ch.id == self.channel && ch.readable())
            .cloned();
        let channel_tab = self.roster.channel_tab && channel.is_some();
        let visible: Vec<_> = self
            .members
            .iter()
            .filter(|member| {
                !channel_tab
                    || self
                        .roster
                        .channel_ids
                        .as_ref()
                        .is_some_and(|ids| ids.contains(&member.user_id))
            })
            .cloned()
            .collect();
        let title = if self.access_pending || (channel_tab && self.roster.channel_ids.is_none()) {
            "УЧАСТНИКИ".to_string()
        } else {
            format!("УЧАСТНИКИ · {}", visible.len())
        };
        let mut panel = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(ink(p.panel))
            .border_l_1()
            .border_color(ink(p.line))
            .child(
                div()
                    .h(px(48.))
                    .flex_shrink_0()
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(title)
                    .child(
                        div()
                            .id("members-close")
                            .p_1()
                            .cursor_pointer()
                            .on_click(cx.listener(|s, _, _, cx| s.set_members_open(false, cx)))
                            .child(icon("icons/close.svg", 13., ink(p.muted))),
                    ),
            )
            .child(div().flex().gap_1().px_2().pb_2().children(
                [(true, "Участники канала"), (false, "Все участники")].map(
                    |(is_channel, label)| {
                        let enabled = !is_channel || channel.is_some();
                        let selected = is_channel == channel_tab;
                        div()
                            .id(("member-tab", u64::from(is_channel)))
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .justify_center()
                            .items_center()
                            .text_center()
                            .py(px(7.))
                            .px_1()
                            .rounded(px(7.))
                            .border_1()
                            .border_color(if selected { ink(p.line) } else { rgba(0) })
                            .bg(if selected { ink(p.surface) } else { rgba(0) })
                            .text_size(px(11.))
                            .text_color(ink(if selected { p.fg } else { p.muted }))
                            .when(enabled, |tab| {
                                tab.cursor_pointer()
                                    .on_click(cx.listener(move |s, _, _, cx| {
                                        if s.roster.channel_tab != is_channel {
                                            s.roster.reset_channel();
                                            s.roster.channel_tab = is_channel;
                                            s.load_channel_members(cx);
                                            cx.notify();
                                        }
                                    }))
                            })
                            .child(label)
                    },
                ),
            ));
        if self.access_pending {
            return panel
                .child(div().p_3().child("Проверяем доступ…"))
                .into_any_element();
        }
        if let Some(channel) = channel.filter(|_| channel_tab) {
            let scope = if channel.kind == "voice" {
                "Могут войти в"
            } else {
                "Могут видеть"
            };
            panel = panel.child(
                div()
                    .px_3()
                    .pb_2()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .truncate()
                    .child(format!("{scope} «{}»", channel.name)),
            );
            if self.roster.channel_ids.is_none() {
                let status = div().p_3().text_size(px(12.)).text_color(ink(p.muted));
                return panel
                    .child(if let Some(error) = self.roster.channel_error.clone() {
                        status.child(error).child(
                            div()
                                .id("members-retry")
                                .p_1()
                                .cursor_pointer()
                                .child("Повторить")
                                .on_click(cx.listener(|s, _, _, cx| s.load_channel_members(cx))),
                        )
                    } else {
                        status.child("Загружаем участников…")
                    })
                    .into_any_element();
            }
        }
        let offset: f32 = (-self.roster.scroll.offset().y).into();
        let height: f32 = self.roster.scroll.bounds().size.height.into();
        let mut y = 0.;
        let mut rows = div()
            .id("member-list")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .track_scroll(&self.roster.scroll)
            .px_2();
        if visible.is_empty() {
            rows = rows.child(
                div()
                    .p_3()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .child("Участников пока нет."),
            );
        }
        let online_users = self
            .online_by_server
            .get(&self.server)
            .cloned()
            .unwrap_or_default();
        for (online, label) in [(true, "В СЕТИ"), (false, "НЕ В СЕТИ")] {
            let group: Vec<_> = visible
                .iter()
                .filter(|member| online_users.contains(&member.user_id) == online)
                .collect();
            if group.is_empty() {
                continue;
            }
            rows = rows.child(
                div()
                    .h(px(32.))
                    .flex()
                    .items_center()
                    .px_3()
                    .text_size(px(12.))
                    .text_color(ink(p.muted))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(format!("{label} — {}", group.len())),
            );
            y += 32.;
            for member in group {
                let is_near_viewport = y + 72. >= offset - 72.
                    && y < offset
                        + if height > 0. {
                            height + 72.
                        } else {
                            self.viewport.1.min(360.) + 72.
                        };
                y += 72.;
                let avatar = member
                    .avatar_version
                    .as_ref()
                    .filter(|version| !version.is_empty())
                    .and_then(|version| {
                        let key = (member.user_id, version.clone());
                        if let Some((image, used)) = self.roster.avatars.get_mut(&key) {
                            if is_near_viewport {
                                *used = std::time::Instant::now();
                            }
                            return Some(image.clone());
                        }
                        if is_near_viewport {
                            self.request_avatar(member.user_id, version.clone(), window, cx);
                        }
                        None
                    });
                let circle = div()
                    .w(px(40.))
                    .h(px(40.))
                    .flex()
                    .justify_center()
                    .items_center()
                    .rounded_full();
                let face = if let Some(image) = avatar {
                    circle.child(
                        gpui::img(image)
                            .size_full()
                            .rounded_full()
                            .object_fit(gpui::ObjectFit::Cover),
                    )
                } else {
                    circle
                        .bg(ink(avatar_color(&member.username)))
                        .text_color(ink(0x151510))
                        .text_size(px(15.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(initials(&member.username))
                };
                let (role, name_color) = role(&member.role, p.light);
                let last_seen = member
                    .last_seen_at
                    .max(self.roster.last_seen.get(&member.user_id).copied());
                rows = rows.child(
                    div()
                        .id(("member", member.user_id as u64))
                        .h(px(72.))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div().relative().flex_shrink_0().child(face).child(
                                div()
                                    .absolute()
                                    .bottom_0()
                                    .right_0()
                                    .w(px(10.))
                                    .h(px(10.))
                                    .rounded_full()
                                    .border_2()
                                    .border_color(ink(p.panel))
                                    .bg(ink(if online { p.lime } else { p.muted })),
                            ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .gap(px(3.))
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(14.))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(ink(name_color))
                                        .child(member.username.clone()),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(11.))
                                        .text_color(ink(p.muted))
                                        .child(role),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(11.))
                                        .text_color(ink(if online {
                                            p.story_accent
                                        } else {
                                            p.muted
                                        }))
                                        .child(presence_label(online, last_seen)),
                                ),
                        ),
                );
            }
        }
        panel.child(rows).into_any_element()
    }
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
