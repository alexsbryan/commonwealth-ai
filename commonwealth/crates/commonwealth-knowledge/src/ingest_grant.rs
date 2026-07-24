// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ephemeral, renewable ingest grants — the out-of-band capability that
//! authorizes a normally local-only corpus to be lent to a user-selected
//! set of mesh peers for a one-off (or standing, renewable) compute assist.
//!
//! # Why this exists
//!
//! A personal corpus (Obsidian vault, watched folder, imported files) is
//! `mesh_sharing = false` / `scope = "local"` — structurally never gossiped
//! or replicated. But a user on a slow, GPU-less box may want *selected*
//! peers to help compute embeddings + enrichment for that source, once,
//! then have it revert to local-only. Flipping `mesh_sharing` would be a
//! *standing* share and the wrong tool: it mutates on-disk metadata,
//! advertises the corpus to the whole mesh, and doesn't expire.
//!
//! An `EphemeralIngestGrant` is the right tool. It lives **only in memory**
//! on the coordinator, is scoped to `(corpus_id, allowed_peers)`, carries a
//! renewable TTL, and is consulted at exactly one point: the collaborate
//! kickoff gate (see `commonwealth-api::routes_internal::corpus_collaborate`).
//! It never touches `CorpusMeta`/`IndexInfo`, so the corpus's standing
//! local-only posture is preserved throughout and after the job — the "no
//! standing share" guarantee.
//!
//! # Renewable, not one-shot
//!
//! Re-issuing a grant for the same corpus supersedes the prior one and
//! extends the expiry. A watched folder that keeps re-ingesting deltas uses
//! this to keep peer help alive across updates until the user explicitly
//! revokes it. A one-off vault import instead lets the grant drop on
//! successful merge (see [`EphemeralGrantStore::remove`]).
//!
//! # Clock injection
//!
//! Every time-dependent method takes `now_ms` explicitly so the store is
//! deterministically unit-testable without a wall clock.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use commonwealth_core::ids::{HandoffId, NodeId};

/// Default grant TTL when the caller doesn't specify one: 6 hours. Generous
/// enough that a massive vault's initial ingest completes inside one grant.
pub const DEFAULT_GRANT_TTL_SECS: u64 = 6 * 60 * 60;

/// Hard cap on a grant's TTL: 24 hours (mirrors the work-atlas claim cap).
/// A caller asking for longer is clamped; renew instead of over-provisioning.
pub const MAX_GRANT_TTL_SECS: u64 = 24 * 60 * 60;

/// One corpus's live authorization to be lent to selected peers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EphemeralIngestGrant {
    pub corpus_id: String,
    /// Bound once collaborate registers the queue, so teardown can correlate
    /// grant → handoff. `None` between issue and kickoff.
    pub handoff_id: Option<HandoffId>,
    /// The user-selected helper peers this grant authorizes. The collaborate
    /// planner intersects the embed-compatible candidate set with this.
    pub allowed_peers: Vec<NodeId>,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub revoked: bool,
}

impl EphemeralIngestGrant {
    /// True when the grant is neither revoked nor expired as of `now_ms`.
    pub fn is_live(&self, now_ms: u64) -> bool {
        !self.revoked && now_ms < self.expires_at_ms
    }

    /// True iff this grant authorizes the given requested peer set —
    /// `allowed_peers ⊇ requested`. An empty `requested` is always
    /// authorized (a local-only self-serve run still passes the gate).
    pub fn authorizes(&self, requested: &[NodeId]) -> bool {
        let allow: HashSet<NodeId> = self.allowed_peers.iter().copied().collect();
        requested.iter().all(|p| allow.contains(p))
    }
}

/// In-memory store of live ingest grants, keyed by `corpus_id` (one active
/// grant per corpus; re-issuing supersedes). Held as an `Arc` on the API
/// `AppStateInner` beside the `WorkQueueManager`. Deliberately in-memory:
/// like the work queue, a grant does not survive a daemon restart — on
/// restart a stranded handoff with no live grant is treated as revoked and
/// torn down, never silently resumed.
#[derive(Default)]
pub struct EphemeralGrantStore {
    inner: Mutex<HashMap<String, EphemeralIngestGrant>>,
}

impl EphemeralGrantStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Issue (or renew) a grant for `corpus_id`. Re-issuing supersedes any
    /// existing grant and extends the expiry — the "standing, renewable"
    /// path a watched folder uses to keep peer help alive across deltas.
    /// `ttl_secs` is clamped to `[1, MAX_GRANT_TTL_SECS]`.
    pub fn issue(
        &self,
        corpus_id: impl Into<String>,
        allowed_peers: Vec<NodeId>,
        ttl_secs: u64,
        now_ms: u64,
    ) -> EphemeralIngestGrant {
        let ttl = ttl_secs.clamp(1, MAX_GRANT_TTL_SECS);
        let corpus_id = corpus_id.into();
        let grant = EphemeralIngestGrant {
            corpus_id: corpus_id.clone(),
            handoff_id: None,
            allowed_peers,
            issued_at_ms: now_ms,
            expires_at_ms: now_ms + ttl * 1000,
            revoked: false,
        };
        let mut guard = self.inner.lock().unwrap();
        guard.insert(corpus_id, grant.clone());
        grant
    }

    /// Return the live grant for `corpus_id`, or `None` if there is none, it
    /// was revoked, or it has expired. Expiry is evaluated lazily here so a
    /// stale grant can never authorize a job even before the reaper sweeps.
    pub fn live(&self, corpus_id: &str, now_ms: u64) -> Option<EphemeralIngestGrant> {
        let guard = self.inner.lock().unwrap();
        guard.get(corpus_id).filter(|g| g.is_live(now_ms)).cloned()
    }

    /// Stamp the handoff id onto the corpus's grant once collaborate has
    /// registered the queue, so teardown can correlate grant → handoff.
    /// No-op when there's no grant for the corpus.
    pub fn bind_handoff(&self, corpus_id: &str, handoff_id: HandoffId) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(g) = guard.get_mut(corpus_id) {
            g.handoff_id = Some(handoff_id);
        }
    }

    /// Mark the corpus's grant revoked (in place) and return the now-revoked
    /// grant so the caller can drive teardown (evict peers, retire the
    /// queue). Returns `None` when no grant exists. The grant stays in the
    /// map, revoked, until [`Self::drain_dead`] sweeps it — so a concurrent
    /// `live()` immediately fails closed.
    pub fn revoke(&self, corpus_id: &str) -> Option<EphemeralIngestGrant> {
        let mut guard = self.inner.lock().unwrap();
        guard.get_mut(corpus_id).map(|g| {
            g.revoked = true;
            g.clone()
        })
    }

    /// Remove the corpus's grant entirely (e.g. after a successful one-shot
    /// merge). Returns the removed grant, if any.
    pub fn remove(&self, corpus_id: &str) -> Option<EphemeralIngestGrant> {
        let mut guard = self.inner.lock().unwrap();
        guard.remove(corpus_id)
    }

    /// Drop and return every grant that has expired or been revoked as of
    /// `now_ms`. The reaper calls this to drive teardown for grants that
    /// lapsed without an explicit revoke.
    pub fn drain_dead(&self, now_ms: u64) -> Vec<EphemeralIngestGrant> {
        let mut guard = self.inner.lock().unwrap();
        let dead: Vec<String> = guard
            .iter()
            .filter(|(_, g)| g.revoked || now_ms >= g.expires_at_ms)
            .map(|(k, _)| k.clone())
            .collect();
        dead.into_iter().filter_map(|k| guard.remove(&k)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(n: u128) -> NodeId {
        NodeId::from_u128(n)
    }

    const T0: u64 = 1_000_000_000_000; // arbitrary fixed "now" in ms

    #[test]
    fn live_grant_authorizes_subset_of_allowed_peers() {
        let store = EphemeralGrantStore::new();
        store.issue("vault", vec![peer(1), peer(2)], 60, T0);

        let g = store.live("vault", T0 + 1_000).expect("grant is live");
        assert!(g.authorizes(&[peer(1)]));
        assert!(g.authorizes(&[peer(1), peer(2)]));
        assert!(g.authorizes(&[])); // local-only self-serve
        assert!(!g.authorizes(&[peer(1), peer(3)])); // peer(3) not granted
    }

    #[test]
    fn expired_grant_is_not_live() {
        let store = EphemeralGrantStore::new();
        store.issue("vault", vec![peer(1)], 60, T0); // expires at T0 + 60_000
        assert!(store.live("vault", T0 + 59_000).is_some());
        assert!(store.live("vault", T0 + 60_000).is_none()); // boundary: expired
        assert!(store.live("vault", T0 + 120_000).is_none());
    }

    #[test]
    fn reissue_renews_expiry() {
        let store = EphemeralGrantStore::new();
        store.issue("vault", vec![peer(1)], 60, T0);
        // Renew just before expiry with a fresh TTL from the later "now".
        store.issue("vault", vec![peer(1)], 60, T0 + 59_000);
        // Original window would have lapsed at T0+60_000; the renewal keeps
        // it live to T0+119_000.
        assert!(store.live("vault", T0 + 90_000).is_some());
    }

    #[test]
    fn revoke_fails_closed_immediately() {
        let store = EphemeralGrantStore::new();
        store.issue("vault", vec![peer(1)], 3600, T0);
        let revoked = store.revoke("vault").expect("grant existed");
        assert!(revoked.revoked);
        assert!(store.live("vault", T0 + 1_000).is_none());
    }

    #[test]
    fn ttl_is_clamped_to_max() {
        let store = EphemeralGrantStore::new();
        let g = store.issue("vault", vec![peer(1)], MAX_GRANT_TTL_SECS * 10, T0);
        assert_eq!(g.expires_at_ms, T0 + MAX_GRANT_TTL_SECS * 1000);
    }

    #[test]
    fn drain_dead_removes_expired_and_revoked_only() {
        let store = EphemeralGrantStore::new();
        store.issue("live", vec![peer(1)], 3600, T0);
        store.issue("expired", vec![peer(1)], 60, T0);
        store.issue("revoked", vec![peer(1)], 3600, T0);
        store.revoke("revoked");

        let dead = store.drain_dead(T0 + 120_000);
        let ids: HashSet<String> = dead.into_iter().map(|g| g.corpus_id).collect();
        assert_eq!(
            ids,
            HashSet::from(["expired".to_string(), "revoked".to_string()])
        );
        // The live grant survives the sweep.
        assert!(store.live("live", T0 + 120_000).is_some());
    }

    #[test]
    fn bind_handoff_stamps_the_grant() {
        let store = EphemeralGrantStore::new();
        store.issue("vault", vec![peer(1)], 3600, T0);
        let hid = HandoffId::generate();
        store.bind_handoff("vault", hid);
        assert_eq!(store.live("vault", T0).unwrap().handoff_id, Some(hid));
    }
}
