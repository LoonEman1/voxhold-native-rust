#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod assets;
mod attachments;
mod audio_capabilities;
mod auth;
mod background;
mod channel_settings;
mod clipboard;
mod draft;
mod file_io;
mod history;
mod inbox;
mod layout;
mod links;
mod media;
mod members;
mod message_tools;
mod notifications;
mod open_url;
mod personal_settings;
mod reactions;
mod recordings;
mod recordings_dialog;
mod search;
mod server_settings;
mod settings;
#[cfg(test)]
mod test_support;
mod theme;
mod thread_panel;
#[cfg(feature = "voxhold-media")]
mod voice_panel;
#[cfg(feature = "voxhold-media")]
mod voice_room;
mod voice_settings;
mod window_chrome;

use gpui::{px, size, App, AppContext, Application, Bounds, WindowBounds, WindowOptions};
use gpui_component::Root;

pub(crate) use app::ui::{icon, ink};
pub(crate) use app::Voxhold;

fn parse_args() -> (String, Option<std::path::PathBuf>, f32, f32) {
    let mut server = String::new();
    let mut layout_path = layout::Settings::default_path();
    let mut width = 1280.0f32;
    let mut height = 900.0f32;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-server" | "--server" => {
                if let Some(value) = args.next() {
                    server = value;
                }
            }
            "-layout" | "--layout" => {
                if let Some(value) = args.next() {
                    layout_path = Some(std::path::PathBuf::from(value));
                }
            }
            "-width" | "--width" => {
                if let Some(value) = args.next().and_then(|v| v.parse::<i32>().ok()) {
                    width = (value as f32).max(320.0);
                }
            }
            "-height" | "--height" => {
                if let Some(value) = args.next().and_then(|v| v.parse::<i32>().ok()) {
                    height = (value as f32).max(360.0);
                }
            }
            _ => {}
        }
    }
    (server, layout_path, width, height)
}

fn main() {
    let (server, layout_path, width, height) = parse_args();
    Application::new()
        .with_assets(assets::Assets::discover())
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            let fonts: Vec<std::borrow::Cow<'static, [u8]>> = vec![
                std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/Inter-Regular.ttf")),
                std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/Inter-Bold.ttf")),
            ];
            let _ = cx.text_system().add_fonts(fonts);
            let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
            let prefill = server.clone();
            let layout_path = layout_path.clone();
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("Voxhold".into()),
                        appears_transparent: false,
                        traffic_light_position: None,
                    }),
                    window_min_size: Some(size(px(320.), px(360.))),
                    ..Default::default()
                },
                move |window, cx| {
                    let view = cx.new(|cx| Voxhold::new(window, cx, &prefill, layout_path.clone()));
                    cx.new(|cx| Root::new(view.into(), window, cx))
                },
            )
            .expect("failed to open window");
            cx.activate(true);
        });
}
