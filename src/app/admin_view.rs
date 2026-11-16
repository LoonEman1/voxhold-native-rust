//! Server administration, channel administration and channel notifications.

use super::settings_view::ButtonKind;
use super::*;
use gpui::ElementId;

impl Voxhold {
    pub(super) fn server_body(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tab = self
            .server_settings
            .as_ref()
            .map(|panel| panel.dialog.active)
            .unwrap_or(0);
        match tab {
            1 => self.server_roles_body(window, cx),
            2 => self.server_storage_body(window, cx),
            3 => self.server_bans_body(window, cx),
            _ => self.server_general_body(window, cx),
        }
    }

    fn server_general_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette;
        let Some(panel) = &self.server_settings else {
            return div().into_any_element();
        };
        let name = panel.name.clone();
        let cors = panel.cors.clone();
        let loaded = panel.general_loaded;
        let notice = panel.general_notice.clone();
        let mut root = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(self.heading("Название", "Его увидят все участники пространства."));
        root = root
            .child(self.form_text("Название сервера", false))
            .child(self.settings_input(&name, cx))
            .child(self.settings_button(
                ElementId::Name("server-rename".into()),
                "Сохранить",
                ButtonKind::Secondary,
                false,
                false,
                SettingsAct::Server(server_settings::Action::SaveName),
                window,
                cx,
            ))
            .child(self.separator())
            .child(self.heading(
                "Доступ внешних клиентов",
                "По одному точному origin на строку: схема, домен и необязательный порт.",
            ))
            .child(self.form_text("Разрешённые origin", false))
            .child(self.settings_input(&cors, cx))
            .child(self.settings_button(
                ElementId::Name("server-cors".into()),
                "Сохранить CORS",
                ButtonKind::Secondary,
                false,
                !loaded,
                SettingsAct::Server(server_settings::Action::SaveCors),
                window,
                cx,
            ))
            .child(self.separator())
            .child(self.heading(
                "Диагностика клиентов",
                "HTTP, WebSocket и WebRTC-метрики хранятся на сервере 24 часа.",
            ))
            .child(self.settings_button(
                ElementId::Name("server-diagnostics".into()),
                "Скачать JSON",
                ButtonKind::Secondary,
                false,
                !loaded,
                SettingsAct::Server(server_settings::Action::Diagnostics),
                window,
                cx,
            ));
        if !notice.is_empty() {
            root = root.child(self.form_text(&notice, false));
        }
        root = root.child(self.separator()).child(self.heading(
            "Владелец инстанса",
            "Владелец не может удалить пространство или свой аккаунт из приложения.",
        ));
        let _ = p;
        root.into_any_element()
    }

    fn server_roles_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette;
        let Some(panel) = &self.server_settings else {
            return div().into_any_element();
        };
        let loaded = panel.roles_loaded;
        let error = panel.roles_error.clone();
        let roles = panel.roles.clone();
        let editing = panel.role_edit.clone();
        let role_name = panel.role_name.clone();
        let role_color = panel.role_color.clone();
        let deleting = panel.role_delete;
        let preview = panel.role_preview.clone();
        let preview_for = panel.role_preview_for;
        let members = panel.members.clone();
        let target = panel.role_target;
        let assigned = panel.assigned.clone();
        let channels = panel.restricted_channels.clone();
        let grant_channel = panel.grant_channel;
        let grants = panel.grants.clone();
        let grants_loaded = panel.grants_loaded;
        let mut root = div().flex().flex_col().gap(px(10.)).child(self.heading(
            "Роли и права",
            "Права нескольких ролей складываются. Владелец и администраторы сохраняют полный доступ.",
        ));
        if !loaded {
            root =
                root.child(self.form_text("Загрузка ролей…", false))
                    .child(self.settings_button(
                        ElementId::Name("roles-retry".into()),
                        "Повторить",
                        ButtonKind::Secondary,
                        false,
                        false,
                        SettingsAct::Server(server_settings::Action::Retry(1)),
                        window,
                        cx,
                    ));
        } else {
            if !error.is_empty() {
                root = root.child(self.form_text(&error, true));
            }
            for role in &roles {
                let id = role.id;
                root = root
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div().w(px(10.)).h(px(10.)).rounded_full().bg(role
                                    .color
                                    .trim_start_matches('#')
                                    .chars()
                                    .take(6)
                                    .collect::<String>()
                                    .parse::<u32>()
                                    .ok()
                                    .map(|value| super::ui::ink(0x1000000 | value))
                                    .unwrap_or(ink(p.muted))),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_size(px(13.))
                                    .child(format!("{} · {}", role.name, role.color)),
                            ),
                    )
                    .child(self.settings_button(
                        ElementId::Name(format!("role-edit-{id}").into()),
                        "Изменить",
                        ButtonKind::Secondary,
                        false,
                        false,
                        SettingsAct::Server(server_settings::Action::RoleEdit(id)),
                        window,
                        cx,
                    ))
                    .child(self.settings_button(
                        ElementId::Name(format!("role-preview-{id}").into()),
                        "Посмотреть доступ",
                        ButtonKind::Secondary,
                        false,
                        false,
                        SettingsAct::Server(server_settings::Action::RolePreview(id)),
                        window,
                        cx,
                    ));
                if deleting == Some(id) {
                    root = root
                        .child(self.settings_button(
                            ElementId::Name(format!("role-delete-{id}").into()),
                            "Подтвердить удаление",
                            ButtonKind::Danger,
                            false,
                            false,
                            SettingsAct::Server(server_settings::Action::RoleDeleteConfirm),
                            window,
                            cx,
                        ))
                        .child(self.settings_button(
                            ElementId::Name(format!("role-delete-cancel-{id}").into()),
                            "Отмена",
                            ButtonKind::Secondary,
                            false,
                            false,
                            SettingsAct::Server(server_settings::Action::RoleDeleteCancel),
                            window,
                            cx,
                        ));
                } else {
                    root = root.child(self.settings_button(
                        ElementId::Name(format!("role-remove-{id}").into()),
                        "Удалить роль",
                        ButtonKind::Danger,
                        false,
                        false,
                        SettingsAct::Server(server_settings::Action::RoleDelete(id)),
                        window,
                        cx,
                    ));
                }
            }
            root = root.child(self.settings_button(
                ElementId::Name("role-create".into()),
                "Создать роль",
                ButtonKind::Secondary,
                false,
                false,
                SettingsAct::Server(server_settings::Action::RoleCreate),
                window,
                cx,
            ));
            if let Some(edit) = &editing {
                root = root
                    .child(self.separator())
                    .child(self.form_text("Название роли", false))
                    .child(self.settings_input(&role_name, cx))
                    .child(self.form_text("Цвет (#RRGGBB)", false))
                    .child(self.settings_input(&role_color, cx))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("Разрешения"),
                    );
                for (bit, label) in crate::settings::PERMISSION_BITS {
                    root = root.child(self.settings_option(
                        ElementId::Name(format!("right-{bit}").into()),
                        label,
                        "",
                        edit.permissions & bit != 0,
                        false,
                        SettingsAct::Server(server_settings::Action::RoleToggle(bit)),
                        window,
                        cx,
                    ));
                }
                root = root
                    .child(self.settings_button(
                        ElementId::Name("role-save".into()),
                        "Сохранить роль",
                        ButtonKind::Primary,
                        false,
                        false,
                        SettingsAct::Server(server_settings::Action::RoleSave),
                        window,
                        cx,
                    ))
                    .child(self.settings_button(
                        ElementId::Name("role-cancel".into()),
                        "Отмена",
                        ButtonKind::Secondary,
                        false,
                        false,
                        SettingsAct::Server(server_settings::Action::RoleCancel),
                        window,
                        cx,
                    ));
            }
            if let Some(preview) = preview {
                root = root
                    .child(self.separator())
                    .child(self.heading("Доступ от имени роли", ""));
                if preview.is_empty() {
                    root = root.child(self.form_text("Нет доступных каналов", false));
                }
                for row in preview {
                    root =
                        root.child(self.form_text(
                            &format!("#{} · права {}", row.name, row.permissions),
                            false,
                        ));
                }
                let _ = preview_for;
            }
            if !members.is_empty() {
                root = root.child(self.separator()).child(self.heading(
                    "Роли участников",
                    "Роль заменяет обычные права участника. Владелец не может получить пользовательскую роль.",
                ));
                for (index, member) in members.iter().enumerate() {
                    root = root.child(self.settings_button(
                        ElementId::Name(format!("assign-target-{index}").into()),
                        member.username.clone(),
                        if index == target {
                            ButtonKind::Tab
                        } else {
                            ButtonKind::Secondary
                        },
                        index == target,
                        false,
                        SettingsAct::Server(server_settings::Action::RoleTarget(index)),
                        window,
                        cx,
                    ));
                }
                for role in &roles {
                    root = root.child(self.settings_option(
                        ElementId::Name(format!("assign-{}", role.id).into()),
                        &role.name,
                        "",
                        assigned.contains(&role.id),
                        false,
                        SettingsAct::Server(server_settings::Action::AssignToggle(role.id)),
                        window,
                        cx,
                    ));
                }
                root = root.child(self.settings_button(
                    ElementId::Name("assign-save".into()),
                    "Назначить роли",
                    ButtonKind::Secondary,
                    false,
                    false,
                    SettingsAct::Server(server_settings::Action::AssignSave),
                    window,
                    cx,
                ));
            }
            if !channels.is_empty() {
                root = root
                    .child(self.separator())
                    .child(self.heading("Роли в закрытых каналах", ""));
                for (index, (_, name)) in channels.iter().enumerate() {
                    root = root.child(self.settings_button(
                        ElementId::Name(format!("grant-channel-{index}").into()),
                        name.clone(),
                        if index == grant_channel {
                            ButtonKind::Tab
                        } else {
                            ButtonKind::Secondary
                        },
                        index == grant_channel,
                        false,
                        SettingsAct::Server(server_settings::Action::GrantChannel(index)),
                        window,
                        cx,
                    ));
                }
                for role in &roles {
                    root = root.child(self.settings_option(
                        ElementId::Name(format!("grant-{}", role.id).into()),
                        &role.name,
                        "",
                        grants.contains(&role.id),
                        false,
                        SettingsAct::Server(server_settings::Action::GrantToggle(role.id)),
                        window,
                        cx,
                    ));
                }
                root = root.child(self.settings_button(
                    ElementId::Name("grants-save".into()),
                    "Сохранить доступ",
                    ButtonKind::Secondary,
                    false,
                    !grants_loaded,
                    SettingsAct::Server(server_settings::Action::GrantsSave),
                    window,
                    cx,
                ));
            }
        }
        root.into_any_element()
    }

    fn server_storage_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(panel) = &self.server_settings else {
            return div().into_any_element();
        };
        let loaded = panel.storage_loaded;
        let error = panel.storage_error.clone();
        let state = panel.storage.clone();
        let quotas = panel.quotas.clone();
        let category = panel.cleanup.category;
        let period = panel.cleanup.period;
        let plan = panel.cleanup.confirmed().cloned();
        let result = panel.storage_result.clone();
        let mut root = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(self.heading("Хранилище", "Распределение места для вложений и записей."));
        if !loaded {
            return root
                .child(self.form_text(
                    if error.is_empty() {
                        "Загрузка хранилища…"
                    } else {
                        &error
                    },
                    false,
                ))
                .child(self.settings_button(
                    ElementId::Name("storage-retry".into()),
                    "Повторить",
                    ButtonKind::Secondary,
                    false,
                    false,
                    SettingsAct::Server(server_settings::Action::Retry(2)),
                    window,
                    cx,
                ))
                .into_any_element();
        }
        let state = state.expect("storage state");
        root = root.child(self.form_text(
            &format!(
                "Доступно {} · свободно {} · резерв {}%",
                crate::settings::storage_size(state.capacity_bytes),
                crate::settings::storage_size(state.available_bytes),
                state.reserve_percent
            ),
            false,
        ));
        for index in 0..3 {
            let used = state
                .categories
                .get(crate::settings::CLEANUP_CATEGORIES[index])
                .map(|value| value.used_bytes)
                .unwrap_or(0);
            root = root
                .child(self.form_text(
                    &format!(
                        "{} · занято {} · лимит ГБ",
                        crate::settings::CLEANUP_CATEGORY_LABELS[index],
                        crate::settings::storage_size(used)
                    ),
                    false,
                ))
                .child(self.settings_input(&quotas[index], cx));
        }
        root = root
            .child(self.settings_button(
                ElementId::Name("storage-save".into()),
                "Сохранить распределение",
                ButtonKind::Secondary,
                false,
                false,
                SettingsAct::Server(server_settings::Action::StorageSave),
                window,
                cx,
            ))
            .child(self.separator())
            .child(self.heading(
                "Управление ресурсами",
                "Перед очисткой сервер покажет число файлов и освобождаемый объём. В сообщениях останется заглушка.",
            ))
            .child(
                div()
                    .text_size(px(13.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Категория"),
            );
        for (index, label) in crate::settings::CLEANUP_CATEGORY_LABELS.iter().enumerate() {
            root = root.child(self.settings_button(
                ElementId::Name(format!("cleanup-category-{index}").into()),
                *label,
                if index == category {
                    ButtonKind::Tab
                } else {
                    ButtonKind::Secondary
                },
                index == category,
                false,
                SettingsAct::Server(server_settings::Action::CleanupCategory(index)),
                window,
                cx,
            ));
        }
        root = root.child(
            div()
                .text_size(px(13.))
                .font_weight(gpui::FontWeight::BOLD)
                .child("Период"),
        );
        for (index, label) in crate::settings::CLEANUP_PERIOD_LABELS.iter().enumerate() {
            root = root.child(self.settings_button(
                ElementId::Name(format!("cleanup-period-{index}").into()),
                *label,
                if index == period {
                    ButtonKind::Tab
                } else {
                    ButtonKind::Secondary
                },
                index == period,
                false,
                SettingsAct::Server(server_settings::Action::CleanupPeriod(index)),
                window,
                cx,
            ));
        }
        root = root.child(self.settings_button(
            ElementId::Name("cleanup-preview".into()),
            "Рассчитать очистку",
            ButtonKind::Secondary,
            false,
            false,
            SettingsAct::Server(server_settings::Action::CleanupPreview),
            window,
            cx,
        ));
        if let Some(plan) = plan {
            root = root
                .child(self.form_text(
                    &format!(
                        "Будет удалено: {} · {}. Расчёт действует {} минут.",
                        plan.count,
                        crate::settings::storage_size(plan.bytes),
                        crate::settings::CLEANUP_PLAN_SECONDS / 60
                    ),
                    false,
                ))
                .child(self.settings_button(
                    ElementId::Name("cleanup-confirm".into()),
                    "Подтвердить удаление",
                    ButtonKind::Danger,
                    false,
                    plan.count == 0,
                    SettingsAct::Server(server_settings::Action::CleanupConfirm),
                    window,
                    cx,
                ));
        }
        if !result.is_empty() {
            root = root.child(self.form_text(&result, false));
        }
        root.into_any_element()
    }

    fn server_bans_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(panel) = &self.server_settings else {
            return div().into_any_element();
        };
        let loaded = panel.bans_loaded;
        let error = panel.bans_error.clone();
        let bans = panel.bans.clone();
        let confirming = panel.confirm_unban;
        let mut root = div().flex().flex_col().gap(px(10.)).child(self.heading(
            "Блокировки",
            "После разблокировки пользователю понадобится новое приглашение.",
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
                    ElementId::Name("bans-retry".into()),
                    "Повторить",
                    ButtonKind::Secondary,
                    false,
                    false,
                    SettingsAct::Server(server_settings::Action::Retry(3)),
                    window,
                    cx,
                ))
                .into_any_element();
        }
        if bans.is_empty() {
            root = root.child(self.form_text("Заблокированных пользователей нет.", false));
        }
        for ban in &bans {
            root =
                root.child(self.form_text(&format!("@{} · {}", ban.username, ban.reason), false));
            if confirming == Some(ban.user_id) {
                root = root
                    .child(self.settings_button(
                        ElementId::Name(format!("unban-confirm-{}", ban.user_id).into()),
                        "Подтвердить разблокировку",
                        ButtonKind::Danger,
                        false,
                        false,
                        SettingsAct::Server(server_settings::Action::UnbanConfirm),
                        window,
                        cx,
                    ))
                    .child(self.settings_button(
                        ElementId::Name(format!("unban-cancel-{}", ban.user_id).into()),
                        "Отмена",
                        ButtonKind::Secondary,
                        false,
                        false,
                        SettingsAct::Server(server_settings::Action::UnbanCancel),
                        window,
                        cx,
                    ));
            } else {
                root = root.child(self.settings_button(
                    ElementId::Name(format!("unban-{}", ban.user_id).into()),
                    "Разбанить",
                    ButtonKind::Secondary,
                    false,
                    false,
                    SettingsAct::Server(server_settings::Action::Unban(ban.user_id)),
                    window,
                    cx,
                ));
            }
        }
        root.into_any_element()
    }
}
