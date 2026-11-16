//! Channel settings and channel notifications views.

use super::settings_view::ButtonKind;
use super::*;
use gpui::ElementId;

impl Voxhold {
    pub(super) fn channel_body(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self
            .channel_settings
            .as_ref()
            .map(|panel| panel.dialog.active)
            .unwrap_or(0);
        match active {
            1 => self.channel_access_body(window, cx),
            2 => self.channel_news_body(window, cx),
            _ => self.channel_general_body(window, cx),
        }
    }

    pub(super) fn channel_general_body(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(panel) = &self.channel_settings else {
            return div().into_any_element();
        };
        let kind = panel.kind.clone();
        let allowed = panel.allowed;
        let name = panel.name_input.clone();
        let slow = panel.slow_input.clone();
        let encryption = panel.encryption;
        let confirm_delete = panel.confirm_delete;
        let error = panel.section_error.clone();
        let mut root = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(self.heading("Название канала", "Тип канала останется прежним."));
        if !allowed {
            return root
                .child(self.form_text("Недостаточно прав.", true))
                .into_any_element();
        }
        root = root
            .child(self.form_text("Название", false))
            .child(self.settings_input(&name, cx))
            .child(self.settings_button(
                ElementId::Name("channel-rename".into()),
                "Сохранить",
                ButtonKind::Secondary,
                false,
                false,
                SettingsAct::Channel(channel_settings::Action::SaveName),
                window,
                cx,
            ));
        if kind == "text" {
            root = root
                .child(self.separator())
                .child(self.heading(
                    "Медленный режим",
                    "Задержка между сообщениями участников, от 0 до 21600 секунд.",
                ))
                .child(self.form_text("Секунды", false))
                .child(self.settings_input(&slow, cx))
                .child(self.settings_button(
                    ElementId::Name("channel-slow".into()),
                    "Сохранить",
                    ButtonKind::Secondary,
                    false,
                    false,
                    SettingsAct::Channel(channel_settings::Action::SaveSlow),
                    window,
                    cx,
                ));
        }
        if kind == "voice" {
            root = root
                .child(self.separator())
                .child(self.heading(
                    "Сквозное шифрование голоса",
                    "При изменении политики участники голосового канала переподключатся.",
                ))
                .child(self.settings_toggle(
                    ElementId::Name("channel-e2ee".into()),
                    "Требовать сквозное шифрование",
                    "Для подключения понадобится клиент с поддержкой шифрования голоса.",
                    encryption,
                    false,
                    SettingsAct::Channel(channel_settings::Action::ToggleEncryption),
                    window,
                    cx,
                ));
        }
        if !error.is_empty() {
            root = root.child(self.form_text(&error, true));
        }
        root = root.child(self.separator()).child(self.heading(
            "Удалить канал",
            "Канал и его история исчезнут для всех участников.",
        ));
        if confirm_delete {
            root = root
                .child(self.settings_button(
                    ElementId::Name("channel-delete-confirm".into()),
                    "Подтвердить удаление",
                    ButtonKind::Danger,
                    false,
                    false,
                    SettingsAct::Channel(channel_settings::Action::ConfirmDelete),
                    window,
                    cx,
                ))
                .child(self.settings_button(
                    ElementId::Name("channel-delete-cancel".into()),
                    "Отмена",
                    ButtonKind::Secondary,
                    false,
                    false,
                    SettingsAct::Channel(channel_settings::Action::CancelDelete),
                    window,
                    cx,
                ));
        } else {
            root = root.child(self.settings_button(
                ElementId::Name("channel-delete".into()),
                "Удалить канал",
                ButtonKind::Danger,
                false,
                false,
                SettingsAct::Channel(channel_settings::Action::Delete),
                window,
                cx,
            ));
        }
        root.into_any_element()
    }

    pub(super) fn channel_access_body(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(panel) = &self.channel_settings else {
            return div().into_any_element();
        };
        let access = panel.access.clone();
        let restricted = panel.restricted;
        let member_role = panel.member_role;
        let members = panel.members.clone();
        let user_ids = panel.user_ids.clone();
        let error = panel.section_error.clone();
        let mut root = div().flex().flex_col().gap(px(10.)).child(self.heading(
            "Доступ к каналу",
            "Выберите роли и участников, которым доступен закрытый канал.",
        ));
        let Some(_access) = access else {
            root = root.child(self.form_text(
                if error.is_empty() {
                    "Загрузка…"
                } else {
                    &error
                },
                !error.is_empty(),
            ));
            if !error.is_empty() {
                root = root.child(self.settings_button(
                    ElementId::Name("channel-access-retry".into()),
                    "Повторить",
                    ButtonKind::Secondary,
                    false,
                    false,
                    SettingsAct::Channel(channel_settings::Action::Retry(1)),
                    window,
                    cx,
                ));
            }
            return root.into_any_element();
        };
        root = root
            .child(self.settings_toggle(
                ElementId::Name("channel-restricted".into()),
                "Закрытый канал",
                "Ограничить доступ выбранными ролями и участниками.",
                restricted,
                false,
                SettingsAct::Channel(channel_settings::Action::ToggleRestricted),
                window,
                cx,
            ))
            .child(self.settings_toggle(
                ElementId::Name("channel-member-role".into()),
                "Участники",
                "Права роли действуют для всех обычных участников.",
                member_role,
                false,
                SettingsAct::Channel(channel_settings::Action::ToggleMemberRole),
                window,
                cx,
            ));
        for member in &members {
            root = root.child(self.settings_option(
                ElementId::Name(format!("channel-user-{}", member.user_id).into()),
                &member.username,
                "",
                user_ids.contains(&member.user_id),
                false,
                SettingsAct::Channel(channel_settings::Action::ToggleUser(member.user_id)),
                window,
                cx,
            ));
        }
        if !error.is_empty() {
            root = root.child(self.form_text(&error, true));
        }
        root = root.child(self.settings_button(
            ElementId::Name("channel-access-save".into()),
            "Сохранить доступ",
            ButtonKind::Secondary,
            false,
            false,
            SettingsAct::Channel(channel_settings::Action::SaveAccess),
            window,
            cx,
        ));
        root.into_any_element()
    }

    pub(super) fn channel_news_body(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(panel) = &self.channel_settings else {
            return div().into_any_element();
        };
        let news = panel.news.clone();
        let comments = panel.comments;
        let members = panel.members.clone();
        let publishers = panel.publishers.clone();
        let error = panel.section_error.clone();
        let mut root = div().flex().flex_col().gap(px(10.)).child(self.heading(
            "Новости",
            "Выберите авторов публикаций и настройте комментарии.",
        ));
        if news.is_none() {
            root = root.child(self.form_text(
                if error.is_empty() {
                    "Загрузка…"
                } else {
                    &error
                },
                !error.is_empty(),
            ));
            if !error.is_empty() {
                root = root.child(self.settings_button(
                    ElementId::Name("channel-news-retry".into()),
                    "Повторить",
                    ButtonKind::Secondary,
                    false,
                    false,
                    SettingsAct::Channel(channel_settings::Action::Retry(2)),
                    window,
                    cx,
                ));
            }
            return root.into_any_element();
        }
        root = root.child(self.settings_toggle(
            ElementId::Name("news-comments".into()),
            "Комментарии",
            "Отдельное обсуждение под каждой публикацией. При отключении история сохраняется.",
            comments,
            false,
            SettingsAct::Channel(channel_settings::Action::ToggleComments),
            window,
            cx,
        ));
        for member in &members {
            root = root.child(self.settings_option(
                ElementId::Name(format!("publisher-{}", member.user_id).into()),
                &member.username,
                "",
                publishers.contains(&member.user_id),
                false,
                SettingsAct::Channel(channel_settings::Action::TogglePublisher(member.user_id)),
                window,
                cx,
            ));
        }
        if !error.is_empty() {
            root = root.child(self.form_text(&error, true));
        }
        root = root.child(self.settings_button(
            ElementId::Name("news-save".into()),
            "Сохранить",
            ButtonKind::Secondary,
            false,
            false,
            SettingsAct::Channel(channel_settings::Action::SaveNews),
            window,
            cx,
        ));
        root.into_any_element()
    }

    pub(super) fn notifications_body(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(panel) = &self.channel_notifications else {
            return div().into_any_element();
        };
        let name = panel.name.clone();
        let mode = panel.mode.clone();
        let saved = panel.saved.clone();
        let loaded = panel.loaded;
        let saving = panel.saving;
        let error = panel.error.clone();
        let notifications = self.notifications.enabled;
        let mut root = div().flex().flex_col().gap(px(10.)).child(self.heading(
            &format!("#{name}"),
            "Настройка сохраняется на аккаунте для этого канала. Уведомления приходят, пока Voxhold открыт.",
        ));
        if !loaded {
            return root
                .child(self.form_text(
                    if error.is_empty() {
                        "Загрузка…"
                    } else {
                        &error
                    },
                    false,
                ))
                .child(self.settings_button(
                    ElementId::Name("notifications-retry".into()),
                    "Повторить загрузку",
                    ButtonKind::Secondary,
                    false,
                    false,
                    SettingsAct::Notifications(channel_settings::NotifAction::Retry),
                    window,
                    cx,
                ))
                .into_any_element();
        }
        for (index, (value, label)) in crate::settings::NOTIFICATION_MODES
            .iter()
            .zip(crate::settings::NOTIFICATION_LABELS.iter())
            .enumerate()
        {
            root = root.child(self.settings_option(
                ElementId::Name(format!("notification-{value}").into()),
                label,
                crate::settings::NOTIFICATION_DESCRIPTIONS[index],
                mode == *value,
                false,
                SettingsAct::Notifications(channel_settings::NotifAction::Choose(index)),
                window,
                cx,
            ));
        }
        if !error.is_empty() {
            root = root.child(self.form_text(&error, true));
        }
        root = root
            .child(self.separator())
            .child(self.settings_toggle(
                ElementId::Name("notification-system".into()),
                "Системные уведомления",
                "Показывать события, пока Voxhold открыт в фоне.",
                notifications,
                false,
                SettingsAct::Notifications(channel_settings::NotifAction::ToggleNotifications),
                window,
                cx,
            ))
            .child(self.separator())
            .child(self.settings_button(
                ElementId::Name("notifications-save".into()),
                "Сохранить",
                ButtonKind::Primary,
                false,
                saving || mode == saved,
                SettingsAct::Notifications(channel_settings::NotifAction::Save),
                window,
                cx,
            ));
        root.into_any_element()
    }
}
