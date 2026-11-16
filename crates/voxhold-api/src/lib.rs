mod client_extra;
mod recordings;
use futures_util::{SinkExt, StreamExt};
pub use recordings::{
    Recording, RecordingList, RECORDING_DELETED, RECORDING_FAILED, RECORDING_LOCAL_ONLY,
    RECORDING_READY, RECORDING_STARTED, RECORDING_UPLOADING,
};
use reqwest::{Method, Url};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::{fmt, net::IpAddr, time::Duration};

#[derive(Debug)]
pub enum Error {
    InvalidOrigin,
    InvalidResponse(&'static str),
    Http(u16),
    /// A local file or pipe problem with a ready user-facing text.
    Local(String),
    Network,
    Json(serde_json::Error),
    WebSocket(String),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOrigin => write!(
                f,
                "Укажите HTTPS адрес сервера (HTTP разрешён только для localhost)."
            ),
            Self::InvalidResponse(s) => write!(f, "{s}"),
            Self::Http(s) => write!(f, "Сервер вернул ошибку {s}."),
            Self::Local(s) => write!(f, "{s}"),
            Self::Network => write!(f, "Не удалось связаться с сервером."),
            Self::Json(_) => write!(f, "Некорректный ответ сервера."),
            Self::WebSocket(_) => write!(f, "Соединение с сервером прервано."),
        }
    }
}
impl Error {
    /// HTTP status of a refused request, used by retry gates and `429`
    /// backoff. Local and transport errors have no status.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Http(status) => Some(*status),
            _ => None,
        }
    }
}
impl std::error::Error for Error {}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone)]
pub struct Client {
    base: Url,
    http: reqwest::Client,
    token: std::sync::Arc<str>,
}
impl Client {
    pub fn new(raw: &str) -> Result<Self> {
        let mut base = Url::parse(raw.trim()).map_err(|_| Error::InvalidOrigin)?;
        if base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || base.path() != "/"
        {
            return Err(Error::InvalidOrigin);
        }
        let host = base.host_str().unwrap();
        let local = host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        if base.scheme() != "https" && !(base.scheme() == "http" && local) {
            return Err(Error::InvalidOrigin);
        }
        base.set_path("/");
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(8))
            .build()
            .map_err(|_| Error::Network)?;
        Ok(Self {
            base,
            http,
            token: std::sync::Arc::from(""),
        })
    }
    pub fn set_token(&mut self, token: String) {
        self.token = token.into();
    }
    pub fn token(&self) -> &str {
        &self.token
    }
    fn url(&self, path: &str) -> Result<Url> {
        if !path.starts_with('/') || path.starts_with("//") {
            return Err(Error::InvalidOrigin);
        }
        let url = self.base.join(path).map_err(|_| Error::InvalidOrigin)?;
        if url.origin() != self.base.origin() {
            return Err(Error::InvalidOrigin);
        }
        Ok(url)
    }
    async fn bytes(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        max: usize,
    ) -> Result<Vec<u8>> {
        self.bytes_with_etag(method, path, body, max)
            .await
            .map(|(body, _)| body)
    }

    async fn bytes_with_etag(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        max: usize,
    ) -> Result<(Vec<u8>, Option<String>)> {
        let mut r = self.http.request(method, self.url(path)?);
        if !self.token.is_empty() {
            r = r.bearer_auth(self.token.as_ref());
        }
        if let Some(b) = body {
            r = r.json(&b);
        }
        let mut response = r.send().await.map_err(|_| Error::Network)?;
        if !response.status().is_success() {
            return Err(Error::Http(response.status().as_u16()));
        }
        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let mut out = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Network)? {
            if out.len().saturating_add(chunk.len()) > max {
                return Err(Error::InvalidResponse(
                    "Ответ сервера превышает допустимый размер.",
                ));
            }
            out.extend_from_slice(&chunk);
        }
        Ok((out, etag))
    }
    pub async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<T> {
        Ok(serde_json::from_slice(
            &self.bytes(method, path, body, 4 << 20).await?,
        )?)
    }
    pub async fn action(&self, method: Method, path: &str, body: Option<Value>) -> Result<()> {
        self.bytes(method, path, body, 4 << 20).await?;
        Ok(())
    }
    pub async fn login(&mut self, username: &str, password: &str) -> Result<()> {
        let v: Value = self
            .request(
                Method::POST,
                "/api/v1/auth/login",
                Some(json!({"username":username,"password":password})),
            )
            .await?;
        let token = v["session"]["token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or(Error::InvalidResponse("Сервер не вернул сессию."))?;
        self.token = std::sync::Arc::from(token);
        Ok(())
    }
    pub async fn servers(&self) -> Result<Vec<Server>> {
        self.request(Method::GET, "/api/v1/me/servers", None).await
    }
    pub async fn channels(&self, sid: i64) -> Result<Vec<Channel>> {
        self.request(
            Method::GET,
            &format!("/api/v1/servers/{sid}/channels"),
            None,
        )
        .await
    }
    pub async fn members(&self, sid: i64) -> Result<Vec<Member>> {
        self.request(Method::GET, &format!("/api/v1/servers/{sid}/members"), None)
            .await
    }
    pub async fn channel_members(&self, sid: i64, cid: i64) -> Result<Vec<i64>> {
        #[derive(Deserialize)]
        struct Members {
            #[serde(default, deserialize_with = "null_default")]
            user_ids: Vec<i64>,
        }
        let members: Members = self
            .request(
                Method::GET,
                &format!("{}/members", channel_path(sid, cid)),
                None,
            )
            .await?;
        Ok(members.user_ids)
    }
    async fn page(&self, path: &str, before: i64) -> Result<MessagePage> {
        let path = format!(
            "{path}?limit=50{}",
            if before > 0 {
                format!("&before_id={before}")
            } else {
                String::new()
            }
        );
        let page: MessagePage = self.request(Method::GET, &path, None).await?;
        if page.pagination.has_more
            && (page
                .pagination
                .next_before_id
                .is_none_or(|id| id <= 0 || (before > 0 && id >= before))
                || page.messages.is_empty())
        {
            return Err(Error::InvalidResponse("Некорректный курсор истории."));
        }
        Ok(page)
    }
    pub async fn messages_page(&self, sid: i64, cid: i64, before: i64) -> Result<MessagePage> {
        self.page(&format!("{}/messages", channel_path(sid, cid)), before)
            .await
    }
    pub async fn thread_messages(
        &self,
        sid: i64,
        cid: i64,
        root: i64,
        before: i64,
    ) -> Result<MessagePage> {
        let page = self
            .page(
                &format!("{}/comments", message_path(sid, cid, root)),
                before,
            )
            .await?;
        // Comments must belong to the requested root and channel; a responder
        // that mixes the main timeline into a thread is rejected.
        if page.messages.iter().any(|message| {
            message.id <= 0 || message.channel_id != cid || message.thread_root_id != Some(root)
        }) {
            return Err(Error::InvalidResponse("Некорректный ответ обсуждения."));
        }
        Ok(page)
    }
    pub async fn send_message(&self, sid: i64, cid: i64, content: &str) -> Result<Message> {
        self.request(
            Method::POST,
            &format!("{}/messages", channel_path(sid, cid)),
            Some(json!({"content":content})),
        )
        .await
    }
    pub async fn reply_message(
        &self,
        sid: i64,
        cid: i64,
        id: i64,
        content: &str,
    ) -> Result<Message> {
        self.request(
            Method::POST,
            &format!("{}/messages", channel_path(sid, cid)),
            Some(json!({"content":content,"reply_to_message_id":id})),
        )
        .await
    }
    pub async fn edit_message(
        &self,
        sid: i64,
        cid: i64,
        id: i64,
        content: &str,
    ) -> Result<Message> {
        self.request(
            Method::PATCH,
            &message_path(sid, cid, id),
            Some(json!({"content":content})),
        )
        .await
    }
    pub async fn delete_message(&self, sid: i64, cid: i64, id: i64) -> Result<()> {
        self.action(Method::DELETE, &message_path(sid, cid, id), None)
            .await
    }
    pub async fn mark_read(&self, sid: i64, cid: i64, id: i64) -> Result<()> {
        self.action(
            Method::PUT,
            &format!("{}/read", channel_path(sid, cid)),
            Some(json!({"last_read_message_id":id})),
        )
        .await
    }
    pub async fn mark_thread_read(&self, cid: i64, root: i64, id: i64) -> Result<()> {
        self.action(
            Method::PUT,
            &format!("/api/v1/channels/{cid}/threads/{root}"),
            Some(json!({"last_read_id":id})),
        )
        .await
    }
    pub async fn recent_conversations(&self) -> Result<Vec<Conversation>> {
        let v: Value = self
            .request(Method::GET, "/api/v1/me/conversations?limit=13", None)
            .await?;
        let rows: Vec<Conversation> = serde_json::from_value(v["conversations"].clone())?;
        Ok(rows
            .into_iter()
            .filter(|c| c.id > 0 && !c.saved)
            .take(12)
            .collect())
    }
    /// Personal history. The backend returns the newest 50 rows first; the
    /// client keeps an ascending window so merge/anchor rules match channels.
    pub async fn conversation_messages(&self, id: i64, before: i64) -> Result<MessagePage> {
        if id <= 0 || before < 0 {
            return Err(Error::InvalidResponse("Некорректный диалог."));
        }
        let v: Value = self
            .request(
                Method::GET,
                &format!(
                    "/api/v1/conversations/{id}/messages{}",
                    if before > 0 {
                        format!("?before={before}")
                    } else {
                        String::new()
                    }
                ),
                None,
            )
            .await?;
        let mut messages: Vec<Message> = serde_json::from_value(v["messages"].clone())?;
        if messages.iter().any(|message| {
            message.id <= 0 || message.conversation_id != id || message.channel_id != 0
        }) {
            return Err(Error::InvalidResponse("Некорректный ответ истории."));
        }
        messages.sort_by_key(|message| message.id);
        let next_before_id = messages.first().map(|message| message.id);
        let has_more = v["has_more"].as_bool().unwrap_or(false);
        if has_more && (next_before_id.is_none_or(|id| id <= 0) || messages.is_empty()) {
            return Err(Error::InvalidResponse("Некорректный курсор истории."));
        }
        Ok(MessagePage {
            messages,
            pagination: Pagination {
                next_before_id,
                has_more,
            },
        })
    }
    pub async fn create_conversation_message(
        &self,
        id: i64,
        content: &str,
        key: &str,
        attachments: &[String],
    ) -> Result<Message> {
        self.request(
            Method::POST,
            &format!("/api/v1/conversations/{id}/messages"),
            Some(json!({"content":content,"client_message_id":key,"attachment_ids":attachments})),
        )
        .await
    }
    pub async fn mark_conversation_read(&self, id: i64, message_id: i64) -> Result<()> {
        self.action(
            Method::PUT,
            &format!("/api/v1/conversations/{id}/read"),
            Some(json!({"message_id":message_id})),
        )
        .await
    }
    pub async fn search_messages(&self, sid: i64, query: &str, before: i64) -> Result<SearchPage> {
        if sid <= 0 || query.trim().is_empty() || query.chars().count() > 200 {
            return Err(Error::InvalidResponse("Введите корректный запрос."));
        }
        let mut u = self.url(&format!("/api/v1/servers/{sid}/messages/search"))?;
        {
            let mut q = u.query_pairs_mut();
            q.append_pair("q", query).append_pair("limit", "25");
            if before > 0 {
                q.append_pair("before_id", &before.to_string());
            }
        }
        let page: SearchPage = self
            .request(
                Method::GET,
                &format!("{}?{}", u.path(), u.query().unwrap_or("")),
                None,
            )
            .await?;
        if page
            .messages
            .iter()
            .any(|hit| hit.message.id <= 0 || hit.message.channel_id <= 0)
        {
            // Search answers are channel rows; a comment or a foreign row is
            // not a valid jump target.
            return Err(Error::InvalidResponse("Некорректный ответ поиска."));
        }
        if page.pagination.has_more
            && page
                .pagination
                .next_before_id
                .is_none_or(|id| id <= 0 || (before > 0 && id >= before))
        {
            return Err(Error::InvalidResponse("Некорректный курсор поиска."));
        }
        Ok(page)
    }
    pub async fn webrtc_config(&self) -> Result<WebRtcConfig> {
        let v: WebRtcConfig = self
            .request(Method::GET, "/api/v1/webrtc/config", None)
            .await?;
        if v.ice_servers.iter().any(|s| {
            s.urls.is_empty()
                || s.urls.iter().any(|u| u.trim().is_empty())
                || s.username.is_empty() != s.credential.is_empty()
        }) {
            return Err(Error::InvalidResponse("invalid ICE server"));
        }
        Ok(v)
    }
}
pub fn channel_path(sid: i64, cid: i64) -> String {
    format!("/api/v1/servers/{sid}/channels/{cid}")
}
pub fn message_path(sid: i64, cid: i64, id: i64) -> String {
    format!("{}/messages/{id}", channel_path(sid, cid))
}

fn null_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}
macro_rules! dto { ($name:ident { $($field:ident : $ty:ty),* $(,)? }) => { #[derive(Debug, Clone, Default, Serialize, Deserialize)] #[serde(default)] pub struct $name { $(#[serde(default, deserialize_with="null_default")] pub $field:$ty),* } }; }
dto!(Server {
    id: i64,
    name: String,
    role: String
});
dto!(Channel {
    id: i64,
    name: String,
    kind: String,
    position: i64,
    permissions: i32,
    access_restricted: bool,
    e2ee_required: bool,
    comments_enabled: bool,
    can_publish: bool,
    unread_count: i64
});
impl Channel {
    pub fn readable(&self) -> bool {
        self.permissions & 1 != 0
    }
    pub fn has_history(&self) -> bool {
        self.readable() && !self.e2ee_required && (self.kind == "text" || self.kind == "news")
    }
    pub fn writable(&self) -> bool {
        self.has_history() && self.permissions & 2 != 0 && (self.kind != "news" || self.can_publish)
    }
}
dto!(Author {
    user_id: i64,
    username: String
});
dto!(MessageReference {
    message_id: i64,
    author: Author,
    content: String,
    deleted: bool
});
dto!(Reaction {
    emoji: String,
    count: i32,
    me: bool
});
dto!(Attachment {
    id: String,
    filename: String,
    mime_type: String,
    size_bytes: i64,
    width: i32,
    height: i32,
    deleted: bool
});
dto!(Message { id:i64, channel_id:i64, conversation_id:i64, content:String, author:Author, created_at:i64, edited_at:Option<i64>, revision:i64, comment_count:i64, thread_root_id:Option<i64>, client_message_id:String, reply:Option<MessageReference>, forward:Option<MessageReference>, reactions:Vec<Reaction>, reactions_revision:i64, attachments:Vec<Attachment>, poll:Value });
dto!(Pagination { next_before_id:Option<i64>, has_more:bool });
dto!(MessagePage { messages:Vec<Message>, pagination:Pagination });
dto!(Member { avatar_version:Option<String>, country_code:Option<String>, user_id:i64, username:String, role:String, last_seen_at:Option<i64> });
dto!(Conversation { id:i64, name:String, direct:bool, members:Vec<Author>, unread_count:i64, saved:bool, muted:bool, last_message_id:i64 });
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchResult {
    #[serde(flatten)]
    pub message: Message,
    #[serde(default)]
    pub channel_name: String,
}
dto!(SearchPage { messages:Vec<SearchResult>, pagination:Pagination });
dto!(PinnedMessage {
    message: Message,
    pinned_by: Author,
    pinned_at: i64
});
dto!(ReactionsUpdate { channel_id:i64, message_id:i64, revision:i64, reactions:Vec<Reaction> });
dto!(DirectReactions { revision:i64, reactions:Vec<Reaction> });
dto!(ForwardDestination {
    kind: String,
    id: i64,
    name: String,
    server_id: i64
});
dto!(MessageContext { messages:Vec<Message>, target_message_id:i64, target_index:usize });
dto!(Profile { user_id:i64, username:String, about:String, country_code:Option<String>, created_at:i64 });
dto!(AccountSession { id:i64, user_agent:String, client_ip:String, current:bool, created_at:i64, last_used_at:Option<i64> });
dto!(MessagePrivacy { mode:String, blocked_user_ids:Vec<i64> });
dto!(ChannelAccess { restricted:bool, user_ids:Vec<i64>, roles:Vec<String> });
dto!(NewsSettings { publisher_ids:Vec<i64>, comments_enabled:bool });
dto!(StorageCategory {
    quota_bytes: i64,
    used_bytes: i64
});
dto!(StorageState { available_bytes:i64, capacity_bytes:i64, reserve_percent:i32, categories:std::collections::HashMap<String,StorageCategory> });
dto!(CleanupPlan {
    category: String,
    before: i64,
    bytes: i64,
    count: i32,
    token: String
});
dto!(ServerBan {
    user_id: i64,
    username: String,
    reason: String,
    created_at: i64
});
dto!(CustomRole { id:i64, name:String, color:String, permissions:i32, user_ids:Vec<i64> });
dto!(RolePreview {
    name: String,
    permissions: i32,
    can_publish: bool
});
dto!(LinkPreview {
    url: String,
    title: String,
    description: String,
    site_name: String,
    image_path: String,
    unavailable: bool
});
dto!(ChannelRead {
    server_id: i64,
    channel_id: i64,
    user_id: i64,
    last_read_message_id: i64
});
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RealtimeEvent {
    #[serde(rename = "type")]
    pub kind: String,
    pub data: Value,
    pub request_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelSubscription {
    pub server_id: i64,
    pub channel_id: i64,
    pub generation: u64,
}

impl ChannelSubscription {
    fn payload(self) -> Value {
        json!({"server_id":self.server_id,"channel_id":self.channel_id})
    }

    fn request_id(self) -> String {
        format!("native-channel-{}", self.generation)
    }
}
dto!(IceServer { urls:Vec<String>, username:String, credential:String });
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebRtcConfig {
    pub ice_servers: Vec<IceServer>,
    #[serde(default)]
    pub ice_transport_policy: String,
}

pub struct VoiceSocket {
    socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}
impl Client {
    pub async fn open_voice_socket(&self) -> Result<VoiceSocket> {
        let mut u = self.url("/api/v1/ws")?;
        u.set_scheme(if u.scheme() == "https" { "wss" } else { "ws" })
            .map_err(|_| Error::InvalidOrigin)?;
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
            max_message_size: Some(1 << 20),
            max_frame_size: Some(1 << 20),
            ..Default::default()
        };
        let (socket, _) =
            tokio_tungstenite::connect_async_with_config(u.as_str(), Some(config), false)
                .await
                .map_err(|e| Error::WebSocket(e.to_string()))?;
        let mut s = VoiceSocket { socket };
        s.send(
            "auth",
            json!({"token":self.token.as_ref(),"multi_streams":true}),
            None,
        )
        .await?;
        Ok(s)
    }
    pub async fn subscribe<F: FnMut(RealtimeEvent)>(
        &self,
        sid: i64,
        cid: i64,
        mut receive: F,
    ) -> Result<()> {
        let mut socket = self.open_voice_socket().await?;
        loop {
            let event = socket.read().await?;
            if event.kind == "ready" && cid != 0 {
                socket
                    .send(
                        "channel.subscribe",
                        json!({"server_id":sid,"channel_id":cid}),
                        Some("native-channel-subscribe"),
                    )
                    .await?;
            }
            if event.kind == "error" {
                if event.request_id == "native-channel-subscribe"
                    && event.data["code"].as_str() == Some("forbidden")
                {
                    return Err(Error::Http(403));
                }
                return Ok(());
            }
            receive(event);
        }
    }

    /// Keep account events on the same socket when the selected channel changes.
    pub async fn subscribe_channels(
        &self,
        mut desired: tokio::sync::watch::Receiver<Option<ChannelSubscription>>,
        mut receive: impl FnMut(RealtimeEvent),
    ) -> Result<()> {
        let mut socket = self.open_voice_socket().await?;
        let ready = tokio::time::timeout(Duration::from_secs(8), socket.read())
            .await
            .map_err(|_| Error::Network)??;
        if ready.kind != "ready" {
            return Err(Error::InvalidResponse(
                "Сервер не подтвердил WebSocket-сессию.",
            ));
        }
        receive(ready);

        let mut subscribed: Option<ChannelSubscription> = None;
        loop {
            let next = *desired.borrow_and_update();
            if next != subscribed {
                if let Some(previous) = subscribed {
                    socket
                        .send("channel.unsubscribe", previous.payload(), None)
                        .await?;
                }
                if let Some(next) = next {
                    socket
                        .send(
                            "channel.subscribe",
                            next.payload(),
                            Some(&next.request_id()),
                        )
                        .await?;
                }
                subscribed = next;
            }

            tokio::select! {
                changed = desired.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                }
                event = socket.read() => {
                    let mut event = event?;
                    if event.kind == "channel.subscribed" {
                        let Some(channel) = subscribed.filter(|channel| event.request_id == channel.request_id()) else {
                            continue;
                        };
                        if event.data["server_id"].as_i64() != Some(channel.server_id)
                            || event.data["channel_id"].as_i64() != Some(channel.channel_id)
                        {
                            continue;
                        }
                        event.data["generation"] = json!(channel.generation);
                    }
                    if event.kind == "error" {
                        if let Some(channel) = subscribed.filter(|channel| event.request_id == channel.request_id()) {
                            if event.data["code"].as_str() == Some("forbidden") {
                                event.kind = "native.subscription_forbidden".into();
                                event.data = channel.payload();
                                event.data["generation"] = json!(channel.generation);
                            }
                        }
                    }
                    receive(event);
                }
            }
        }
    }
    pub async fn image(&self, path: &str, max_edge: u32) -> Result<image::DynamicImage> {
        let bytes = self.bytes(Method::GET, path, None, 4 << 20).await?;
        Self::decode_image(&bytes, max_edge, 4_000_000)
    }

    fn decode_image(bytes: &[u8], max_edge: u32, max_pixels: u64) -> Result<image::DynamicImage> {
        let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|_| Error::InvalidResponse("invalid image"))?;
        let format = reader.format();
        let (w, h) = reader
            .into_dimensions()
            .map_err(|_| Error::InvalidResponse("invalid image"))?;
        if w == 0
            || h == 0
            || w > max_edge
            || h > max_edge
            || u64::from(w) * u64::from(h) > max_pixels
        {
            return Err(Error::InvalidResponse("invalid image dimensions"));
        }
        image::load_from_memory_with_format(
            bytes,
            format.ok_or(Error::InvalidResponse("invalid image"))?,
        )
        .map_err(|_| Error::InvalidResponse("invalid image"))
    }
    pub async fn avatar(&self, id: i64, version: &str) -> Result<image::DynamicImage> {
        if id <= 0 || !valid_id(version, 64) {
            return Err(Error::InvalidResponse(
                "Некорректный ID или версия аватара.",
            ));
        }
        let path = format!("/api/v1/users/{id}/avatar?v={version}");
        let (bytes, etag) = self
            .bytes_with_etag(Method::GET, &path, None, 4 << 20)
            .await?;
        if etag
            .as_deref()
            .and_then(|tag| tag.strip_prefix('"')?.strip_suffix('"'))
            != Some(version)
        {
            return Err(Error::InvalidResponse("Версия аватара изменилась."));
        }
        Self::decode_image(&bytes, 256, 4_000_000)
    }
    pub async fn attachment_thumbnail(&self, id: &str) -> Result<image::DynamicImage> {
        if !valid_id(id, 32) {
            return Err(Error::InvalidResponse("invalid attachment id"));
        }
        self.image(&format!("/api/v1/attachments/{id}/thumbnail"), 512)
            .await
    }
    /// Original chat picture for the viewer. Go limits: 48 MiB compressed,
    /// 8192 px per side and 40 megapixels decoded.
    pub async fn attachment_content_image(&self, id: &str) -> Result<image::DynamicImage> {
        if !valid_id(id, 32) {
            return Err(Error::InvalidResponse("invalid attachment id"));
        }
        let bytes = self
            .bytes(
                Method::GET,
                &format!("/api/v1/attachments/{id}/content"),
                None,
                48 << 20,
            )
            .await?;
        Self::decode_image(&bytes, 8192, 40_000_000)
    }
    pub async fn link_preview_image(&self, path: &str) -> Result<image::DynamicImage> {
        let prefix = "/api/v1/link-preview/images/";
        if !path.strip_prefix(prefix).is_some_and(|s| valid_id(s, 64)) {
            return Err(Error::InvalidResponse("invalid preview image path"));
        }
        self.image(path, 1024).await
    }
}

fn valid_id(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl VoiceSocket {
    pub async fn send(&mut self, kind: &str, data: Value, id: Option<&str>) -> Result<()> {
        let mut v = json!({"type":kind,"data":data});
        if let Some(id) = id.filter(|s| !s.is_empty()) {
            v["request_id"] = json!(id);
        }
        tokio::time::timeout(
            Duration::from_secs(8),
            self.socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    v.to_string().into(),
                )),
        )
        .await
        .map_err(|_| Error::Network)?
        .map_err(|e| Error::WebSocket(e.to_string()))
    }
    pub async fn read(&mut self) -> Result<RealtimeEvent> {
        loop {
            let m = self
                .socket
                .next()
                .await
                .ok_or(Error::Network)?
                .map_err(|e| Error::WebSocket(e.to_string()))?;
            match m {
                tokio_tungstenite::tungstenite::Message::Text(s) => {
                    return Ok(serde_json::from_str(&s)?)
                }
                tokio_tungstenite::tungstenite::Message::Binary(b) => {
                    return Ok(serde_json::from_slice(&b)?)
                }
                tokio_tungstenite::tungstenite::Message::Close(_) => return Err(Error::Network),
                _ => {}
            }
        }
    }
    pub async fn close(&mut self) -> Result<()> {
        self.socket
            .close(None)
            .await
            .map_err(|e| Error::WebSocket(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_channels_reuses_the_authenticated_socket() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(5), async {
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let mut client = Client::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
                    client.set_token("fixture".into());
                    let first = ChannelSubscription { server_id: 1, channel_id: 10, generation: 1 };
                    let second = ChannelSubscription { server_id: 1, channel_id: 11, generation: 2 };
                    let (target, desired) = tokio::sync::watch::channel(Some(first));
                    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
                    let mut worker = tokio::spawn(async move {
                        client.subscribe_channels(desired, |event| {
                            let _ = events.send(event);
                        }).await
                    });
                    let (connection, _) = tokio::select! {
                        connection = listener.accept() => connection.unwrap(),
                        result = &mut worker => panic!("WebSocket client stopped before connecting: {result:?}"),
                    };
                    let mut peer = tokio_tungstenite::accept_async(connection).await.unwrap();
                    async fn next(
                        peer: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
                    ) -> RealtimeEvent {
                        let message = peer.next().await.unwrap().unwrap();
                        serde_json::from_str(message.to_text().unwrap()).unwrap()
                    }
                    async fn send(
                        peer: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
                        event: Value,
                    ) {
                        peer.send(tokio_tungstenite::tungstenite::Message::Text(event.to_string().into())).await.unwrap();
                    }
                    assert_eq!(next(&mut peer).await.kind, "auth");
                    send(&mut peer, json!({"type":"ready"})).await;
                    let subscribe = next(&mut peer).await;
                    assert_eq!(subscribe.kind, "channel.subscribe");
                    assert_eq!(subscribe.data, first.payload());

                    target.send(Some(second)).unwrap();
                    let unsubscribe = next(&mut peer).await;
                    assert_eq!(unsubscribe.kind, "channel.unsubscribe");
                    assert_eq!(unsubscribe.data, first.payload());
                    let subscribe = next(&mut peer).await;
                    assert_eq!(subscribe.kind, "channel.subscribe");
                    assert_eq!(subscribe.data, second.payload());

                    send(&mut peer, json!({"type":"error","request_id":first.request_id(),"data":{"code":"forbidden"}})).await;
                    send(&mut peer, json!({"type":"error","request_id":second.request_id(),"data":{"code":"forbidden"}})).await;
                    assert_eq!(received.recv().await.unwrap().kind, "ready");
                    assert_eq!(received.recv().await.unwrap().kind, "error");
                    let denied = received.recv().await.unwrap();
                    assert_eq!(denied.kind, "native.subscription_forbidden");
                    assert_eq!(denied.data["channel_id"], 11);
                    assert_eq!(denied.data["generation"], 2);

                    target.send(None).unwrap();
                    assert_eq!(next(&mut peer).await.kind, "channel.unsubscribe");
                    send(&mut peer, json!({"type":"presence.updated","data":{"user_id":4,"status":"online"}})).await;
                    assert_eq!(received.recv().await.unwrap().kind, "presence.updated");
                    drop(target);
                    worker.await.unwrap().unwrap();
                }).await.unwrap();
            });
    }
    #[test]
    fn origin_security() {
        for bad in [
            "http://example.com",
            "https://example.com/path",
            "https://u:p@example.com",
            "https://example.com/?token=x",
            "https://example.com/#x",
            "http://localhost.evil.test",
        ] {
            assert!(Client::new(bad).is_err(), "{bad}");
        }
        for good in [
            "https://example.com",
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
        ] {
            assert!(Client::new(good).is_ok(), "{good}");
        }
    }
    #[test]
    fn path_security() {
        let c = Client::new("https://example.com").unwrap();
        assert!(c.url("//evil.test/x").is_err());
        assert!(c.url("https://evil.test").is_err());
        assert_eq!(c.url("/api/v1/ws").unwrap().host_str(), Some("example.com"));
    }
    #[test]
    fn permissions() {
        let mut c = Channel {
            kind: "news".into(),
            permissions: 3,
            ..Default::default()
        };
        assert!(c.has_history());
        assert!(!c.writable());
        c.can_publish = true;
        assert!(c.writable());
        c.e2ee_required = true;
        assert!(!c.has_history());
    }
}
#[cfg(test)]
mod compat_tests;
