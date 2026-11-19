//! The «Запомнить вход» flow: checking a stored session after the address is
//! confirmed, retrying a transient failure, saving a manual login and keeping
//! the record current through token rotation.
//!
//! Every disk or network step runs on the background executor. The UI thread
//! only applies an answer that still belongs to the current attempt: a late
//! answer from an older address, a manual login, a logout or a toggled box is
//! dropped by `auto_attempt` and `session_epoch`.

use super::*;
use crate::auth_session::{now_unix, Session};
use crate::session_store::{RememberedSession, SessionStore};

/// A short text that fits the error row next to the «Повторить вход» link.
const TRANSIENT_REASON: &str = "Нет связи с сервером.";
const EXPIRED_REASON: &str = "Сессия истекла. Войдите снова.";
const WRONG_USER_REASON: &str = "Сохранённый вход другого аккаунта.";
const FAILED_REASON: &str = "Не удалось восстановить вход.";
const SAVE_ERROR: &str = "Не удалось сохранить вход: нет доступа.";
const DELETE_ERROR: &str = "Не удалось удалить сохранённый вход.";

/// The answer of the one background network step.
pub(super) enum AutoNetwork {
    SignedIn {
        client: Client,
        servers: Vec<Server>,
        session: Session,
    },
    /// The stored credential is gone for good; the record was deleted.
    Unauthorized,
    /// The record belongs to another user; it was deleted.
    WrongUser,
    /// The record is kept and the card offers «Повторить».
    Retry {
        record: RememberedSession,
        reason: String,
    },
    /// The record is kept but no retry is offered (a contract error).
    Manual {
        record: RememberedSession,
        reason: String,
    },
}

impl Voxhold {
    /// Start checking the store for the confirmed origin. Phase one only reads
    /// and decrypts the file, so the checkbox can show the found record before
    /// the network request starts.
    pub(super) fn begin_auto_login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.auto_attempt = self.auto_attempt.wrapping_add(1);
        let attempt = self.auto_attempt;
        self.auth.auto_login = true;
        self.auth.auto_retry = false;
        self.auth.remember_login = false;
        self.auto_record = None;
        let origin = self.auth.origin.clone();
        let store = self.session_store.clone();
        let rt = self.runtime.clone();
        let task = cx.background_spawn(async move {
            rt.block_on(async move { store.load_for_origin(&origin) })
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                s.finish_auto_load(attempt, result, window, cx)
            });
        })
        .detach();
    }

    fn finish_auto_load(
        &mut self,
        attempt: u64,
        result: Result<Option<RememberedSession>, crate::session_store::StoreError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.auto_still_owns(attempt) {
            return;
        }
        self.auth.auto_login = false;
        match result {
            // A missing, unreadable or foreign blob is a manual login. A save
            // after that login may replace a damaged file.
            Ok(None) | Err(_) => {
                self.auth.remember_login = false;
                self.auto_record = None;
            }
            Ok(Some(record)) if record.is_expired(now_unix()) => {
                // A stale record is useless; drop it before the user types.
                self.forget_record(record.origin.clone(), record.account.clone());
                self.auth.remember_login = false;
                self.auto_record = None;
            }
            Ok(Some(record)) => {
                self.auth.remember_login = true;
                self.auto_record = Some(record.clone());
                self.start_auto_network(attempt, record, window, cx);
            }
        }
        cx.notify();
    }

    /// Phase two: the exact origin, the stored token, profile and servers.
    fn start_auto_network(
        &mut self,
        attempt: u64,
        record: RememberedSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if record.is_expired(now_unix()) {
            self.forget_record(record.origin.clone(), record.account.clone());
            self.auth.auto_login = false;
            self.auth.remember_login = false;
            self.auto_record = None;
            cx.notify();
            return;
        }
        self.auth.auto_login = true;
        self.auth.auto_retry = false;
        self.auth.remember_login = true;
        self.auto_record = Some(record.clone());
        let store = self.session_store.clone();
        let rt = self.runtime.clone();
        let task = cx.background_spawn(async move {
            rt.block_on(async move { auto_network(&store, record).await })
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                s.finish_auto_network(attempt, result, window, cx)
            });
        })
        .detach();
    }

    pub(super) fn finish_auto_network(
        &mut self,
        attempt: u64,
        result: AutoNetwork,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.auto_still_owns(attempt) {
            return;
        }
        self.auth.auto_login = false;
        match result {
            AutoNetwork::SignedIn {
                client,
                servers,
                session,
            } => {
                // The user unchecked the box while the answer was in flight;
                // the late success must not enter the workspace.
                if !self.auth.remember_login {
                    return;
                }
                self.auth.auto_retry = false;
                self.auto_record = None;
                self.last_stored_token = Some(session.token().to_owned());
                self.enter_workspace(client, servers, session, window, cx);
            }
            AutoNetwork::Unauthorized => {
                self.auth.remember_login = false;
                self.auth.auto_retry = false;
                self.auto_record = None;
                self.auth.status = EXPIRED_REASON.into();
            }
            AutoNetwork::WrongUser => {
                self.auth.remember_login = false;
                self.auth.auto_retry = false;
                self.auto_record = None;
                self.auth.status = WRONG_USER_REASON.into();
            }
            AutoNetwork::Retry { record, reason } => {
                // The record is still valid; a network outage must not force
                // the user to retype the address or the password.
                self.auth.remember_login = true;
                self.auth.auto_retry = true;
                self.auto_record = Some(record);
                self.auth.status = reason;
            }
            AutoNetwork::Manual { record, reason } => {
                self.auth.remember_login = true;
                self.auth.auto_retry = false;
                self.auto_record = Some(record);
                self.auth.status = reason;
            }
        }
        cx.notify();
    }

    /// «Повторить» on the login card. It reuses the kept record, so the
    /// confirmed address is not asked for again.
    pub(super) fn retry_auto_login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.auth.screen != auth::AuthScreen::Login || self.session.is_some() || self.auth.busy {
            return;
        }
        let Some(record) = self.auto_record.clone() else {
            return;
        };
        if record.origin != self.auth.origin {
            return;
        }
        self.auto_attempt = self.auto_attempt.wrapping_add(1);
        let attempt = self.auto_attempt;
        self.start_auto_network(attempt, record, window, cx);
        cx.notify();
    }

    /// After a manual login: write the record when the box was checked at
    /// submit, otherwise remove any record for the same origin and account.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn remember_manual_login(
        &mut self,
        remember: bool,
        origin: String,
        account: String,
        user_id: i64,
        token: String,
        expires_at: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session_epoch = self.session_epoch;
        let store_epoch = self.store_epoch.clone();
        let expected_epoch = store_epoch.load(std::sync::atomic::Ordering::SeqCst);
        let store = self.session_store.clone();
        let rt = self.runtime.clone();
        let task = cx.background_spawn(async move {
            rt.block_on(async move {
                if store_epoch.load(std::sync::atomic::Ordering::SeqCst) != expected_epoch {
                    return Ok(None);
                }
                if remember {
                    let record = RememberedSession {
                        origin,
                        account,
                        user_id,
                        token: token.clone(),
                        expires_at,
                        saved_at: now_unix(),
                    };
                    store.save(&record).map(|_| Some(token))
                } else {
                    store.delete(&origin, &account).map(|_| None)
                }
            })
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, _window, cx| {
                if s.session_epoch != session_epoch {
                    return;
                }
                match result {
                    Ok(Some(token)) => {
                        s.last_stored_token = Some(token);
                        s.remember_error.clear();
                    }
                    Ok(None) => s.remember_error.clear(),
                    Err(_) => {
                        s.remember_error = if remember {
                            SAVE_ERROR.to_string()
                        } else {
                            DELETE_ERROR.to_string()
                        };
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Keep the stored token in step with an accepted rotation. The previous
    /// stored token is accepted too, so a failed write is repaired by the next
    /// rotation instead of leaving an unusable record behind.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn persist_rotation(
        &mut self,
        epoch: u64,
        origin: String,
        account: String,
        old_token: String,
        new_token: String,
        expires_at: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut accepted: Vec<String> = Vec::new();
        if !old_token.is_empty() {
            accepted.push(old_token);
        }
        if let Some(token) = self.last_stored_token.clone() {
            if !token.is_empty() && !accepted.contains(&token) {
                accepted.push(token);
            }
        }
        let store = self.session_store.clone();
        let store_epoch = self.store_epoch.clone();
        let expected_epoch = store_epoch.load(std::sync::atomic::Ordering::SeqCst);
        let rt = self.runtime.clone();
        let token_for_ui = new_token.clone();
        let task = cx.background_spawn(async move {
            rt.block_on(async move {
                if store_epoch.load(std::sync::atomic::Ordering::SeqCst) != expected_epoch {
                    return false;
                }
                let tokens: Vec<&str> = accepted.iter().map(String::as_str).collect();
                store.update_after_refresh(&origin, &account, &tokens, &new_token, expires_at)
            })
        });
        cx.spawn_in(window, async move |this, window| {
            let updated = task.await;
            let _ = this.update_in(window, |s, _window, cx| {
                if s.session_epoch != epoch {
                    return;
                }
                if updated {
                    s.last_stored_token = Some(token_for_ui);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Delete the record of the session that just ended. `None` (no session)
    /// is a no-op; the delete itself is local, so it also works offline.
    pub(super) fn forget_stored(&self, stored: Option<(String, String)>) {
        if let Some((origin, account)) = stored {
            self.forget_record(origin, account);
        }
    }

    fn forget_record(&self, origin: String, account: String) {
        let store = self.session_store.clone();
        self.runtime.spawn(async move {
            let _ = store.delete(&origin, &account);
        });
    }

    fn auto_still_owns(&self, attempt: u64) -> bool {
        self.auto_attempt == attempt
            && self.auth.screen == auth::AuthScreen::Login
            && self.session.is_none()
            && !self.auth.busy
    }
}

/// The background network step. Nothing here touches GPUI state; only the
/// final enum crosses back to the UI thread.
async fn auto_network(store: &SessionStore, record: RememberedSession) -> AutoNetwork {
    let mut client = match Client::new(&record.origin) {
        Ok(client) => client,
        Err(_) => {
            return AutoNetwork::Manual {
                record,
                reason: FAILED_REASON.into(),
            }
        }
    };
    client.set_token(record.token.clone());
    let profile = match client.profile().await {
        Ok(profile) => profile,
        Err(error) => return auto_error(store, &record, &error),
    };
    let servers = match client.servers().await {
        Ok(servers) => servers,
        Err(error) => return auto_error(store, &record, &error),
    };
    if profile.user_id != record.user_id
        || !record
            .account
            .trim()
            .eq_ignore_ascii_case(profile.username.trim())
    {
        // A record that now answers as another user is not this login.
        let _ = store.delete(&record.origin, &record.account);
        return AutoNetwork::WrongUser;
    }
    let Some(session) = Session::new(
        record.origin.clone(),
        profile.user_id,
        profile.username,
        record.token.clone(),
        record.expires_at,
    ) else {
        return AutoNetwork::Manual {
            record,
            reason: FAILED_REASON.into(),
        };
    };
    AutoNetwork::SignedIn {
        client,
        servers,
        session,
    }
}

/// `AuthContext.tsx` treats only a network error, `408`, `429` and `>= 500` as
/// transient. `401` names a credential that can never work again; everything
/// else is a contract error that a retry cannot repair.
fn auto_error(
    store: &SessionStore,
    record: &RememberedSession,
    error: &voxhold_api::Error,
) -> AutoNetwork {
    match error {
        voxhold_api::Error::Http(401) => {
            let _ = store.delete(&record.origin, &record.account);
            AutoNetwork::Unauthorized
        }
        voxhold_api::Error::Network
        | voxhold_api::Error::Http(408)
        | voxhold_api::Error::Http(429) => AutoNetwork::Retry {
            record: record.clone(),
            reason: TRANSIENT_REASON.into(),
        },
        voxhold_api::Error::Http(status) if *status >= 500 => AutoNetwork::Retry {
            record: record.clone(),
            reason: TRANSIENT_REASON.into(),
        },
        _ => AutoNetwork::Manual {
            record: record.clone(),
            reason: FAILED_REASON.into(),
        },
    }
}
