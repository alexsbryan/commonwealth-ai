// SPDX-License-Identifier: AGPL-3.0-or-later
//! The attribution half of the two-daemon knowledge fan-out tests: the
//! requester's `X-Node-Id` reaching the peer's ledger, the served chat
//! turn naming the peer, and a corpus read served while the inference
//! slot is held. Fixtures live in `knowledge_fanout_e2e.rs`.
use std::sync::Arc;

use corpus_index::index::{CorpusIndex, InsertChunk};
use corpus_index::types::EmbedFn;
use kernel_types::NodeId;
use oicp_types::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use oicp_types::knowledge::CorpusShardInfo;
use sovereign_contracts::daemon_wire::mesh::MemberStatus;
use sovereign_daemon::server::{client_router, internal_router};
use sovereign_daemon::state::AppState;

use crate::common;
use crate::common::ledger_double::RecordingLedger;
use crate::common::spawn_router;

use crate::knowledge_fanout_e2e::{caps_with_hosted, install_corpus, mock_embed_fn, EMBED_DIM};
use crate::knowledge_served_e2e::served_records;

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
    let engine_a = Arc::new(crate::common::reading_double(indexes_a, mock_embed_fn()));

    let id_a = NodeId::from_u128(0xA1_A1_A1_A1_A1_A1_A1_A1);
    let id_b = NodeId::from_u128(0xB2_B2_B2_B2_B2_B2_B2_B2);

    // A's mesh: solo. A's store ports are the recording double; we
    // keep a handle to it for the assertion.
    let double_a = Arc::new(RecordingLedger::new(id_a));
    let state_a = AppState::new_with_seeds(
        id_a,
        Some(engine_a.clone()),
        None,
        Default::default(),
        Default::default(),
        Default::default(),
        double_a.seed(),
    );
    let addr_a = spawn_router(internal_router(state_a.clone())).await;

    // B's roster: knows A as an Online peer hosting "sep".
    let state_b = common::state_over_roster(
        id_b,
        "ledger-stamp-test",
        vec![
            common::peer_row(
                id_b,
                "Joiner",
                MemberStatus::Online,
                caps_with_hosted(&[]),
                vec!["127.0.0.1:0".parse().unwrap()],
            ),
            common::peer_row(
                id_a,
                "Founder",
                MemberStatus::Online,
                caps_with_hosted(&["sep"]),
                vec![addr_a],
            ),
        ],
    );
    let addr_b = spawn_router(client_router(state_b)).await;

    // Pre-condition: A's ledger has no KnowledgeQueryServed.
    assert!(
        served_records(&double_a).is_empty(),
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
    let served = served_records(&double_a);

    assert!(
        served.is_empty(),
        "a hop A cannot verify must be SERVED and attributed to nobody — \
         crediting the header would let any caller that can reach this port \
         spend another member's reciprocity. Got: {served:?}"
    );
}

/// The attributed path since pb-mesh-exit-transport: cw-rails forwards a
/// member's search to A's registered peer prefix with the verified triple
/// and the live tie, and A's ledger credits that member — the dialer cw-rails
/// verified, never the node an `X-Node-Id` claims. Successor of the daemon's
/// iroh_verified_principal_e2e
/// `the_knowledge_ledger_names_the_dialer_and_not_the_node_it_claimed_to_be`.
/// Failing input: attribution read from `X-Node-Id`.
#[tokio::test]
async fn a_search_cw_rails_forwards_is_credited_to_the_verified_member() {
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use tower::ServiceExt;
    const MEMBER_KEY: [u8; 32] = [7u8; 32];

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
    let engine_a = Arc::new(crate::common::reading_double(indexes_a, mock_embed_fn()));
    let id_a = NodeId::from_u128(0xA1_A1_A1_A1_A1_A1_A1_A1);
    // Distinct HIGH bytes: a NodeId displays its first eight.
    let member = NodeId::from_u128(0x7777_7777_7777_7777_0000_0000_0000_0077);
    let claimed = NodeId::from_u128(0xC3C3_C3C3_C3C3_C3C3_C3C3_C3C3_C3C3_C3C3);
    let (roster, seed) = common::roster_seed(id_a, "attribution");
    common::name_member_with_key(&roster, member, "LittleMac", MEMBER_KEY);
    let double_a = Arc::new(RecordingLedger::new(id_a));
    let state_a = AppState::new_with_seeds(
        id_a,
        Some(engine_a),
        None,
        seed,
        Default::default(),
        Default::default(),
        double_a.seed(),
    );
    let _tie = common::tie_as_cw_rails(&state_a, common::TIE);

    let mut req = axum::http::Request::post("/internal/knowledge/search")
        .header("content-type", "application/json")
        .header("X-Mesh-Member", "LittleMac")
        .header("X-Mesh-Node", member.to_string())
        .header("X-Mesh-Pubkey", hex::encode(MEMBER_KEY))
        .header(kernel_types::member::ORIGIN_TIE_HEADER, common::TIE)
        .header("X-Node-Id", claimed.to_string())
        .body(Body::from(
            serde_json::json!({
                "query_embedding": vec![0.0_f32; EMBED_DIM],
                "query_text": "philosophy",
                "corpora": ["sep"],
                "limit": 5,
            })
            .to_string(),
        ))
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            51000,
        ))));
    let resp = internal_router(state_a).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let served = served_records(&double_a);
    assert_eq!(served.len(), 1, "one served query, recorded: {served:?}");
    assert!(
        served[0].contains(&format!("{member:?}")) || served[0].contains(&member.to_string()),
        "the record credits the verified member {member}: {served:?}"
    );
    assert!(
        !served[0].contains(&claimed.to_string()) && !served[0].contains(&format!("{claimed:?}")),
        "the record credits the node the header claimed: {served:?}"
    );
}
