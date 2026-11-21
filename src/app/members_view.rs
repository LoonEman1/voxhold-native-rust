use super::*;

use super::context_menus::{context_item, context_menu, context_separator};
use crate::members::{
    avatar_color, avatar_frame, initials, presence_label, role, Roster, AVATAR_CONCURRENCY,
};
use gpui::{AnyElement, Context, Pixels, Point, Window};

impl Voxhold {
    pub(super) fn reset_roster(&mut self, window: &mut Window) {
        for (_, (image, _)) in self.roster.avatars.drain() {
            let _ = window.drop_image(image);
        }
        self.roster = Roster::default();
    }

    pub(super) fn load_channel_members(&mut self, cx: &mut Context<Self>) {
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

    pub(super) fn member_avatar(
        &mut self,
        user_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<std::sync::Arc<gpui::RenderImage>> {
        let version = self
            .members
            .iter()
            .find(|member| member.user_id == user_id)?
            .avatar_version
            .clone()
            .filter(|version| !version.is_empty())?;
        let key = (user_id, version.clone());
        if let Some((image, used)) = self.roster.avatars.get_mut(&key) {
            *used = std::time::Instant::now();
            return Some(image.clone());
        }
        self.request_avatar(user_id, version, window, cx);
        None
    }

    pub(super) fn request_avatar(
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

    pub(super) fn members_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette;
        let close_focus = window
            .use_keyed_state("members-close-focus", cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let tab_focus = [
            window
                .use_keyed_state("members-tab-channel", cx, |_, cx| cx.focus_handle())
                .read(cx)
                .clone(),
            window
                .use_keyed_state("members-tab-server", cx, |_, cx| cx.focus_handle())
                .read(cx)
                .clone(),
        ];
        let channel = self
            .channels
            .iter()
            .find(|ch| ch.id == self.channel && ch.readable())
            .cloned();
        let channel_tab = self.roster.channel_tab && channel.is_some();
        let visible: Vec<_> = if channel_tab {
            self.roster
                .channel_ids
                .as_ref()
                .map(|ids| crate::members::channel_roster(&self.members, ids))
                .unwrap_or_default()
        } else {
            self.members.clone()
        };
        let title = format!("УЧАСТНИКИ · {}", visible.len());
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
                            .track_focus(&close_focus)
                            .tab_index(0)
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
                                    .track_focus(&tab_focus[usize::from(is_channel)])
                                    .tab_index(0)
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
                let menu_x = member.user_id;
                let row_focus = window
                    .use_keyed_state(("member-focus", member.user_id as u64), cx, |_, cx| {
                        cx.focus_handle()
                    })
                    .read(cx)
                    .clone();
                rows = rows.child(
                    div()
                        .id(("member", member.user_id as u64))
                        .debug_selector(move || format!("member-row-{menu_x}").into())
                        .h(px(72.))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_3()
                        .cursor_pointer()
                        .track_focus(&row_focus)
                        .tab_index(0)
                        .on_key_down(cx.listener(move |s, event: &gpui::KeyDownEvent, _, cx| {
                            let key = &event.keystroke;
                            let position =
                                gpui::point(px((s.viewport.0 - 290.).max(12.)), px(180.));
                            if key.key == "enter" || key.key == "space" {
                                s.open_member_profile(menu_x, position, cx);
                            } else if key.key == "context_menu"
                                || (key.key == "f10" && key.modifiers.shift)
                            {
                                s.open_member_menu(menu_x, position, false, cx);
                                cx.stop_propagation();
                            }
                        }))
                        .on_click(cx.listener(move |s, event: &gpui::ClickEvent, _, cx| {
                            s.open_member_profile(menu_x, event.position(), cx);
                        }))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |s, event: &gpui::MouseDownEvent, _, cx| {
                                s.open_member_menu(menu_x, event.position, false, cx);
                                cx.stop_propagation();
                            }),
                        )
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

    pub(super) fn open_member_menu(
        &mut self,
        user_id: i64,
        position: Point<Pixels>,
        voice: bool,
        cx: &mut Context<Self>,
    ) {
        let known = self.member_action_allowed(crate::settings::MemberAction::ViewProfile, user_id);
        if user_id <= 0 || self.access_pending || (!voice && user_id != self.self_user_id && !known)
        {
            return;
        }
        self.message_menu = None;
        self.channel_menu = None;
        self.member_menu = Some(MemberMenu {
            user_id,
            x: position.x.into(),
            y: position.y.into(),
            profile: None,
            profile_result: None,
            voice,
            confirm_block: false,
            confirm_ban: false,
        });
        cx.notify();
    }

    #[cfg(feature = "voxhold-media")]
    pub(super) fn open_voice_member_menu(
        &mut self,
        user_id: i64,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.voice_rooms.iter().any(|(&(server, channel), people)| {
            self.voice_channel_visible(server, channel)
                && people.iter().any(|person| person.user_id == user_id)
        }) {
            self.open_member_menu(user_id, position, true, cx);
        }
    }

    pub(super) fn open_member_profile(
        &mut self,
        user_id: i64,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.open_member_menu(user_id, position, false, cx);
        if self
            .member_menu
            .as_ref()
            .is_some_and(|menu| menu.user_id == user_id)
        {
            self.load_member_profile(cx);
        }
    }

    fn load_member_profile(&mut self, cx: &mut Context<Self>) {
        let (Some(menu), Some(client)) = (&mut self.member_menu, self.client.clone()) else {
            return;
        };
        let user_id = menu.user_id;
        let request = uuid::Uuid::new_v4();
        menu.profile = Some(request);
        menu.profile_result = None;
        let runtime = self.runtime.clone();
        let session = self.session_generation;
        let task =
            cx.background_spawn(async move { runtime.block_on(client.user_profile(user_id)) });
        cx.spawn(async move |this, cx| {
            let result = task.await.map_err(|error| error.to_string());
            if let Some(view) = this.upgrade() {
                let _ = view.update(cx, |s, cx| {
                    if s.session_generation == session {
                        if let Some(menu) = s
                            .member_menu
                            .as_mut()
                            .filter(|menu| menu.profile == Some(request))
                        {
                            menu.profile_result = Some(result);
                            cx.notify();
                        }
                    }
                });
            }
        })
        .detach();
        cx.notify();
    }

    fn actor_role(&self) -> crate::settings::Role {
        crate::settings::Role::parse(
            self.servers
                .iter()
                .find(|server| server.id == self.server)
                .map(|server| server.role.as_str())
                .unwrap_or("member"),
        )
    }

    pub(super) fn refresh_members(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let (server, access) = (self.server, self.access_generation);
        let session = self.session_generation;
        let runtime = self.runtime.clone();
        let task = cx.background_spawn(async move { runtime.block_on(client.members(server)) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(view) = this.upgrade() {
                let _ = view.update(cx, |s, cx| {
                    if s.server != server
                        || s.access_generation != access
                        || s.session_generation != session
                    {
                        return;
                    }
                    if let Ok(mut members) = result {
                        crate::members::sort_members(&mut members);
                        s.members = members;
                        cx.notify();
                    }
                });
            }
        })
        .detach();
        cx.notify();
    }

    fn member_snapshot(&self, user_id: i64) -> Option<voxhold_api::Member> {
        self.members
            .iter()
            .find(|member| member.user_id == user_id)
            .cloned()
    }

    fn member_action_allowed(&self, action: crate::settings::MemberAction, user_id: i64) -> bool {
        let Some(member) = self.member_snapshot(user_id) else {
            return false;
        };
        crate::settings::member_action_visible(
            action,
            self.actor_role(),
            self.self_user_id,
            crate::settings::Role::parse(&member.role),
            member.user_id,
            &member.username,
            &self.auth.username,
        )
    }

    pub(super) fn change_member_role(&mut self, user_id: i64, role: &str, cx: &mut Context<Self>) {
        if !self.member_action_allowed(crate::settings::MemberAction::SetRole, user_id) {
            self.member_menu = None;
            cx.notify();
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let server = self.server;
        let session = self.session_generation;
        let role = role.to_owned();
        let runtime = self.runtime.clone();
        self.member_menu = None;
        let task = cx.background_spawn(async move {
            runtime.block_on(client.update_member_role(server, user_id, &role))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(view) = this.upgrade() {
                let _ = view.update(cx, |s, cx| {
                    if s.server != server || s.session_generation != session {
                        return;
                    }
                    match result {
                        Ok(mut member) => {
                            s.members.retain(|row| row.user_id != member.user_id);
                            s.members.push(std::mem::take(&mut member));
                            crate::members::sort_members(&mut s.members);
                            s.action_error = None;
                        }
                        Err(error) => s.action_error = Some((0, error.to_string())),
                    }
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }

    pub(super) fn ban_member(&mut self, user_id: i64, cx: &mut Context<Self>) {
        if !self.member_action_allowed(crate::settings::MemberAction::Ban, user_id) {
            self.member_menu = None;
            cx.notify();
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let server = self.server;
        let session = self.session_generation;
        let runtime = self.runtime.clone();
        self.member_menu = None;
        let task = cx
            .background_spawn(async move { runtime.block_on(client.ban_member(server, user_id)) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(view) = this.upgrade() {
                let _ = view.update(cx, |s, cx| {
                    if s.server != server || s.session_generation != session {
                        return;
                    }
                    match result {
                        Ok(()) => {
                            s.members.retain(|member| member.user_id != user_id);
                            if let Some(ids) = s.roster.channel_ids.as_mut() {
                                ids.remove(&user_id);
                            }
                            s.action_error = None;
                        }
                        Err(error) => s.action_error = Some((0, error.to_string())),
                    }
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }

    pub(super) fn block_member(&mut self, user_id: i64, cx: &mut Context<Self>) {
        if !self.member_action_allowed(crate::settings::MemberAction::BlockMessages, user_id) {
            self.member_menu = None;
            cx.notify();
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let session = self.session_generation;
        let runtime = self.runtime.clone();
        self.member_menu = None;
        let task =
            cx.background_spawn(async move { runtime.block_on(client.block_user(user_id, true)) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Some(view) = this.upgrade() {
                let _ = view.update(cx, |s, cx| {
                    if s.session_generation != session {
                        return;
                    }
                    match result {
                        Ok(()) => {
                            s.action_error = None;
                            s.action_notice = "Пользователь заблокирован".into();
                        }
                        Err(error) => s.action_error = Some((0, error.to_string())),
                    }
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }

    pub(super) fn member_menu_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let menu = self.member_menu.clone()?;
        let member = self.member_snapshot(menu.user_id);
        let name = member
            .as_ref()
            .map(|member| member.username.clone())
            .unwrap_or_else(|| {
                if menu.user_id == self.self_user_id {
                    self.auth.username.clone()
                } else {
                    format!("Участник {}", menu.user_id)
                }
            });
        let target = crate::settings::Role::parse(
            member
                .as_ref()
                .map_or("member", |member| member.role.as_str()),
        );
        let p = self.palette;
        let position = gpui::point(px(menu.x), px(menu.y));
        let mut panel = context_menu("member-menu", &name, position, self.viewport, 245., p);
        if menu.profile.is_some() {
            let image = self.member_avatar(menu.user_id, window, cx);
            let name = match &menu.profile_result {
                Some(Ok(profile)) => &profile.username,
                _ => &name,
            };
            panel = panel.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .child(super::ui::avatar(image, name, 40.))
                    .child(
                        div()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .child(div().truncate().child(name.clone()))
                            .child(div().text_color(ink(p.muted)).child(target.label())),
                    ),
            );
            panel = match menu.profile_result {
                Some(Ok(profile)) => panel
                    .when(!profile.about.is_empty(), |panel| {
                        panel.child(div().p_2().child(profile.about))
                    })
                    .when_some(profile.country_code, |panel, country| {
                        panel.child(div().p_2().text_color(ink(p.muted)).child(country))
                    }),
                Some(Err(error)) => panel.child(div().p_2().text_color(ink(p.danger)).child(error)),
                None => panel.child(div().p_2().text_color(ink(p.muted)).child("Загрузка…")),
            };
            return Some(panel.into_any_element());
        }
        panel = panel.child(
            context_item(
                "member-profile",
                "Просмотреть профиль",
                "icons/people.svg",
                false,
                p,
            )
            .on_click(cx.listener(|s, _, _, cx| s.load_member_profile(cx))),
        );
        let other =
            self.member_action_allowed(crate::settings::MemberAction::BlockMessages, menu.user_id);
        if other {
            if menu.confirm_block {
                panel = panel
                    .child(format!("Заблокировать {name}?"))
                    .child(
                        div()
                            .id("member-block-confirm")
                            .cursor_pointer()
                            .text_color(ink(p.danger))
                            .on_click(
                                cx.listener(move |s, _, _, cx| s.block_member(menu.user_id, cx)),
                            )
                            .child("Заблокировать"),
                    )
                    .child(
                        div()
                            .id("member-block-cancel")
                            .cursor_pointer()
                            .text_color(ink(p.muted))
                            .on_click(cx.listener(|s, _, _, cx| {
                                if let Some(menu) = s.member_menu.as_mut() {
                                    menu.confirm_block = false;
                                }
                                cx.notify();
                            }))
                            .child("Отмена"),
                    );
            } else {
                panel = panel.child(
                    context_item(
                        "member-block",
                        "Заблокировать личные сообщения",
                        "icons/lock.svg",
                        false,
                        p,
                    )
                    .on_click(cx.listener(|s, _, _, cx| {
                        if let Some(menu) = s.member_menu.as_mut() {
                            menu.confirm_block = true;
                        }
                        cx.notify();
                    })),
                );
            }
        }
        if self.member_action_allowed(crate::settings::MemberAction::SetRole, menu.user_id) {
            let mut roles = div()
                .h(px(36.))
                .flex()
                .items_center()
                .gap_1()
                .px(px(9.))
                .child(div().flex_1().text_color(ink(p.muted)).child("Роль"));
            for (role, selected) in crate::settings::role_choices(target) {
                let label = if role == crate::settings::Role::Admin {
                    "Админ"
                } else {
                    "Участник"
                };
                let button = div()
                    .id(("member-role", role as u64))
                    .px(px(6.))
                    .py(px(3.))
                    .rounded(px(5.))
                    .bg(ink(if selected { p.lime } else { p.selection }))
                    .text_color(ink(if selected { 0x17170f } else { p.fg }))
                    .child(if selected {
                        format!("✓ {label}")
                    } else {
                        label.to_owned()
                    });
                roles = roles.child(if selected {
                    button
                } else {
                    let value = role.as_str();
                    button.cursor_pointer().on_click(
                        cx.listener(move |s, _, _, cx| {
                            s.change_member_role(menu.user_id, value, cx)
                        }),
                    )
                });
            }
            panel = panel.child(roles);
        }
        if self.member_action_allowed(crate::settings::MemberAction::Ban, menu.user_id) {
            if menu.confirm_ban {
                panel = panel
                    .child(format!("Забанить {name}?"))
                    .child(
                        div()
                            .id("member-ban-confirm")
                            .cursor_pointer()
                            .text_color(ink(p.danger))
                            .on_click(
                                cx.listener(move |s, _, _, cx| s.ban_member(menu.user_id, cx)),
                            )
                            .child("Забанить"),
                    )
                    .child(
                        div()
                            .id("member-ban-cancel")
                            .cursor_pointer()
                            .text_color(ink(p.muted))
                            .on_click(cx.listener(|s, _, _, cx| {
                                if let Some(menu) = s.member_menu.as_mut() {
                                    menu.confirm_ban = false;
                                }
                                cx.notify();
                            }))
                            .child("Отмена"),
                    );
            } else {
                panel = panel.child(
                    context_item(
                        "member-ban",
                        "Забанить на инстансе",
                        "icons/warning.svg",
                        true,
                        p,
                    )
                    .on_click(cx.listener(|s, _, _, cx| {
                        if let Some(menu) = s.member_menu.as_mut() {
                            menu.confirm_ban = true;
                        }
                        cx.notify();
                    })),
                );
            }
        }
        if menu.voice && menu.user_id != self.self_user_id {
            let volume = self
                .voice_settings
                .prefs
                .user_volumes
                .get(&menu.user_id)
                .copied()
                .unwrap_or(100);
            panel = panel
                .child(context_separator(p))
                .child(self.settings_slider(
                    "member-volume",
                    "Громкость",
                    volume,
                    200,
                    move |value| {
                        SettingsAct::Personal(personal_settings::Action::SetVoiceUserVolume(
                            menu.user_id,
                            value,
                        ))
                    },
                    window,
                    cx,
                ));
        }
        Some(panel.into_any_element())
    }
}
