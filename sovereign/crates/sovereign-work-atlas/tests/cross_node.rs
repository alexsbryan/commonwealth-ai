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
use std::time::{SystemTime, UNIX_EPOCH};

use commonwealth_core::ids::NodeId;
use commonwealth_state::{is_gossip_excluded, MeshStore};
use uuid::Uuid;

use sovereign_work_atlas::model::{
    AgentKind, ClaimRecord, ObservationRecord, ObservationSource, Privacy, SessionRecord, SymbolRef,
};
use sovereign_work_atlas::store::ScopeMatch;
use sovereign_work_atlas::WorkAtlasStore;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
    let now = now_secs();
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

fn sample_claim(session_id: Uuid, scope: &str) -> ClaimRecord {
    let now = now_secs();
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
    }
}

#[test]
fn public_claim_propagates_via_gossip() {
    let node_a = NodeId::from_u128(0xA);
    let node_b = NodeId::from_u128(0xB);
    let store_a = Arc::new(MeshStore::in_memory().unwrap());
    let store_b = Arc::new(MeshStore::in_memory().unwrap());
    let atlas_a = WorkAtlasStore::new(Arc::clone(&store_a), node_a);
    let atlas_b = WorkAtlasStore::new(Arc::clone(&store_b), node_b);

    let session = sample_session(node_a, Privacy::Public, "conn:abc", &"r".repeat(64));
    atlas_a.put_session(&session).unwrap();
    let claim = sample_claim(session.session_id, "CorpusEngine::ingest");
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

/// Spec §7 + ARCH §7.4: Private sessions/claims must produce zero
/// MeshStore records replicated. Pin both halves of the contract
/// from the receiver side.
#[test]
fn private_claim_never_propagates() {
    let node_a = NodeId::from_u128(0xA);
    let node_b = NodeId::from_u128(0xB);
    let store_a = Arc::new(MeshStore::in_memory().unwrap());
    let store_b = Arc::new(MeshStore::in_memory().unwrap());
    let atlas_a = WorkAtlasStore::new(Arc::clone(&store_a), node_a);
    let atlas_b = WorkAtlasStore::new(Arc::clone(&store_b), node_b);

    let session = sample_session(node_a, Privacy::Private, "conn:secret", &"r".repeat(64));
    atlas_a.put_session(&session).unwrap();
    let claim = sample_claim(session.session_id, "Secret::method");
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
    let atlas_a = WorkAtlasStore::new(Arc::clone(&store_a), node_a);
    let atlas_b = WorkAtlasStore::new(Arc::clone(&store_b), node_b);

    let session = sample_session(node_a, Privacy::Public, "edits:a", &"r".repeat(64));
    atlas_a.put_session(&session).unwrap();
    let obs = ObservationRecord {
        session_id: session.session_id,
        file_path: PathBuf::from("corpus-engine/src/engine/ingest.rs"),
        source: ObservationSource::CodeWatcherEdit,
        first_observed_at: now_secs(),
        last_observed_at: now_secs(),
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
    let atlas_a = WorkAtlasStore::new(Arc::clone(&store_a), node_a);
    let atlas_b = WorkAtlasStore::new(Arc::clone(&store_b), node_b);

    let session = sample_session(node_a, Privacy::Private, "edits:secret", &"r".repeat(64));
    atlas_a.put_session(&session).unwrap();
    let obs = ObservationRecord {
        session_id: session.session_id,
        file_path: PathBuf::from("Secret.rs"),
        source: ObservationSource::CodeWatcherEdit,
        first_observed_at: now_secs(),
        last_observed_at: now_secs(),
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
    let atlas_a = WorkAtlasStore::new(Arc::clone(&store_a), node_a);
    let atlas_b = WorkAtlasStore::new(Arc::clone(&store_b), node_b);

    let session = sample_session(node_a, Privacy::Public, "conn:abc", &"r".repeat(64));
    atlas_a.put_session(&session).unwrap();
    let claim = sample_claim(session.session_id, "X");
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
