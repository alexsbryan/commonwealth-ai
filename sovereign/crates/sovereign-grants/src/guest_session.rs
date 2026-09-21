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
//! the bearer PRESENTED ON THIS REQUEST reaches — never what the grant the
//! handle was claimed under reaches. A handle only ever NAMES.
//!
//! # The session belongs to the door, not to one grant
//!
//! A grant names exactly one rail namespace ([`crate::guest_grant::Scope`]),
//! so a wall holding two apps hands out two grants and two QR codes. The
//! person does not change because the scope did: under
//! [`GuestSessionBinding::Door`] — the default — a handle is recognised under
//! ANY live grant this door minted, so the phone that typed its name on the
//! expenses app is not asked again on the doc. The strict setting,
//! [`GuestSessionBinding::Grant`], is the original binding: a handle is live
//! only under the grant it was claimed on.
//!
//! Either way a session cannot outlive its grants:
//! [`GuestSession::expires_at_ms`] is COPIED from a grant rather than computed
//! from a TTL of its own, [`GuestSessionStore::live`] evaluates the presented
//! grant's liveness on every read, and the auth layer has already refused a
//! dead bearer before any handle is read. Under `Door` the expiry is EXTENDED
//! to a later grant's when the handle is presented under one — which is still
//! a grant's own expiry, so the 24 h cap
//! ([`crate::guest_grant::MAX_GUEST_TTL_SECS`]) bounds it without this store
//! knowing the number (ARCH 10).
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

/// What a session handle is recognised under. **CLOSED SET**, and the one
/// decider for both of the questions the binding changes: whose names collide
/// ([`GuestSessionStore::claim`]) and whose handle is live
/// ([`GuestSessionStore::live`]).
///
/// Configured once as `[daemon] guest_sessions` and carried as a construction
/// argument, never re-read per request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GuestSessionBinding {
    /// **The default.** A handle belongs to this daemon's guest door and is
    /// recognised under any live grant the door minted; a name is held across
    /// the whole wall. One person walking between two apps on one wall types
    /// their name once — the 0→1 path, which is what a wall is for.
    #[default]
    Door,
    /// The strict setting: a handle is live only under the grant it was
    /// claimed on, and names collide only within that grant. A wall whose
    /// apps are handed to different rooms wants this — the second app asks
    /// the name again, which is the cost, and it is the operator's to choose.
    Grant,
}

impl GuestSessionBinding {
    /// Parse the configured value. Refuses an unknown one rather than falling
    /// back to a default — an operator who typed `guest_sessions = "grants"`
    /// asked for strict and would otherwise silently get the opposite
    /// (ARCH 6).
    pub fn parse(raw: &str) -> Result<Self, UnknownBinding> {
        match raw.trim() {
            "door" => Ok(Self::Door),
            "grant" => Ok(Self::Grant),
            other => Err(UnknownBinding {
                value: other.to_string(),
            }),
        }
    }

    /// The configured spelling, for a trace that says which setting is live.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Door => "door",
            Self::Grant => "grant",
        }
    }
}

/// `[daemon] guest_sessions` named something that is not a binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownBinding {
    /// What was configured, so the refusal can show it back.
    pub value: String,
}

impl std::fmt::Display for UnknownBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[daemon] guest_sessions = '{}' is not a session binding — it is \
             \"door\" (a name holds across this wall) or \"grant\" (a name \
             holds under one link)",
            self.value
        )
    }
}

impl std::error::Error for UnknownBinding {}

/// One person behind one grant, for as long as that grant lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestSession {
    /// The opaque string the phone presents. This session's primary key.
    pub handle: String,
    /// The grant this session was claimed under. Under
    /// [`GuestSessionBinding::Grant`] a handle presented with any other bearer
    /// is not this session; under the default `Door` binding it still records
    /// where the name was first typed, and [`GuestSessionStore::under`] reads
    /// it. Never a statement about reach — see the module docs.
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
    /// What a handle is recognised under. A construction argument, resolved
    /// from `[daemon] guest_sessions` before the store exists — so no request
    /// path reads config, and the two questions it changes cannot disagree.
    binding: GuestSessionBinding,
}

impl GuestSessionStore {
    /// An empty store under `binding`. [`GuestSessionBinding::Door`] is the
    /// default a `Default` store takes.
    pub fn new(binding: GuestSessionBinding) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            binding,
        }
    }

    /// Which binding this store was built with, for the trace that says so.
    pub fn binding(&self) -> GuestSessionBinding {
        self.binding
    }

    /// Bind `name` to `handle` under `grant`, or refuse because a live session
    /// already holds that name in this store's collision domain — the whole
    /// door under [`GuestSessionBinding::Door`], this grant alone under
    /// [`GuestSessionBinding::Grant`].
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
            .any(|s| self.in_domain(s, grant) && s.is_live(now_ms) && s.is_named(&name));
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

    /// Whether `session` is in the collision-and-recognition domain of
    /// `grant`. **The ONE place the binding is interpreted** — `claim` and
    /// `live` both ask it, so "whose name collides" and "whose handle is
    /// live" can never answer from two different rules (ARCH 8).
    fn in_domain(&self, session: &GuestSession, grant: &GuestGrant) -> bool {
        match self.binding {
            // Every grant in this store was minted by this door, so being in
            // the store IS being under the door.
            GuestSessionBinding::Door => true,
            GuestSessionBinding::Grant => session.grant_token == grant.token,
        }
    }

    /// The live session for `handle` on a request presenting `grant`, or
    /// `None`.
    ///
    /// **THE decider for "is this handle usable on this request".** Three
    /// things must hold and all three are evaluated here, lazily, so no sweep
    /// has to have run: the grant is live, the handle is in that grant's
    /// domain ([`Self::in_domain`]), and the session itself has not lapsed.
    ///
    /// It decides WHO, never WHAT: the caller's reach is
    /// `GuestGrant::permits_path` on the grant presented here, and this
    /// function returns a name. Under [`GuestSessionBinding::Door`] a handle
    /// presented under a grant that outlives it takes THAT grant's expiry —
    /// the person does not lapse mid-room because the link they first scanned
    /// was the shorter one, and the new expiry is still a grant's own, so no
    /// TTL is invented here.
    pub fn live(&self, handle: &str, grant: &GuestGrant, now_ms: u64) -> Option<GuestSession> {
        if !grant.is_live(now_ms) {
            return None;
        }
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let session = guard.get_mut(handle)?;
        if !self.in_domain(session, grant) || !session.is_live(now_ms) {
            return None;
        }
        if session.expires_at_ms < grant.expires_at_ms {
            session.expires_at_ms = grant.expires_at_ms;
        }
        Some(session.clone())
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
        let sessions = GuestSessionStore::new(GuestSessionBinding::Door);

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
        let sessions = GuestSessionStore::new(GuestSessionBinding::Door);
        sessions.claim("h1", &grant, "Wren", T0).expect("first");

        let refused = sessions
            .claim("h2", &grant, "  wren ", T0)
            .expect_err("a second phone must not take a name already in the room");
        assert_eq!(refused.name, "  wren ");
        // The first session is untouched: the refusal changed nothing.
        assert_eq!(sessions.under("tok").len(), 1);
        assert!(sessions.live("h2", &grant, T0).is_none());
    }

    /// **The collision domain follows the binding.** On one wall (`Door`) two
    /// people called the same thing is the confusion the refusal exists to
    /// prevent, whichever app they scanned; under `Grant` the two links are
    /// two rooms and the same name in each is fine.
    #[test]
    fn the_collision_domain_is_the_door_by_default_and_the_grant_when_strict() {
        let grants = GuestGrantStore::new();
        let one = wall_grant(&grants, "tok1", 60);
        let two = wall_grant(&grants, "tok2", 60);

        let door = GuestSessionStore::new(GuestSessionBinding::Door);
        door.claim("h1", &one, "Wren", T0).expect("first app");
        let refused = door
            .claim("h2", &two, "wren", T0)
            .expect_err("one wall, one Wren");
        assert_eq!(refused.name, "wren");

        let strict = GuestSessionStore::new(GuestSessionBinding::Grant);
        strict.claim("h1", &one, "Wren", T0).expect("first room");
        strict
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
        let sessions = GuestSessionStore::new(GuestSessionBinding::Door);
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

    /// **The person does not change because the scope did.** Two apps on one
    /// wall are two grants; by default the handle claimed on one is the same
    /// person on the other, so nobody is asked their name twice. Under the
    /// strict binding it is not live there — the setting a wall whose links
    /// went to different rooms chooses.
    #[test]
    fn a_handle_is_the_doors_by_default_and_the_grants_when_strict() {
        let grants = GuestGrantStore::new();
        let one = wall_grant(&grants, "tok1", 60);
        let two = wall_grant(&grants, "tok2", 60);

        let door = GuestSessionStore::new(GuestSessionBinding::Door);
        door.claim("h1", &one, "Wren", T0).expect("claim");
        assert!(door.live("h1", &one, T0).is_some());
        assert_eq!(
            door.live("h1", &two, T0).map(|s| s.name).as_deref(),
            Some("Wren"),
            "the second app on the same wall asked the name again"
        );

        let strict = GuestSessionStore::new(GuestSessionBinding::Grant);
        strict.claim("h1", &one, "Wren", T0).expect("claim");
        assert!(strict.live("h1", &one, T0).is_some());
        assert!(strict.live("h1", &two, T0).is_none());
    }

    /// **A handle presented under a longer-lived grant takes that grant's
    /// expiry, and never a TTL of this store's.** The person should not lapse
    /// mid-room because the first link they scanned was the shorter one; the
    /// new expiry is still a grant's own, so the 24 h cap bounds it.
    #[test]
    fn a_door_session_takes_the_expiry_of_a_grant_that_outlives_it() {
        let grants = GuestGrantStore::new();
        let short = wall_grant(&grants, "tok1", 60);
        let long = wall_grant(&grants, "tok2", 600);
        let sessions = GuestSessionStore::new(GuestSessionBinding::Door);
        let s = sessions.claim("h1", &short, "Wren", T0).expect("claim");
        assert_eq!(s.expires_at_ms, short.expires_at_ms);

        let seen = sessions.live("h1", &long, T0).expect("live on the wall");
        assert_eq!(seen.expires_at_ms, long.expires_at_ms);
        // And it is remembered, so the short grant lapsing does not un-name
        // somebody still holding the long one.
        assert!(sessions
            .live("h1", &long, short.expires_at_ms + 1)
            .is_some());
        // The shorter grant is dead by then, so the handle is not live under
        // IT — the presented grant is always the one that decides.
        assert!(sessions
            .live("h1", &short, short.expires_at_ms + 1)
            .is_none());
    }

    /// The knob is parsed in one place and refuses what it does not know,
    /// rather than reading a typo as the opposite setting.
    #[test]
    fn the_binding_parses_two_values_and_refuses_the_rest() {
        assert_eq!(
            GuestSessionBinding::parse("door"),
            Ok(GuestSessionBinding::Door)
        );
        assert_eq!(
            GuestSessionBinding::parse(" grant "),
            Ok(GuestSessionBinding::Grant)
        );
        assert_eq!(GuestSessionBinding::default(), GuestSessionBinding::Door);
        let e = GuestSessionBinding::parse("grants").expect_err("a typo is not a setting");
        assert_eq!(e.value, "grants");
        assert!(e.to_string().contains("guest_sessions"));
    }

    /// The sweep removes what `live` already refuses, and nothing else.
    #[test]
    fn the_sweep_drops_only_lapsed_sessions() {
        let grants = GuestGrantStore::new();
        let short = wall_grant(&grants, "tok1", 1);
        let long = wall_grant(&grants, "tok2", 600);
        let sessions = GuestSessionStore::new(GuestSessionBinding::Door);
        sessions.claim("h1", &short, "Wren", T0).expect("claim");
        sessions.claim("h2", &long, "Ash", T0).expect("claim");

        let swept = sessions.drain_dead(T0 + 2_000);
        assert_eq!(swept.len(), 1);
        assert_eq!(swept[0].handle, "h1");
        assert!(sessions.live("h2", &long, T0 + 2_000).is_some());
    }
}
