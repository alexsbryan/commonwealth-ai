// SPDX-License-Identifier: AGPL-3.0-or-later
//! Port-level counterparts to the real-store proofs that now live beside the
//! store (`commonwealth/crates/commonwealth-state/tests/work_atlas_store.rs`).
//!
//! That file drives `WorkAtlasStore` over the REAL `MeshStore`, so it can
//! assert what replication does with a record — the ring, the travel, the
//! receiver side. This file pins what the PORT owes, over `SoloReplicatedKv`:
//! two independent store instances sharing one port see each other's writes
//! and deletes, the claims-rail receipt stamps on the observing side only and
//! never rides the stored bytes, and a Private record lands in the private
//! namespace and nowhere else. A capability is buildable and testable without
//! the mesh substrate (cw-lift 3b), so these stay here.

use std::path::PathBuf;
use std::sync::Arc;

use kernel_types::NodeId;
use sovereign_contracts::peer::{ReplicatedKv, SoloReplicatedKv};
use uuid::Uuid;

use sovereign_time::unix_now_u64;
use sovereign_work_atlas::model::{
    AgentKind, ClaimRecord, ObservationRecord, ObservationSource, Privacy, SessionRecord, SymbolRef,
};
use sovereign_work_atlas::store::ScopeMatch;
use sovereign_work_atlas::tools::collect_in_flight;
use sovereign_work_atlas::WorkAtlasStore;

/// Two independent `WorkAtlasStore` instances over ONE shared port — the
/// port-level shape of "two workstations, one mesh": what instance A sets,
/// instance B already reads, because the port is the shared medium.
fn two_instances() -> (Arc<SoloReplicatedKv>, WorkAtlasStore, WorkAtlasStore) {
    let port = Arc::new(SoloReplicatedKv::new());
    let a = WorkAtlasStore::new(
        port.clone() as Arc<dyn ReplicatedKv>,
        NodeId::from_u128(0xA),
    );
    let b = WorkAtlasStore::new(
        port.clone() as Arc<dyn ReplicatedKv>,
        NodeId::from_u128(0xB),
    );
    (port, a, b)
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
        // The claim's node is the OWNING node — the reader-side
        // `node_is_self`/receipt logic keys on it (fix 1, commons-fluency).
        node_id: Some(node_id),
        received_at: None,
    }
}

/// Port-level half of `public_claim_propagates_via_the_ring`: a claim
/// written through instance A is listed by instance B over the same port.
/// The replication round itself is the mesh's half, asserted beside the
/// store.
#[test]
fn claim_written_by_one_instance_is_visible_through_another() {
    let (_port, a, b) = two_instances();

    let session = sample_session(
        NodeId::from_u128(0xA),
        Privacy::Public,
        "conn:abc",
        &"r".repeat(64),
    );
    a.put_session(&session).unwrap();
    let claim = sample_claim(
        session.session_id,
        "CorpusEngine::ingest",
        NodeId::from_u128(0xA),
    );
    a.put_claim(Privacy::Public, &claim).unwrap();

    let seen = b
        .list_claims_for_scope("CorpusEngine::ingest", ScopeMatch::Symbol)
        .unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].claim_id, claim.claim_id);
    assert_eq!(seen[0].intent, "tuning fanout");
}

/// Port-level half of `peer_claim_gets_received_at_on_first_observation`:
/// the receipt is a read-side stamp of the OBSERVING instance. The origin's
/// own reads stay `None`, the peer instance stamps on first observation,
/// the stamp is idempotent, the scope-scan surface stamps the same way, and
/// the stored bytes carry no receipt.
#[test]
fn received_at_is_stamped_by_the_observing_instance_only() {
    let (port, a, b) = two_instances();

    let session = sample_session(
        NodeId::from_u128(0xA),
        Privacy::Public,
        "conn:rec",
        &"r".repeat(64),
    );
    a.put_session(&session).unwrap();
    let claim = sample_claim(session.session_id, "Engine::ingest", NodeId::from_u128(0xA));
    a.put_claim(Privacy::Public, &claim).unwrap();

    // The origin's own read: no receipt — it never received its own claim.
    let (_, own) = a
        .get_claim(claim.claim_id)
        .unwrap()
        .expect("origin reads its own claim");
    assert_eq!(own.received_at, None, "origin must not stamp its own claim");

    // B's first observation stamps the receipt.
    let (_, peer) = b
        .get_claim(claim.claim_id)
        .unwrap()
        .expect("peer reads the claim");
    let received = peer.received_at.expect("peer receipt stamped");

    // Idempotent: a later read keeps the first stamp, not a new one.
    let (_, again) = b
        .get_claim(claim.claim_id)
        .unwrap()
        .expect("peer reads again");
    assert_eq!(again.received_at, Some(received), "first observation wins");

    // The scope-scan surface (what work_in_flight uses) stamps the same way.
    let scanned = b
        .list_claims_for_scope("Engine::ingest", ScopeMatch::Symbol)
        .unwrap();
    assert_eq!(scanned[0].received_at, Some(received));

    // Wire stability: the receipt is a read-side stamp — the stored bytes
    // must NOT carry it, so replication between old and new binaries stays
    // byte-compatible.
    let raw = port
        .get(
            Privacy::Public.app_id(),
            &format!("claim:{}", claim.claim_id),
        )
        .unwrap()
        .expect("stored claim");
    let parsed: serde_json::Value = serde_json::from_slice(&raw.value).unwrap();
    assert!(
        parsed.get("received_at").is_none(),
        "receipt must not ride the replicated bytes"
    );

    // B's own claim, read by B: no receipt.
    let claim_b = sample_claim(session.session_id, "Engine::other", NodeId::from_u128(0xB));
    b.put_claim(Privacy::Public, &claim_b).unwrap();
    let (_, own_b) = b
        .get_claim(claim_b.claim_id)
        .unwrap()
        .expect("B reads its own claim");
    assert_eq!(own_b.received_at, None);
}

/// Port-level half of `private_claim_never_propagates`: a Private claim
/// lands ONLY in the private namespace — the claim analogue of
/// `private_session_writes_only_to_private_app_id`. That the write is then
/// never queued for the rail is the mesh's half, asserted beside the store.
#[test]
fn private_claim_writes_only_to_the_private_app_id() {
    let (port, a, _b) = two_instances();

    let session = sample_session(
        NodeId::from_u128(0xA),
        Privacy::Private,
        "conn:secret",
        &"r".repeat(64),
    );
    a.put_session(&session).unwrap();
    let claim = sample_claim(session.session_id, "Secret::method", NodeId::from_u128(0xA));
    a.put_claim(Privacy::Private, &claim).unwrap();

    let public_hits = port.scan(Privacy::Public.app_id(), "claim:").unwrap();
    assert!(
        public_hits.is_empty(),
        "private claim leaked to public namespace"
    );

    let private_hits = port.scan(Privacy::Private.app_id(), "claim:").unwrap();
    assert_eq!(private_hits.len(), 1);
}

/// Port-level half of `public_observation_propagates_via_the_ring`: an
/// observation written through instance A is listed by instance B — the
/// wire signal behind the cross-mesh "peer is editing this file" view.
#[test]
fn observation_written_by_one_instance_is_visible_through_another() {
    let (_port, a, b) = two_instances();

    let session = sample_session(
        NodeId::from_u128(0xA),
        Privacy::Public,
        "edits:a",
        &"r".repeat(64),
    );
    a.put_session(&session).unwrap();
    let obs = ObservationRecord {
        session_id: session.session_id,
        file_path: PathBuf::from("corpus-engine/src/engine/ingest.rs"),
        source: ObservationSource::CodeWatcherEdit,
        first_observed_at: unix_now_u64(),
        last_observed_at: unix_now_u64(),
        event_count: 4,
        symbol_refs: vec![],
    };
    a.put_observation(Privacy::Public, &obs).unwrap();

    let hits = b
        .list_observations_for_scope("corpus-engine/src/engine/ingest.rs", ScopeMatch::File)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].event_count, 4);
}

/// Port-level half of `private_observation_never_propagates`: Private
/// observations follow the same namespace contract as Private claims.
#[test]
fn private_observation_writes_only_to_the_private_app_id() {
    let (port, a, b) = two_instances();

    let session = sample_session(
        NodeId::from_u128(0xA),
        Privacy::Private,
        "edits:secret",
        &"r".repeat(64),
    );
    a.put_session(&session).unwrap();
    let obs = ObservationRecord {
        session_id: session.session_id,
        file_path: PathBuf::from("Secret.rs"),
        source: ObservationSource::CodeWatcherEdit,
        first_observed_at: unix_now_u64(),
        last_observed_at: unix_now_u64(),
        event_count: 1,
        symbol_refs: vec![],
    };
    a.put_observation(Privacy::Private, &obs).unwrap();

    // Never in the public namespace — the only one
    // `list_observations_for_scope` reads.
    let hits = b
        .list_observations_for_scope("Secret.rs", ScopeMatch::File)
        .unwrap();
    assert!(hits.is_empty());

    // The private namespace holds it for the owner.
    let private_hits = port
        .scan(Privacy::Private.app_id(), "observation:")
        .unwrap();
    assert_eq!(private_hits.len(), 1);
}

/// Port-level half of `release_propagates_as_a_tombstone`: a release
/// through instance A deletes the record instance B sees. That the delete
/// TRAVELS (reaching a peer only via a replication round) is the mesh's
/// half, asserted beside the store.
#[test]
fn release_by_one_instance_deletes_what_the_other_sees() {
    let (_port, a, b) = two_instances();

    let session = sample_session(
        NodeId::from_u128(0xA),
        Privacy::Public,
        "conn:abc",
        &"r".repeat(64),
    );
    a.put_session(&session).unwrap();
    let claim = sample_claim(session.session_id, "X", NodeId::from_u128(0xA));
    a.put_claim(Privacy::Public, &claim).unwrap();

    // Control: B holds it until the delete lands.
    let post = b.list_claims_for_scope("X", ScopeMatch::Symbol).unwrap();
    assert_eq!(post.len(), 1);

    a.release_claim(claim.claim_id).unwrap();
    let local = a.get_claim(claim.claim_id).unwrap();
    assert!(local.is_none(), "release left record on A");

    let post = b.list_claims_for_scope("X", ScopeMatch::Symbol).unwrap();
    assert!(
        post.is_empty(),
        "the delete through the port left B's view stale"
    );
}

/// Port-level shape of the 2026-08-07 misread
/// (`same_scope_on_two_nodes_is_distinguishable_by_node_is_self`, beside
/// the store): scope strings are not node-qualified, so two instances'
/// claims on the SAME scope string land in one bucket, and the collected
/// view must say which claim is the reader's own.
#[test]
fn same_scope_from_two_instances_is_distinguishable_by_node_is_self() {
    const HOST_LOCAL_SCOPE: &str = "daemon-runtime:9741-primary-slot";
    let (_port, a, b) = two_instances();

    // The peer instance claims ITS daemon's primary slot.
    let sess_a = sample_session(
        NodeId::from_u128(0xA),
        Privacy::Public,
        "conn:peer",
        &"r".repeat(64),
    );
    a.put_session(&sess_a).unwrap();
    let claim_a = sample_claim(sess_a.session_id, HOST_LOCAL_SCOPE, NodeId::from_u128(0xA));
    a.put_claim(Privacy::Public, &claim_a).unwrap();

    // A different session on THIS instance claims the local daemon, same string.
    let sess_b = sample_session(
        NodeId::from_u128(0xB),
        Privacy::Public,
        "conn:sibling",
        &"r".repeat(64),
    );
    b.put_session(&sess_b).unwrap();
    let claim_b = sample_claim(sess_b.session_id, HOST_LOCAL_SCOPE, NodeId::from_u128(0xB));
    b.put_claim(Privacy::Public, &claim_b).unwrap();

    // Query from instance B as a third session (its own token matches
    // neither claim), so both records are in view.
    let in_flight = collect_in_flight(
        &b,
        HOST_LOCAL_SCOPE,
        ScopeMatch::File,
        Some("conn:reader"),
        false,
    )
    .expect("collect");

    assert_eq!(
        in_flight.claims.len(),
        2,
        "both instances' claims should match this host-agnostic scope — that collision is the premise"
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
        "peer instance's claim reported as local — this is the misread that stalled real work"
    );
    assert!(
        self_flag(claim_b.claim_id),
        "this instance's own claim reported as remote"
    );
}
