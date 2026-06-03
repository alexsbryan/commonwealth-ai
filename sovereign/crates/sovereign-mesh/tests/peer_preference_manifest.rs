//! Peer-preference manifest stamping — HTTP-surface integration.
//!
//! `routes_oicp::apply_peer_preference` is L1-pinned at the function
//! level (three lib tests in `routes_oicp.rs::tests`). What those
//! don't cover: does the `X-Node-Id` header actually flow from the
//! wire, through `parse_x_node_id`, into the per-peer lookup, and
//! into the multiplication, on a real `GET /oicp/v1/capabilities`
//! response?
//!
//! Why this matters: §7.4 "Layer the enforcement" promises three
//! mechanisms keep peer preferences private and effective —
//! gossip-exclusion (pinned in `commonwealth-state::peer_preferences`),
//! clamping at construction (pinned in `peer_preferences::tests`),
//! and **manifest-stamping at the wire**. The third is the user-
//! visible enforcement; a regression that broke the header parse
//! or the lookup would silently disable the sanction without any
//! caller seeing an error. Until this test, the third layer was
//! tested only by reading code.
//!
//! Three assertions:
//!
//! 1. **Header present, preference set → all claims' `affinity`
//!    multiplied** by the configured multiplier.
//! 2. **Header present, no preference for that peer → affinities
//!    unchanged**. Anyone-can-fetch isn't penalized by a stranger's
//!    presence in the store.
//! 3. **Header absent → affinities unchanged**. The local-origin
//!    /unidentified-peer path is the safe-default and must not
//!    accidentally pick up some other peer's preference.
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use commonwealth_api::server::client_router;
use commonwealth_api::state::{AppState, LocalInferenceService};
use commonwealth_app::registry::AppRegistry;
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use commonwealth_state::{MeshStore, PeerPreference};
use sovereign_core::traits::InferenceProvider;
use sovereign_mesh::inference_adapter::SovereignInferenceAdapter;

mod common;
use common::{id_to_hex, member, spawn_router, TestProvider};

/// Build an AppState with the manifest-producing adapter wired in.
fn build_state(self_id: NodeId) -> AppState {
    let mut members = HashMap::new();
    members.insert(
        self_id,
        member(self_id, "self", "127.0.0.1:9742".parse().unwrap()),
    );
    let mesh = Mesh {
        id: MeshId::from_u128(1),
        name: "preference-test".into(),
        join_key_hash: [9u8; 32],
        members,
        peers: vec![],
    };
    let mesh_store = Arc::new(MeshStore::in_memory().unwrap());
    let app_registry = Arc::new(AppRegistry::new());
    let state =
        AppState::new_with_platform_and_engine(self_id, mesh, mesh_store, app_registry, None);
    let provider: Arc<dyn InferenceProvider> =
        Arc::new(TestProvider::new().with_model_id("manifest-stub"));
    let adapter: Arc<dyn LocalInferenceService> =
        Arc::new(SovereignInferenceAdapter::new(provider));
    state.with_local_inference(adapter)
}

async fn spawn(state: AppState) -> SocketAddr {
    spawn_router(client_router(state)).await
}

/// Pull every claim's affinity out of the JSON manifest, in document
/// order. The shape is `data.models[].claims[].affinity`.
fn affinities(body: &serde_json::Value) -> Vec<f64> {
    let mut out = Vec::new();
    if let Some(models) = body["models"].as_array() {
        for m in models {
            if let Some(claims) = m["claims"].as_array() {
                for c in claims {
                    if let Some(a) = c["affinity"].as_f64() {
                        out.push(a);
                    }
                }
            }
        }
    }
    out
}

async fn fetch_manifest(addr: SocketAddr, header: Option<(&str, String)>) -> serde_json::Value {
    let mut req = reqwest::Client::new().get(format!("http://{addr}/oicp/v1/capabilities"));
    if let Some((name, value)) = header {
        req = req.header(name, value);
    }
    let resp = req.send().await.expect("/oicp/v1/capabilities reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "manifest endpoint should return 200"
    );
    resp.json().await.unwrap()
}

#[tokio::test]
async fn x_node_id_with_set_preference_halves_all_claim_affinities() {
    let self_id = NodeId::from_u128(0xAAAA);
    let target_peer = NodeId::from_u128(0xBBBB);
    let state = build_state(self_id);

    // Set a half-strength preference for `target_peer`.
    state
        .inner
        .peer_preferences
        .set(
            &target_peer,
            PeerPreference::new(0.5, Some("sanction reason".into())).unwrap(),
        )
        .expect("set preference");

    let addr = spawn(state).await;

    // Baseline: no header → no preference applied.
    let baseline = fetch_manifest(addr, None).await;
    let baseline_affinities = affinities(&baseline);
    assert!(
        !baseline_affinities.is_empty(),
        "manifest must include at least one claim with an affinity field: {baseline}"
    );

    // Stamped: X-Node-Id == target → every claim halved.
    let stamped = fetch_manifest(addr, Some(("X-Node-Id", id_to_hex(&target_peer)))).await;
    let stamped_affinities = affinities(&stamped);
    assert_eq!(
        stamped_affinities.len(),
        baseline_affinities.len(),
        "claim count must be stable across the multiplication"
    );
    for (i, (b, s)) in baseline_affinities
        .iter()
        .zip(stamped_affinities.iter())
        .enumerate()
    {
        let expected = b * 0.5;
        assert!(
            (s - expected).abs() < 1e-6,
            "claim[{i}]: baseline {b} * 0.5 = {expected} but got {s}"
        );
    }
}

#[tokio::test]
async fn x_node_id_for_unmatched_peer_does_not_modify_affinities() {
    // Header present, but the requester isn't in the preference
    // store → no multiplication. The store has a preference for
    // someone else; anyone-can-fetch isn't penalized by a stranger's
    // presence in the table.
    let self_id = NodeId::from_u128(0xCCCC);
    let stored_peer = NodeId::from_u128(0xDDDD);
    let unrelated_peer = NodeId::from_u128(0xEEEE);
    let state = build_state(self_id);
    state
        .inner
        .peer_preferences
        .set(&stored_peer, PeerPreference::new(0.25, None).unwrap())
        .expect("set preference");

    let addr = spawn(state).await;
    let baseline = fetch_manifest(addr, None).await;
    let baseline_affinities = affinities(&baseline);

    let fetched = fetch_manifest(addr, Some(("X-Node-Id", id_to_hex(&unrelated_peer)))).await;
    let fetched_affinities = affinities(&fetched);
    assert_eq!(
        baseline_affinities, fetched_affinities,
        "unmatched X-Node-Id must not change any affinity. \
         baseline={baseline_affinities:?} fetched={fetched_affinities:?}"
    );
}

#[tokio::test]
async fn no_header_does_not_pick_up_any_stored_preference() {
    // Local-origin / unidentified-peer path. Even though a
    // preference for `peer_X` exists, the absence of an X-Node-Id
    // header means `parse_x_node_id` returns None and the
    // multiplication is short-circuited.
    let self_id = NodeId::from_u128(0xFAFA);
    let stored_peer = NodeId::from_u128(0xFAFB);
    let state = build_state(self_id);
    state
        .inner
        .peer_preferences
        .set(&stored_peer, PeerPreference::new(0.1, None).unwrap())
        .expect("set preference");

    let addr = spawn(state).await;

    // We can't really compare against a baseline without a header
    // here — the baseline IS the no-header fetch. Instead, build
    // a second state that has NO preference set at all, and assert
    // the manifests are identical.
    let bare_state = build_state(NodeId::from_u128(0xFAFC));
    let bare_addr = spawn(bare_state).await;

    let m_with_pref_no_header = fetch_manifest(addr, None).await;
    let m_bare = fetch_manifest(bare_addr, None).await;

    let with_aff = affinities(&m_with_pref_no_header);
    let bare_aff = affinities(&m_bare);
    assert_eq!(
        with_aff, bare_aff,
        "no header + preference-store-with-irrelevant-entry must produce \
         the same affinities as a daemon with an empty preference store. \
         with={with_aff:?} bare={bare_aff:?}"
    );
}
