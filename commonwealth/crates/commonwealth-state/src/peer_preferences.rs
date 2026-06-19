// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-peer affinity preferences (Ostrom-style sanctions).
//!
//! A node operator can privately tell their daemon "serve peer X
//! at 50% of my advertised affinity" — a reversible, local-only
//! adjustment that the manifest endpoint applies before
//! serializing. The penalized peer's scorer sees lower affinities
//! in the manifest it fetches and naturally routes elsewhere; it
//! is never told *why* its routes shifted.
//!
//! Design contract (per Mesh Health design §5):
//!
//! 1. **Local only, never gossiped.** The
//!    `peer_preferences` `app_id` is excluded from
//!    [`MeshStore::all_entries_for_gossip`] by the
//!    `gossip_excluded_app_ids` filter — the structural invariant
//!    is pinned by `gossip_excludes_peer_preferences_app_id`.
//!
//! 2. **Multiplier clamped to `(0.0, 1.0]` at construction.** The
//!    constructor returns `Err` for any other value — there is no
//!    way to construct a `PeerPreference` outside the legal range.
//!    Pinned by `peer_preference_constructor_rejects_out_of_range`.
//!
//! 3. **No favoritism.** Values strictly above 1.0 are rejected so
//!    a provider cannot create a preferential routing lane for a
//!    favoured peer — the mechanism is for protection (offering
//!    less), not promotion (offering more).
//!
//! Both invariants are *structural* per ARCH_PRINCIPLES §7.1:
//! a caller cannot violate them via config, CLI, or remote
//! request. Tests pin the invariants per §7.2.

use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use commonwealth_core::ids::NodeId;

use crate::error::{Error, Result};
use crate::store::MeshStore;

/// `app_id` namespace for peer-preference state. Reserved — the
/// gossip path excludes this namespace, so writes here never leave
/// the local machine.
pub const PEER_PREFERENCES_APP_ID: &str = "peer_preferences";

/// A single peer preference. Constructed via [`PeerPreference::new`]
/// which enforces the `(0.0, 1.0]` clamp; direct field
/// construction is impossible because the type is a struct with
/// private invariants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerPreference {
    multiplier: f64,
    reason: Option<String>,
    set_at: u64,
}

impl PeerPreference {
    /// Construct a preference. `multiplier` must lie in
    /// `(0.0, 1.0]` — values outside this range, NaN, and
    /// non-finite f64s are all rejected with `Err`. The error path
    /// is deliberately the *only* way to fail to set a preference;
    /// callers don't have to defensively re-validate elsewhere.
    pub fn new(multiplier: f64, reason: Option<String>) -> Result<Self> {
        if !multiplier.is_finite() {
            return Err(Error::Backend(format!(
                "peer-preference multiplier must be finite, got {multiplier}"
            )));
        }
        if multiplier <= 0.0 || multiplier > 1.0 {
            return Err(Error::Backend(format!(
                "peer-preference multiplier must be in (0.0, 1.0], got {multiplier}"
            )));
        }
        let set_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Ok(Self {
            multiplier,
            reason,
            set_at,
        })
    }

    pub fn multiplier(&self) -> f64 {
        self.multiplier
    }

    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    pub fn set_at(&self) -> u64 {
        self.set_at
    }
}

/// Local-only store of per-peer preferences. Backed by `MeshStore`
/// under [`PEER_PREFERENCES_APP_ID`].
#[derive(Clone)]
pub struct PeerPreferenceStore {
    store: MeshStore,
    self_node_id: NodeId,
}

impl PeerPreferenceStore {
    pub fn new(store: MeshStore, self_node_id: NodeId) -> Self {
        Self {
            store,
            self_node_id,
        }
    }

    /// Set or replace the preference for `peer`. The clamp is
    /// enforced at `PeerPreference::new` — by the time a
    /// `PeerPreference` exists, it is valid by construction.
    pub fn set(&self, peer: &NodeId, pref: PeerPreference) -> Result<()> {
        let key = node_key(peer);
        let bytes = serde_json::to_vec(&pref)
            .map_err(|e| Error::Backend(format!("serialize peer preference: {e}")))?;
        tracing::info!(
            peer = %fmt_peer(peer),
            multiplier = pref.multiplier,
            has_reason = pref.reason.is_some(),
            "peer_pref: set"
        );
        self.store.set(
            PEER_PREFERENCES_APP_ID,
            &key,
            Bytes::from(bytes),
            self.self_node_id,
        )?;
        Ok(())
    }

    /// Look up a peer's preference, or `None` if no preference is
    /// set. Reading is hot-path on every manifest fetch, so the
    /// store is consulted directly without caching.
    pub fn get(&self, peer: &NodeId) -> Result<Option<PeerPreference>> {
        let key = node_key(peer);
        let entry = match self.store.get(PEER_PREFERENCES_APP_ID, &key)? {
            None => return Ok(None),
            Some(e) => e,
        };
        let pref = serde_json::from_slice::<PeerPreference>(entry.value.as_ref())
            .map_err(|e| Error::Backend(format!("deserialize peer preference: {e}")))?;
        Ok(Some(pref))
    }

    /// Clear a peer's preference. Idempotent — clearing a peer
    /// with no preference returns `Ok(false)`.
    pub fn clear(&self, peer: &NodeId) -> Result<bool> {
        let key = node_key(peer);
        tracing::info!(peer = %fmt_peer(peer), "peer_pref: clear");
        self.store.delete(PEER_PREFERENCES_APP_ID, &key)
    }

    /// All current preferences as `(NodeId, PeerPreference)` pairs.
    /// Used by the CLI `peer-preference list` subcommand.
    pub fn list(&self) -> Result<Vec<(NodeId, PeerPreference)>> {
        let entries = self.store.scan(PEER_PREFERENCES_APP_ID, "")?;
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            let Some(node_id) = node_from_key(&entry.key) else {
                continue;
            };
            if let Ok(pref) = serde_json::from_slice::<PeerPreference>(entry.value.as_ref()) {
                out.push((node_id, pref));
            }
        }
        Ok(out)
    }
}

/// Set of `app_id`s that the gossip layer must skip.
///
/// Each entry is a structural privacy invariant (ARCH_PRINCIPLES §7) —
/// records under these namespaces never leave the local node.
///
/// - `peer_preferences` — private operator sanctions that must not
///   leak to the penalized peer.
/// - `work-atlas-private` — Private sessions/claims from the work
///   atlas (see `sovereign-work-atlas`). The work-atlas crate must
///   never write Public records to this namespace and never write
///   Private records to `work-atlas`; both halves of the contract
///   are pinned by `Privacy::app_id()` returning a hardcoded literal.
/// - `notes-private` — Per-note opt-out for the NoteStore mesh
///   propagation surface (see `corpus-engine-notes`). The store
///   only writes propagation events to `notes` when
///   `scope=global && !private`; private notes route to
///   `notes-private` and never enter the wire. Symmetric to the
///   work-atlas pattern.
/// - `activity-private` — The local Activity ledger (see
///   `activity` module). Records the user's own resource usage
///   (tokens generated, embeddings produced, chunks ingested/
///   enriched, local inference/knowledge served) for the glassbox
///   "Activity & Sharing" surface. This is a local-first sovereignty
///   guarantee: what work your daemon did *for you* is yours and
///   never gossips. Contrast `contributions` (what you provided to
///   peers), which *does* gossip.
/// - `portfolio-private` — A user's Proxy Voting portfolio: the named
///   set of company/fund `corpus_id`s they hold (and, later, their
///   read-only holdings + authored voting policy). WHICH companies a
///   user owns reveals the user and is among the most sensitive data
///   in the system; it is local-first and must never gossip, even
///   though the per-issuer `proxy-cik…` corpora it references are
///   freely replicable public EDGAR data. Same structural guarantee as
///   the others (FR-11 / AC-7).
///
/// Each entry is pinned by a test that asserts `is_gossip_excluded`
/// returns `true` for it.
pub const GOSSIP_EXCLUDED_APP_IDS: &[&str] = &[
    PEER_PREFERENCES_APP_ID,
    "work-atlas-private",
    "notes-private",
    "activity-private",
    "portfolio-private",
];

/// `app_id` namespace for a user's Proxy Voting portfolios — the named
/// sets of corpus_ids they hold. Reserved + gossip-excluded (FR-11):
/// writes here never leave the local machine.
pub const PORTFOLIO_PRIVATE_APP_ID: &str = "portfolio-private";

/// Returns true when the given `app_id` is excluded from gossip
/// replication. Centralized helper so the gossip path doesn't have
/// to hard-code the list — every caller goes through here.
pub fn is_gossip_excluded(app_id: &str) -> bool {
    GOSSIP_EXCLUDED_APP_IDS.contains(&app_id)
}

fn node_key(peer: &NodeId) -> String {
    peer.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

fn node_from_key(key: &str) -> Option<NodeId> {
    if key.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for (i, b) in bytes.iter_mut().enumerate() {
        let pair = key.get(i * 2..i * 2 + 2)?;
        *b = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(NodeId::from_u128(u128::from_be_bytes(bytes)))
}

fn fmt_peer(id: &NodeId) -> String {
    id.as_bytes()
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nid(b: u8) -> NodeId {
        NodeId::from_u128(b as u128)
    }

    #[test]
    fn peer_preference_constructor_accepts_legal_range() {
        assert!(PeerPreference::new(1.0, None).is_ok());
        assert!(PeerPreference::new(0.5, None).is_ok());
        assert!(PeerPreference::new(0.001, None).is_ok());
        assert!(PeerPreference::new(0.999, Some("reason".into())).is_ok());
    }

    #[test]
    fn peer_preference_constructor_rejects_out_of_range() {
        // Above 1.0 — no favoritism.
        assert!(PeerPreference::new(1.0001, None).is_err());
        assert!(PeerPreference::new(2.0, None).is_err());
        assert!(PeerPreference::new(f64::INFINITY, None).is_err());
        // At or below 0.0 — open lower bound (use `clear` to remove
        // a peer; do not zero them out structurally).
        assert!(PeerPreference::new(0.0, None).is_err());
        assert!(PeerPreference::new(-0.0001, None).is_err());
        assert!(PeerPreference::new(-1.0, None).is_err());
        assert!(PeerPreference::new(f64::NEG_INFINITY, None).is_err());
        // NaN.
        assert!(PeerPreference::new(f64::NAN, None).is_err());
    }

    #[test]
    fn set_get_clear_round_trips() {
        let store = MeshStore::in_memory().unwrap();
        let prefs = PeerPreferenceStore::new(store, nid(1));
        let pref = PeerPreference::new(0.5, Some("over-consuming".into())).unwrap();
        prefs.set(&nid(2), pref.clone()).unwrap();
        let got = prefs.get(&nid(2)).unwrap().unwrap();
        assert!((got.multiplier() - 0.5).abs() < 1e-12);
        assert_eq!(got.reason(), Some("over-consuming"));
        let removed = prefs.clear(&nid(2)).unwrap();
        assert!(removed);
        assert!(prefs.get(&nid(2)).unwrap().is_none());
    }

    #[test]
    fn list_returns_all_preferences() {
        let store = MeshStore::in_memory().unwrap();
        let prefs = PeerPreferenceStore::new(store, nid(1));
        prefs
            .set(&nid(2), PeerPreference::new(0.8, None).unwrap())
            .unwrap();
        prefs
            .set(&nid(3), PeerPreference::new(0.5, None).unwrap())
            .unwrap();
        let mut listed = prefs.list().unwrap();
        listed.sort_by_key(|(id, _)| id.as_bytes().to_vec());
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].0, nid(2));
        assert_eq!(listed[1].0, nid(3));
    }

    /// **Structural invariant pin** (ARCH_PRINCIPLES §7.2). The
    /// peer-preferences `app_id` MUST be excluded from gossip — a
    /// regression would cause private operator adjustments to leak
    /// to the very peer being penalized, breaking the
    /// "social-not-algorithmic sanction" property of the design.
    #[test]
    fn gossip_excludes_peer_preferences_app_id() {
        assert!(is_gossip_excluded(PEER_PREFERENCES_APP_ID));
        // Other namespaces stay gossip-replicated.
        assert!(!is_gossip_excluded("contributions"));
        assert!(!is_gossip_excluded("inference"));
        assert!(!is_gossip_excluded("knowledge"));
        // Public work-atlas records gossip; Private ones don't.
        assert!(!is_gossip_excluded("work-atlas"));
    }

    /// **Structural invariant pin** for the work atlas privacy model.
    /// Mirrored by a test in `sovereign-work-atlas` that asserts the
    /// other half — `Privacy::Private.app_id()` returns this exact
    /// literal. If either side drifts, one test fails.
    #[test]
    fn gossip_excludes_work_atlas_private_app_id() {
        assert!(is_gossip_excluded("work-atlas-private"));
    }

    /// **Structural invariant pin** for the NoteStore mesh
    /// propagation surface (see `corpus-engine-notes`). Per-note
    /// `private` writes route to `notes-private`; the structural
    /// gossip filter is what guarantees those records never leave
    /// the local node. Public notes ride `app_id="notes"` and
    /// must continue to gossip.
    #[test]
    fn gossip_excludes_notes_private_app_id() {
        assert!(is_gossip_excluded("notes-private"));
        assert!(!is_gossip_excluded("notes"));
    }

    /// **Structural invariant pin** for the Proxy Voting portfolio
    /// (FR-11 / AC-7). Which companies/funds a user holds reveals the
    /// user and must never gossip, even though the per-issuer
    /// `proxy-cik…` corpora it references are freely-replicable public
    /// EDGAR data. The const + the helper are pinned to the same
    /// literal so a drift fails here.
    #[test]
    fn gossip_excludes_portfolio_private_app_id() {
        assert!(is_gossip_excluded("portfolio-private"));
        assert!(is_gossip_excluded(PORTFOLIO_PRIVATE_APP_ID));
        // The public per-issuer corpora are NOT excluded — they replicate.
        assert!(!is_gossip_excluded("proxy-cik0000034088"));
    }

    /// **Structural invariant pin** for the local Activity ledger
    /// (see `activity` module). The user's own resource usage —
    /// tokens, embeddings, chunks ingested — is recorded under
    /// `activity-private` and must never gossip. The gossiped
    /// `contributions` namespace (what you provided to peers) is the
    /// deliberate counterpart and must continue to replicate.
    #[test]
    fn gossip_excludes_activity_private_app_id() {
        assert!(is_gossip_excluded("activity-private"));
        assert!(!is_gossip_excluded("contributions"));
    }

    /// **Completeness guard — constant ↔ list agreement, both ways.**
    /// The exclusion list holds string literals; the namespaces have
    /// named constants elsewhere. These can drift independently (rename
    /// the constant, forget the literal — or vice versa). Pin both
    /// directions against the actual constants so the privacy posture
    /// can't silently rot:
    /// - every PRIVATE constant must BE excluded (else it starts
    ///   gossiping — the leak), and
    /// - every PUBLIC constant must NOT be excluded (else replication
    ///   silently stops — the inverse break, e.g. the contribution
    ///   ledger going dark mesh-wide).
    ///
    /// A new local-only namespace added without a matching
    /// `GOSSIP_EXCLUDED_APP_IDS` entry won't be caught here unless it
    /// also exposes a constant — so the rule for reviewers stands:
    /// a private namespace ships its `app_id` as a constant AND a line
    /// in this test. The string-only members (`work-atlas-private`,
    /// `notes-private`) are pinned by their own tests above and, for
    /// work-atlas, by `Privacy::app_id()`'s in-crate agreement test.
    #[test]
    fn gossip_exclusion_list_agrees_with_named_constants() {
        use crate::{ACTIVITY_APP_ID, CONTRIBUTIONS_APP_ID, PROCESSED_SHARDS_APP_ID};

        // Private — must be excluded.
        assert!(
            is_gossip_excluded(ACTIVITY_APP_ID),
            "ACTIVITY_APP_ID ({ACTIVITY_APP_ID}) is local-only and must be \
             in GOSSIP_EXCLUDED_APP_IDS"
        );
        assert!(
            is_gossip_excluded(PEER_PREFERENCES_APP_ID),
            "PEER_PREFERENCES_APP_ID ({PEER_PREFERENCES_APP_ID}) is local-only"
        );

        // Public — must NOT be excluded (over-exclusion silently kills
        // replication of work you DID share with peers).
        assert!(
            !is_gossip_excluded(CONTRIBUTIONS_APP_ID),
            "CONTRIBUTIONS_APP_ID ({CONTRIBUTIONS_APP_ID}) must gossip — it is \
             the peer-facing counterpart to activity-private"
        );
        assert!(
            !is_gossip_excluded(PROCESSED_SHARDS_APP_ID),
            "PROCESSED_SHARDS_APP_ID ({PROCESSED_SHARDS_APP_ID}) must gossip"
        );
    }
}
