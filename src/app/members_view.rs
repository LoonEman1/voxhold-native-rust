use super::*;

use crate::members::{
    avatar_color, avatar_frame, initials, presence_label, role, Roster, AVATAR_CONCURRENCY,
};
use gpui::{AnyElement, Context, Window};

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
                        .h(px(72.))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_3()
                        .cursor_pointer()
                        .track_focus(&row_focus)
                        .tab_index(0)
                        .on_key_down(cx.listener(move |s, event: &gpui::KeyDownEvent, _, cx| {
                            if event.keystroke.key == "enter" || event.keystroke.key == "space" {
                                let x = (s.viewport.0 - 290.).max(12.);
                                s.open_member_menu(menu_x, x, 180., cx);
                            }
                        }))
                        .on_click(cx.listener(move |s, event: &gpui::ClickEvent, _, cx| {
                            let position = event.position();
                            s.open_member_menu(menu_x, position.x.into(), position.y.into(), cx);
                        }))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |s, event: &gpui::MouseDownEvent, _, cx| {
                                let position = event.position;
                                s.open_member_menu(
                                    menu_x,
                                    position.x.into(),
                                    position.y.into(),
                                    cx,
                                );
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

    fn open_member_menu(&mut self, user_id: i64, x: f32, y: f32, cx: &mut Context<Self>) {
        if self.members.iter().all(|member| member.user_id != user_id) {
            return;
        }
        self.message_menu = None;
        self.forward = None;
        self.member_menu = Some(MemberMenu {
            user_id,
            x,
            y,
            profile: false,
            confirm_block: false,
            confirm_ban: false,
        });
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

    pub(super) fn member_menu_overlay(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.member_menu.clone()?;
        let member = self.member_snapshot(menu.user_id)?;
        let target = crate::settings::Role::parse(&member.role);
        let p = self.palette;
        let x = menu.x.clamp(12., (self.viewport.0 - 280.).max(12.));
        let y = menu.y.clamp(12., (self.viewport.1 - 300.).max(12.));
        let mut panel = div()
            .id("member-menu")
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(268.))
            .occlude()
            .rounded(px(10.))
            .shadow_lg()
            .border_1()
            .border_color(ink(p.line))
            .bg(ink(p.surface))
            .text_color(ink(p.fg))
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .text_size(px(12.))
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .truncate()
                    .child(member.username.clone()),
            );
        if menu.profile {
            panel = panel
                .child(div().text_color(ink(p.muted)).child(target.label()))
                .child(
                    div()
                        .id("member-menu-back")
                        .cursor_pointer()
                        .text_color(ink(p.muted))
                        .on_click(cx.listener(|s, _, _, cx| {
                            if let Some(menu) = s.member_menu.as_mut() {
                                menu.profile = false;
                            }
                            cx.notify();
                        }))
                        .child("Назад"),
                );
            return Some(panel.into_any_element());
        }
        debug_assert!(
            self.member_action_allowed(crate::settings::MemberAction::ViewProfile, menu.user_id)
        );
        panel = panel.child(
            div()
                .id("member-profile")
                .cursor_pointer()
                .on_click(cx.listener(|s, _, _, cx| {
                    if let Some(menu) = s.member_menu.as_mut() {
                        menu.profile = true;
                    }
                    cx.notify();
                }))
                .child("Просмотреть профиль"),
        );
        let other =
            self.member_action_allowed(crate::settings::MemberAction::BlockMessages, menu.user_id);
        if other {
            if menu.confirm_block {
                panel = panel
                    .child(format!("Заблокировать {}?", member.username))
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
                    div()
                        .id("member-block")
                        .cursor_pointer()
                        .on_click(cx.listener(|s, _, _, cx| {
                            if let Some(menu) = s.member_menu.as_mut() {
                                menu.confirm_block = true;
                            }
                            cx.notify();
                        }))
                        .child("Заблокировать личные сообщения"),
                );
            }
        }
        if self.member_action_allowed(crate::settings::MemberAction::SetRole, menu.user_id) {
            panel = panel.child(div().text_color(ink(p.muted)).child("Роль"));
            for (role, selected) in crate::settings::role_choices(target) {
                let label = if role == crate::settings::Role::Admin {
                    "Админ"
                } else {
                    "Участник"
                };
                let button = div()
                    .id(("member-role", role as u64))
                    .px_1()
                    .py(px(3.))
                    .rounded(px(5.))
                    .when(selected, |row| row.bg(ink(p.selection)))
                    .child(if selected {
                        format!("✓ {label}")
                    } else {
                        label.to_owned()
                    });
                panel = panel.child(if selected {
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
        }
        if self.member_action_allowed(crate::settings::MemberAction::Ban, menu.user_id) {
            if menu.confirm_ban {
                panel = panel
                    .child(format!("Забанить {}?", member.username))
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
                    div()
                        .id("member-ban")
                        .cursor_pointer()
                        .text_color(ink(p.danger))
                        .on_click(cx.listener(|s, _, _, cx| {
                            if let Some(menu) = s.member_menu.as_mut() {
                                menu.confirm_ban = true;
                            }
                            cx.notify();
                        }))
                        .child("Забанить на инстансе"),
                );
            }
        }
        Some(panel.into_any_element())
    }
}
