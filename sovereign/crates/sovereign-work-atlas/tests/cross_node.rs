// SPDX-License-Identifier: AGPL-3.0-or-later
//! Cross-node behaviour without spinning up daemons.
//!
//! Models two workstations sharing a mesh by replicating MeshStore
//! entries via `all_entries_for_gossip` + `merge_entry` — the exact
//! path the gossip loop uses. This catches the privacy invariant
//! (a Private claim cannot reach the peer's view via gossip) without
//! the test infrastructure cost of two tokio runtimes + axum.
//!
//! For full HTTP-level integration, see the planned daemon tests
//! under `sovereign-cli/tests/` (not yet wired in Phase 1).

use std::path::PathBuf;
use std::sync::Arc;

use commonwealth_state::{is_gossip_excluded, MeshStore};
use kernel_types::NodeId;
use sovereign_contracts::peer::{PeerEntry, PeerStore, PeerStoreError};
use uuid::Uuid;

use sovereign_core::time::unix_now_u64;
use sovereign_work_atlas::model::{
    AgentKind, ClaimRecord, ObservationRecord, ObservationSource, Privacy, SessionRecord, SymbolRef,
};
use sovereign_work_atlas::store::ScopeMatch;
use sovereign_work_atlas::WorkAtlasStore;

/// `PeerStore` over the REAL `MeshStore`, so these tests drive the actual
/// gossip path rather than a stand-in — `all_entries_for_gossip` and
/// `merge_entry` below are the mesh's own, and the privacy invariant is only
/// worth asserting against them.
///
/// This is the same delegation `sovereign_mesh::peer_adapter::MeshPeerStore`
/// performs, and it is repeated here because this crate cannot reach that one:
/// `sovereign-mesh` sits in the `mesh-api` layer, above `capabilities`, and
/// the arrow only points down. Twenty lines in a test file is the cheaper
/// half of that trade.
struct MeshPeer(Arc<MeshStore>);

impl MeshPeer {
    fn new(store: &Arc<MeshStore>) -> Arc<dyn PeerStore> {
        Arc::new(Self(Arc::clone(store)))
    }
}

fn port_err(e: commonwealth_state::error::Error) -> PeerStoreError {
    PeerStoreError::Backend(e.to_string())
}

fn port_entry(e: commonwealth_state::StoreEntry) -> PeerEntry {
    PeerEntry {
        app_id: e.app_id,
        key: e.key,
        value: e.value,
        timestamp: e.timestamp,
        origin: e.origin,
    }
}

impl PeerStore for MeshPeer {
    fn get(&self, app_id: &str, key: &str) -> Result<Option<PeerEntry>, PeerStoreError> {
        self.0
            .get(app_id, key)
            .map(|o| o.map(port_entry))
            .map_err(port_err)
    }

    fn set(
        &self,
        app_id: &str,
        key: &str,
        value: bytes::Bytes,
        origin: NodeId,
    ) -> Result<bool, PeerStoreError> {
        self.0.set(app_id, key, value, origin).map_err(port_err)
    }

    fn delete(&self, app_id: &str, key: &str) -> Result<bool, PeerStoreError> {
        self.0.delete(app_id, key).map_err(port_err)
    }

    fn scan(&self, app_id: &str, prefix: &str) -> Result<Vec<PeerEntry>, PeerStoreError> {
        self.0
            .scan(app_id, prefix)
            .map(|v| v.into_iter().map(port_entry).collect())
            .map_err(port_err)
    }
}

/// Mirror the gossip loop: take every record from `src` that the
/// gossip layer would broadcast, merge into `dst`. Private app_ids
/// are filtered out at the source — the same way the real loop
/// works.
fn replicate(src: &MeshStore, dst: &MeshStore) {
    for entry in src.all_entries_for_gossip().expect("gossip enumerate") {
        // Sanity: nothing under an excluded app_id should ever appear here.
        assert!(
            !is_gossip_excluded(&entry.app_id),
            "gossip leaked excluded app_id '{}'",
            entry.app_id
        );
        dst.merge_entry(entry).expect("merge");
    }
}

fn sample_session(node_id: NodeId, privacy: Privacy, token: &str, repo_id: &str) -> SessionRecord {
    let now = unix_now_u64();
    SessionRecord {
        session_id: Uuid::new_v4(),
        node_id,
        agent_kind: AgentKind::Agent,
        agent_session_token: Some(token.into()),
        repo_id: repo_id.into(),
        repo_root: PathBuf::from("/tmp/x"),
        current_branch: Some("main".into()),
        privacy,
        created_at: now,
        last_activity_at: now,
    }
}

fn sample_claim(session_id: Uuid, scope: &str, node_id: NodeId) -> ClaimRecord {
    let now = unix_now_u64();
    ClaimRecord {
        claim_id: Uuid::new_v4(),
        session_id,
        intent: "tuning fanout".into(),
        symbol_refs: vec![SymbolRef {
            scip_symbol: None,
            file_path: PathBuf::from(scope),
            scip_was_fresh: false,
        }],
        declared_at: now,
        ttl_expires_at: now + 3600,
        // Fix 1 (commons-fluency): claims carry their node; the
        // cross-node tests pin that attribution no longer depends on
        // the session replicating first. The claim's node must be the
        // OWNING node — a test that tags node A's claim as node B
        // would make A's own claim read as remote.
        node_id: Some(node_id),
        // Fix 3b (commons-fluency): the origin writes no receipt —
        // peers stamp `received_at` on first observation.
        received_at: None,
    }
}

#[test]
fn public_claim_propagates_via_gossip() {
    let node_a = NodeId::from_u128(0xA);
    let node_b = NodeId::from_u128(0xB);
    let store_a = Arc::new(MeshStore::in_memory().unwrap());
    let store_b = Arc::new(MeshStore::in_memory().unwrap());
    let atlas_a = WorkAtlasStore::new(MeshPeer::new(&store_a), node_a);
    let atlas_b = WorkAtlasStore::new(MeshPeer::new(&store_b), node_b);

    let session = sample_session(node_a, Privacy::Public, "conn:abc", &"r".repeat(64));
    atlas_a.put_session(&session).unwrap();
    let claim = sample_claim(session.session_id, "CorpusEngine::ingest", node_a);
    atlas_a.put_claim(Privacy::Public, &claim).unwrap();

    // Before gossip: B sees nothing.
    let pre = atlas_b
        .list_claims_for_scope("CorpusEngine::ingest", ScopeMatch::Symbol)
        .unwrap();
    assert!(pre.is_empty());

    // After one round: B sees the claim.
    replicate(&store_a, &store_b);
    let post = atlas_b
        .list_claims_for_scope("CorpusEngine::ingest", ScopeMatch::Symbol)
        .unwrap();
    assert_eq!(post.len(), 1);
    assert_eq!(post[0].claim_id, claim.claim_id);
    assert_eq!(post[0].intent, "tuning fanout");
}

/// Fix 3b (commons-fluency): claims-rail receipt. A peer stamps
/// `received_at` on FIRST local observation of a remote claim; the
/// origin's own reads stay `None`; re-reads keep the first stamp
/// (idempotent — the receipt means "first observed", not "last read");
/// and the stamp never rides the stored/gossiped bytes (it is a local
/// fact, and old binaries must keep parsing our claims).
#[test]
fn peer_claim_gets_received_at_on_first_observation() {
    let node_a = NodeId::from_u128(0xA);
    let node_b = NodeId::from_u128(0xB);
    let store_a = Arc::new(MeshStore::in_memory().unwrap());
    let store_b = Arc::new(MeshStore::in_memory().unwrap());
    let atlas_a = WorkAtlasStore::new(MeshPeer::new(&store_a), node_a);
    let atlas_b = WorkAtlasStore::new(MeshPeer::new(&store_b), node_b);

    let session = sample_session(node_a, Privacy::Public, "conn:rec", &"r".repeat(64));
    atlas_a.put_session(&session).unwrap();
    let claim = sample_claim(session.session_id, "Engine::ingest", node_a);
    atlas_a.put_claim(Privacy::Public, &claim).unwrap();

    // The origin's own read: no receipt — it never received its own claim.
    let (_, own) = atlas_a
        .get_claim(claim.claim_id)
        .unwrap()
        .expect("origin reads its own claim");
    assert_eq!(own.received_at, None, "origin must not stamp its own claim");

    // Before gossip: B has never observed the claim.
    assert!(atlas_b.get_claim(claim.claim_id).unwrap().is_none());

    // After one gossip round: B's first observation stamps the receipt
    // inside the observation bracket (seconds-granular clocks).
    let before = unix_now_u64();
    replicate(&store_a, &store_b);
    let (_, peer) = atlas_b
        .get_claim(claim.claim_id)
        .unwrap()
        .expect("peer reads the claim");
    let received = peer.received_at.expect("peer receipt stamped");
    let after = unix_now_u64();
    assert!(
        (before..=after).contains(&received),
        "receipt {received} outside observation bracket [{before},{after}]"
    );

    // Idempotent: a later read keeps the first stamp, not a new one.
    let (_, again) = atlas_b
        .get_claim(claim.claim_id)
        .unwrap()
        .expect("peer reads again");
    assert_eq!(again.received_at, Some(received), "first observation wins");

    // The scope-scan surface (what work_in_flight uses) stamps the
    // same way.
    let scanned = atlas_b
        .list_claims_for_scope("Engine::ingest", ScopeMatch::Symbol)
        .unwrap();
    assert_eq!(scanned[0].received_at, Some(received));

    // B's own claim, read by B: no receipt.
    let claim_b = sample_claim(session.session_id, "Engine::other", node_b);
    atlas_b.put_claim(Privacy::Public, &claim_b).unwrap();
    let (_, own_b) = atlas_b
        .get_claim(claim_b.claim_id)
        .unwrap()
        .expect("B reads its own claim");
    assert_eq!(own_b.received_at, None);

    // Wire stability: the receipt is a read-side stamp — the stored
    // bytes must NOT carry it, so gossip between old and new binaries
    // stays byte-compatible.
    let raw = store_a
        .get(
            Privacy::Public.app_id(),
            &format!("claim:{}", claim.claim_id),
        )
        .unwrap()
        .expect("stored claim");
    let parsed: serde_json::Value = serde_json::from_slice(&raw.value).unwrap();
    assert!(
        parsed.get("received_at").is_none(),
        "receipt must not ride the gossiped bytes"
    );
}

/// Spec §7 + ARCH §7.4: Private sessions/claims must produce zero
/// MeshStore records replicated. Pin both halves of the contract
/// from the receiver side.
#[test]
fn private_claim_never_propagates() {
    let node_a = NodeId::from_u128(0xA);
    let node_b = NodeId::from_u128(0xB);
    let store_a = Arc::new(MeshStore::in_memory().unwrap());
    let store_b = Arc::new(MeshStore::in_memory().unwrap());
    let atlas_a = WorkAtlasStore::new(MeshPeer::new(&store_a), node_a);
    let atlas_b = WorkAtlasStore::new(MeshPeer::new(&store_b), node_b);

    let session = sample_session(node_a, Privacy::Private, "conn:secret", &"r".repeat(64));
    atlas_a.put_session(&session).unwrap();
    let claim = sample_claim(session.session_id, "Secret::method", node_a);
    atlas_a.put_claim(Privacy::Private, &claim).unwrap();

    // Replicate. The function asserts no excluded app_id appears in
    // the gossip set, so this would already fail-loud on a regression.
    replicate(&store_a, &store_b);

    let post = atlas_b
        .list_claims_for_scope("Secret::method", ScopeMatch::Symbol)
        .unwrap();
    assert!(post.is_empty(), "private claim leaked to peer");

    // Defence-in-depth: even direct scan of the private namespace on B
    // returns nothing — the gossip layer never delivered the entry.
    let raw = store_b.scan("work-atlas-private", "claim:").unwrap();
    assert!(raw.is_empty(), "private claim landed in peer's store");
}

/// Phase 2: Observations propagate via gossip just like Claims.
/// Pinned because this is the wire signal that powers the cross-mesh
/// "mac-peer is editing this file" experience.
#[test]
fn public_observation_propagates_via_gossip() {
    let node_a = NodeId::from_u128(0xA);
    let node_b = NodeId::from_u128(0xB);
    let store_a = Arc::new(MeshStore::in_memory().unwrap());
    let store_b = Arc::new(MeshStore::in_memory().unwrap());
    let atlas_a = WorkAtlasStore::new(MeshPeer::new(&store_a), node_a);
    let atlas_b = WorkAtlasStore::new(MeshPeer::new(&store_b), node_b);

    let session = sample_session(node_a, Privacy::Public, "edits:a", &"r".repeat(64));
    atlas_a.put_session(&session).unwrap();
    let obs = ObservationRecord {
        session_id: session.session_id,
        file_path: PathBuf::from("corpus-engine/src/engine/ingest.rs"),
        source: ObservationSource::CodeWatcherEdit,
        first_observed_at: unix_now_u64(),
        last_observed_at: unix_now_u64(),
        event_count: 4,
        symbol_refs: vec![],
    };
    atlas_a.put_observation(Privacy::Public, &obs).unwrap();

    replicate(&store_a, &store_b);

    let hits = atlas_b
        .list_observations_for_scope("corpus-engine/src/engine/ingest.rs", ScopeMatch::File)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].event_count, 4);
}

/// Private observations follow the same privacy contract as Private
/// claims — never gossiped, never visible to peers.
#[test]
fn private_observation_never_propagates() {
    let node_a = NodeId::from_u128(0xA);
    let node_b = NodeId::from_u128(0xB);
    let store_a = Arc::new(MeshStore::in_memory().unwrap());
    let store_b = Arc::new(MeshStore::in_memory().unwrap());
    let atlas_a = WorkAtlasStore::new(MeshPeer::new(&store_a), node_a);
    let atlas_b = WorkAtlasStore::new(MeshPeer::new(&store_b), node_b);

    let session = sample_session(node_a, Privacy::Private, "edits:secret", &"r".repeat(64));
    atlas_a.put_session(&session).unwrap();
    let obs = ObservationRecord {
        session_id: session.session_id,
        file_path: PathBuf::from("Secret.rs"),
        source: ObservationSource::CodeWatcherEdit,
        first_observed_at: unix_now_u64(),
        last_observed_at: unix_now_u64(),
        event_count: 1,
        symbol_refs: vec![],
    };
    atlas_a.put_observation(Privacy::Private, &obs).unwrap();

    replicate(&store_a, &store_b);

    let leaked = store_b.scan("work-atlas-private", "observation:").unwrap();
    assert!(
        leaked.is_empty(),
        "private observation reached peer's store"
    );
    let hits = atlas_b
        .list_observations_for_scope("Secret.rs", ScopeMatch::File)
        .unwrap();
    assert!(hits.is_empty());
}

/// Spec §3: release drops the claim with no history. The peer
/// receives the deletion on the next gossip round.
#[test]
fn release_drops_claim_locally_and_via_gossip_after_resync() {
    let node_a = NodeId::from_u128(0xA);
    let node_b = NodeId::from_u128(0xB);
    let store_a = Arc::new(MeshStore::in_memory().unwrap());
    let store_b = Arc::new(MeshStore::in_memory().unwrap());
    let atlas_a = WorkAtlasStore::new(MeshPeer::new(&store_a), node_a);
    let atlas_b = WorkAtlasStore::new(MeshPeer::new(&store_b), node_b);

    let session = sample_session(node_a, Privacy::Public, "conn:abc", &"r".repeat(64));
    atlas_a.put_session(&session).unwrap();
    let claim = sample_claim(session.session_id, "X", node_a);
    atlas_a.put_claim(Privacy::Public, &claim).unwrap();
    replicate(&store_a, &store_b);

    // A releases.
    atlas_a.release_claim(claim.claim_id).unwrap();
    let local = atlas_a.get_claim(claim.claim_id).unwrap();
    assert!(local.is_none(), "release left record on A");

    // Note: MeshStore's gossip path is anti-entropy push of LIVE
    // entries — a deletion on A doesn't push a tombstone to B; B
    // simply stops re-receiving the entry. B's existing copy stays
    // until B's own TTL-based gc sweeps it. This is the same
    // semantics atos-sessions and contributions live with today,
    // and is acceptable for the work atlas — claim TTLs are short
    // (≤24h hard ceiling). Pin the current behaviour so a future
    // refactor that adds tombstones is a deliberate change.
    let post = atlas_b
        .list_claims_for_scope("X", ScopeMatch::Symbol)
        .unwrap();
    assert_eq!(
        post.len(),
        1,
        "release-as-deletion is locally immediate but does not propagate via anti-entropy push"
    );
}

/// A host-local resource claimed on TWO machines under the SAME scope
/// string must be distinguishable by the reader.
///
/// This is the regression that motivated `node_is_self` (2026-08-07).
/// Every node's daemon listens on :9741, so a scope like
/// `daemon-runtime:9741-primary-slot` is not node-qualified — one bucket
/// holds every node's claim on its OWN daemon. An agent querying it saw a
/// peer's claim ("ci-bench running on the primary slot, please coordinate
/// before restarting"), read it as a lock on the box it was sitting on, and
/// stalled work that was never actually blocked. `node_id` was present the
/// whole time but is an opaque hash, and nothing in the response said which
/// hash was the caller's.
///
/// Pinned from the READER's side: what matters is not that the field is
/// stored but that a consumer can tell the two apart in one pass.
#[test]
fn same_scope_on_two_nodes_is_distinguishable_by_node_is_self() {
    const HOST_LOCAL_SCOPE: &str = "daemon-runtime:9741-primary-slot";

    let node_a = NodeId::from_u128(0xA);
    let node_b = NodeId::from_u128(0xB);
    let store_a = Arc::new(MeshStore::in_memory().unwrap());
    let store_b = Arc::new(MeshStore::in_memory().unwrap());
    let atlas_a = WorkAtlasStore::new(MeshPeer::new(&store_a), node_a);
    let atlas_b = WorkAtlasStore::new(MeshPeer::new(&store_b), node_b);

    // Peer machine claims ITS daemon's primary slot.
    let sess_a = sample_session(node_a, Privacy::Public, "conn:peer", &"r".repeat(64));
    atlas_a.put_session(&sess_a).unwrap();
    let claim_a = sample_claim(sess_a.session_id, HOST_LOCAL_SCOPE, node_a);
    atlas_a.put_claim(Privacy::Public, &claim_a).unwrap();

    // A different session on THIS machine claims the local daemon, same string.
    let sess_b = sample_session(node_b, Privacy::Public, "conn:sibling", &"r".repeat(64));
    atlas_b.put_session(&sess_b).unwrap();
    let claim_b = sample_claim(sess_b.session_id, HOST_LOCAL_SCOPE, node_b);
    atlas_b.put_claim(Privacy::Public, &claim_b).unwrap();

    replicate(&store_a, &store_b);

    // Query from node B as a third session (its own token matches neither
    // claim), so both records are in view — the situation that misled.
    let in_flight = sovereign_work_atlas::tools::collect_in_flight(
        &atlas_b,
        HOST_LOCAL_SCOPE,
        ScopeMatch::File,
        Some("conn:reader"),
        false,
    )
    .expect("collect");

    assert_eq!(
        in_flight.claims.len(),
        2,
        "both nodes' claims should match this host-agnostic scope — that collision is the premise"
    );

    let self_flag = |claim_id: Uuid| -> bool {
        in_flight
            .claims
            .iter()
            .find(|c| c["claim_id"] == serde_json::json!(claim_id.to_string()))
            .unwrap_or_else(|| panic!("claim {claim_id} missing from view"))["node_is_self"]
            .as_bool()
            .expect("node_is_self must be a bool on every claim")
    };

    assert!(
        !self_flag(claim_a.claim_id),
        "peer node's claim reported as local — this is the misread that stalled real work"
    );
    assert!(
        self_flag(claim_b.claim_id),
        "this node's own claim reported as remote"
    );
}
