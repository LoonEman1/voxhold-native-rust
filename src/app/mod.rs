mod auth_view;
mod composer;
mod history_ops;
mod history_view;
mod media_ops;
mod members_view;
mod navigation_view;
mod realtime;
mod session;
mod shell_view;
#[cfg(test)]
mod tests;
mod theme_ops;
pub(crate) mod ui;
mod workspace;

use gpui::{
    div, list, prelude::*, px, rgba, AnyElement, Context, Entity, ListAlignment, ListState,
    MouseButton, Render, Window,
};
use gpui_component::input::{InputState, TextInput};
use layout::{SplitterDrag, SplitterSide};
use voxhold_api::{Channel, ChannelSubscription, Client, Member, Message, Server};

#[cfg(feature = "voxhold-media")]
use crate::voice_panel;
use crate::{
    attachments, auth, background, channel_settings, draft, inbox, layout, links, media, members,
    message_tools, personal_settings, reactions, search, server_settings, theme, window_chrome,
};
use ui::{icon, ink};

/// (id, body revision, reactions revision, date divider, grouped, unread).
/// The last three force a re-measure when only grouping or a divider changes.
type HistoryKey = (i64, i64, i64, String, bool, bool);

#[derive(Clone, Copy, Debug)]
struct ScrollbarDrag {
    /// Window-space top of the scrollbar track.
    top: f32,
    /// Track height in logical pixels.
    height: f32,
    /// Thumb height in logical pixels.
    thumb: f32,
    /// Pointer offset inside the thumb.
    grab: f32,
}
pub(crate) struct Voxhold {
    runtime: std::sync::Arc<tokio::runtime::Runtime>,
    instance: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    composer: Entity<InputState>,
    client: Option<Client>,
    self_user_id: i64,
    search: Option<Entity<search::SearchPanel>>,
    show_search: bool,
    message_tools: Option<Entity<message_tools::MessageTools>>,
    tools_revision: u64,
    show_tools: bool,
    attachments: Option<Entity<attachments::Attachments>>,
    settings: Option<Entity<personal_settings::PersonalSettings>>,
    show_settings: bool,
    channel_settings: Option<Entity<channel_settings::ChannelSettings>>,
    show_channel_settings: bool,
    server_settings: Option<Entity<server_settings::ServerSettings>>,
    show_server_settings: bool,
    inbox: Option<Entity<inbox::Inbox>>,
    show_inbox: bool,
    show_members: bool,
    show_channels: bool,
    show_servers: bool,
    #[cfg(feature = "voxhold-media")]
    voice: Option<Entity<voice_panel::VoicePanel>>,
    #[cfg(feature = "voxhold-media")]
    voice_channel: i64,
    servers: Vec<Server>,
    channels: Vec<Channel>,
    members: Vec<Member>,
    roster: members::Roster,
    messages: Vec<Message>,
    history_list: ListState,
    history_keys: Vec<HistoryKey>,
    before: Option<i64>,
    has_more: bool,
    history_has_newer: bool,
    loading_older: bool,
    /// Bumped whenever the visible access scope changes (channel switch, access
    /// event, logout). In-flight history/send results from an older value are
    /// discarded before they touch the UI.
    access_generation: u64,
    access_pending: bool,
    access_reload_in_flight: bool,
    access_refresh_again: bool,
    refresh_request: Option<u64>,
    refresh_again: bool,
    refresh_new_ids: std::collections::HashSet<i64>,
    pending_edits: std::collections::HashMap<i64, Message>,
    pending_reactions: std::collections::HashMap<i64, voxhold_api::ReactionsUpdate>,
    request_seq: u64,
    older_request: u64,
    older_retry_at: Option<std::time::Instant>,
    older_error: String,
    older_scheduled: bool,
    read_scheduled: bool,
    read_retry_at: Option<std::time::Instant>,
    painted_read: std::rc::Rc<std::cell::Cell<i64>>,
    /// The last confirmed read id per `(server, channel)`, used to place the
    /// "Новые сообщения" divider when a channel is opened again.
    channel_reads: std::collections::HashMap<(i64, i64), i64>,
    unread_after: i64,
    /// One request per `(message, emoji)` so a double click cannot toggle twice.
    reaction_requests: std::collections::HashSet<(i64, String)>,
    recent_reactions: reactions::SharedRecents,
    /// Custom history scrollbar drag captured from the root pointer handler.
    scrollbar_drag: Option<ScrollbarDrag>,
    /// A bounded `message_context` request for a quote outside the window.
    context_request: Option<u64>,
    /// Scroll to this id after the spliced list has the matching row.
    pending_jump: Option<i64>,
    /// The next successful refresh replaces the window instead of merging it.
    /// Used by "К последним" after a quote opened a bounded context page.
    history_reset_pending: bool,
    hovered_message: i64,
    /// Bounded thumbnails, link previews and the single image viewer.
    media: media::Media,
    event_task: Option<tokio::task::JoinHandle<()>>,
    realtime_generation: u64,
    event_subscription: Option<tokio::sync::watch::Sender<Option<ChannelSubscription>>>,
    online_by_server: std::collections::HashMap<i64, std::collections::HashSet<i64>>,
    events: Option<
        std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<voxhold_api::RealtimeEvent>>>,
    >,
    events_overflow: std::sync::Arc<std::sync::atomic::AtomicBool>,
    reads: std::collections::HashMap<i64, i64>,
    read_sent: i64,
    read_pending: bool,
    server: i64,
    channel: i64,
    error: String,
    /// Error or limit text shown next to the composer (Go `SendError`).
    composer_error: String,
    /// One confirmed send in flight for this chat; the button is disabled.
    sending: bool,
    send_attempt: u64,
    busy: bool,
    generation: u64,
    drafts: draft::Shared,
    auth: auth::AuthState,
    palette: theme::Palette,
    theme_mode: theme::ThemeMode,
    theme_path: Option<std::path::PathBuf>,
    system_light: bool,
    shell: layout::Settings,
    shell_path: Option<std::path::PathBuf>,
    shell_initialized: bool,
    drag: Option<SplitterDrag>,
    viewport: (f32, f32),
    auth_background: Option<(u32, u32, bool, std::sync::Arc<gpui::RenderImage>)>,
    initial_focus_done: bool,
    /// `(message_id, offset_in_item)` captured before a prepend so the visible
    /// row stays under the same pixel after older rows are merged.
    pending_anchor: Option<(i64, gpui::Pixels)>,
    /// Deleting a message is terminal: a late page or a repeated event must not
    /// bring the row back.
    deleted_ids: std::collections::HashSet<i64>,
    _activation_subscription: gpui::Subscription,
    _appearance_subscription: gpui::Subscription,
}

impl Drop for Voxhold {
    fn drop(&mut self) {
        self.stop_realtime();
    }
}

impl Voxhold {
    fn stop_realtime(&mut self) {
        if let Some(task) = self.event_task.take() {
            task.abort();
        }
        self.events = None;
        self.event_subscription = None;
        self.realtime_generation = self.realtime_generation.wrapping_add(1);
        self.online_by_server.clear();
    }

    fn new_history_list(alignment: ListAlignment, cx: &Context<Self>) -> ListState {
        let history_list = ListState::new(0, alignment, px(400.));
        let weak = cx.entity().downgrade();
        history_list.set_scroll_handler(move |_, window, cx| {
            if let Some(view) = weak.upgrade() {
                view.update(cx, |s, cx| {
                    // The list is mutably borrowed while this callback runs, so
                    // never touch `ListState` here. Both follow-ups recompute
                    // their range from a later frame.
                    if !s.loading_older && !s.older_scheduled && s.has_more {
                        s.schedule_older_check(window, cx);
                    }
                });
            }
        });
        history_list
    }

    pub(super) fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        prefill: &str,
        shell_path: Option<std::path::PathBuf>,
    ) -> Self {
        let history_list = Self::new_history_list(ListAlignment::Bottom, cx);
        let system_light = theme::system_light();
        let theme_path = theme::theme_path();
        let theme_mode = theme::load_mode(theme_path.as_deref());
        let shell = shell_path
            .as_deref()
            .map(layout::Settings::load)
            .unwrap_or_default();
        let view = Self {
            runtime: std::sync::Arc::new(tokio::runtime::Runtime::new().expect("Tokio runtime")),
            instance: cx.new(|cx| InputState::new(window, cx).placeholder("https://example.com")),
            username: cx.new(|cx| InputState::new(window, cx).placeholder("например, neva")),
            password: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Минимум 8 символов")
                    .masked(true)
            }),
            composer: cx.new(|cx| {
                InputState::new(window, cx)
                    .multi_line()
                    .placeholder("Написать сообщение")
            }),
            client: None,
            self_user_id: 0,
            search: None,
            show_search: false,
            message_tools: None,
            tools_revision: 0,
            show_tools: false,
            attachments: None,
            settings: None,
            show_settings: false,
            channel_settings: None,
            show_channel_settings: false,
            server_settings: None,
            show_server_settings: false,
            inbox: None,
            show_inbox: false,
            show_members: false,
            show_channels: false,
            show_servers: false,
            #[cfg(feature = "voxhold-media")]
            voice: None,
            #[cfg(feature = "voxhold-media")]
            voice_channel: 0,
            servers: vec![],
            channels: vec![],
            members: vec![],
            roster: members::Roster::default(),
            messages: vec![],
            history_list,
            history_keys: Vec::new(),
            before: None,
            has_more: false,
            history_has_newer: false,
            loading_older: false,
            access_generation: 0,
            access_pending: false,
            access_reload_in_flight: false,
            access_refresh_again: false,
            refresh_request: None,
            refresh_again: false,
            refresh_new_ids: Default::default(),
            pending_edits: Default::default(),
            pending_reactions: Default::default(),
            request_seq: 0,
            older_request: 0,
            older_retry_at: None,
            older_error: String::new(),
            older_scheduled: false,
            read_scheduled: false,
            read_retry_at: None,
            painted_read: Default::default(),
            channel_reads: Default::default(),
            unread_after: 0,
            reaction_requests: Default::default(),
            recent_reactions: std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
            scrollbar_drag: None,
            context_request: None,
            pending_jump: None,
            history_reset_pending: false,
            hovered_message: 0,
            media: media::Media::default(),
            event_task: None,
            realtime_generation: 0,
            event_subscription: None,
            online_by_server: Default::default(),
            events: None,
            events_overflow: Default::default(),
            reads: Default::default(),
            read_sent: 0,
            read_pending: false,
            server: 0,
            channel: 0,
            error: String::new(),
            composer_error: String::new(),
            sending: false,
            send_attempt: 0,
            busy: false,
            generation: 0,
            drafts: draft::Drafts::shared(),
            auth: auth::AuthState::with_prefill(prefill),
            palette: theme::Palette::resolve(theme_mode, system_light),
            theme_mode,
            theme_path,
            system_light,
            shell,
            shell_path,
            shell_initialized: false,
            drag: None,
            viewport: (0.0, 0.0),
            auth_background: None,
            initial_focus_done: false,
            pending_anchor: None,
            deleted_ids: Default::default(),
            _activation_subscription: cx.observe_window_activation(window, |s, window, cx| {
                s.sync_system_theme(window, cx);
                cx.notify();
            }),
            _appearance_subscription: cx.observe_window_appearance(window, Self::sync_system_theme),
        };
        gpui_component::Theme::change(
            theme_ops::component_theme_mode(view.palette.light),
            Some(window),
            cx,
        );
        gpui_component::Theme::global_mut(cx).font_family = "Inter".into();
        window_chrome::apply(window, view.palette.light);
        Self::bind_composer_keys(cx);
        view
    }
}
