//! The one owner of the signed-in session.
//!
//! Everything here is pure state: no sockets, no timers, no GPUI. The
//! application duplicates the rules over its timer, and the tests drive the
//! same transitions directly. The contract follows the web client
//! (`src/auth/AuthContext.tsx`, frontend `ed7cb00a`):
//!
//! * refresh five minutes before `expires_at`;
//! * one rotation at a time, retried with bounded backoff while the session is
//!   still valid;
//! * a late 401 is applied only when it names the current token, and a
//!   current-token 401 waits for its own rotation to settle first;
//! * a rotation is accepted only for the generation that started it.

use std::time::{SystemTime, UNIX_EPOCH};

/// Refresh this long before the server expiry (`REFRESH_MARGIN_SECONDS`).
pub const REFRESH_MARGIN_SECS: i64 = 5 * 60;
/// First retry delay after a transient refresh failure.
pub const RETRY_MIN_SECS: i64 = 1;
/// Largest retry delay; the session stays usable until `expires_at`.
pub const RETRY_MAX_SECS: i64 = 30;

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// Login data from `internal/account/http/response.go`.
///
/// The token and expiry come from `session`, the identity from `user`.
#[derive(Clone, PartialEq, Eq)]
pub struct Session {
    origin: String,
    user_id: i64,
    username: String,
    token: String,
    expires_at: i64,
    generation: u64,
}

/// A derived `Debug` would print the credential. Diagnostics need the identity
/// and the expiry, never the token.
impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("origin", &self.origin)
            .field("user_id", &self.user_id)
            .field("username", &self.username)
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("generation", &self.generation)
            .finish()
    }
}

/// A server-side revocation request taken exactly once when the user logs out.
#[derive(Clone, PartialEq, Eq)]
pub struct Revocation {
    pub origin: String,
    pub token: String,
}

impl std::fmt::Debug for Revocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Revocation")
            .field("origin", &self.origin)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl Session {
    pub fn new(
        origin: String,
        user_id: i64,
        username: String,
        token: String,
        expires_at: i64,
    ) -> Option<Self> {
        if origin.trim().is_empty() || user_id <= 0 || username.trim().is_empty() {
            return None;
        }
        if token.is_empty() || expires_at <= 0 {
            return None;
        }
        Some(Self {
            origin,
            user_id,
            username,
            token,
            expires_at,
            generation: 1,
        })
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn user_id(&self) -> i64 {
        self.user_id
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// Server expiry in Unix seconds. Public state for UI/diagnostics; the
    /// refresh decision itself is made by `refresh_due`.
    #[allow(dead_code)]
    pub fn expires_at(&self) -> i64 {
        self.expires_at
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// A token that was invalidated locally leaves an empty credential.
    pub fn is_active(&self) -> bool {
        !self.token.is_empty()
    }

    /// `now` and `expires_at` are Unix seconds.
    pub fn refresh_due(&self, now: i64, margin: i64) -> bool {
        self.is_active() && now >= self.expires_at.saturating_sub(margin.max(0))
    }

    pub fn is_expired(&self, now: i64) -> bool {
        now >= self.expires_at
    }

    /// Accept the rotated pair only for the generation that asked for it.
    ///
    /// A stale answer (another rotation or a logout happened meanwhile) must
    /// never replace the current token. The new token and expiry are validated
    /// before the generation moves.
    pub fn apply_rotation(
        &mut self,
        expected_generation: u64,
        token: String,
        expires_at: i64,
    ) -> bool {
        if expected_generation != self.generation || token.is_empty() || expires_at <= 0 {
            return false;
        }
        self.token = token;
        self.expires_at = expires_at;
        self.generation = self.generation.wrapping_add(1);
        true
    }

    /// Apply a `401` only when it names the current token. A late refusal of an
    /// older token returns `false` and changes nothing; a current one clears the
    /// credential and moves the generation so in-flight refreshes cannot apply.
    pub fn handle_unauthorized(&mut self, expected_token: &str) -> bool {
        if expected_token.is_empty() || expected_token != self.token {
            return false;
        }
        self.invalidate();
        true
    }

    /// Confirmed loss of the current session. Moves the generation and clears
    /// the credential.
    pub fn invalidate(&mut self) {
        self.token.clear();
        self.generation = self.generation.wrapping_add(1);
    }

    /// End the session for a real logout and return the one request that still
    /// has to reach the server. The token is cleared here, so a second call
    /// returns `None` and a duplicate DELETE cannot be scheduled.
    pub fn take_revocation(&mut self) -> Option<Revocation> {
        if self.token.is_empty() {
            return None;
        }
        let request = Revocation {
            origin: self.origin.clone(),
            token: std::mem::take(&mut self.token),
        };
        self.generation = self.generation.wrapping_add(1);
        Some(request)
    }
}

/// Why a refresh attempt failed.
///
/// The split follows `AuthContext.tsx` (`ed7cb00a`): only a network error,
/// `408`, `429` and `>= 500` are transient. Every other answer — `400`, `403`,
/// `401` and a malformed body — ends the session immediately, because retrying
/// cannot fix a rejected credential or a broken contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshFailure {
    Unauthorized,
    Transient,
    Invalid,
}

pub fn classify_refresh_error(error: &voxhold_api::Error) -> RefreshFailure {
    match error {
        voxhold_api::Error::Http(401) => RefreshFailure::Unauthorized,
        voxhold_api::Error::Network => RefreshFailure::Transient,
        voxhold_api::Error::Http(status) if matches!(status, 408 | 429) || *status >= 500 => {
            RefreshFailure::Transient
        }
        _ => RefreshFailure::Invalid,
    }
}

/// What the session timer should do on this tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// There is no usable session; the timer can stop.
    Idle,
    /// `expires_at` passed without a confirmed rotation.
    Expired,
    /// Not due yet, a rotation is in flight, or a retry is waiting.
    Waiting,
    /// One request may start now.
    Refresh { generation: u64, token: String },
}

/// Single-flight and bounded-retry state for the refresh request.
#[derive(Clone, Debug)]
pub struct RefreshGate {
    flight: Option<u64>,
    retry_at: Option<i64>,
    retry_delay: i64,
}

impl Default for RefreshGate {
    fn default() -> Self {
        Self {
            flight: None,
            retry_at: None,
            retry_delay: RETRY_MIN_SECS,
        }
    }
}

impl RefreshGate {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    #[allow(dead_code)]
    pub fn in_flight(&self) -> bool {
        self.flight.is_some()
    }

    /// Whether the rotation for exactly this generation is still open. The
    /// application uses it to defer a `401` of the current token until the
    /// refresh answer decides whether the token really died.
    pub fn in_flight_for(&self, generation: u64) -> bool {
        self.flight == Some(generation)
    }

    #[allow(dead_code)]
    pub fn retry_delay(&self) -> i64 {
        self.retry_delay
    }

    /// Decide the next step. The first caller inside the refresh margin gets
    /// `Refresh`; every concurrent caller sees `Waiting` because the flight is
    /// recorded before the network request starts.
    pub fn plan(&mut self, session: &Session, now: i64) -> Plan {
        if !session.is_active() {
            return Plan::Idle;
        }
        if session.is_expired(now) {
            return Plan::Expired;
        }
        if self.flight == Some(session.generation()) {
            return Plan::Waiting;
        }
        // A rotation, a logout or a new login moved the generation. The old
        // request is still allowed to finish, but it no longer blocks a new
        // attempt for the current session.
        self.flight = None;
        if let Some(retry_at) = self.retry_at {
            if retry_at > now {
                return Plan::Waiting;
            }
            self.retry_at = None;
        }
        if !session.refresh_due(now, REFRESH_MARGIN_SECS) {
            return Plan::Waiting;
        }
        self.flight = Some(session.generation());
        Plan::Refresh {
            generation: session.generation(),
            token: session.token().to_owned(),
        }
    }

    /// A rotation answered; the flight ends whether or not it was applied.
    pub fn finish(&mut self, generation: u64) {
        if self.flight == Some(generation) {
            self.flight = None;
        }
    }

    /// A confirmed rotation resets the backoff.
    pub fn succeeded(&mut self, generation: u64) {
        self.finish(generation);
        self.retry_at = None;
        self.retry_delay = RETRY_MIN_SECS;
    }

    /// A transient or format failure: retry after the bounded delay, keeping
    /// the session. Returns the delay that was scheduled.
    pub fn retry_later(&mut self, generation: u64, now: i64) -> i64 {
        self.finish(generation);
        let delay = self.retry_delay.clamp(RETRY_MIN_SECS, RETRY_MAX_SECS);
        self.retry_at = Some(now.saturating_add(delay));
        self.retry_delay = (delay * 2).min(RETRY_MAX_SECS);
        delay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(expires_at: i64) -> Session {
        Session::new(
            "https://chat.example.org".into(),
            7,
            "neva".into(),
            "token-1".into(),
            expires_at,
        )
        .expect("valid session")
    }

    #[test]
    fn refresh_starts_five_minutes_before_expiry() {
        let value = session(1_000);
        assert!(!value.refresh_due(699, 300));
        assert!(value.refresh_due(700, 300));
        assert!(value.refresh_due(999, 300));
        assert!(value.refresh_due(1_000, 300));
        assert!(!value.is_expired(999));
        assert!(value.is_expired(1_000));
    }

    #[test]
    fn rotation_accepts_only_the_expected_generation() {
        let mut value = session(1_000);
        let generation = value.generation();
        // A stale answer from a previous attempt must not replace the token.
        assert!(!value.apply_rotation(generation.wrapping_sub(1), "stale".into(), 5_000));
        assert_eq!(value.token(), "token-1");
        assert_eq!(value.generation(), generation);
        // A malformed answer is rejected for the correct generation too.
        assert!(!value.apply_rotation(generation, String::new(), 5_000));
        assert!(!value.apply_rotation(generation, "token-2".into(), 0));
        // The expected one rotates once.
        assert!(value.apply_rotation(generation, "token-2".into(), 5_000));
        assert_eq!(value.token(), "token-2");
        assert_eq!(value.expires_at(), 5_000);
        assert_eq!(value.generation(), generation.wrapping_add(1));
        // The old generation cannot be replayed.
        assert!(!value.apply_rotation(generation, "token-3".into(), 6_000));
        assert_eq!(value.token(), "token-2");
    }

    #[test]
    fn late_401_of_an_old_token_keeps_the_new_session() {
        let mut value = session(1_000);
        let generation = value.generation();
        assert!(value.apply_rotation(generation, "token-2".into(), 5_000));

        assert!(!value.handle_unauthorized("token-1"));
        assert!(value.is_active());
        assert_eq!(value.token(), "token-2");

        // A refusal of the current token is authoritative.
        assert!(value.handle_unauthorized("token-2"));
        assert!(!value.is_active());
        assert!(!value.handle_unauthorized("token-2"));
    }

    #[test]
    fn expired_token_is_cleared_once() {
        let mut value = session(1_000);
        assert!(value.is_expired(1_200));
        let generation = value.generation();
        value.invalidate();
        assert!(!value.is_active());
        assert_eq!(value.generation(), generation.wrapping_add(1));
        assert!(!value.handle_unauthorized("token-1"));
    }

    #[test]
    fn two_concurrent_plans_start_exactly_one_refresh() {
        let value = session(1_000);
        let mut gate = RefreshGate::default();
        let first = gate.plan(&value, 800);
        assert_eq!(
            first,
            Plan::Refresh {
                generation: value.generation(),
                token: "token-1".into()
            }
        );
        assert_eq!(gate.plan(&value, 800), Plan::Waiting);
        assert_eq!(gate.plan(&value, 900), Plan::Waiting);
        assert!(gate.in_flight());

        gate.succeeded(value.generation());
        assert!(!gate.in_flight());
        // Still inside the margin, but the token did not rotate in this test.
        assert!(matches!(gate.plan(&value, 901), Plan::Refresh { .. }));
    }

    #[test]
    fn a_rotation_unblocks_the_next_plan() {
        let mut value = session(1_000);
        let mut gate = RefreshGate::default();
        let generation = value.generation();
        assert!(matches!(gate.plan(&value, 800), Plan::Refresh { .. }));
        assert!(value.apply_rotation(generation, "token-2".into(), 5_000));
        gate.succeeded(generation);
        // The new expiry is far away, so nothing is due.
        assert_eq!(gate.plan(&value, 901), Plan::Waiting);
    }

    #[test]
    fn transient_failures_retry_with_bounded_backoff_and_keep_the_session() {
        let value = session(1_000);
        let mut gate = RefreshGate::default();
        let generation = value.generation();
        assert!(matches!(gate.plan(&value, 800), Plan::Refresh { .. }));
        assert_eq!(gate.retry_later(generation, 800), 1);
        assert!(value.is_active());
        assert_eq!(gate.plan(&value, 800), Plan::Waiting);
        assert!(matches!(gate.plan(&value, 801), Plan::Refresh { .. }));
        gate.finish(generation);
        assert_eq!(gate.retry_later(generation, 801), 2);
        gate.finish(generation);
        assert_eq!(gate.retry_later(generation, 803), 4);
        for at in [807, 815, 831] {
            gate.finish(generation);
            gate.retry_later(generation, at);
        }
        gate.finish(generation);
        assert_eq!(gate.retry_later(generation, 900), RETRY_MAX_SECS);
        gate.finish(generation);
        assert_eq!(gate.retry_later(generation, 931), RETRY_MAX_SECS);
        assert!(value.is_active());
    }

    #[test]
    fn a_lost_rotation_answer_is_retried_and_then_confirmed_as_lost() {
        let mut value = session(1_000);
        let mut gate = RefreshGate::default();
        let generation = value.generation();
        assert!(matches!(gate.plan(&value, 800), Plan::Refresh { .. }));
        // The server rotated but the answer never arrived: the transport error
        // keeps the session for one bounded retry.
        assert_eq!(gate.retry_later(generation, 800), 1);
        assert!(value.is_active());
        // The retry sends the token the server already revoked, so its 401 is
        // the confirmed loss and clears the session.
        assert_eq!(
            classify_refresh_error(&voxhold_api::Error::Http(401)),
            RefreshFailure::Unauthorized
        );
        let token = value.token().to_owned();
        assert!(value.handle_unauthorized(&token));
        assert!(!value.is_active());
    }

    #[test]
    fn refresh_errors_follow_the_web_client_policy() {
        use voxhold_api::Error;
        // `AuthContext.tsx`: transient means status 0, 408, 429 or >= 500.
        for transient in [
            Error::Network,
            Error::Http(408),
            Error::Http(429),
            Error::Http(500),
            Error::Http(503),
        ] {
            assert_eq!(
                classify_refresh_error(&transient),
                RefreshFailure::Transient,
                "{transient:?}"
            );
        }
        // 401 is the confirmed loss of the credential.
        assert_eq!(
            classify_refresh_error(&Error::Http(401)),
            RefreshFailure::Unauthorized
        );
        // A rejected request or a malformed answer is terminal: the web client
        // calls expireNow instead of retrying, because another attempt cannot
        // repair the contract.
        for terminal in [
            Error::Http(400),
            Error::Http(403),
            Error::Http(404),
            Error::Http(422),
            Error::InvalidResponse("bad"),
        ] {
            assert_eq!(
                classify_refresh_error(&terminal),
                RefreshFailure::Invalid,
                "{terminal:?}"
            );
        }
    }

    #[test]
    fn a_flight_blocks_only_its_own_generation() {
        let value = session(1_000);
        let mut gate = RefreshGate::default();
        let generation = value.generation();
        assert!(!gate.in_flight_for(generation));
        assert!(matches!(gate.plan(&value, 800), Plan::Refresh { .. }));
        assert!(gate.in_flight_for(generation));
        assert!(!gate.in_flight_for(generation.wrapping_add(1)));
        gate.succeeded(generation);
        assert!(!gate.in_flight_for(generation));
    }

    #[test]
    fn debug_output_never_prints_the_credential() {
        let value = session(1_000);
        let debug = format!("{value:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("token-1"), "{debug}");

        let revocation = value.clone().take_revocation().expect("revocation");
        let debug = format!("{revocation:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("token-1"), "{debug}");
    }

    #[test]
    fn logout_returns_exactly_one_server_revocation() {
        let mut value = session(1_000);
        let first = value.take_revocation().expect("one request");
        assert_eq!(first.origin, "https://chat.example.org");
        assert_eq!(first.token, "token-1");
        assert!(value.take_revocation().is_none());
        assert!(!value.is_active());

        // An already invalidated session has nothing to send.
        let mut expired = session(1_000);
        expired.invalidate();
        assert!(expired.take_revocation().is_none());
    }

    #[test]
    fn invalid_login_data_is_rejected() {
        assert!(Session::new("https://x".into(), 0, "u".into(), "t".into(), 1).is_none());
        assert!(Session::new("https://x".into(), 1, " ".into(), "t".into(), 1).is_none());
        assert!(Session::new("https://x".into(), 1, "u".into(), String::new(), 1).is_none());
        assert!(Session::new("https://x".into(), 1, "u".into(), "t".into(), 0).is_none());
        assert!(Session::new(" ".into(), 1, "u".into(), "t".into(), 1).is_none());
    }
}
