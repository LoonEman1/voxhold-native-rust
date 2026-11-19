//! Login, token rotation and logout.
//!
//! The response shapes come from `internal/account/http/response.go` at the
//! pinned backend revision: login answers `{user,session}` and refresh answers
//! the bare `{token,expires_at}` pair. Neither method logs the token or the
//! response body.

use super::*;

/// The login answer: the user row and the session pair. The token is kept here
/// once and never logged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginSession {
    pub user_id: i64,
    pub username: String,
    pub token: String,
    pub expires_at: i64,
}

/// The `POST /api/v1/auth/refresh` answer, top-level `{token,expires_at}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionUpdate {
    pub token: String,
    pub expires_at: i64,
}

/// Credentials and the 401 signal shared by every clone of one client.
///
/// The token is interior-mutable on purpose: a rotation must reach the request
/// clones held by attachments, search, recordings and the voice panel without
/// rebuilding them. `unauthorized` keeps the token that the refused request
/// actually sent, so the application can tell a late 401 from an old token
/// apart from one that belongs to the current session.
#[derive(Default)]
pub(crate) struct AuthShared {
    token: std::sync::Mutex<std::sync::Arc<str>>,
    /// Bounded queue of refused tokens in arrival order. Keeping more than one
    /// matters when a current-token 401 and a late old-token 401 land inside the
    /// same application tick; the old one must not hide the current one.
    unauthorized: std::sync::Mutex<std::collections::VecDeque<String>>,
}

const UNAUTHORIZED_QUEUE: usize = 8;

impl AuthShared {
    fn lock_token(&self) -> std::sync::MutexGuard<'_, std::sync::Arc<str>> {
        self.token
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_unauthorized(&self) -> std::sync::MutexGuard<'_, std::collections::VecDeque<String>> {
        self.unauthorized
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn token(&self) -> std::sync::Arc<str> {
        self.lock_token().clone()
    }

    pub(crate) fn set_token(&self, token: String) {
        *self.lock_token() = std::sync::Arc::from(token);
    }

    pub(crate) fn note_unauthorized(&self, token: &str) {
        if token.is_empty() {
            return;
        }
        let mut queue = self.lock_unauthorized();
        if queue.len() >= UNAUTHORIZED_QUEUE {
            queue.pop_front();
        }
        queue.push_back(token.to_owned());
    }

    pub(crate) fn take_unauthorized(&self) -> Option<String> {
        self.lock_unauthorized().pop_front()
    }
}

impl Client {
    /// Rotate the token for this client and every clone made from it.
    pub fn set_token(&mut self, token: String) {
        self.auth.set_token(token);
    }

    pub fn token(&self) -> String {
        self.auth.token().to_string()
    }

    /// Canonical origin without a trailing slash or default port; the login
    /// screen shows this value and the session records it.
    pub fn origin(&self) -> String {
        self.base.origin().ascii_serialization()
    }

    /// Take the token of the latest refused request (`401`). A late answer from
    /// a token that was already rotated away is still reported with that old
    /// token; the caller decides whether it still matches the session.
    pub fn take_unauthorized(&self) -> Option<String> {
        self.auth.take_unauthorized()
    }

    /// `POST /api/v1/auth/login`. The answer is `{user,session}`; `expires_at`
    /// is Unix seconds. Nothing from the response is logged and the token is
    /// installed only after every required field was validated.
    pub async fn login(&mut self, username: &str, password: &str) -> Result<LoginSession> {
        let v: Value = self
            .request(
                Method::POST,
                "/api/v1/auth/login",
                Some(json!({"username":username,"password":password})),
            )
            .await?;
        let invalid = || Error::InvalidResponse("Сервер не вернул сессию.");
        let user_id = v["user"]["id"]
            .as_i64()
            .filter(|id| *id > 0)
            .ok_or_else(invalid)?;
        let username = v["user"]["username"]
            .as_str()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .ok_or_else(invalid)?;
        let token = v["session"]["token"]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(invalid)?;
        let expires_at = v["session"]["expires_at"]
            .as_i64()
            .filter(|value| *value > 0)
            .ok_or_else(invalid)?;
        self.auth.set_token(token.clone());
        Ok(LoginSession {
            user_id,
            username,
            token,
            expires_at,
        })
    }

    /// `POST /api/v1/auth/refresh`. The backend rotates (revokes the old token
    /// and issues a new one), so this method never changes the local token:
    /// the owner must compare the generation first and apply the pair through
    /// `Session::apply_rotation`. A missing field is a format error and leaves
    /// the current session untouched.
    pub async fn refresh(&self) -> Result<SessionUpdate> {
        let v: Value = self
            .request(Method::POST, "/api/v1/auth/refresh", None)
            .await?;
        let invalid = || Error::InvalidResponse("Сервер не вернул сессию.");
        let token = v["token"]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(invalid)?;
        let expires_at = v["expires_at"]
            .as_i64()
            .filter(|value| *value > 0)
            .ok_or_else(invalid)?;
        Ok(SessionUpdate { token, expires_at })
    }

    /// `POST /api/v1/auth/logout` revokes the bearer token that this client
    /// holds. Callers end the local session first and treat the request as
    /// best effort.
    pub async fn logout(&self) -> Result<()> {
        self.action(Method::POST, "/api/v1/auth/logout", None).await
    }
}
