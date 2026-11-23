#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod assets;
mod attachments;
mod audio_capabilities;
mod auth;
mod auth_session;
mod background;
mod channel_settings;
mod clipboard;
mod draft;
mod file_io;
mod history;
mod inbox;
mod instance_prefs;
mod layout;
mod links;
mod media;
mod members;
mod message_tools;
mod messages;
mod notifications;
mod open_url;
mod personal_settings;
mod reactions;
mod recordings;
mod recordings_dialog;
mod search;
mod server_settings;
mod session_store;
mod settings;
#[cfg(test)]
mod test_support;
mod theme;
mod thread_panel;
#[cfg(feature = "voxhold-media")]
mod ui_sounds;
mod update;
mod voice_form_text;
#[cfg(feature = "voxhold-media")]
mod voice_panel;
#[cfg(feature = "voxhold-media")]
mod voice_room;
mod voice_settings;
mod window_chrome;
#[cfg(feature = "voxhold-media")]
mod window_state;

use gpui::{px, size, App, AppContext, Application, Bounds, WindowBounds, WindowOptions};
use gpui_component::Root;

pub(crate) use app::ui::{icon, ink};
pub(crate) use app::Voxhold;

fn parse_args_from<I: Iterator<Item = String>>(
    args: I,
) -> (
    String,
    Option<std::path::PathBuf>,
    f32,
    f32,
    Option<std::path::PathBuf>,
    Option<String>,
) {
    let mut server = String::new();
    let mut layout_path = layout::Settings::default_path();
    let mut width = 1280.0f32;
    let mut height = 900.0f32;
    let mut handshake = None;
    let mut operation = None;
    let mut args = args;
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
            "--update-handshake" => {
                if let Some(value) = args.next() {
                    handshake = Some(std::path::PathBuf::from(value));
                }
            }
            "--update-operation" => {
                if let Some(value) = args.next() {
                    operation = Some(value);
                }
            }
            _ => {}
        }
    }
    (server, layout_path, width, height, handshake, operation)
}

fn parse_args() -> (
    String,
    Option<std::path::PathBuf>,
    f32,
    f32,
    Option<std::path::PathBuf>,
    Option<String>,
) {
    parse_args_from(std::env::args().skip(1))
}

fn main() {
    let (server, layout_path, width, height, handshake, operation) = parse_args();
    let instance_path = instance_prefs::InstancePrefs::default_path();
    let session_path = session_store::SessionStore::default_path();
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
            let instance_path = instance_path.clone();
            let session_path = session_path.clone();
            let handshake = handshake.clone();
            let operation = operation.clone();
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
                // `Voxhold::new` owns the explicit «Восстановление входа»
                // phase and starts it before the root's first frame. The
                // update handshake still runs immediately after the window
                // and the view exist.
                move |window, cx| {
                    let view = cx.new(|cx| {
                        Voxhold::new(
                            window,
                            cx,
                            &prefill,
                            layout_path.clone(),
                            instance_path.clone(),
                            session_path.clone(),
                        )
                    });
                    #[cfg(feature = "voxhold-media")]
                    view.update(cx, |view, cx| view.install_power(window, cx));
                    // The permanent tray icon and the close hook are set up
                    // before the window is shown so the cross button can hide
                    // it from the very first frame.
                    view.update(cx, |view, cx| view.install_tray(window, cx));
                    // The helper waits for this file after the swap. It is
                    // written only once the window is up; a failure is shown
                    // in the workspace instead of being swallowed. Write it
                    // before the startup cleanup so that cleanup can protect
                    // the staging directory by its handshake marker.
                    let mut handshake_written = false;
                    if let Some(path) = handshake.as_ref() {
                        match update::write_handshake(path, operation.as_deref()) {
                            Ok(()) => handshake_written = true,
                            Err(error) => {
                                eprintln!("voxhold-update: cannot write handshake: {error}");
                                view.update(cx, |view, cx| {
                                    view.report_handshake_failure(
                                        "не удалось записать файл подтверждения".into(),
                                    );
                                    cx.notify();
                                });
                            }
                        }
                    }
                    // The observer may finish a confirmed operation. After an
                    // update the handshake just written is the proof; without
                    // it the state stays unconfirmed so the helper can roll
                    // back.
                    view.update(cx, |view, cx| {
                        view.start_update(handshake.is_some(), handshake_written, window, cx)
                    });
                    cx.new(|cx| Root::new(view.into(), window, cx))
                },
            )
            .expect("failed to open window");
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_update_handshake_argument() {
        let args = vec![
            "--update-handshake".to_string(),
            r"C:\Temp\handshake.ok".to_string(),
            "--update-operation".to_string(),
            "op-1".to_string(),
        ];
        let (_, _, _, _, handshake, operation) = parse_args_from(args.into_iter());
        assert_eq!(
            handshake.as_deref(),
            Some(std::path::Path::new(r"C:\Temp\handshake.ok"))
        );
        assert_eq!(operation.as_deref(), Some("op-1"));
        let (_, _, _, _, missing, no_operation) =
            parse_args_from(vec!["--update-handshake".to_string()].into_iter());
        assert!(missing.is_none());
        assert!(no_operation.is_none());
    }
}
