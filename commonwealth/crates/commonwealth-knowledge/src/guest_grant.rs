// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ephemeral guest grants — a bearer that is **not** mesh membership.
//!
//! # Why this exists
//!
//! Handing someone a mesh invite makes them a *member*: they receive
//! `mesh_secret`, they gossip, and they can mint further invites. The only
//! other credential this daemon has is `client_token` — one daemon-wide,
//! non-expiring secret that unlocks the entire client surface (inference,
//! knowledge search, the apps API, the Ollama shim). There was no way to say
//! "use my 27B for two hours, and nothing else."
//!
//! A [`GuestGrant`] is that third thing. It is a short-lived bearer bound to a
//! [`Scope`] set, consulted at exactly one point — `client_auth_layer` in
//! `commonwealth-api` — and it never touches `Mesh`. A guest is not a member,
//! is never gossiped, never learns the invite key, and cannot mint anything.
//!
//! # Scope is a closed enum, and that IS the security property
//!
//! [`Scope`] enumerates every capability a grant can express (ARCH §2.1). There
//! is no `Invite`, no `Gossip`, no `ListMembers` variant — so those are not
//! capabilities a guest is *denied* by a check somebody remembered to write,
//! they are capabilities no grant can name (§7.1). Adding one later is a
//! variant plus its [`Scope::paths`] arm; the auth layer, the wire format, and
//! this store are untouched.
//!
//! [`Scope::paths`] is also the ONLY source of the route allowlist. A separate
//! `GUEST_PATHS` const would be a second answer to "what may a guest reach",
//! and a policy table with two implementations is the §10.6 failure — the
//! copies diverge and nothing goes red.
//!
//! # In memory, and that is deliberate
//!
//! Like [`crate::ingest_grant`], grants live only in RAM on the issuing node.
//! A restart drops every grant, which for something called *ephemeral* is the
//! correct default rather than a limitation: the alternative (signed, stateless
//! tokens) buys restart-survival and pays for it with a revocation denylist —
//! the same state, minus the instant revoke.
//!
//! Grants are also **never gossiped**. Replicating them would put revocation
//! convergence inside an access-control path, where "eventually" is the wrong
//! word.
//!
//! # Clock and entropy are both injected
//!
//! Every time-dependent method takes `now_ms`, and [`GuestGrantStore::issue`]
//! takes the token rather than minting it. Same reason for both: the store
//! stays deterministically testable without a wall clock or an RNG. The token
//! comes from `commonwealth_transport::identity::generate_bearer_token`, which
//! is the one definition of what a bearer this daemon accepts looks like.

use std::collections::HashMap;
use std::sync::Mutex;

/// Default guest-grant lifetime when the caller doesn't specify: 2 hours.
/// Shorter than an ingest grant's 6h — lending compute for a conversation is a
/// smaller commitment than lending a corpus through a long ingest.
pub const DEFAULT_GUEST_TTL_SECS: u64 = 2 * 60 * 60;

/// Hard cap on a guest grant's lifetime: 24 hours. Mirrors
/// [`crate::ingest_grant::MAX_GRANT_TTL_SECS`] and the work-atlas claim cap.
/// A caller asking for longer is clamped; re-issue instead of over-provisioning.
pub const MAX_GUEST_TTL_SECS: u64 = 24 * 60 * 60;

/// What a guest grant permits.
///
/// **CLOSED SET.** A capability with no variant here is a capability no grant
/// can express — that is the point, and it is why "a guest cannot invite
/// people" needs no check anywhere. See the module docs.
///
/// When adding a variant: give it a [`Self::paths`] arm, add its per-request
/// refinement next to the handler that serves those routes (not in
/// `client_auth`), and add a row to
/// `every_scope_paths_are_mounted_and_never_privileged` in `commonwealth-api`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Chat completions against exactly these model ids, and the listing that
    /// advertises them. Exact string match — no prefixes, no globs: a grant
    /// that can be widened by a cleverly-named model is not a grant.
    Models(Vec<String>),
}

impl Scope {
    /// The exact request paths this scope unlocks.
    ///
    /// THE source of the guest allowlist. Matched by exact equality, the same
    /// discipline `client_auth::AUTH_EXEMPT_PATHS` documents — no child path
    /// inherits access by prefix.
    ///
    /// `/v1/models` rides with `/v1/chat/completions` rather than being a scope
    /// of its own: a caller who may dispatch a model must be able to discover
    /// its name, and the handler filters the listing to the granted set, so it
    /// discloses nothing the grant didn't already give.
    pub fn paths(&self) -> &'static [&'static str] {
        match self {
            Scope::Models(_) => &["/v1/models", "/v1/chat/completions"],
        }
    }

    /// Short human label for the refusal renderer and `grant --list`.
    pub fn label(&self) -> &'static str {
        match self {
            Scope::Models(_) => "models",
        }
    }
}

/// One live guest authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestGrant {
    /// The bearer the guest presents. Also this grant's primary key — a token
    /// is what the auth layer has in hand, so it is what the store is keyed by.
    pub token: String,
    /// What this grant permits. Empty means a grant that permits nothing, which
    /// is a legal (if useless) state and must never be read as "permits
    /// everything".
    pub scopes: Vec<Scope>,
    /// Operator-supplied free text, surfaced by `grant --list` so a human can
    /// tell two live links apart. Never consulted for a decision.
    pub label: Option<String>,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub revoked: bool,
}

impl GuestGrant {
    /// True when the grant is neither revoked nor expired as of `now_ms`.
    pub fn is_live(&self, now_ms: u64) -> bool {
        !self.revoked && now_ms < self.expires_at_ms
    }

    /// **THE decider for "may this grant reach this path".**
    ///
    /// `client_auth_layer` calls this and nothing else — it never matches on a
    /// `Scope` variant, which is what keeps a new scope from touching the auth
    /// layer at all.
    ///
    /// A grant with no scopes permits nothing: `any()` over an empty iterator
    /// is `false`, which is the right direction. Absence of permission is not
    /// permission (ARCH §18.3).
    pub fn permits_path(&self, path: &str) -> bool {
        self.scopes.iter().any(|s| s.paths().contains(&path))
    }

    /// The model ids this grant allows, if it carries a [`Scope::Models`].
    /// `None` means the grant has no model scope at all — distinct from
    /// `Some(&[])`, which would be a model scope naming nothing. Callers must
    /// not collapse the two.
    pub fn models(&self) -> Option<&[String]> {
        self.scopes.iter().find_map(|s| match s {
            Scope::Models(ids) => Some(ids.as_slice()),
        })
    }

    /// Whether `model` is dispatchable under this grant. Exact match.
    pub fn allows_model(&self, model: &str) -> bool {
        self.models()
            .is_some_and(|ids| ids.iter().any(|m| m == model))
    }

    /// One-line rendering of what this grant buys, for the link's display
    /// string and `grant --list`. Never parsed — see the `summary` field on
    /// `DeepLink::Guest`.
    pub fn summary(&self) -> String {
        if self.scopes.is_empty() {
            return "nothing".to_string();
        }
        self.scopes
            .iter()
            .map(|s| match s {
                Scope::Models(ids) => ids.join(", "),
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// In-memory store of live guest grants, keyed by token.
///
/// Held as an `Arc` on the API `AppStateInner` beside the ingest
/// `grant_store`. Deliberately in-memory and never gossiped — see module docs.
#[derive(Default)]
pub struct GuestGrantStore {
    inner: Mutex<HashMap<String, GuestGrant>>,
}

impl GuestGrantStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a grant for `token`. `ttl_secs` is clamped to
    /// `[1, MAX_GUEST_TTL_SECS]`.
    ///
    /// The token is a parameter, not minted here: entropy is injected for the
    /// same reason `now_ms` is, so this store is testable without an RNG. Mint
    /// with `commonwealth_transport::identity::generate_bearer_token`.
    ///
    /// Unlike an ingest grant (one per corpus, re-issue supersedes), each call
    /// creates a SEPARATE grant — the key is the token, and two links handed to
    /// two people must be independently revocable.
    pub fn issue(
        &self,
        token: impl Into<String>,
        scopes: Vec<Scope>,
        label: Option<String>,
        ttl_secs: u64,
        now_ms: u64,
    ) -> GuestGrant {
        let ttl = ttl_secs.clamp(1, MAX_GUEST_TTL_SECS);
        let token = token.into();
        let grant = GuestGrant {
            token: token.clone(),
            scopes,
            label,
            issued_at_ms: now_ms,
            expires_at_ms: now_ms + ttl * 1000,
            revoked: false,
        };
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.insert(token, grant.clone());
        grant
    }

    /// The live grant for `token`, or `None` if there is none, it was revoked,
    /// or it has expired.
    ///
    /// **Expiry is evaluated here, lazily.** That is what makes the auth path
    /// safe without depending on the reaper having run: a lapsed grant cannot
    /// authorize a request even in the window before it is swept.
    pub fn live(&self, token: &str, now_ms: u64) -> Option<GuestGrant> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.get(token).filter(|g| g.is_live(now_ms)).cloned()
    }

    /// Every grant, live or not, for `grant --list`. Sorted by issue time so
    /// the rendering is stable across calls.
    pub fn all(&self) -> Vec<GuestGrant> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<GuestGrant> = guard.values().cloned().collect();
        out.sort_by_key(|g| (g.issued_at_ms, g.token.clone()));
        out
    }

    /// Mark a grant revoked in place and return it. The entry STAYS in the map,
    /// revoked, until [`Self::drain_dead`] sweeps it — so a concurrent
    /// [`Self::live`] fails closed immediately rather than racing the sweep.
    pub fn revoke(&self, token: &str) -> Option<GuestGrant> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.get_mut(token).map(|g| {
            g.revoked = true;
            g.clone()
        })
    }

    /// Drop and return every grant expired or revoked as of `now_ms`.
    ///
    /// **This must have a production caller.** `ingest_grant`'s equivalent does
    /// not, which is why that grant's TTL is enforced only at kickoff and
    /// expiry fails open on in-flight work. Here the auth path is already safe
    /// via lazy expiry in [`Self::live`]; this exists so the map does not grow
    /// without bound, and it is wired to the reaper in `AppState`.
    pub fn drain_dead(&self, now_ms: u64) -> Vec<GuestGrant> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let dead: Vec<String> = guard
            .iter()
            .filter(|(_, g)| g.revoked || now_ms >= g.expires_at_ms)
            .map(|(k, _)| k.clone())
            .collect();
        dead.into_iter().filter_map(|k| guard.remove(&k)).collect()
    }

    /// Spawn the sweep that gives [`Self::drain_dead`] its production caller.
    ///
    /// Shape copied from `WorkQueueManager::spawn_reaper`. This is the half
    /// `ingest_grant` never got: its `drain_dead` is written, documented and
    /// tested, and nothing calls it — so its TTL is enforced at the kickoff
    /// gate and nowhere else. A creation loop without its closure loop rots
    /// while still reading as authoritative.
    ///
    /// Auth correctness does NOT depend on this task running: [`Self::live`]
    /// evaluates expiry itself. This bounds the map's growth and gives the
    /// operator a log line when links lapse.
    pub fn spawn_reaper(self: std::sync::Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(GUEST_REAPER_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let swept = self.drain_dead(commonwealth_core::clock::unix_now_millis());
                if !swept.is_empty() {
                    tracing::info!(
                        count = swept.len(),
                        "guest_grant reaper: swept expired/revoked grants"
                    );
                }
            }
        })
    }
}

/// How often the reaper sweeps. Coarse on purpose — expiry is already enforced
/// on every read, so this is bookkeeping, not enforcement, and a tight loop
/// would spend wakeups to remove entries nobody can use anyway.
const GUEST_REAPER_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const T0: u64 = 1_000_000_000_000; // arbitrary fixed "now" in ms

    fn models(ids: &[&str]) -> Vec<Scope> {
        vec![Scope::Models(ids.iter().map(|s| s.to_string()).collect())]
    }

    #[test]
    fn a_grant_permits_only_its_scopes_paths() {
        let store = GuestGrantStore::new();
        store.issue("tok", models(&["big"]), None, 60, T0);
        let g = store.live("tok", T0 + 1_000).expect("live");

        assert!(g.permits_path("/v1/chat/completions"));
        assert!(g.permits_path("/v1/models"));
        // The whole point: everything else is refused with no per-route work.
        assert!(!g.permits_path("/v1/knowledge/search"));
        assert!(!g.permits_path("/v1/embeddings"));
        assert!(!g.permits_path("/v1/apps"));
        assert!(!g.permits_path("/internal/guest/grant"));
    }

    /// Absence of permission must never read as permission.
    #[test]
    fn a_scopeless_grant_permits_nothing() {
        let store = GuestGrantStore::new();
        store.issue("tok", vec![], None, 60, T0);
        let g = store.live("tok", T0).expect("live");
        assert!(!g.permits_path("/v1/chat/completions"));
        assert!(!g.permits_path("/v1/models"));
        assert_eq!(g.models(), None);
        assert!(!g.allows_model("big"));
    }

    #[test]
    fn model_match_is_exact_not_prefix() {
        let store = GuestGrantStore::new();
        store.issue("tok", models(&["Qwen3.8-27B"]), None, 60, T0);
        let g = store.live("tok", T0).expect("live");
        assert!(g.allows_model("Qwen3.8-27B"));
        // A grant that a longer or shorter name can widen is not a grant.
        assert!(!g.allows_model("Qwen3.8-27B-Instruct"));
        assert!(!g.allows_model("Qwen3.8"));
        assert!(!g.allows_model(""));
    }

    /// `None` (no model scope) and `Some(&[])` (a model scope naming nothing)
    /// point in different directions and must not collapse — §18.3's
    /// `Unpredictable`/`Infeasible` rule, one domain over.
    #[test]
    fn no_model_scope_is_distinct_from_an_empty_model_scope() {
        let store = GuestGrantStore::new();
        store.issue("none", vec![], None, 60, T0);
        store.issue("empty", vec![Scope::Models(vec![])], None, 60, T0);

        assert_eq!(store.live("none", T0).unwrap().models(), None);
        assert_eq!(
            store.live("empty", T0).unwrap().models(),
            Some(&[] as &[String])
        );
        // Both still refuse every model — they differ in what they SAY, not in
        // what they permit.
        assert!(!store.live("none", T0).unwrap().allows_model("big"));
        assert!(!store.live("empty", T0).unwrap().allows_model("big"));
    }

    #[test]
    fn expired_grant_is_not_live() {
        let store = GuestGrantStore::new();
        store.issue("tok", models(&["big"]), None, 60, T0); // expires at T0+60_000
        assert!(store.live("tok", T0 + 59_000).is_some());
        assert!(store.live("tok", T0 + 60_000).is_none()); // boundary: expired
        assert!(store.live("tok", T0 + 120_000).is_none());
    }

    #[test]
    fn revoke_fails_closed_immediately() {
        let store = GuestGrantStore::new();
        store.issue("tok", models(&["big"]), None, 3600, T0);
        let revoked = store.revoke("tok").expect("grant existed");
        assert!(revoked.revoked);
        // Still in the map, but no longer live — a concurrent request loses.
        assert!(store.live("tok", T0 + 1_000).is_none());
        assert_eq!(store.all().len(), 1);
    }

    #[test]
    fn ttl_is_clamped_to_max() {
        let store = GuestGrantStore::new();
        let g = store.issue("tok", models(&["big"]), None, MAX_GUEST_TTL_SECS * 10, T0);
        assert_eq!(g.expires_at_ms, T0 + MAX_GUEST_TTL_SECS * 1000);
    }

    /// Two links to two people revoke independently — the reason this store is
    /// keyed by token rather than by subject the way `ingest_grant` is keyed by
    /// corpus.
    #[test]
    fn two_grants_are_independently_revocable() {
        let store = GuestGrantStore::new();
        store.issue("alice", models(&["big"]), None, 3600, T0);
        store.issue("bob", models(&["big"]), None, 3600, T0);

        store.revoke("alice");
        assert!(store.live("alice", T0).is_none());
        assert!(store.live("bob", T0).is_some(), "bob's link is unaffected");
    }

    #[test]
    fn drain_dead_removes_expired_and_revoked_only() {
        let store = GuestGrantStore::new();
        store.issue("live", models(&["big"]), None, 3600, T0);
        store.issue("expired", models(&["big"]), None, 60, T0);
        store.issue("revoked", models(&["big"]), None, 3600, T0);
        store.revoke("revoked");

        let dead: HashSet<String> = store
            .drain_dead(T0 + 120_000)
            .into_iter()
            .map(|g| g.token)
            .collect();
        assert_eq!(
            dead,
            HashSet::from(["expired".to_string(), "revoked".to_string()])
        );
        assert!(store.live("live", T0 + 120_000).is_some());
        assert_eq!(store.all().len(), 1, "the sweep removed the dead entries");
    }

    #[test]
    fn summary_renders_scopes_and_says_nothing_when_empty() {
        let store = GuestGrantStore::new();
        store.issue("a", models(&["big", "small"]), None, 60, T0);
        store.issue("b", vec![], None, 60, T0);
        assert_eq!(store.live("a", T0).unwrap().summary(), "big, small");
        assert_eq!(store.live("b", T0).unwrap().summary(), "nothing");
    }
}
