mod action_io;
mod admin_view;
mod auth_view;
mod auto_login;
mod avatar_ops;
mod channel_ops;
mod channel_view;
mod chat_gap;
mod composer;
mod context_menus;
mod history_actions;
mod history_ops;
mod history_view;
#[cfg(all(feature = "voxhold-media", test))]
mod hotkey_tests;
#[cfg(feature = "voxhold-media")]
mod hotkeys;
mod inbox_ops;
mod lifecycle;
mod media_ops;
mod members_view;
mod messages_creator;
mod messages_ops;
mod messages_view;
mod navigation_view;
mod overlays_view;
mod personal_ops;
#[cfg(feature = "voxhold-media")]
mod power;
mod preview_ops;
mod profile_view;
mod realtime;
mod recordings_download;
mod recordings_ops;
#[cfg(test)]
mod recordings_ops_tests;
#[cfg(test)]
mod recordings_tests;
mod recordings_view;
mod room_chat;
#[cfg(all(feature = "voxhold-media", test))]
mod room_chat_tests;
mod server_ops;
mod session;
#[cfg(test)]
mod settings_review;
mod settings_slider;
#[cfg(test)]
mod settings_slider_tests;
#[cfg(test)]
mod settings_tests;
mod settings_view;
mod settings_voice_shortcuts;
mod settings_voice_view;
mod shell_view;
mod startup_view;
#[cfg(test)]
mod tests;
mod theme_ops;
mod thread_ops;
pub(crate) mod ui;
mod update_ops;
#[cfg(test)]
mod update_tests;
mod update_view;
#[cfg(all(feature = "voxhold-media", test))]
mod voice_cue_tests;
#[cfg(test)]
mod voice_dock_tests;
#[cfg(test)]
mod voice_form_tests;
#[cfg(test)]
mod voice_gain_tests;
mod voice_ops;
#[cfg(test)]
mod voice_pip_tests;
#[cfg(test)]
mod voice_screen_audio_tests;
#[cfg(test)]
mod voice_screen_confirmed_tests;
#[cfg(test)]
mod voice_screen_identity_tests;
#[cfg(test)]
mod voice_screen_operation_tests;
#[cfg(test)]
mod voice_screen_preview_tests;
#[cfg(test)]
mod voice_screen_restart_tests;
#[cfg(test)]
mod voice_screen_tests;
mod voice_settings_ops;
#[cfg(test)]
mod voice_settings_tests;
#[cfg(all(feature = "voxhold-media", test))]
mod voice_speech_tests;
#[cfg(test)]
mod voice_tests;
mod workspace;

use gpui::{
    div, list, prelude::*, px, rgba, AnyElement, Context, Entity, FocusHandle, ListAlignment,
    ListState, MouseButton, Render, Window,
};
use gpui_component::input::{InputEvent, InputState, TextInput};
use layout::{SplitterDrag, SplitterSide};
use voxhold_api::{Channel, ChannelSubscription, Client, Member, Message, Server};

#[cfg(feature = "voxhold-media")]
use crate::voice_panel;
use crate::{
    attachments, auth, auth_session, background, channel_settings, draft, inbox, instance_prefs,
    layout, links, media, members, message_tools, messages, personal_settings, reactions,
    recordings, recordings_dialog, search, server_settings, session_store, theme, thread_panel,
    window_chrome,
};
use ui::{icon, ink};

/// (id, body revision, reactions revision, date divider, grouped, unread,
/// attachment epoch). The last four force a re-measure when only grouping, a
/// divider or a server-side attachment deletion changes.
type HistoryKey = (i64, i64, i64, String, bool, bool, u64);

/// One in-flight send for a chat. The attempt number distinguishes two sends
/// of the same chat; the abort handle stops the runtime task on logout. The
/// plan snapshot lets an own `message.created`/`conversation.message` echo
/// complete the same attempt the HTTP answer would.
struct PendingSend {
    attempt: u64,
    abort: tokio::task::AbortHandle,
    plan: draft::SendPlan,
}

/// The unsent drafts of the account whose session expired in this process.
/// They are held only for the same canonical origin and user id; any other
/// sign-in, or an explicit logout, releases them with their temporary files.
struct SavedDrafts {
    origin: String,
    user_id: i64,
    drafts: draft::Shared,
}

/// One runtime preview request. The generation is the dialog preview scope; an
/// answer from a previous scope may not clear or replace the next request's
/// pending entry.
struct PreviewAbort {
    key: String,
    dialog: u64,
    generation: u64,
    abort: tokio::task::AbortHandle,
}

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

/// One open member context menu (`openMemberMenu` in Go). Every paint re-reads
/// the member and the current server role before enabling an action.
#[derive(Clone, Debug)]
struct MemberMenu {
    user_id: i64,
    x: f32,
    y: f32,
    profile: Option<uuid::Uuid>,
    profile_result: Option<Result<voxhold_api::Profile, String>>,
    voice: bool,
    confirm_block: bool,
    confirm_ban: bool,
}

/// One open context menu. Only the id is authoritative: every paint re-reads
/// the row and its permissions from the current snapshot.
#[derive(Clone, Debug)]
struct MessageMenu {
    message_id: i64,
    x: f32,
    y: f32,
    expanded: bool,
    confirm_delete: bool,
}

/// A short operation in flight. The token guards pending/error state against
/// a late answer after a chat switch.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ActionRequest {
    chat: draft::ChatKey,
    access: u64,
    request: u64,
}

/// One reaction-only GET for a loaded personal message. A second event while
/// the GET is active only sets `again`; the completion starts one repeat.
#[derive(Clone, Debug)]
struct ConversationReactionRequest {
    request: u64,
    again: bool,
}

/// One settings surface at a time, like the Go client's single `settings`
/// dialog. Opening another surface closes the current one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsKind {
    Personal,
    Server,
    Channel,
    ChannelNotifications,
}

/// A visible settings control reachable by Tab. `None` keeps text inputs in
/// the traversal ring without giving them a mouse action.
#[derive(Clone, Debug)]
enum SettingsAct {
    Close,
    Tab(usize),
    Personal(personal_settings::Action),
    Server(server_settings::Action),
    Channel(channel_settings::Action),
    Notifications(channel_settings::NotifAction),
}

#[derive(Clone, Debug)]
struct ForwardState {
    message_id: i64,
    source: draft::ChatKey,
    source_kind: &'static str,
    request: u64,
    destinations: Vec<voxhold_api::ForwardDestination>,
    loading: bool,
    sending: bool,
    error: String,
    /// One `client_message_id` per `(destination kind, destination id)` while
    /// this dialog is open. A retry of a failed recipient repeats its key,
    /// while another recipient gets its own. The keys are dropped with the
    /// dialog on success or close, so a new dialog is a new operation.
    keys: std::collections::HashMap<(String, i64), String>,
}
/// One cue the test sink captured instead of opening an audio device.
#[cfg(all(feature = "voxhold-media", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CueCall {
    pub(crate) cue: crate::ui_sounds::VoiceCue,
    pub(crate) volume: i64,
    pub(crate) output: String,
}

pub(crate) struct Voxhold {
    runtime: std::sync::Arc<tokio::runtime::Runtime>,
    instance: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    composer: Entity<InputState>,
    client: Option<Client>,
    /// The signed-in session: identity, token, expiry and rotation generation.
    /// It is the only place that owns these values.
    session: Option<auth_session::Session>,
    /// Changes on every login and local end of the session; a late refresh or
    /// 401 from another login is discarded.
    session_epoch: u64,
    /// Single-flight and bounded retry state for `POST /auth/refresh`.
    session_gate: auth_session::RefreshGate,
    /// One periodic expiry/refresh timer while a session exists. Dropping the
    /// task cancels it.
    session_clock: Option<gpui::Task<()>>,
    /// Optional «Запомнить вход» storage. It is created once and cloned into
    /// every background operation.
    session_store: session_store::SessionStore,
    /// Monotonic id of the current automatic login attempt. A late answer from
    /// another attempt, a closed login screen or a toggled box is discarded.
    auto_attempt: u64,
    /// The token known to be stored, used to catch up when a rotation write is
    /// lost and the next refresh must accept the older record.
    last_stored_token: Option<String>,
    /// Visible workspace notice when saving or deleting the record failed.
    remember_error: String,
    /// The kept record that «Повторить» uses without asking for the address.
    auto_record: Option<session_store::RememberedSession>,
    /// Store generation captured when the current automatic attempt started.
    /// A result from an older generation is never applied or written.
    auto_epoch: u64,
    /// A failed logout cleanup suppresses automatic restore in this process
    /// until the retry succeeds.
    auto_blocked: bool,
    /// The explicit startup phase: the neutral «Восстанавливаем вход…»
    /// surface is shown before any address or credential field.
    startup_restoring: bool,
    /// The startup store check runs exactly once per process.
    startup_checked: bool,
    /// The whole restoration attempt ends at this instant.
    startup_deadline: std::time::Instant,
    /// `STARTUP_BUDGET`; a test may shorten it to exercise the timeout.
    startup_budget: std::time::Duration,
    /// The chosen legacy record still has to be written as the target after
    /// the first successful restoration.
    migration_pending: bool,
    /// Local cleanup is in flight; closing the window completes it.
    cleanup_pending: bool,
    /// The generation captured by the in-flight cleanup. A new login means the
    /// close-time retry is stale and must not erase that login.
    cleanup_epoch: u64,
    /// The generation the in-flight cleanup claimed on disk, shared with the
    /// close-time fallback. Zero until that claim lands.
    cleanup_claim: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// The cleanup failed and «Повторить очистку» is offered.
    cleanup_retry: bool,
    /// First Tab stop of the neutral start surface.
    restore_cancel_focus: FocusHandle,
    /// The cancel action received the initial focus once.
    restore_focus_done: bool,
    self_user_id: i64,
    search: Option<Entity<search::SearchPanel>>,
    show_search: bool,
    /// The search popup may only query against a channel snapshot confirmed
    /// for the current server/access generation.
    search_permissions_ready: bool,
    search_permissions_request: Option<u64>,
    thread: Option<Entity<thread_panel::ThreadPanel>>,
    show_thread: bool,
    message_menu: Option<MessageMenu>,
    member_menu: Option<MemberMenu>,
    channel_menu: Option<context_menus::ChannelMenu>,
    forward: Option<ForwardState>,
    action_requests: std::collections::HashMap<String, ActionRequest>,
    action_seq: u64,
    action_error: Option<(i64, String)>,
    action_notice: String,
    editing: Option<i64>,
    /// Changes on every open and close of the inline editor. A PATCH answer may
    /// only close the editor whose generation it captured.
    edit_open_seq: u64,
    /// Failure of an in-flight PATCH, bound to its editor generation so a stale
    /// attempt cannot paint over a reopened editor.
    edit_error: Option<(i64, u64, String)>,
    edit_input: Entity<InputState>,
    reaction_picker: Entity<reactions::ReactionPicker>,
    confirm_delete: Option<i64>,
    pinned_ids: std::collections::HashSet<i64>,
    pinned_request: Option<u64>,
    scroll_epoch: u64,
    highlighted_message: Option<(i64, u64)>,
    highlight_seq: u64,
    selected_messages: Vec<i64>,
    conversation: Option<voxhold_api::Conversation>,
    conversations: Vec<voxhold_api::Conversation>,
    recents_request: Option<u64>,
    recents_again: bool,
    /// A `conversation.changed` event asks the next index answer to reconcile
    /// the active conversation: update it when present, clear it when gone.
    /// Scoped to that conversation, selection, surface epoch and session.
    conversations_reconcile: Option<messages::ConversationReconcile>,
    /// Bumped on every conversation selection. An index page requested before
    /// the selection cannot clear the newly opened conversation.
    conversation_select_seq: u64,
    channel_snapshot_request: Option<u64>,
    attachments: Option<Entity<attachments::Attachments>>,
    personal: Option<personal_settings::PersonalSettings>,
    server_settings: Option<server_settings::ServerSettings>,
    channel_settings: Option<channel_settings::ChannelSettings>,
    channel_notifications: Option<channel_settings::ChannelNotifications>,
    /// Server recordings snapshot and the one open dialog.
    recordings: recordings::RecordingsState,
    recording_access_epoch: u64,
    /// Monotonic id of one open recordings modal. It stays stable until close.
    recordings_dialog_seq: u64,
    /// Monotonic attempt id for download/delete operations.
    recordings_op_seq: u64,
    recordings_dialog: Option<recordings_dialog::DialogState>,
    recordings_return_focus: Option<FocusHandle>,
    recordings_entry_focus: FocusHandle,
    recordings_close_focus: FocusHandle,
    recordings_voice_entry: bool,
    recordings_preview_aborts: Vec<PreviewAbort>,
    /// Owner name of the active recording in the open voice channel; shared
    /// with the voice panel so the red marker and label read one snapshot.
    recordings_live: std::rc::Rc<std::cell::RefCell<Option<String>>>,
    voice_settings: crate::voice_settings::VoiceSettings,
    notifications: crate::notifications::NotificationCenter,
    notification_tray: crate::notifications::Tray,
    active_settings: Option<SettingsKind>,
    settings_return_focus: Option<FocusHandle>,
    settings_ring: Vec<(FocusHandle, Option<SettingsAct>)>,
    /// Slider focus handles rebuilt on every settings paint, so Tab reaches a
    /// slider and the tests can drive its keys.
    settings_slider_focus: Vec<(String, FocusHandle)>,
    /// Measured track boxes of the settings sliders, keyed by slider id.
    slider_tracks: std::collections::HashMap<
        String,
        std::rc::Rc<std::cell::Cell<settings_slider::SliderTrack>>,
    >,
    slider_drag: Option<settings_slider::SliderDrag>,
    /// Logical window scale reported by the platform. The settings card uses it
    /// for its DPI-aware margins; a test can pin it instead of a real monitor
    /// DPI (`test_pin_ui_scale`).
    ui_scale: f32,
    /// True once a test pinned `ui_scale`; the render then stops sampling the
    /// window.
    ui_scale_pinned: bool,
    settings_scroll: gpui::ScrollHandle,
    settings_close_focus: FocusHandle,
    settings_entry_focus: FocusHandle,
    server_entry_focus: FocusHandle,
    #[cfg(test)]
    notifications_entry_focus: FocusHandle,
    window_active: bool,
    /// Personal "Сообщения" mode. The server/channel context is kept in
    /// `personal_return` so "К каналам" can restore it when still permitted.
    personal_mode: bool,
    personal_return: Option<(i64, i64)>,
    /// Channel the user asked to reopen while an ACL reload was still in
    /// flight. Its answer picks this channel or the first readable fallback.
    pending_return_channel: Option<i64>,
    /// `show_members` from the channel surface, restored when personal mode
    /// closes instead of permanently discarding the drawer state.
    personal_members: bool,
    messaging: messages::Messages,
    /// A background answer cleared the active chat; the next render closes the
    /// window-owned viewer/thread/settings and clears the composer.
    pending_personal_cleanup: bool,
    _conversation_query_subscription: gpui::Subscription,
    _creator_subscriptions: Vec<gpui::Subscription>,
    show_members: bool,
    show_channels: bool,
    show_servers: bool,
    #[cfg(feature = "voxhold-media")]
    voice: Option<Entity<voice_panel::VoicePanel>>,
    /// Power poller; it exists only while a call is active. Dropping the task
    /// cancels the 250 ms timer.
    #[cfg(feature = "voxhold-media")]
    power_task: Option<gpui::Task<()>>,
    /// Channel shown in the room pane; independent from the call channel.
    #[cfg(feature = "voxhold-media")]
    voice_room: i64,
    /// Rooms whose chat panel the user opened, keyed by `(server, channel)`.
    /// Membership survives navigation and is pruned when the channel stops
    /// being readable.
    #[cfg(feature = "voxhold-media")]
    voice_chat_rooms: std::collections::HashSet<(i64, i64)>,
    /// Channel of the active call, mirrored from the panel for the dock.
    #[cfg(feature = "voxhold-media")]
    voice_channel: i64,
    /// Local interface-cue player. It owns one background WinMM thread and
    /// never blocks the UI; the tests replace it with a capture sink.
    #[cfg(all(feature = "voxhold-media", not(test)))]
    voice_cue_player: voxhold_media::preview::CuePlayer,
    #[cfg(all(feature = "voxhold-media", test))]
    voice_cue_calls: Vec<CueCall>,
    /// Confirmed rosters keyed by `(server_id, channel_id)`, so browsing
    /// another server does not discard its cached room.
    #[cfg(feature = "voxhold-media")]
    voice_rooms: std::collections::HashMap<(i64, i64), Vec<voxhold_media::controller::Participant>>,
    /// Screen publications keyed by `(server_id, channel_id)`.
    #[cfg(feature = "voxhold-media")]
    voice_streams:
        std::collections::HashMap<(i64, i64), Vec<voxhold_media::controller::Publication>>,
    /// While an ACL reload is pending, cached room data is hidden until the
    /// fresh channel list confirms it again.
    #[cfg(feature = "voxhold-media")]
    voice_unconfirmed: std::collections::HashSet<i64>,
    servers: Vec<Server>,
    channels: Vec<Channel>,
    members: Vec<Member>,
    roster: members::Roster,
    messages: Vec<Message>,
    history_list: ListState,
    history_keys: Vec<HistoryKey>,
    /// Bumped when the room chat opens or closes so a read scheduled for the
    /// previous surface cannot mark anything visible.
    chat_scope_epoch: u64,
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
    /// One reaction-only refresh per personal message. The answer is merged
    /// into the loaded window instead of replacing it with the latest page.
    conversation_reaction_requests:
        std::collections::HashMap<(draft::ChatKey, i64), ConversationReactionRequest>,
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
    /// Realtime rows that arrived behind the gap of a window being replaced by
    /// an in-flight "К последним" request. Bound to that request and chat.
    latest_buffer: Option<chat_gap::LatestBuffer>,
    hovered_message: i64,
    /// Bumped when `attachments.changed` confirms a deletion, so the row
    /// re-measures without touching the server revision.
    attachment_epoch: u64,
    /// Bounded thumbnails, link previews and the single image viewer.
    media: media::Media,
    /// One timer for the nearest preview retry gate.
    preview_wakeup: Option<gpui::Task<()>>,
    preview_wake_at: Option<std::time::Instant>,
    event_task: Option<tokio::task::JoinHandle<()>>,
    realtime_generation: u64,
    /// The account socket ended and the outer loop is waiting for the next
    /// attempt. The loaded history stays visible; only the status line changes.
    realtime_reconnecting: bool,
    /// Live connection state written by the socket task. It is not an event:
    /// a full event queue cannot lose it, and `stop_realtime` replaces it so a
    /// stale task cannot touch a newer session.
    realtime_link: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
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
    /// One confirmed send in flight per chat. The runtime task is aborted on
    /// logout, so no POST starts after the session ended.
    pending_sends: std::collections::HashMap<draft::ChatKey, PendingSend>,
    /// The last POST of a chat whose outcome is unknown (a network error or a
    /// `504`). Its plan is kept because a later own WebSocket echo can still
    /// confirm it and consume the draft. A new send of the same chat replaces
    /// the entry, so the old attempt cannot clear the new composition's key.
    unconfirmed_sends: std::collections::HashMap<draft::ChatKey, draft::SendPlan>,
    /// Monotonic id of the latest send, stored in `PendingSend::attempt`.
    send_attempt: u64,
    /// Changes on login and logout; a send result from another session is
    /// discarded even when it arrives after the channel was switched.
    session_generation: u64,
    /// PATCH in flight for the personal profile. The lock belongs to the
    /// session, not to one dialog instance, so reopening the form cannot start
    /// a second write to the same resource.
    active_profile_save: Option<u64>,
    /// PUT in flight per notification channel; same session-level rule.
    notification_saves: std::collections::HashMap<(i64, i64), u64>,
    busy: bool,
    generation: u64,
    /// Update controller: background check, staging, helper launch and the
    /// header popup. It never blocks the UI thread.
    update: update_ops::UpdateUi,
    drafts: draft::Shared,
    /// Drafts kept across a forced expiry for the same `(origin, user_id)`.
    saved_drafts: Option<SavedDrafts>,
    auth: auth::AuthState,
    palette: theme::Palette,
    theme_mode: theme::ThemeMode,
    theme_path: Option<std::path::PathBuf>,
    system_light: bool,
    shell: layout::Settings,
    shell_path: Option<std::path::PathBuf>,
    /// Local "remember instance" preference. `None` or an empty path means the
    /// write is a no-op (tests); the shipped app always has a real path.
    instance_path: Option<std::path::PathBuf>,
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

impl Voxhold {
    pub(super) fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        prefill: &str,
        shell_path: Option<std::path::PathBuf>,
        instance_path: Option<std::path::PathBuf>,
        session_path: Option<std::path::PathBuf>,
    ) -> Self {
        Self::new_with_startup(
            window,
            cx,
            prefill,
            shell_path,
            instance_path,
            session_path,
            true,
        )
    }

    /// `start_restore == false` is used by fixtures that install a signed-in
    /// client immediately; they must not spawn the cold-start check or draw an
    /// extra auth frame before the workspace.
    pub(super) fn new_with_startup(
        window: &mut Window,
        cx: &mut Context<Self>,
        prefill: &str,
        shell_path: Option<std::path::PathBuf>,
        instance_path: Option<std::path::PathBuf>,
        session_path: Option<std::path::PathBuf>,
        start_restore: bool,
    ) -> Self {
        #[cfg(feature = "voxhold-media")]
        cx.bind_keys([
            gpui::KeyBinding::new("tab", crate::voice_panel::VoiceTab, Some("VoiceRoom")),
            gpui::KeyBinding::new(
                "shift-tab",
                crate::voice_panel::VoiceTabPrev,
                Some("VoiceRoom"),
            ),
            // The screen picker is a modal over the room; its own context is
            // deeper, so Tab cycles the picker ring instead of the room.
            gpui::KeyBinding::new("tab", crate::voice_panel::VoiceTab, Some("ScreenPicker")),
            gpui::KeyBinding::new(
                "shift-tab",
                crate::voice_panel::VoiceTabPrev,
                Some("ScreenPicker"),
            ),
        ]);
        let history_list = Self::new_history_list(ListAlignment::Bottom, cx);
        let system_light = theme::system_light();
        let theme_path = theme::theme_path();
        let theme_mode = theme::load_mode(theme_path.as_deref());
        let shell = shell_path
            .as_deref()
            .map(layout::Settings::load)
            .unwrap_or_default();
        let saved_instance = instance_path
            .as_deref()
            .map(instance_prefs::InstancePrefs::load)
            .unwrap_or_default();
        let prefill = prefill.trim();
        // An explicit CLI target always wins over the saved address. The saved
        // checkbox state is kept, but the saved origin never replaces what the
        // operator typed. Without a CLI target a valid remembered origin fills
        // the field and the screen still waits for confirmation.
        let (start_address, start_remember) = if !prefill.is_empty() {
            (prefill.to_string(), saved_instance.remember)
        } else if saved_instance.remember {
            (saved_instance.instance.clone().unwrap_or_default(), true)
        } else {
            (String::new(), false)
        };
        // The one startup check may only use the exact target the operator
        // named: `-server` wins, then the confirmed «Запомнить адрес» value.
        // Without either, the old format may still offer a single profile-wide
        // record; the new format never searches when its target is absent.
        let cli_origin = if prefill.is_empty() {
            None
        } else {
            instance_prefs::canonical_origin(prefill)
        };
        let saved_origin = saved_instance
            .instance
            .clone()
            .filter(|_| saved_instance.remember);
        let cli_invalid = !prefill.is_empty() && cli_origin.is_none();
        let startup_origin = cli_origin.or(saved_origin);
        let allow_profile_single = prefill.is_empty();
        // An explicit but invalid `-server` must never fall back to a stored
        // target: the operator asked for this address, and the form shows it.
        let startup_disabled = cli_invalid;
        let recent_reactions = reactions::SharedRecents::default();
        let reaction_picker =
            cx.new(|cx| reactions::ReactionPicker::new(recent_reactions.clone(), window, cx));
        let messaging = messages::Messages::new(window, cx);
        let conversation_query_subscription = cx.subscribe(
            &messaging.query,
            |this, _, event: &InputEvent, cx| match event {
                InputEvent::Change => {
                    if this.messaging.global {
                        // A new query is a new intent: a hit clicked for the
                        // previous query must not open behind fresh results.
                        this.messaging.open_request = None;
                        this.messaging.search.invalidate();
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    if this.messaging.global {
                        this.submit_messages_search(cx);
                    }
                }
                _ => {}
            },
        );
        let notification_path = shell_path
            .as_deref()
            .map(crate::notifications::path_from_layout);
        let voice_path = shell_path
            .as_deref()
            .map(crate::voice_settings::path_from_layout);
        let mut view = Self {
            runtime: std::sync::Arc::new(tokio::runtime::Runtime::new().expect("Tokio runtime")),
            instance: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("https://example.com")
                    .default_value(start_address.clone())
            }),
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
            session: None,
            session_epoch: 0,
            session_gate: auth_session::RefreshGate::default(),
            session_clock: None,
            session_store: session_store::SessionStore::new(session_path),
            auto_attempt: 0,
            last_stored_token: None,
            remember_error: String::new(),
            auto_record: None,
            auto_epoch: 0,
            auto_blocked: false,
            startup_restoring: start_restore,
            startup_checked: !start_restore,
            startup_deadline: std::time::Instant::now(),
            startup_budget: auto_login::STARTUP_BUDGET,
            migration_pending: false,
            cleanup_pending: false,
            cleanup_epoch: 0,
            cleanup_claim: Default::default(),
            cleanup_retry: false,
            restore_cancel_focus: cx.focus_handle(),
            restore_focus_done: false,
            self_user_id: 0,
            search: None,
            show_search: false,
            search_permissions_ready: false,
            search_permissions_request: None,
            thread: None,
            show_thread: false,
            message_menu: None,
            member_menu: None,
            channel_menu: None,
            forward: None,
            action_requests: Default::default(),
            action_seq: 0,
            action_error: None,
            action_notice: String::new(),
            editing: None,
            edit_open_seq: 0,
            edit_error: None,
            edit_input: cx.new(|cx| {
                InputState::new(window, cx)
                    .multi_line()
                    .placeholder("Изменить сообщение")
            }),
            reaction_picker,
            confirm_delete: None,
            pinned_ids: Default::default(),
            pinned_request: None,
            scroll_epoch: 0,
            highlighted_message: None,
            highlight_seq: 0,
            selected_messages: Vec::new(),
            conversation: None,
            conversations: Vec::new(),
            recents_request: None,
            recents_again: false,
            conversations_reconcile: None,
            conversation_select_seq: 0,
            channel_snapshot_request: None,
            attachments: None,
            personal: None,
            server_settings: None,
            channel_settings: None,
            channel_notifications: None,
            recordings: recordings::RecordingsState::default(),
            recording_access_epoch: 0,
            recordings_dialog_seq: 0,
            recordings_op_seq: 0,
            recordings_dialog: None,
            recordings_return_focus: None,
            recordings_entry_focus: cx.focus_handle(),
            recordings_close_focus: cx.focus_handle(),
            recordings_voice_entry: false,
            recordings_preview_aborts: Vec::new(),
            recordings_live: std::rc::Rc::new(std::cell::RefCell::new(None)),
            voice_settings: crate::voice_settings::VoiceSettings::with_path(voice_path),
            notifications: crate::notifications::NotificationCenter::with_path(notification_path),
            notification_tray: crate::notifications::Tray::default(),
            active_settings: None,
            settings_return_focus: None,
            settings_ring: Vec::new(),
            settings_slider_focus: Vec::new(),
            slider_tracks: std::collections::HashMap::new(),
            slider_drag: None,
            ui_scale: 1.0,
            ui_scale_pinned: false,
            settings_scroll: gpui::ScrollHandle::new(),
            settings_close_focus: cx.focus_handle(),
            settings_entry_focus: cx.focus_handle(),
            server_entry_focus: cx.focus_handle(),
            #[cfg(test)]
            notifications_entry_focus: cx.focus_handle(),
            window_active: true,
            personal_mode: false,
            personal_return: None,
            pending_return_channel: None,
            personal_members: false,
            messaging,
            pending_personal_cleanup: false,
            _conversation_query_subscription: conversation_query_subscription,
            _creator_subscriptions: Vec::new(),
            show_members: false,
            show_channels: false,
            show_servers: false,
            #[cfg(feature = "voxhold-media")]
            voice: None,
            #[cfg(feature = "voxhold-media")]
            power_task: None,
            #[cfg(feature = "voxhold-media")]
            voice_room: 0,
            #[cfg(feature = "voxhold-media")]
            voice_chat_rooms: std::collections::HashSet::new(),
            #[cfg(feature = "voxhold-media")]
            voice_channel: 0,
            #[cfg(all(feature = "voxhold-media", not(test)))]
            voice_cue_player: voxhold_media::preview::CuePlayer::new(),
            #[cfg(all(feature = "voxhold-media", test))]
            voice_cue_calls: Vec::new(),
            #[cfg(feature = "voxhold-media")]
            voice_unconfirmed: std::collections::HashSet::new(),
            voice_rooms: std::collections::HashMap::new(),
            #[cfg(feature = "voxhold-media")]
            voice_streams: std::collections::HashMap::new(),
            servers: vec![],
            channels: vec![],
            members: vec![],
            roster: members::Roster::default(),
            messages: vec![],
            history_list,
            history_keys: Vec::new(),
            chat_scope_epoch: 0,
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
            conversation_reaction_requests: Default::default(),
            recent_reactions: recent_reactions.clone(),
            scrollbar_drag: None,
            context_request: None,
            pending_jump: None,
            history_reset_pending: false,
            latest_buffer: None,
            hovered_message: 0,
            attachment_epoch: 0,
            media: media::Media::default(),
            preview_wakeup: None,
            preview_wake_at: None,
            event_task: None,
            realtime_generation: 0,
            realtime_reconnecting: false,
            realtime_link: None,
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
            pending_sends: Default::default(),
            unconfirmed_sends: Default::default(),
            send_attempt: 0,
            session_generation: 0,
            active_profile_save: None,
            notification_saves: Default::default(),
            busy: false,
            generation: 0,
            update: update_ops::UpdateUi::new(),
            drafts: draft::Drafts::shared(),
            saved_drafts: None,
            auth: {
                let mut auth = auth::AuthState::with_prefill(&start_address);
                auth.remember_instance = start_remember;
                auth
            },
            palette: theme::Palette::resolve(theme_mode, system_light),
            theme_mode,
            theme_path,
            system_light,
            shell,
            shell_path,
            instance_path,
            shell_initialized: false,
            drag: None,
            viewport: (0.0, 0.0),
            auth_background: None,
            initial_focus_done: false,
            pending_anchor: None,
            deleted_ids: Default::default(),
            _activation_subscription: cx.observe_window_activation(window, |s, window, cx| {
                s.window_active = window.is_window_active();
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
        gpui_component::Theme::global_mut(cx).font_family = theme::UI_FONT.into();
        window_chrome::apply(window, view.palette.light);
        Self::bind_composer_keys(cx);
        // The explicit startup phase: the store is read and the target is
        // checked once, before the first frame can paint a manual form.
        if start_restore {
            view.begin_startup_restore(
                window,
                cx,
                startup_origin,
                allow_profile_single,
                startup_disabled,
            );
        }
        view
    }
}
