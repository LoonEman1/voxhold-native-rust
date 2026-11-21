//! The cold-start restoration and the «Запомнить вход» flow: checking the
//! protected store once per launch, restoring the named target, retrying a
//! transient failure, saving a manual login and keeping the record current
//! through token rotation.
//!
//! Every disk or network step runs on the background executor. The UI thread
//! only applies an answer that still belongs to the current attempt: a late
//! answer from an older address, a manual login, a logout or a toggled box is
//! dropped by `auto_attempt`, the selected origin and the store generation.

use std::time::{Duration, Instant};

use super::*;
use crate::auth_session::{now_unix, Session};
use crate::session_store::{
    accounts_match, AutoLogin, RememberedSession, SessionStore, StoreError,
};

/// The whole restoration attempt, including decrypting the store and both
/// requests, is bounded by this budget. `Client` also has an 8 s per-request
/// timeout, so two requests can never stretch the startup surface past it.
pub(super) const STARTUP_BUDGET: Duration = Duration::from_secs(8);

/// A short text that fits the error row next to the «Повторить вход» link.
const TRANSIENT_REASON: &str = "Нет связи.";
const EXPIRED_REASON: &str = "Сессия истекла. Войдите снова.";
const WRONG_USER_REASON: &str = "Сохранённый вход другого аккаунта.";
const FAILED_REASON: &str = "Не удалось восстановить вход.";
const AMBIGUOUS_REASON: &str = "Выберите инстанс вручную.";
const SAVE_ERROR: &str = "Не удалось сохранить вход: нет доступа.";
pub(super) const DELETE_ERROR: &str = "Не удалось удалить сохранённый вход.";
/// The cleanup of every remembered login failed; the retry link is shown.
pub(super) const CLEAR_ERROR: &str = "Не удалось удалить сохранённые входы";
/// The disk refused even the marker that would suppress the next start.
pub(super) const CLEAR_UNCONFIRMED: &str = "Очистка для следующего запуска не подтверждена.";

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
    /// Another process advanced the store generation while this answer was on
    /// the wire; the result is not applied.
    Stale,
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
    /// Start the one startup check that runs before the first manual form is
    /// shown. `explicit_origin` is the canonical `-server` target or the
    /// confirmed «Запомнить адрес» value, if any. The address form is only
    /// rendered after this attempt settles or the user cancels it.
    pub(super) fn begin_startup_restore(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        explicit_origin: Option<String>,
        allow_profile_single: bool,
        disabled: bool,
    ) {
        if self.startup_checked {
            return;
        }
        self.startup_checked = true;
        self.startup_restoring = true;
        self.startup_deadline = Instant::now() + self.startup_budget;
        self.auto_attempt = self.auto_attempt.wrapping_add(1);
        let attempt = self.auto_attempt;
        self.auto_epoch = self.session_store.epoch();
        self.auth.auto_login = true;
        self.auth.auto_retry = false;
        self.auth.remember_login = false;
        self.auto_record = None;
        if self.auto_blocked || disabled {
            self.startup_manual(attempt, "");
            return;
        }
        // A lost answer must not keep the neutral surface forever: the budget
        // is the hard bound of the whole startup phase.
        self.arm_startup_deadline(attempt, self.startup_deadline, window, cx);
        let store = self.session_store.clone();
        let rt = self.runtime.clone();
        let budget = self.startup_budget;
        let task = cx.background_spawn(async move {
            rt.block_on(async move {
                // The decryption and the file read are blocking; the shared
                // budget still bounds the whole attempt.
                let plan = tokio::task::spawn_blocking(move || {
                    store.plan_auto_login(
                        now_unix(),
                        explicit_origin.as_deref(),
                        allow_profile_single,
                    )
                });
                match tokio::time::timeout(budget, plan).await {
                    Ok(Ok(result)) => result,
                    // A held lock or an unreadable store is a manual start.
                    _ => Err(StoreError::Lock),
                }
            })
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, window, cx| {
                s.finish_startup_load(attempt, result, window, cx)
            });
        })
        .detach();
    }

    fn finish_startup_load(
        &mut self,
        attempt: u64,
        result: Result<AutoLogin, StoreError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.startup_still_owns(attempt) {
            return;
        }
        match result {
            // A present target (or one unambiguous legacy record) starts the
            // network step while the neutral surface stays on screen.
            Ok(AutoLogin::Ready { record, legacy }) => {
                self.migration_pending = legacy;
                self.auth.screen = auth::AuthScreen::Login;
                self.auth.origin = record.origin.clone();
                self.auth.remember_login = true;
                self.auto_record = Some(record.clone());
                let deadline = self.startup_deadline;
                self.start_auto_network(attempt, record, deadline, window, cx);
            }
            Ok(AutoLogin::Ambiguous) => {
                self.startup_manual(attempt, AMBIGUOUS_REASON);
            }
            // A missing, unreadable, blocked or explicitly targetless store is
            // a manual start. No older record is searched.
            Ok(AutoLogin::Manual) | Err(_) => {
                self.startup_manual(attempt, "");
            }
        }
        cx.notify();
    }

    /// Leave the neutral start surface for the address form. The attempt id is
    /// kept, so a late store or network answer for it is ignored.
    fn startup_manual(&mut self, attempt: u64, status: &str) {
        if self.auto_attempt != attempt || self.session.is_some() {
            return;
        }
        self.startup_restoring = false;
        self.auth.auto_login = false;
        self.auth.auto_retry = false;
        self.auth.remember_login = false;
        self.auth.screen = auth::AuthScreen::Instance;
        self.auth.status = status.to_string();
        self.auto_record = None;
        self.migration_pending = false;
        // The first manual surface takes focus on its first frame.
        self.initial_focus_done = false;
    }

    /// «Выбрать инстанс» on the neutral surface. It cancels the attempt even
    /// when the store read or the network answer is still in flight.
    pub(super) fn cancel_startup_restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.auto_attempt = self.auto_attempt.wrapping_add(1);
        self.startup_restoring = false;
        self.auth.auto_login = false;
        self.auth.auto_retry = false;
        self.auth.remember_login = false;
        self.auth.screen = auth::AuthScreen::Instance;
        self.auth.status.clear();
        self.auto_record = None;
        self.migration_pending = false;
        self.instance.update(cx, |input, cx| {
            input.set_value(self.auth.origin.clone(), window, cx);
        });
        self.initial_focus_done = false;
        cx.notify();
    }

    /// Phase two: the exact origin, the stored token, profile and servers.
    fn start_auto_network(
        &mut self,
        attempt: u64,
        record: RememberedSession,
        deadline: Instant,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if record.is_expired(now_unix()) {
            self.forget_record(
                record.origin.clone(),
                record.account.clone(),
                record.token.clone(),
                cx,
            );
            self.auto_expired(attempt, cx);
            return;
        }
        self.auth.auto_login = true;
        self.auth.auto_retry = false;
        self.auth.remember_login = true;
        self.auto_record = Some(record.clone());
        self.auto_epoch = self.session_store.epoch();
        let store = self.session_store.clone();
        let rt = self.runtime.clone();
        let task = cx.background_spawn(async move {
            rt.block_on(async move {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return AutoNetwork::Retry {
                        record,
                        reason: TRANSIENT_REASON.into(),
                    };
                }
                match tokio::time::timeout(
                    remaining,
                    auto_network(&store, record.clone(), deadline),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => AutoNetwork::Retry {
                        record,
                        reason: TRANSIENT_REASON.into(),
                    },
                }
            })
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
                // The background step compares the disk generation around its
                // own reads; this is the last cheap check before a token is
                // installed. A newer generation cannot be verified without a
                // blocking read here, so the answer becomes a manual start
                // instead of entering a workspace that another window replaced.
                if self.auto_stored_generation_moved() {
                    self.startup_manual(attempt, "");
                    return;
                }
                // The server expiry is re-checked after the request: the stored
                // record may have expired while profile and servers were on
                // the wire.
                if session.is_expired(now_unix()) {
                    self.forget_record(
                        session.origin().to_owned(),
                        session.username().to_owned(),
                        session.token().to_owned(),
                        cx,
                    );
                    self.auto_expired(attempt, cx);
                    return;
                }
                if client.origin() != session.origin() {
                    return;
                }
                self.auth.auto_retry = false;
                let attempted = self.auto_record.clone();
                self.auto_record = None;
                self.last_stored_token = Some(session.token().to_owned());
                let migration = std::mem::take(&mut self.migration_pending);
                self.startup_restoring = false;
                self.enter_workspace(client, servers, session, window, cx);
                if migration {
                    if let Some(record) = attempted {
                        self.persist_migrated_target(record, window, cx);
                    }
                }
            }
            AutoNetwork::Unauthorized => {
                self.delete_attempted_record(cx);
                self.auth.remember_login = false;
                self.auth.auto_retry = false;
                self.auto_record = None;
                self.startup_restoring = false;
                self.auth.status = EXPIRED_REASON.into();
                self.initial_focus_done = false;
            }
            AutoNetwork::WrongUser => {
                self.delete_attempted_record(cx);
                self.auth.remember_login = false;
                self.auth.auto_retry = false;
                self.auto_record = None;
                self.startup_restoring = false;
                self.auth.status = WRONG_USER_REASON.into();
                self.initial_focus_done = false;
            }
            // A logout in another process owns the store generation now. The
            // network answer must not enter the workspace or write the record.
            AutoNetwork::Stale => {
                self.startup_manual(attempt, "");
            }
            AutoNetwork::Retry { record, reason } => {
                // The record is still valid; a network outage must not force
                // the user to retype the address or the password.
                self.auth.remember_login = true;
                self.auth.auto_retry = true;
                self.auto_record = Some(record);
                self.auth.status = reason;
                self.startup_restoring = false;
                self.initial_focus_done = false;
            }
            AutoNetwork::Manual { record, reason } => {
                self.auth.remember_login = true;
                self.auth.auto_retry = false;
                self.auto_record = Some(record);
                self.auth.status = reason;
                self.startup_restoring = false;
                self.initial_focus_done = false;
            }
        }
        cx.notify();
    }

    fn auto_expired(&mut self, attempt: u64, cx: &mut Context<Self>) {
        if self.auto_attempt != attempt {
            return;
        }
        self.auth.auto_login = false;
        self.auth.remember_login = false;
        self.auth.auto_retry = false;
        self.auto_record = None;
        self.startup_restoring = false;
        self.auth.status = EXPIRED_REASON.into();
        self.initial_focus_done = false;
        cx.notify();
    }

    /// «Повторить» on the login card. It reuses the kept record, so the
    /// confirmed address is not asked for again. Exactly one new attempt is
    /// started for the original target.
    pub(super) fn retry_auto_login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.auth.screen != auth::AuthScreen::Login
            || self.session.is_some()
            || self.auth.busy
            || self.auth.auto_login
            || self.startup_restoring
        {
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
        let deadline = Instant::now() + self.startup_budget;
        self.start_auto_network(attempt, record, deadline, window, cx);
        cx.notify();
    }

    /// After a manual login: write the record and the one target when the box
    /// is checked, otherwise remove any record for the same origin and account
    /// and the target when it pointed there. The flag is read when the answer
    /// arrives, so toggling the box during the request decides.
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
        let expected_epoch = self.session_store.epoch();
        let store = self.session_store.clone();
        let rt = self.runtime.clone();
        let task = cx.background_spawn(async move {
            rt.block_on(async move {
                if remember {
                    let record = RememberedSession {
                        origin,
                        account,
                        user_id,
                        token: token.clone(),
                        expires_at,
                        saved_at: now_unix(),
                    };
                    // The claim reserves this login's generation on disk, above
                    // every earlier one, so another process's older delete or
                    // cleanup cannot reach this record. Both steps are blocking
                    // (DPAPI and the named mutex) and stay together, so the save
                    // uses the generation the claim just wrote.
                    let writer = store.clone();
                    let saved = tokio::task::spawn_blocking(move || {
                        let claim = writer.begin_login();
                        writer.save(&record, claim)
                    })
                    .await;
                    match saved {
                        Ok(Ok(())) => Ok(Some(token)),
                        // The session ended while this save was queued.
                        Ok(Err(StoreError::Stale)) => Ok(None),
                        Ok(Err(error)) => Err(error),
                        Err(_) => Err(StoreError::Io),
                    }
                } else {
                    match store.forget(&origin, &account, expected_epoch) {
                        Ok(()) | Err(StoreError::Stale) => Ok(None),
                        Err(error) => Err(error),
                    }
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
                        // A new remembered login supersedes a failed logout
                        // cleanup and its suppression marker.
                        s.auto_blocked = false;
                        s.cleanup_retry = false;
                        if s.auth.status.starts_with(CLEAR_ERROR) {
                            s.auth.status.clear();
                        }
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
        let expected_epoch = self.session_store.epoch();
        let rt = self.runtime.clone();
        let token_for_ui = new_token.clone();
        let task = cx.background_spawn(async move {
            rt.block_on(async move {
                let tokens: Vec<&str> = accepted.iter().map(String::as_str).collect();
                match store.update_after_refresh(
                    &origin,
                    &account,
                    &tokens,
                    &new_token,
                    expires_at,
                    expected_epoch,
                ) {
                    Ok(updated) => updated,
                    // A logout, a cleanup or a new login owns the file now.
                    Err(_) => false,
                }
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
    /// is a no-op; the delete itself is local, so it also works offline. The
    /// delete is scoped to the token that ended, so a login saved afterwards by
    /// another window or process is never removed.
    pub(super) fn forget_stored(
        &self,
        stored: Option<(String, String, String)>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((origin, account, token)) = stored {
            self.forget_record(origin, account, token, cx);
        }
    }

    pub(super) fn forget_record(
        &self,
        origin: String,
        account: String,
        token: String,
        cx: &mut Context<Self>,
    ) {
        let store = self.session_store.clone();
        let expected_epoch = store.epoch();
        cx.background_spawn(async move {
            // One retry for a briefly held lock; the visible result is handled
            // by the caller that owns the screen.
            let result = store.delete(&origin, &account, &token, expected_epoch);
            if matches!(result, Err(StoreError::Lock) | Err(StoreError::Io)) {
                let _ = store.delete(&origin, &account, &token, expected_epoch);
            }
        })
        .detach();
    }

    /// Remove every remembered login and the target of this Windows profile in
    /// one atomic operation, then report whether the disk accepted it.
    pub(super) fn clear_saved_logins(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cleanup_pending {
            return;
        }
        self.cleanup_pending = true;
        self.cleanup_retry = false;
        let expected_epoch = self.session_store.epoch();
        self.cleanup_epoch = expected_epoch;
        self.cleanup_claim
            .store(0, std::sync::atomic::Ordering::SeqCst);
        let claim_slot = self.cleanup_claim.clone();
        let store = self.session_store.clone();
        let rt = self.runtime.clone();
        let task = cx.background_spawn(async move {
            rt.block_on(async move {
                // Reserve this cleanup's generation on disk before clearing.
                // A login that another window or process saves after this point
                // claims a higher generation, so this clear is refused with
                // `Stale` and that record survives. The claim reads, decrypts
                // and rewrites the file, so it runs on a blocking worker.
                let claim = store.clone();
                let expected = tokio::task::spawn_blocking(move || claim.begin_login())
                    .await
                    .unwrap_or(expected_epoch);
                claim_slot.store(expected, std::sync::atomic::Ordering::SeqCst);
                let result = store.clear_all(expected);
                // If the records could not be removed, try to leave a marker
                // that suppresses the next automatic restoration.
                let blocked = result.is_err() && store.block_restore(expected).is_ok();
                (result, blocked)
            })
        });
        cx.spawn_in(window, async move |this, window| {
            let (result, blocked) = task.await;
            let _ = this.update_in(window, |s, _window, cx| {
                s.cleanup_pending = false;
                match result {
                    // Another process already moved the generation on; its
                    // newer login or logout owns the file.
                    Ok(_) | Err(StoreError::Stale) => {
                        s.auto_blocked = false;
                        s.cleanup_retry = false;
                        if s.session.is_none() && s.auth.status.starts_with(CLEAR_ERROR) {
                            s.auth.status.clear();
                        }
                    }
                    Err(_) => {
                        s.auto_blocked = true;
                        s.cleanup_retry = true;
                        let message = if blocked {
                            CLEAR_ERROR.to_string()
                        } else {
                            format!("{CLEAR_ERROR}. {CLEAR_UNCONFIRMED}")
                        };
                        if s.session.is_none() {
                            s.auth.status = message;
                        } else {
                            // The user already started a new session; keep the
                            // failure visible in the workspace notice instead
                            // of the auth form.
                            s.remember_error = message;
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// «Повторить очистку» after a failed logout cleanup.
    pub(super) fn retry_cleanup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.cleanup_retry || self.cleanup_pending || self.session.is_some() {
            return;
        }
        self.auth.status.clear();
        self.clear_saved_logins(window, cx);
        cx.notify();
    }

    /// Synchronous last attempt used when the window closes while the cleanup
    /// is still queued. The result cannot be shown any more, so the marker is
    /// the explicit fallback.
    pub(super) fn finish_pending_cleanup(&mut self) {
        if !self.cleanup_pending {
            return;
        }
        self.cleanup_pending = false;
        // The claimed generation when the background step already reserved one;
        // otherwise the generation captured at the logout. Never the current
        // one: a login that another window saved in between is above both and
        // must survive this close.
        let claim = self.cleanup_claim.load(std::sync::atomic::Ordering::SeqCst);
        let expected = if claim > 0 { claim } else { self.cleanup_epoch };
        match self.session_store.clear_all(expected) {
            Ok(_) | Err(StoreError::Stale) => {}
            Err(_) => {
                let _ = self.session_store.block_restore(expected);
            }
        }
    }

    fn persist_migrated_target(
        &mut self,
        record: RememberedSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = self.session_store.clone();
        let expected_epoch = self.session_store.epoch();
        let rt = self.runtime.clone();
        let task = cx.background_spawn(async move {
            rt.block_on(async move { store.set_target(&record, expected_epoch) })
        });
        cx.spawn_in(window, async move |this, window| {
            let result = task.await;
            let _ = this.update_in(window, |s, _window, cx| {
                if s.session.is_none() {
                    return;
                }
                if result.is_err() && result != Err(StoreError::Stale) {
                    s.remember_error = SAVE_ERROR.to_string();
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn delete_attempted_record(&mut self, cx: &mut Context<Self>) {
        if let Some(record) = self.auto_record.clone() {
            self.forget_record(record.origin, record.account, record.token, cx);
        }
    }

    fn startup_still_owns(&self, attempt: u64) -> bool {
        // The store read is read-only and may legitimately raise the in-process
        // generation to the disk one; the write barrier is captured again in
        // `start_auto_network`.
        self.auto_attempt == attempt && self.session.is_none() && self.startup_restoring
    }

    fn auto_still_owns(&self, attempt: u64) -> bool {
        // The store generation is deliberately not part of this check. The
        // background step reads the store, and such a read raises the
        // in-process generation to the disk one; comparing it here would drop
        // every late answer of this attempt and leave `startup_restoring` set
        // forever. The generation is checked where it changes the decision:
        // before a session is installed.
        self.auto_attempt == attempt
            && self.auth.screen == auth::AuthScreen::Login
            && self.session.is_none()
            && !self.auth.busy
    }

    /// A store generation above the one captured for this attempt means another
    /// window or process logged in or out while the answer was on the wire. The
    /// check is one atomic load; the file is never read on the UI thread.
    fn auto_stored_generation_moved(&self) -> bool {
        self.session_store.epoch() > self.auto_epoch
    }

    /// The hard bound of the startup phase. A background answer is expected
    /// within the budget, but a lost one must hand the window over to the
    /// manual form instead of leaving the neutral surface on screen.
    fn arm_startup_deadline(
        &mut self,
        attempt: u64,
        deadline: Instant,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| {
            executor
                .timer(deadline.saturating_duration_since(Instant::now()))
                .await;
            let _ = this.update_in(cx, |s, _window, cx| {
                if s.auto_attempt != attempt || !s.startup_restoring || s.session.is_some() {
                    return;
                }
                // Keep the screen: an answer that still arrives for this
                // attempt is applied by its own branch.
                s.startup_restoring = false;
                s.auth.auto_login = false;
                s.auth.auto_retry = false;
                s.initial_focus_done = false;
                cx.notify();
            });
        })
        .detach();
    }
}

/// The background network step. Nothing here touches GPUI state; only the
/// final enum crosses back to the UI thread.
async fn auto_network(
    store: &SessionStore,
    record: RememberedSession,
    deadline: Instant,
) -> AutoNetwork {
    // Every delete in this attempt belongs to the generation that started it
    // and to the exact token of the record; a logout or a newer login that
    // moved the generation or the token makes a late delete harmless.
    let expected_epoch = store.epoch();
    let mut client = match Client::new(&record.origin) {
        // The canonical origin must round-trip exactly before a Bearer token
        // is attached; this also checks scheme, host and port.
        Ok(client) if client.origin() == record.origin => client,
        _ => {
            return AutoNetwork::Manual {
                record,
                reason: FAILED_REASON.into(),
            }
        }
    };
    client.set_token(record.token.clone());
    let profile = match request_with_deadline(deadline, client.profile()).await {
        Ok(profile) => profile,
        Err(error) => return auto_error(store, &record, &error, expected_epoch),
    };
    // The expiry is checked again after each wire trip; a token that ran out
    // while the request was in flight is not used for the next one.
    if record.is_expired(now_unix()) {
        let _ = store.delete(
            &record.origin,
            &record.account,
            &record.token,
            expected_epoch,
        );
        return AutoNetwork::Unauthorized;
    }
    if store
        .state()
        .is_ok_and(|state| state.generation > expected_epoch)
    {
        // Another window or process logged out (or started a new generation)
        // while this answer was on the wire.
        return AutoNetwork::Stale;
    }
    if profile.user_id != record.user_id || !accounts_match(&record.account, &profile.username) {
        // A record that now answers as another user is not this login.
        let _ = store.delete(
            &record.origin,
            &record.account,
            &record.token,
            expected_epoch,
        );
        return AutoNetwork::WrongUser;
    }
    let servers = match request_with_deadline(deadline, client.servers()).await {
        Ok(servers) => servers,
        Err(error) => return auto_error(store, &record, &error, expected_epoch),
    };
    if record.is_expired(now_unix()) {
        let _ = store.delete(
            &record.origin,
            &record.account,
            &record.token,
            expected_epoch,
        );
        return AutoNetwork::Unauthorized;
    }
    if store
        .state()
        .is_ok_and(|state| state.generation > expected_epoch)
    {
        return AutoNetwork::Stale;
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

/// Await one request only while the shared startup budget allows it. A timeout
/// is the same transient reason as a dropped connection.
async fn request_with_deadline<T>(
    deadline: Instant,
    request: impl std::future::Future<Output = voxhold_api::Result<T>>,
) -> voxhold_api::Result<T> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(voxhold_api::Error::Network);
    }
    match tokio::time::timeout(remaining, request).await {
        Ok(result) => result,
        Err(_) => Err(voxhold_api::Error::Network),
    }
}

/// `AuthContext.tsx` treats only a network error, `408`, `429` and `>= 500` as
/// transient. `401` names a credential that can never work again; everything
/// else is a contract error that a retry cannot repair.
fn auto_error(
    store: &SessionStore,
    record: &RememberedSession,
    error: &voxhold_api::Error,
    expected_epoch: u64,
) -> AutoNetwork {
    match error {
        voxhold_api::Error::Http(401) => {
            let _ = store.delete(
                &record.origin,
                &record.account,
                &record.token,
                expected_epoch,
            );
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
