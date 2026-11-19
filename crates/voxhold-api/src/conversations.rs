//! Personal conversation index, creation, per-conversation settings and the
//! cross-conversation message search. Contracts are checked against backend
//! `internal/community/{pagination,dm_search,conversations,message_features}.go`
//! at `9b29fdd6d5dc818b7408e4892b8e68b27d123831`.
//!
//! Channel and conversation requests stay separate: the personal index starts
//! at `/api/v1/me/conversations`, while a channel request always carries the
//! server id.

use crate::{Client, Conversation, Error, Result};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use url::form_urlencoded;

pub const MAX_LIMIT: u32 = 200;
pub const MAX_USERS: usize = 15;
pub const MAX_NAME_RUNES: usize = 64;
pub const MAX_QUERY_RUNES: usize = 200;

/// `GET /api/v1/me/conversations` with a `limit`: the paged object contract.
/// Without `limit` the backend still returns a bare array, which the paged
/// parser accepts as a complete first page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConversationPage {
    pub conversations: Vec<Conversation>,
    pub has_more: bool,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConversationSearchHit {
    pub id: i64,
    pub conversation_id: i64,
    pub author_id: i64,
    pub author_username: String,
    pub content: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConversationSearchPage {
    pub results: Vec<ConversationSearchHit>,
    pub has_more: bool,
    pub next_before: Option<i64>,
}

/// `GET/PUT /api/v1/conversations/{id}/notifications`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConversationNotification {
    pub conversation_id: i64,
    pub muted: bool,
}

/// A pagination cursor is base64url of `<activity>:<id>`; only the alphabet is
/// checked here, the server validates the decoded pair.
pub fn valid_cursor(cursor: &str) -> bool {
    !cursor.is_empty()
        && cursor.len() <= 96
        && cursor
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn valid_id(id: i64) -> bool {
    id > 0
}

impl Client {
    /// One page of the personal index. `before` is the opaque `next_cursor`
    /// from the previous page, never an activity or conversation id.
    pub async fn conversations_page(
        &self,
        limit: u32,
        before: Option<&str>,
    ) -> Result<ConversationPage> {
        if !(1..=MAX_LIMIT).contains(&limit) {
            return Err(Error::InvalidResponse("Некорректный размер страницы."));
        }
        if let Some(cursor) = before {
            if !valid_cursor(cursor) {
                return Err(Error::InvalidResponse("Некорректный курсор списка."));
            }
        }
        let mut path = format!("/api/v1/me/conversations?limit={limit}");
        if let Some(cursor) = before {
            path.push_str("&before=");
            path.push_str(cursor);
        }
        let value: Value = self.request(Method::GET, &path, None).await?;
        let page: ConversationPage = if value.is_array() {
            // An older server keeps the array contract even for a paged call.
            ConversationPage {
                conversations: serde_json::from_value(value)?,
                has_more: false,
                next_cursor: None,
            }
        } else {
            serde_json::from_value(value)?
        };
        if page.conversations.iter().any(|row| !valid_id(row.id)) {
            return Err(Error::InvalidResponse("Некорректный список диалогов."));
        }
        if page.has_more {
            let next = page
                .next_cursor
                .as_deref()
                .ok_or(Error::InvalidResponse("Некорректный курсор списка."))?;
            if !valid_cursor(next) || before == Some(next) {
                return Err(Error::InvalidResponse("Некорректный курсор списка."));
            }
        }
        Ok(page)
    }

    /// `GET /api/v1/conversations/{id}` for a conversation that is outside the
    /// loaded index page (for example a search hit).
    pub async fn conversation(&self, id: i64) -> Result<Conversation> {
        if !valid_id(id) {
            return Err(Error::InvalidResponse("Некорректный диалог."));
        }
        let row: Conversation = self
            .request(Method::GET, &format!("/api/v1/conversations/{id}"), None)
            .await?;
        if row.id != id {
            return Err(Error::InvalidResponse("Некорректный ответ диалога."));
        }
        Ok(row)
    }

    /// `POST /api/v1/me/conversations`. A pair of users is unique per pair, so
    /// the backend may answer with the existing direct row.
    pub async fn create_conversation(&self, user_ids: &[i64], name: &str) -> Result<Conversation> {
        let name = name.trim();
        let mut seen = std::collections::HashSet::new();
        if user_ids.is_empty()
            || user_ids.len() > MAX_USERS
            || user_ids
                .iter()
                .any(|id| !valid_id(*id) || !seen.insert(*id))
            || name.chars().count() > MAX_NAME_RUNES
        {
            return Err(Error::InvalidResponse("Проверьте участников и название."));
        }
        let row: Conversation = self
            .request(
                Method::POST,
                "/api/v1/me/conversations",
                Some(json!({"user_ids": user_ids, "name": name})),
            )
            .await?;
        if !valid_id(row.id) {
            return Err(Error::InvalidResponse("Некорректный ответ диалога."));
        }
        Ok(row)
    }

    /// `GET /api/v1/me/conversations/search?q=` returns message-content hits
    /// across every conversation of the caller. `before` is numeric, not the
    /// list cursor.
    pub async fn search_conversations(
        &self,
        query: &str,
        before: i64,
    ) -> Result<ConversationSearchPage> {
        let query = query.trim();
        if query.is_empty() || query.chars().count() > MAX_QUERY_RUNES || before < 0 {
            return Err(Error::InvalidResponse("Введите корректный запрос."));
        }
        let mut serializer = form_urlencoded::Serializer::new(String::new());
        serializer.append_pair("q", query);
        if before > 0 {
            serializer.append_pair("before", &before.to_string());
        }
        let page: ConversationSearchPage = self
            .request(
                Method::GET,
                &format!("/api/v1/me/conversations/search?{}", serializer.finish()),
                None,
            )
            .await?;
        if page
            .results
            .iter()
            .any(|hit| !valid_id(hit.id) || !valid_id(hit.conversation_id))
        {
            return Err(Error::InvalidResponse("Некорректный ответ поиска."));
        }
        if page.has_more {
            let next = page
                .next_before
                .filter(|next| *next > 0)
                .ok_or(Error::InvalidResponse("Некорректный курсор поиска."))?;
            if before > 0 && next >= before {
                return Err(Error::InvalidResponse("Некорректный курсор поиска."));
            }
        }
        Ok(page)
    }

    pub async fn conversation_notification(&self, id: i64) -> Result<ConversationNotification> {
        if !valid_id(id) {
            return Err(Error::InvalidResponse("Некорректный диалог."));
        }
        let value: ConversationNotification = self
            .request(
                Method::GET,
                &format!("/api/v1/conversations/{id}/notifications"),
                None,
            )
            .await?;
        if value.conversation_id != id {
            return Err(Error::InvalidResponse("Некорректные настройки диалога."));
        }
        Ok(value)
    }

    pub async fn set_conversation_notification(
        &self,
        id: i64,
        muted: bool,
    ) -> Result<ConversationNotification> {
        if !valid_id(id) {
            return Err(Error::InvalidResponse("Некорректный диалог."));
        }
        let value: ConversationNotification = self
            .request(
                Method::PUT,
                &format!("/api/v1/conversations/{id}/notifications"),
                Some(json!({"muted": muted})),
            )
            .await?;
        if value.conversation_id != id {
            return Err(Error::InvalidResponse("Некорректные настройки диалога."));
        }
        Ok(value)
    }

    pub async fn leave_conversation(&self, id: i64) -> Result<()> {
        if !valid_id(id) {
            return Err(Error::InvalidResponse("Некорректный диалог."));
        }
        self.action(
            Method::DELETE,
            &format!("/api/v1/conversations/{id}/members/me"),
            None,
        )
        .await
    }
}
