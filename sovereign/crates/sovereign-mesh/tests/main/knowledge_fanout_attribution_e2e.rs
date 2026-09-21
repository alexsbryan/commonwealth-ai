// SPDX-License-Identifier: AGPL-3.0-or-later
//! The attribution half of the two-daemon knowledge fan-out tests: the
//! requester's `X-Node-Id` reaching the peer's ledger, the served chat
//! turn naming the peer, and a corpus read served while the inference
//! slot is held. Fixtures live in `knowledge_fanout_e2e.rs`.
use std::collections::HashMap;
use std::sync::Arc;

use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use commonwealth_core::contributions::LedgerEventKind;
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use commonwealth_state::MeshStore;
use corpus_engine::index::{CorpusIndex, InsertChunk};
use corpus_engine::{CorpusEngine, EmbedFn};
use oicp_types::knowledge::CorpusShardInfo;
use sovereign_daemon::server::{client_router, internal_router};
use sovereign_daemon::state::AppState;
use sovereign_meshapp_registry::registry::AppRegistry;

use crate::common;
use crate::common::spawn_router;

use crate::knowledge_fanout_e2e::{caps_with_hosted, install_corpus, mock_embed_fn, EMBED_DIM};

#[tokio::test]
async fn a_fan_out_hop_the_server_cannot_verify_is_served_and_not_attributed() {
    // §10 accounting, as it stands after 2026-09-20: B fans out to A over a
    // PLAIN HTTP hop to A's internal port, with no iroh acceptor in front.
    // A serves the request and attributes it to NOBODY.
    //
    // The old shape of this test asserted one `KnowledgeQueryServed` stamped
    // `for_node = id_b`, on the strength of the `X-Node-Id` the fan-out
    // stamps. That header is what B TYPES about itself, and A cannot tell it
    // apart from any other caller's typing, so believing it is how one node
    // spends another's reciprocity — `mp-1-deciders-read-the-principal`.
    // Attribution now comes from the key the iroh handshake proved, which
    // this hop does not carry; the attributed path is proved over a real
    // handshake in `iroh_dialer_admission_e2e`.
    //
    // The sender is deliberately unchanged and still stamps: a receiver on an
    // older build routes on the header's PRESENCE, and an unstamped turn
    // there is admitted as that node's own local traffic with pause,
    // foreground yield and `max_peer_inflight` all dark.
    //
    // On this host's mesh the case below does not arise in production:
    // `require_encryption` routes every member-to-member call over iroh, so
    // every real fan-out reaches A through the acceptor and IS attributed.
    let tmp_a = tempfile::tempdir().unwrap();
    let indexes_a = tmp_a.path().join("indexes");
    std::fs::create_dir_all(&indexes_a).unwrap();
    install_corpus(
        &indexes_a,
        "sep",
        "Stanford Encyclopedia of Philosophy",
        "Some philosophical content.",
    )
    .await;
    let recipes_a = tmp_a.path().join("recipes");
    std::fs::create_dir_all(&recipes_a).unwrap();
    let engine_a = Arc::new(
        CorpusEngine::new(recipes_a, indexes_a, mock_embed_fn())
            .with_embedding_model("qwen3-embedding-0.6b"),
    );

    let id_a = NodeId::from_u128(0xA1_A1_A1_A1_A1_A1_A1_A1);
    let id_b = NodeId::from_u128(0xB2_B2_B2_B2_B2_B2_B2_B2);

    // A's mesh: solo. The ledger emitter is on A's AppState; we
    // keep a handle to it for the assertion.
    let state_a = AppState::new_with_platform_and_engine(
        id_a,
        Mesh {
            mesh_secret: [0u8; 32],
            invite_expires_at: None,
            id: MeshId::from_u128(1),
            name: "ledger-stamp-test".into(),
            invite_key_hash: [0u8; 32],
            invite_version: 0,
            require_encryption: false,
            members: HashMap::new(),
            peers: vec![],
        },
        Arc::new(MeshStore::in_memory().unwrap()),
        Arc::new(AppRegistry::new()),
        Some(Arc::clone(&engine_a)),
    );
    let addr_a = spawn_router(internal_router(state_a.clone())).await;

    // B's mesh: knows A as an Online peer hosting "sep".
    let mut members_b = HashMap::new();
    members_b.insert(
        id_b,
        MemberRecord {
            removed_at: None,
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
            dial_info_version: 0,
            dial_info_sig: None,
            node_id: id_b,
            name: "Joiner".into(),
            invited_by: id_a,
            joined_at: 0,
            last_seen: 0,
            status: NodeStatus::Online,
            capabilities: caps_with_hosted(&[]),
            addresses: vec!["127.0.0.1:0".parse().unwrap()],
        },
    );
    members_b.insert(
        id_a,
        MemberRecord {
            removed_at: None,
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
            dial_info_version: 0,
            dial_info_sig: None,
            node_id: id_a,
            name: "Founder".into(),
            invited_by: id_a,
            joined_at: 0,
            last_seen: 0,
            status: NodeStatus::Online,
            capabilities: caps_with_hosted(&["sep"]),
            addresses: vec![addr_a],
        },
    );
    let state_b = AppState::new_with_platform_and_engine(
        id_b,
        Mesh {
            mesh_secret: [0u8; 32],
            invite_expires_at: None,
            id: MeshId::from_u128(1),
            name: "ledger-stamp-test".into(),
            invite_key_hash: [0u8; 32],
            invite_version: 0,
            require_encryption: false,
            members: members_b,
            peers: vec![],
        },
        Arc::new(MeshStore::in_memory().unwrap()),
        Arc::new(AppRegistry::new()),
        None,
    );
    let addr_b = spawn_router(client_router(state_b)).await;

    // Pre-condition: A's ledger has no KnowledgeQueryServed.
    let pre_events = state_a
        .inner
        .fabric
        .contribution_emitter
        .events()
        .expect("emitter.events() ok");
    assert!(
        pre_events
            .iter()
            .all(|e| !matches!(e.kind, LedgerEventKind::KnowledgeQueryServed { .. })),
        "ledger should be clean before the fan-out fires"
    );

    // Fire the fan-out from B.
    let resp = reqwest::Client::new()
        .post(format!("http://{addr_b}/v1/knowledge/search"))
        .json(&serde_json::json!({
            "query_embedding": vec![0.0_f32; EMBED_DIM],
            "query_text": "philosophy",
            "corpora": ["sep"],
            "limit": 5,
        }))
        .send()
        .await
        .expect("/v1/knowledge/search reachable on Joiner");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // A's emitter must show NO KnowledgeQueryServed: the hop carried a claim
    // and no proof.
    let post_events = state_a
        .inner
        .fabric
        .contribution_emitter
        .events()
        .expect("emitter.events() ok");
    let served: Vec<(NodeId, String, u32)> = post_events
        .iter()
        .filter_map(|e| match &e.kind {
            LedgerEventKind::KnowledgeQueryServed {
                for_node,
                corpus_id,
                chunks_returned,
            } => Some((*for_node, corpus_id.clone(), *chunks_returned)),
            _ => None,
        })
        .collect();

    assert!(
        served.is_empty(),
        "a hop A cannot verify must be SERVED and attributed to nobody — \
         crediting the header would let any caller that can reach this port \
         spend another member's reciprocity. Got: {served:?}"
    );
}
