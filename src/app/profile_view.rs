//! Profile tab of the personal settings dialog.

use super::settings_view::ButtonKind;
use super::*;
use gpui::ElementId;

impl Voxhold {
    pub(super) fn profile_body(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette;
        let Some(panel) = &self.personal else {
            return div().into_any_element();
        };
        let profile = panel.profile.clone();
        let loading = panel.profile_loading;
        let profile_saving = self.active_profile_save.is_some();
        let avatar = panel.avatar.clone();
        let crop = panel
            .crop
            .as_ref()
            .and_then(|draft| Some((draft.preview.clone(), draft.crop, draft.preview_token)));
        let about = panel.about.clone();
        let country_search = panel.country_search.clone();
        let country_index = panel.country_index;
        let country_open = panel.country_open;
        let countries = panel.countries.clone();
        let validation = panel.validation.clone();
        let local_error = panel.local_error.clone();
        let notice = panel.notice.clone();
        let password_current = panel.password_current.clone();
        let password_next = panel.password_next.clone();
        let password_confirm = panel.password_confirm.clone();
        let sessions = panel.sessions.clone();
        let sessions_loading = panel.sessions_loading;
        let sessions_error = panel.sessions_error.clone();
        let delete_confirm = panel.delete_confirm;
        let delete_name = panel.delete_name.clone();
        let delete_password = panel.delete_password.clone();
        let theme = self.theme_mode;
        let has_avatar = panel.avatar_version.is_some() && !panel.avatar_missing;
        let is_owner = self.servers.iter().any(|server| server.role == "owner");

        let mut root = div().flex().flex_col().gap(px(12.));
        root =
            root.child(self.heading("Профиль", "Расскажите о себе и выберите страну проживания."));
        if profile.is_none() {
            root = root.child(self.form_text(
                if loading {
                    "Загружаем профиль…"
                } else {
                    "Профиль недоступен."
                },
                false,
            ));
            if !loading {
                root = root.child(self.settings_button(
                    ElementId::Name("profile-retry".into()),
                    "Повторить",
                    ButtonKind::Secondary,
                    false,
                    false,
                    SettingsAct::Personal(personal_settings::Action::Retry),
                    window,
                    cx,
                ));
            }
        } else if let Some(profile) = &profile {
            let face = if let Some(image) = avatar {
                gpui::img(image)
                    .size(px(62.))
                    .rounded_full()
                    .object_fit(gpui::ObjectFit::Cover)
                    .into_any_element()
            } else {
                div()
                    .w(px(62.))
                    .h(px(62.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(ink(crate::members::avatar_color(&profile.username)))
                    .text_color(ink(0x151510))
                    .text_size(px(22.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(crate::members::initials(&profile.username))
                    .into_any_element()
            };
            root =
                root.child(
                    div().flex().items_center().gap_3().child(face).child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(17.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child(format!("@{}", profile.username)),
                            )
                            .child(div().text_size(px(12.)).text_color(ink(p.muted)).child(
                                format!(
                                    "В Voxhold с {}",
                                    crate::settings::format_date(profile.created_at)
                                ),
                            )),
                    ),
                );
            if let Some((preview, crop, _)) = crop {
                root = root.child(
                    div()
                        .w(px(120.))
                        .h(px(120.))
                        .rounded_full()
                        .overflow_hidden()
                        .bg(ink(p.surface))
                        .children(preview.map(|image| {
                            gpui::img(image)
                                .size_full()
                                .object_fit(gpui::ObjectFit::Cover)
                        })),
                );
                root = root
                    .child(self.settings_value_slider(
                        "avatar-zoom",
                        "Приближение",
                        i64::from(crop.zoom),
                        100,
                        5,
                        |value| {
                            SettingsAct::Personal(personal_settings::Action::CropZoom(value as i32))
                        },
                        window,
                        cx,
                    ))
                    .child(self.settings_value_slider(
                        "avatar-x",
                        "По горизонтали",
                        i64::from(crop.x),
                        100,
                        5,
                        |value| {
                            SettingsAct::Personal(personal_settings::Action::CropX(value as i32))
                        },
                        window,
                        cx,
                    ))
                    .child(self.settings_value_slider(
                        "avatar-y",
                        "По вертикали",
                        i64::from(crop.y),
                        100,
                        5,
                        |value| {
                            SettingsAct::Personal(personal_settings::Action::CropY(value as i32))
                        },
                        window,
                        cx,
                    ))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(self.settings_button(
                                ElementId::Name("avatar-save".into()),
                                "Сохранить аватар",
                                ButtonKind::Primary,
                                false,
                                false,
                                SettingsAct::Personal(personal_settings::Action::SaveAvatar),
                                window,
                                cx,
                            ))
                            .child(self.settings_button(
                                ElementId::Name("avatar-cancel".into()),
                                "Отмена",
                                ButtonKind::Secondary,
                                false,
                                false,
                                SettingsAct::Personal(personal_settings::Action::CancelAvatar),
                                window,
                                cx,
                            )),
                    );
            } else {
                root = root.child(self.settings_button(
                    ElementId::Name("avatar-upload".into()),
                    "Выбрать изображение",
                    ButtonKind::Secondary,
                    false,
                    false,
                    SettingsAct::Personal(personal_settings::Action::PickAvatar),
                    window,
                    cx,
                ));
                if has_avatar {
                    root = root.child(self.settings_button(
                        ElementId::Name("avatar-remove".into()),
                        "Удалить аватар",
                        ButtonKind::Secondary,
                        false,
                        false,
                        SettingsAct::Personal(personal_settings::Action::RemoveAvatar),
                        window,
                        cx,
                    ));
                }
            }
            let count = about.read(cx).value().chars().count();
            root = root
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(format!("О себе · {count}/512")),
                )
                .child(self.settings_input(&about, cx));
            let labels = crate::settings::country_labels(&countries);
            let chosen = labels
                .get(country_index)
                .cloned()
                .unwrap_or_else(|| "Не указана".into());
            root = root
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child("Страна проживания"),
                )
                .child(self.settings_button(
                    ElementId::Name("profile-country".into()),
                    chosen,
                    ButtonKind::Secondary,
                    false,
                    false,
                    SettingsAct::Personal(personal_settings::Action::OpenCountry),
                    window,
                    cx,
                ));
            if country_open {
                let query = country_search.read(cx).value().to_string();
                let indexes = crate::settings::filter_countries(&countries, &query);
                let mut list = div()
                    .id("country-list")
                    .max_h(px(220.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_1()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(ink(p.line))
                    .bg(ink(p.surface));
                for index in indexes.into_iter().take(80) {
                    let label = labels.get(index + 1).cloned().unwrap_or_default();
                    list = list.child(self.settings_button(
                        ElementId::Name(format!("country-{index}").into()),
                        label,
                        if index + 1 == country_index {
                            ButtonKind::Tab
                        } else {
                            ButtonKind::Secondary
                        },
                        index + 1 == country_index,
                        false,
                        SettingsAct::Personal(personal_settings::Action::ChooseCountry(index + 1)),
                        window,
                        cx,
                    ));
                }
                root = root
                    .child(self.settings_input(&country_search, cx))
                    .child(list)
                    .child(self.settings_button(
                        ElementId::Name("country-close".into()),
                        "Закрыть список",
                        ButtonKind::Secondary,
                        false,
                        false,
                        SettingsAct::Personal(personal_settings::Action::CloseCountry),
                        window,
                        cx,
                    ));
            }
            root = root.child(
                div()
                    .flex()
                    .gap_2()
                    .child(self.settings_button(
                        ElementId::Name("profile-save".into()),
                        "Сохранить",
                        ButtonKind::Primary,
                        false,
                        profile_saving,
                        SettingsAct::Personal(personal_settings::Action::SaveProfile),
                        window,
                        cx,
                    ))
                    .child(self.settings_button(
                        ElementId::Name("profile-cancel".into()),
                        "Отмена",
                        ButtonKind::Secondary,
                        false,
                        false,
                        SettingsAct::Personal(personal_settings::Action::CancelProfile),
                        window,
                        cx,
                    )),
            );
        }
        for text in [validation, local_error, notice] {
            if !text.is_empty() {
                root = root.child(self.form_text(&text, false));
            }
        }
        root = root
            .child(self.separator())
            .child(self.heading("Оформление", "Настройка сохраняется на этом устройстве."))
            .child(self.settings_button(
                ElementId::Name("profile-theme".into()),
                if theme.is_light(self.system_light) {
                    "Тёмная тема"
                } else {
                    "Светлая тема"
                },
                ButtonKind::Secondary,
                false,
                false,
                SettingsAct::Personal(personal_settings::Action::ToggleTheme),
                window,
                cx,
            ))
            .child(self.separator())
            .child(self.heading(
                "Пароль и доступ",
                "Защитите аккаунт и управляйте устройствами.",
            ))
            .child(self.settings_input(&password_current, cx))
            .child(self.settings_input(&password_next, cx))
            .child(self.settings_input(&password_confirm, cx))
            .child(self.form_text(
                "От 8 символов. После смены пароля остальные устройства выйдут из аккаунта.",
                false,
            ))
            .child(self.settings_button(
                ElementId::Name("password-save".into()),
                "Сменить пароль",
                ButtonKind::Secondary,
                false,
                false,
                SettingsAct::Personal(personal_settings::Action::ChangePassword),
                window,
                cx,
            ))
            .child(self.separator())
            .child(self.heading(
                "Активные сессии",
                "Завершите сеанс на устройстве, которым больше не пользуетесь.",
            ));
        if !sessions_error.is_empty() {
            root = root
                .child(self.form_text(&sessions_error, true))
                .child(self.settings_button(
                    ElementId::Name("sessions-retry".into()),
                    "Повторить",
                    ButtonKind::Secondary,
                    false,
                    false,
                    SettingsAct::Personal(personal_settings::Action::RetrySessions),
                    window,
                    cx,
                ));
        }
        if sessions.is_empty() {
            root = root.child(self.form_text(
                if sessions_loading {
                    "Загружаем устройства…"
                } else {
                    "Нет доступных сессий."
                },
                false,
            ));
        }
        let mut has_other = false;
        for session in &sessions {
            has_other |= !session.current;
            let name = if session.user_agent.trim().is_empty() {
                "Неизвестное устройство".to_owned()
            } else {
                session
                    .user_agent
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            let mut card = div()
                .w_full()
                .p_3()
                .rounded(px(12.))
                .border_1()
                .border_color(if session.current {
                    super::ui::ink_alpha(p.lime, 90)
                } else {
                    ink(p.line)
                })
                .bg(ink(p.surface))
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(name),
                );
            if session.current {
                card = card.child(
                    div()
                        .text_size(px(10.))
                        .text_color(ink(p.lime))
                        .child("Эта сессия"),
                );
            }
            if !session.client_ip.trim().is_empty() {
                card = card.child(
                    div()
                        .text_size(px(12.))
                        .text_color(ink(p.muted))
                        .child(session.client_ip.clone()),
                );
            }
            let when = session
                .last_used_at
                .filter(|at| *at > 0)
                .map(|at| format!("Активность: {}", crate::settings::format_date(at),))
                .or_else(|| {
                    (session.created_at > 0).then(|| {
                        format!("Вход: {}", crate::settings::format_date(session.created_at))
                    })
                });
            if let Some(when) = when {
                card = card.child(
                    div()
                        .text_size(px(12.))
                        .text_color(ink(p.muted))
                        .child(when),
                );
            }
            if !session.current {
                card = card.child(self.settings_button(
                    ElementId::Name(format!("session-{}", session.id).into()),
                    "Завершить",
                    ButtonKind::Secondary,
                    false,
                    false,
                    SettingsAct::Personal(personal_settings::Action::RevokeSession(session.id)),
                    window,
                    cx,
                ));
            }
            root = root.child(card);
        }
        if has_other {
            root = root.child(self.settings_button(
                ElementId::Name("sessions-revoke-others".into()),
                "Завершить все остальные",
                ButtonKind::Danger,
                false,
                false,
                SettingsAct::Personal(personal_settings::Action::RevokeOthers),
                window,
                cx,
            ));
        }
        if !is_owner && profile.is_some() {
            root = root
                .child(self.separator())
                .child(self.heading(
                    "Опасная зона",
                    "Удаление аккаунта удалит профиль, настройки и доступ к этому инстансу. Старые сообщения останутся обезличенными.",
                ));
            if delete_confirm {
                root = root
                    .child(self.form_text(
                        &format!(
                            "Для подтверждения введите имя пользователя: {}",
                            profile.as_ref().map(|profile| profile.username.as_str()).unwrap_or("")
                        ),
                        false,
                    ))
                    .child(self.settings_input(&delete_name, cx))
                    .child(self.settings_input(&delete_password, cx))
                    .child(self.settings_button(
                        ElementId::Name("delete-confirm".into()),
                        "Удалить аккаунт навсегда",
                        ButtonKind::Danger,
                        false,
                        false,
                        SettingsAct::Personal(personal_settings::Action::ConfirmDelete),
                        window,
                        cx,
                    ))
                    .child(self.settings_button(
                        ElementId::Name("delete-cancel".into()),
                        "Отменить удаление",
                        ButtonKind::Secondary,
                        false,
                        false,
                        SettingsAct::Personal(personal_settings::Action::CancelDelete),
                        window,
                        cx,
                    ));
            } else {
                root = root.child(self.settings_button(
                    ElementId::Name("delete-account".into()),
                    "Удалить аккаунт",
                    ButtonKind::Danger,
                    false,
                    false,
                    SettingsAct::Personal(personal_settings::Action::DeleteAccount),
                    window,
                    cx,
                ));
            }
        }
        root.into_any_element()
    }
}
