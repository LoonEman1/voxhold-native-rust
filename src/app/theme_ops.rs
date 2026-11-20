use super::*;

pub(super) fn component_theme_mode(light: bool) -> gpui_component::ThemeMode {
    if light {
        gpui_component::ThemeMode::Light
    } else {
        gpui_component::ThemeMode::Dark
    }
}

impl Voxhold {
    pub(super) fn save_shell(&self) {
        if let Some(path) = &self.shell_path {
            let _ = self.shell.save(path);
        }
    }

    pub(super) fn apply_theme(&mut self, mode: theme::ThemeMode, cx: &mut Context<Self>) {
        self.theme_mode = mode;
        self.system_light = theme::system_light();
        self.palette = theme::Palette::resolve(mode, self.system_light);
        theme::save_mode(self.theme_path.as_deref(), mode);
        #[cfg(feature = "voxhold-media")]
        self.sync_voice_palette(cx);
        cx.notify();
    }

    /// The voice room is a separate entity; hand it the new palette so its
    /// fixed surfaces follow the workspace theme.
    #[cfg(feature = "voxhold-media")]
    pub(super) fn sync_voice_palette(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.voice.clone() else {
            return;
        };
        let palette = self.palette;
        panel.update(cx, |panel, cx| {
            panel.set_palette(palette);
            cx.notify();
        });
    }

    pub(super) fn sync_system_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.theme_mode == theme::ThemeMode::System {
            let light = theme::system_light();
            if light != self.system_light {
                self.system_light = light;
                self.palette = theme::Palette::resolve(self.theme_mode, light);
                self.auth_background = None;
                gpui_component::Theme::change(component_theme_mode(light), Some(window), cx);
                window_chrome::set_theme(window, light);
                #[cfg(feature = "voxhold-media")]
                self.sync_voice_palette(cx);
                cx.notify();
            }
        }
    }

    pub(super) fn toggle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mode = self.theme_mode.toggled(self.system_light);
        self.apply_theme(mode, cx);
        gpui_component::Theme::change(component_theme_mode(self.palette.light), Some(window), cx);
        window_chrome::set_theme(window, self.palette.light);
    }
}
