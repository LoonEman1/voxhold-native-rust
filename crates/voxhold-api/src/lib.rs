use futures_util::{SinkExt, StreamExt};
use reqwest::{Method, Url};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::{fmt, net::IpAddr, time::Duration};

#[derive(Debug)]
pub enum Error {
    InvalidOrigin,
    InvalidResponse(&'static str),
    Http(u16),
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
            Self::Network => write!(f, "Не удалось связаться с сервером."),
            Self::Json(_) => write!(f, "Некорректный ответ сервера."),
            Self::WebSocket(_) => write!(f, "Соединение с сервером прервано."),
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
        let mut out = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Network)? {
            if out.len().saturating_add(chunk.len()) > max {
                return Err(Error::InvalidResponse(
                    "Ответ сервера превышает допустимый размер.",
                ));
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out)
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
        let v: Value = self
            .request(
                Method::GET,
                &format!("{}/members", channel_path(sid, cid)),
                None,
            )
            .await?;
        Ok(serde_json::from_value(v["user_ids"].clone())?)
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
        self.page(
            &format!("{}/comments", message_path(sid, cid, root)),
            before,
        )
        .await
    }
    pub async fn send_message(&self, sid: i64, cid: i64, content: &str) -> Result<Message> {
        self.request(
            Method::POST,
            &format!("{}/messages", channel_path(sid, cid)),
            Some(json!({"content":content})),
        )
        .await
    }
    pub async fn send_thread_message(
        &self,
        sid: i64,
        cid: i64,
        root: i64,
        content: &str,
        key: &str,
    ) -> Result<Message> {
        self.request(
            Method::POST,
            &format!("{}/messages", channel_path(sid, cid)),
            Some(json!({"content":content,"thread_root_id":root,"client_message_id":key})),
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
    pub async fn conversation_messages(&self, id: i64, before: i64) -> Result<MessagePage> {
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
        let messages: Vec<Message> = serde_json::from_value(v["messages"].clone())?;
        let next_before_id = messages.iter().map(|m| m.id).min();
        Ok(MessagePage {
            messages,
            pagination: Pagination {
                next_before_id,
                has_more: v["has_more"].as_bool().unwrap_or(false),
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
        let mut u = self.url(&format!("/api/v1/servers/{sid}/messages/search"))?;
        {
            let mut q = u.query_pairs_mut();
            q.append_pair("q", query).append_pair("limit", "25");
            if before > 0 {
                q.append_pair("before_id", &before.to_string());
            }
        }
        self.request(
            Method::GET,
            &format!("{}?{}", u.path(), u.query().unwrap_or("")),
            None,
        )
        .await
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
    width: i32,
    height: i32,
    deleted: bool
});
dto!(Message { id:i64, channel_id:i64, conversation_id:i64, content:String, author:Author, created_at:i64, edited_at:Option<i64>, revision:i64, comment_count:i64, thread_root_id:Option<i64>, client_message_id:String, reply:Option<MessageReference>, forward:Option<MessageReference>, reactions:Vec<Reaction>, reactions_revision:i64, attachments:Vec<Attachment>, poll:Value });
dto!(Pagination { next_before_id:Option<i64>, has_more:bool });
dto!(MessagePage { messages:Vec<Message>, pagination:Pagination });
dto!(Member { avatar_version:Option<String>, country_code:Option<String>, user_id:i64, username:String, role:String, last_seen_at:Option<i64> });
dto!(Conversation { id:i64, name:String, direct:bool, members:Vec<Author>, unread_count:i64, saved:bool });
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
    image_path: String
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
                        None,
                    )
                    .await?;
            }
            if event.kind == "error" {
                return Ok(());
            }
            receive(event);
        }
    }
    pub async fn upload_attachment(
        &self,
        kind: &str,
        destination: i64,
        id: &str,
        filename: &str,
        data: Vec<u8>,
    ) -> Result<Attachment> {
        if !(kind == "channels" || kind == "conversations") || destination <= 0 || !valid_id(id, 32)
        {
            return Err(Error::InvalidResponse("invalid attachment destination"));
        }
        let mut u = self.url(&format!("/api/v1/{kind}/{destination}/attachments/{id}"))?;
        u.query_pairs_mut().append_pair("filename", filename);
        let bytes = self.binary(Method::PUT, u, data, 1 << 20).await?;
        let result: Attachment = serde_json::from_slice(&bytes)?;
        if result.id != id {
            return Err(Error::InvalidResponse("invalid attachment response"));
        }
        Ok(result)
    }
    pub async fn download_attachment(&self, id: &str) -> Result<Vec<u8>> {
        if !valid_id(id, 32) {
            return Err(Error::InvalidResponse("invalid attachment id"));
        }
        self.binary(
            Method::GET,
            self.url(&format!("/api/v1/attachments/{id}/content"))?,
            vec![],
            64 << 20,
        )
        .await
    }
    async fn binary(&self, method: Method, url: Url, data: Vec<u8>, max: usize) -> Result<Vec<u8>> {
        let mut r = self
            .http
            .request(method.clone(), url)
            .timeout(Duration::from_secs(180));
        if !self.token.is_empty() {
            r = r.bearer_auth(self.token.as_ref());
        }
        if method == Method::PUT {
            r = r
                .header("Content-Type", "application/octet-stream")
                .body(data);
        }
        let mut res = r.send().await.map_err(|_| Error::Network)?;
        if !res.status().is_success() {
            return Err(Error::Http(res.status().as_u16()));
        }
        let mut out = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(|_| Error::Network)? {
            if out.len().saturating_add(chunk.len()) > max {
                return Err(Error::InvalidResponse("response too large"));
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }
    pub async fn image(&self, path: &str, max_edge: u32) -> Result<image::DynamicImage> {
        let bytes = self.bytes(Method::GET, path, None, 4 << 20).await?;
        let reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
            .with_guessed_format()
            .map_err(|_| Error::InvalidResponse("invalid image"))?;
        let format = reader.format();
        let (w, h) = reader
            .into_dimensions()
            .map_err(|_| Error::InvalidResponse("invalid image"))?;
        if w == 0 || h == 0 || w > max_edge || h > max_edge || u64::from(w) * u64::from(h) > 4 << 20
        {
            return Err(Error::InvalidResponse("invalid image dimensions"));
        }
        image::load_from_memory_with_format(
            &bytes,
            format.ok_or(Error::InvalidResponse("invalid image"))?,
        )
        .map_err(|_| Error::InvalidResponse("invalid image"))
    }
    pub async fn avatar(&self, id: i64) -> Result<image::DynamicImage> {
        if id <= 0 {
            return Err(Error::InvalidResponse("invalid avatar id"));
        }
        self.image(&format!("/api/v1/users/{id}/avatar"), 256).await
    }
    pub async fn attachment_thumbnail(&self, id: &str) -> Result<image::DynamicImage> {
        if !valid_id(id, 32) {
            return Err(Error::InvalidResponse("invalid attachment id"));
        }
        self.image(&format!("/api/v1/attachments/{id}/thumbnail"), 480)
            .await
    }
    pub async fn link_preview_image(&self, path: &str) -> Result<image::DynamicImage> {
        let prefix = "/api/v1/link-preview/images/";
        if !path.strip_prefix(prefix).is_some_and(|s| valid_id(s, 64)) {
            return Err(Error::InvalidResponse("invalid preview image path"));
        }
        self.image(path, 4096).await
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

impl Client {
    pub async fn edit_channel(&self, s: i64, c: i64, name: &str) -> Result<()> {
        self.action(
            Method::PATCH,
            &channel_path(s, c),
            Some(json!({"name":name})),
        )
        .await
    }
    pub async fn delete_channel(&self, s: i64, c: i64) -> Result<()> {
        self.action(Method::DELETE, &channel_path(s, c), None).await
    }
    pub async fn channel_notifications(&self, s: i64, c: i64) -> Result<String> {
        let v: Value = self
            .request(
                Method::GET,
                &format!("{}/notifications", channel_path(s, c)),
                None,
            )
            .await?;
        Ok(v["mode"].as_str().unwrap_or_default().to_owned())
    }
    pub async fn set_channel_notifications(&self, s: i64, c: i64, mode: &str) -> Result<()> {
        self.action(
            Method::PUT,
            &format!("{}/notifications", channel_path(s, c)),
            Some(json!({"mode":mode})),
        )
        .await
    }
    pub async fn update_member_role(&self, s: i64, u: i64, role: &str) -> Result<Member> {
        self.request(
            Method::PATCH,
            &format!("/api/v1/servers/{s}/members/{u}/role"),
            Some(json!({"role":role})),
        )
        .await
    }
    pub async fn ban_member(&self, s: i64, u: i64) -> Result<()> {
        self.action(Method::POST, &format!("/api/v1/servers/{s}/bans/{u}"), None)
            .await
    }
    pub async fn set_reaction(
        &self,
        s: i64,
        c: i64,
        id: i64,
        emoji: &str,
        active: bool,
    ) -> Result<ReactionsUpdate> {
        let segment: String = url::form_urlencoded::byte_serialize(emoji.as_bytes()).collect();
        self.request(
            if active { Method::PUT } else { Method::DELETE },
            &format!("{}/reactions/{segment}", message_path(s, c, id)),
            None,
        )
        .await
    }
    pub async fn pinned_message_list(&self, s: i64, c: i64) -> Result<Vec<PinnedMessage>> {
        self.request(Method::GET, &format!("{}/pins", channel_path(s, c)), None)
            .await
    }
    pub async fn set_message_pin(&self, s: i64, c: i64, id: i64, pinned: bool) -> Result<()> {
        self.action(
            if pinned { Method::PUT } else { Method::DELETE },
            &format!("{}/pin", message_path(s, c, id)),
            None,
        )
        .await
    }
    pub async fn message_context(
        &self,
        s: i64,
        c: i64,
        id: i64,
        before: i32,
        after: i32,
    ) -> Result<MessageContext> {
        let v: MessageContext = self
            .request(
                Method::GET,
                &format!(
                    "{}/context?before={before}&after={after}",
                    message_path(s, c, id)
                ),
                None,
            )
            .await?;
        if v.target_message_id != id
            || v.messages.get(v.target_index).is_none_or(|m| m.id != id)
            || v.messages.iter().any(|m| m.channel_id != c)
            || v.messages.windows(2).any(|w| w[0].id >= w[1].id)
        {
            return Err(Error::InvalidResponse("Некорректный ответ истории."));
        }
        Ok(v)
    }
    pub async fn send_attached_message(
        &self,
        s: i64,
        c: i64,
        content: &str,
        key: &str,
        ids: &[String],
        reply: i64,
    ) -> Result<Message> {
        let mut v = json!({"content":content,"client_message_id":key,"attachment_ids":ids});
        if reply > 0 {
            v["reply_to_message_id"] = json!(reply);
        }
        self.request(
            Method::POST,
            &format!("{}/messages", channel_path(s, c)),
            Some(v),
        )
        .await
    }
    pub async fn forward_message(
        &self,
        s: i64,
        source: i64,
        id: i64,
        destination: i64,
        key: &str,
    ) -> Result<Message> {
        self.request(Method::POST,&format!("{}/messages/forward",channel_path(s,destination)),Some(json!({"source_server_id":s,"source_channel_id":source,"source_message_id":id,"client_message_id":key}))).await
    }
    pub async fn save_favorite(&self, id: i64, key: &str) -> Result<()> {
        let v: Value = self
            .request(Method::GET, "/api/v1/me/favorites", None)
            .await?;
        self.action(Method::POST,"/api/v1/me/forward",Some(json!({"source":{"kind":"channel","id":id},"destination":{"kind":"conversation","id":v["id"]},"client_message_id":key}))).await
    }
    pub async fn link_preview(&self, href: &str) -> Result<LinkPreview> {
        let q: String = url::form_urlencoded::byte_serialize(href.as_bytes()).collect();
        self.request(Method::GET, &format!("/api/v1/link-preview?url={q}"), None)
            .await
    }
    pub async fn profile(&self) -> Result<Profile> {
        self.request(Method::GET, "/api/v1/me/profile", None).await
    }
    pub async fn update_profile(&self, about: &str, country: &str) -> Result<Profile> {
        self.request(
            Method::PATCH,
            "/api/v1/me/profile",
            Some(json!({"about":about,"country_code":country})),
        )
        .await
    }
    pub async fn remove_avatar(&self) -> Result<()> {
        self.action(Method::DELETE, "/api/v1/me/avatar", None).await
    }
    pub async fn upload_avatar(
        &self,
        data: Vec<u8>,
        crop: Option<(i32, i32, i32)>,
    ) -> Result<String> {
        let mut u = self.url("/api/v1/me/avatar")?;
        if let Some((x, y, size)) = crop {
            u.query_pairs_mut()
                .append_pair("crop_x", &x.to_string())
                .append_pair("crop_y", &y.to_string())
                .append_pair("crop_size", &size.to_string());
        }
        let b = self.binary(Method::PUT, u, data, 1 << 20).await?;
        let v: Value = serde_json::from_slice(&b)?;
        Ok(v["etag"].as_str().unwrap_or_default().to_owned())
    }
    pub async fn change_password(&self, current: &str, next: &str, confirm: &str) -> Result<()> {
        self.action(Method::PUT,"/api/v1/me/password",Some(json!({"current_password":current,"new_password":next,"new_password_confirm":confirm}))).await
    }
    pub async fn sessions(&self) -> Result<Vec<AccountSession>> {
        let v: Value = self
            .request(Method::GET, "/api/v1/me/sessions", None)
            .await?;
        Ok(serde_json::from_value(v["sessions"].clone())?)
    }
    pub async fn revoke_session(&self, id: i64) -> Result<()> {
        self.action(Method::DELETE, &format!("/api/v1/me/sessions/{id}"), None)
            .await
    }
    pub async fn revoke_other_sessions(&self) -> Result<()> {
        self.action(Method::DELETE, "/api/v1/me/sessions", None)
            .await
    }
    pub async fn delete_account(&self, password: &str) -> Result<()> {
        self.action(
            Method::DELETE,
            "/api/v1/account",
            Some(json!({"password":password})),
        )
        .await
    }
    pub async fn message_privacy(&self) -> Result<MessagePrivacy> {
        self.request(Method::GET, "/api/v1/me/message-privacy", None)
            .await
    }
    pub async fn set_message_privacy(&self, mode: &str) -> Result<MessagePrivacy> {
        self.request(
            Method::PUT,
            "/api/v1/me/message-privacy",
            Some(json!({"mode":mode})),
        )
        .await
    }
    pub async fn block_user(&self, id: i64, blocked: bool) -> Result<()> {
        self.action(
            if blocked { Method::PUT } else { Method::DELETE },
            &format!("/api/v1/me/blocks/{id}"),
            None,
        )
        .await
    }
    pub async fn rename_server(&self, id: i64, name: &str) -> Result<()> {
        self.action(
            Method::PATCH,
            &format!("/api/v1/servers/{id}"),
            Some(json!({"name":name})),
        )
        .await
    }
    pub async fn channel_access(&self, s: i64, c: i64) -> Result<ChannelAccess> {
        self.request(Method::GET, &format!("{}/access", channel_path(s, c)), None)
            .await
    }
    pub async fn set_channel_access(&self, s: i64, c: i64, v: &ChannelAccess) -> Result<()> {
        self.action(
            Method::PUT,
            &format!("{}/access", channel_path(s, c)),
            Some(json!(v)),
        )
        .await
    }
    pub async fn news_settings(&self, s: i64, c: i64) -> Result<NewsSettings> {
        self.request(Method::GET, &format!("{}/news", channel_path(s, c)), None)
            .await
    }
    pub async fn set_news_settings(&self, s: i64, c: i64, v: &NewsSettings) -> Result<()> {
        self.action(
            Method::PUT,
            &format!("{}/news", channel_path(s, c)),
            Some(json!(v)),
        )
        .await
    }
    pub async fn roles(&self, s: i64) -> Result<Vec<CustomRole>> {
        self.request(Method::GET, &format!("/api/v1/servers/{s}/roles"), None)
            .await
    }
    pub async fn save_role(&self, s: i64, r: &CustomRole) -> Result<()> {
        let mut path = format!("/api/v1/servers/{s}/roles");
        let method = if r.id != 0 {
            path.push_str(&format!("/{}", r.id));
            Method::PUT
        } else {
            Method::POST
        };
        self.action(
            method,
            &path,
            Some(json!({"name":r.name,"color":r.color,"permissions":r.permissions})),
        )
        .await
    }
    pub async fn delete_role(&self, s: i64, id: i64) -> Result<()> {
        self.action(
            Method::DELETE,
            &format!("/api/v1/servers/{s}/roles/{id}"),
            None,
        )
        .await
    }
    pub async fn role_preview(&self, s: i64, id: i64) -> Result<Vec<RolePreview>> {
        self.request(
            Method::GET,
            &format!("/api/v1/servers/{s}/roles/{id}/preview"),
            None,
        )
        .await
    }
    pub async fn set_member_roles(&self, s: i64, u: i64, ids: &[i64]) -> Result<()> {
        self.action(
            Method::PUT,
            &format!("/api/v1/servers/{s}/members/{u}/roles"),
            Some(json!({"role_ids":ids})),
        )
        .await
    }
    pub async fn role_grants(&self, s: i64, c: i64) -> Result<Vec<i64>> {
        let v: Value = self
            .request(
                Method::GET,
                &format!("{}/role-grants", channel_path(s, c)),
                None,
            )
            .await?;
        Ok(serde_json::from_value(v["role_ids"].clone())?)
    }
    pub async fn set_role_grants(&self, s: i64, c: i64, ids: &[i64]) -> Result<()> {
        self.action(
            Method::PUT,
            &format!("{}/role-grants", channel_path(s, c)),
            Some(json!({"role_ids":ids})),
        )
        .await
    }
}
impl Client {
    pub async fn client_diagnostics(&self) -> Result<Value> {
        Ok(serde_json::from_slice(
            &self
                .bytes(
                    Method::GET,
                    "/api/v1/diagnostics/client-events?limit=5000",
                    None,
                    32 << 20,
                )
                .await?,
        )?)
    }
    pub async fn cors_origins(&self) -> Result<Vec<String>> {
        let v: Value = self
            .request(Method::GET, "/api/v1/instance/cors-origins", None)
            .await?;
        Ok(serde_json::from_value(v["origins"].clone())?)
    }
    pub async fn set_cors_origins(&self, origins: &[String]) -> Result<()> {
        self.action(
            Method::PUT,
            "/api/v1/instance/cors-origins",
            Some(json!({"origins":origins})),
        )
        .await
    }
    pub async fn slowmode(&self, s: i64, c: i64) -> Result<i64> {
        let v: Value = self
            .request(
                Method::GET,
                &format!("{}/slowmode", channel_path(s, c)),
                None,
            )
            .await?;
        v["seconds"]
            .as_i64()
            .ok_or(Error::InvalidResponse("invalid slowmode"))
    }
    pub async fn set_slowmode(&self, s: i64, c: i64, seconds: i64) -> Result<()> {
        self.action(
            Method::PUT,
            &format!("{}/slowmode", channel_path(s, c)),
            Some(json!({"seconds":seconds})),
        )
        .await
    }
    pub async fn voice_encryption(&self, s: i64, c: i64) -> Result<bool> {
        let v: Value = self
            .request(Method::GET, &format!("{}/e2ee", channel_path(s, c)), None)
            .await?;
        v["required"]
            .as_bool()
            .ok_or(Error::InvalidResponse("invalid encryption state"))
    }
    pub async fn set_voice_encryption(&self, s: i64, c: i64, required: bool) -> Result<()> {
        self.action(
            Method::PUT,
            &format!("{}/e2ee", channel_path(s, c)),
            Some(json!({"required":required})),
        )
        .await
    }
    pub async fn storage(&self) -> Result<StorageState> {
        self.request(Method::GET, "/api/v1/instance/storage", None)
            .await
    }
    pub async fn set_storage(&self, quotas: Value) -> Result<StorageState> {
        self.request(Method::PUT, "/api/v1/instance/storage", Some(quotas))
            .await
    }
    pub async fn preview_cleanup(&self, category: &str, before: i64) -> Result<CleanupPlan> {
        self.request(
            Method::POST,
            "/api/v1/instance/storage/preview",
            Some(json!({"category":category,"before":before})),
        )
        .await
    }
    pub async fn cleanup(&self, token: &str) -> Result<CleanupPlan> {
        self.request(
            Method::POST,
            "/api/v1/instance/storage/cleanup",
            Some(json!({"token":token})),
        )
        .await
    }
    pub async fn bans(&self, s: i64) -> Result<Vec<ServerBan>> {
        let v: Value = self
            .request(Method::GET, &format!("/api/v1/servers/{s}/bans"), None)
            .await?;
        Ok(serde_json::from_value(v["bans"].clone())?)
    }
    pub async fn unban(&self, s: i64, u: i64) -> Result<()> {
        self.action(
            Method::DELETE,
            &format!("/api/v1/servers/{s}/bans/{u}"),
            None,
        )
        .await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
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
impl Client {
    /// Subscribe to read snapshots and updates for one channel. Dropping the
    /// future closes the socket, which allows callers to cancel on navigation.
    pub async fn read_receipts<F: FnMut(Vec<ChannelRead>)>(
        &self,
        server_id: i64,
        channel_id: i64,
        mut receive: F,
    ) -> Result<()> {
        self.subscribe(server_id, channel_id, |event| {
            if event.kind == "channel.read_snapshot" {
                let data = &event.data;
                if data["server_id"].as_i64() == Some(server_id)
                    && data["channel_id"].as_i64() == Some(channel_id)
                {
                    if let Ok(reads) = serde_json::from_value(data["reads"].clone()) {
                        receive(reads);
                    }
                }
            } else if event.kind == "channel.read" {
                if let Ok(read) = serde_json::from_value::<ChannelRead>(event.data) {
                    if read.server_id == server_id && read.channel_id == channel_id {
                        receive(vec![read]);
                    }
                }
            }
        })
        .await
    }
}
#[cfg(test)]
mod compatibility_tests {
    use super::*;
    #[test]
    fn null_arrays_match_go_zero_values() {
        let message: Message =
            serde_json::from_str(r#"{"id":1,"attachments":null,"reactions":null}"#).unwrap();
        assert!(message.attachments.is_empty());
        assert!(message.reactions.is_empty());
        let channel: Channel = serde_json::from_str(r#"{"id":1,"name":null}"#).unwrap();
        assert!(channel.name.is_empty());
    }
}
