// SPDX-License-Identifier: AGPL-3.0-or-later
//! Guest sessions — a NAME under a grant, and nothing else.
//!
//! # Why this exists
//!
//! One QR code serves a room. Every phone that scans it presents the same
//! [`GuestGrant`](crate::GuestGrant) bearer, so the grant cannot say who is
//! holding the phone — a grant is one link handed to many people. A guest's
//! name therefore rode the act's payload, which meant the PAGE chose it: a
//! guard that reads what the subject supplies is not a guard (ARCH 5), and an
//! app that sends no name at all has its guest's act shown under the member's.
//!
//! A [`GuestSession`] is where the name lives instead. The phone claims one
//! once through the door, the door hands back an opaque handle, and every
//! later request carries the handle so the door can name the person without
//! asking the page.
//!
//! # A session is NOT a second credential
//!
//! It names no scope and decides nothing. `GuestGrant::permits_path` remains
//! the sole decider of what may be reached, and a session reaches exactly what
//! its grant reaches, for exactly as long: [`GuestSession::expires_at_ms`] is
//! COPIED from the grant by [`GuestSessionStore::claim`] rather than computed
//! from a TTL of its own, so "a session cannot outlive its grant" is
//! arithmetic rather than a rule somebody has to remember (ARCH 10). A handle
//! presented under a different grant is not live — [`GuestSessionStore::live`]
//! takes the grant and checks the binding.
//!
//! # In memory, keyed by handle, never gossiped
//!
//! Same posture as [`crate::guest_grant`] and for the same reasons: a restart
//! drops every session, revocation of the grant kills its sessions instantly
//! because liveness is evaluated against the grant on every read, and `now_ms`
//! and the handle are both injected so this store is testable without a clock
//! or an RNG.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::guest_grant::GuestGrant;

/// One person behind one grant, for as long as that grant lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestSession {
    /// The opaque string the phone presents. This session's primary key.
    pub handle: String,
    /// The grant this session was claimed under. A handle presented with any
    /// other bearer is not this session — see the module docs.
    pub grant_token: String,
    /// What the guest typed, as they typed it. Rendered; never parsed.
    pub name: String,
    pub issued_at_ms: u64,
    /// Copied from the grant, never computed here. See the module docs.
    pub expires_at_ms: u64,
}

impl GuestSession {
    /// True when this session has not yet lapsed as of `now_ms`. Liveness of
    /// the GRANT is checked by [`GuestSessionStore::live`], which has it in
    /// hand; this is the session's own half.
    pub fn is_live(&self, now_ms: u64) -> bool {
        now_ms < self.expires_at_ms
    }

    /// Whether `other` is the same name to a reader. Case and surrounding
    /// space do not make a different name — the same rule the door applies
    /// when comparing a claim against the roster.
    pub fn is_named(&self, other: &str) -> bool {
        self.name.trim().eq_ignore_ascii_case(other.trim())
    }
}

/// The one refusal this store makes: somebody in this room is already using
/// that name. Returned rather than logged so the door can render it with the
/// name the guest typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameHeld {
    /// The name as the newcomer typed it.
    pub name: String,
}

/// In-memory store of live guest sessions, keyed by handle.
///
/// Held as an `Arc` on the API `AppStateInner` beside
/// [`GuestGrantStore`](crate::GuestGrantStore). Deliberately in-memory and
/// never gossiped — see module docs.
#[derive(Default)]
pub struct GuestSessionStore {
    inner: Mutex<HashMap<String, GuestSession>>,
}

impl GuestSessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `name` to `handle` under `grant`, or refuse because a live session
    /// on the same grant already holds that name.
    ///
    /// The handle is a parameter, not minted here: entropy is injected for the
    /// same reason `now_ms` is. Mint with
    /// `commonwealth_transport::identity::generate_bearer_token`.
    ///
    /// The distinctness check and the insert happen under ONE lock: two phones
    /// typing the same name at the same moment must not both be admitted, and
    /// a check the caller does first cannot promise that.
    pub fn claim(
        &self,
        handle: impl Into<String>,
        grant: &GuestGrant,
        name: impl Into<String>,
        now_ms: u64,
    ) -> Result<GuestSession, NameHeld> {
        let name = name.into();
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let taken = guard
            .values()
            .any(|s| s.grant_token == grant.token && s.is_live(now_ms) && s.is_named(&name));
        if taken {
            return Err(NameHeld { name });
        }
        let session = GuestSession {
            handle: handle.into(),
            grant_token: grant.token.clone(),
            name,
            issued_at_ms: now_ms,
            // The grant's own expiry, not a TTL of this store's. See the
            // module docs: this is what makes outliving the grant impossible.
            expires_at_ms: grant.expires_at_ms,
        };
        guard.insert(session.handle.clone(), session.clone());
        Ok(session)
    }

    /// The live session for `handle` under `grant`, or `None`.
    ///
    /// **THE decider for "is this handle usable on this request".** Three
    /// things must hold and all three are evaluated here, lazily, so no sweep
    /// has to have run: the grant is live, the handle was claimed under THAT
    /// grant, and the session itself has not lapsed.
    pub fn live(&self, handle: &str, grant: &GuestGrant, now_ms: u64) -> Option<GuestSession> {
        if !grant.is_live(now_ms) {
            return None;
        }
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .get(handle)
            .filter(|s| s.grant_token == grant.token && s.is_live(now_ms))
            .cloned()
    }

    /// Every session held under `grant`, live or not, for a refusal that wants
    /// to say who is already in the room. Sorted by claim time so the
    /// rendering is stable across calls.
    pub fn under(&self, grant_token: &str) -> Vec<GuestSession> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<GuestSession> = guard
            .values()
            .filter(|s| s.grant_token == grant_token)
            .cloned()
            .collect();
        out.sort_by_key(|s| (s.issued_at_ms, s.handle.clone()));
        out
    }

    /// Drop and return every session lapsed as of `now_ms`.
    ///
    /// Bookkeeping, not enforcement — [`Self::live`] evaluates expiry itself.
    /// This is what keeps the map from growing over a long-lived daemon, and
    /// it has a production caller ([`Self::spawn_reaper`]) for the reason
    /// `GuestGrantStore::drain_dead` documents.
    pub fn drain_dead(&self, now_ms: u64) -> Vec<GuestSession> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let dead: Vec<String> = guard
            .iter()
            .filter(|(_, s)| !s.is_live(now_ms))
            .map(|(k, _)| k.clone())
            .collect();
        dead.into_iter().filter_map(|k| guard.remove(&k)).collect()
    }

    /// Spawn the sweep that gives [`Self::drain_dead`] its production caller.
    /// Shape copied from `GuestGrantStore::spawn_reaper`, including why the
    /// interval is coarse: expiry is already enforced on every read.
    pub fn spawn_reaper(self: std::sync::Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(SESSION_REAPER_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let swept = self.drain_dead(commonwealth_core::clock::unix_now_millis());
                if !swept.is_empty() {
                    tracing::info!(
                        count = swept.len(),
                        "guest_session reaper: swept lapsed sessions"
                    );
                }
            }
        })
    }
}

/// How often the reaper sweeps. Coarse for the same reason the grant reaper is.
const SESSION_REAPER_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guest_grant::{GuestGrantStore, Scope};

    const T0: u64 = 1_000_000_000_000;

    fn wall_grant(store: &GuestGrantStore, token: &str, ttl_secs: u64) -> GuestGrant {
        store.issue(
            token,
            vec![Scope::Rails("wall".to_string())],
            None,
            ttl_secs,
            T0,
        )
    }

    /// The point of the session: ONE grant, two phones, two names. Before
    /// this, distinctness cost nothing because nothing verified it — the name
    /// rode the payload the page authored.
    #[test]
    fn two_phones_on_one_grant_hold_two_names() {
        let grants = GuestGrantStore::new();
        let grant = wall_grant(&grants, "tok", 60);
        let sessions = GuestSessionStore::new();

        let a = sessions.claim("h1", &grant, "Wren", T0).expect("first");
        let b = sessions.claim("h2", &grant, "Ash", T0).expect("second");

        assert_eq!(a.name, "Wren");
        assert_eq!(b.name, "Ash");
        assert_eq!(
            sessions.live("h1", &grant, T0).map(|s| s.name).as_deref(),
            Some("Wren")
        );
        assert_eq!(
            sessions.live("h2", &grant, T0).map(|s| s.name).as_deref(),
            Some("Ash")
        );
    }

    /// **A name already held under the same grant is refused, and the refusal
    /// names it.** Two guests shown as one person on the wall is the failure
    /// the session exists to prevent, and "second one wins" would silently
    /// rename the first.
    #[test]
    fn a_name_already_held_under_the_grant_is_refused() {
        let grants = GuestGrantStore::new();
        let grant = wall_grant(&grants, "tok", 60);
        let sessions = GuestSessionStore::new();
        sessions.claim("h1", &grant, "Wren", T0).expect("first");

        let refused = sessions
            .claim("h2", &grant, "  wren ", T0)
            .expect_err("a second phone must not take a name already in the room");
        assert_eq!(refused.name, "  wren ");
        // The first session is untouched: the refusal changed nothing.
        assert_eq!(sessions.under("tok").len(), 1);
        assert!(sessions.live("h2", &grant, T0).is_none());
    }

    /// The same name under a DIFFERENT grant is a different room, and is fine.
    #[test]
    fn the_same_name_under_another_grant_is_a_different_room() {
        let grants = GuestGrantStore::new();
        let one = wall_grant(&grants, "tok1", 60);
        let two = wall_grant(&grants, "tok2", 60);
        let sessions = GuestSessionStore::new();
        sessions.claim("h1", &one, "Wren", T0).expect("first room");
        sessions
            .claim("h2", &two, "Wren", T0)
            .expect("another room's Wren is not this room's");
    }

    /// **A session cannot outlive its grant.** Its expiry IS the grant's, and
    /// a lapsed grant makes every handle under it unusable on the same tick —
    /// without the reaper having run.
    #[test]
    fn a_session_dies_with_its_grant() {
        let grants = GuestGrantStore::new();
        let grant = wall_grant(&grants, "tok", 60);
        let sessions = GuestSessionStore::new();
        let s = sessions.claim("h1", &grant, "Wren", T0).expect("claim");

        assert_eq!(s.expires_at_ms, grant.expires_at_ms);
        assert!(sessions
            .live("h1", &grant, grant.expires_at_ms - 1)
            .is_some());
        assert!(
            sessions.live("h1", &grant, grant.expires_at_ms).is_none(),
            "a session outlived its grant's expiry"
        );

        // And a revoked grant kills it immediately, not at the next sweep.
        let revoked = grants.revoke("tok").expect("revoke");
        assert!(sessions.live("h1", &revoked, T0).is_none());
    }

    /// A handle is only a handle under the grant it was claimed on. Otherwise
    /// a second link handed to somebody else would inherit the first room's
    /// names.
    #[test]
    fn a_handle_is_not_live_under_another_grant() {
        let grants = GuestGrantStore::new();
        let one = wall_grant(&grants, "tok1", 60);
        let two = wall_grant(&grants, "tok2", 60);
        let sessions = GuestSessionStore::new();
        sessions.claim("h1", &one, "Wren", T0).expect("claim");

        assert!(sessions.live("h1", &one, T0).is_some());
        assert!(sessions.live("h1", &two, T0).is_none());
    }

    /// The sweep removes what `live` already refuses, and nothing else.
    #[test]
    fn the_sweep_drops_only_lapsed_sessions() {
        let grants = GuestGrantStore::new();
        let short = wall_grant(&grants, "tok1", 1);
        let long = wall_grant(&grants, "tok2", 600);
        let sessions = GuestSessionStore::new();
        sessions.claim("h1", &short, "Wren", T0).expect("claim");
        sessions.claim("h2", &long, "Ash", T0).expect("claim");

        let swept = sessions.drain_dead(T0 + 2_000);
        assert_eq!(swept.len(), 1);
        assert_eq!(swept[0].handle, "h1");
        assert!(sessions.live("h2", &long, T0 + 2_000).is_some());
    }
}
