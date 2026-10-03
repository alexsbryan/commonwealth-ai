// SPDX-License-Identifier: AGPL-3.0-or-later
//! Every ledger door round-trips over a real listener: a write through the
//! door is read back through the door, and a read through the door agrees
//! with the typed reader over the same store.

use std::collections::{BTreeSet, HashMap};

use commonwealth_core::activity::{ActivityEvent, ActivityEventKind};
use commonwealth_core::contributions::{LedgerEvent, LedgerEventKind, NodeContributions};
use commonwealth_core::ids::{ModelId, NodeId};
use commonwealth_core::model::{ModelArchitecture, ModelInfo};
use commonwealth_core::oicp::{EmbedModelInfo, NormalizationStrategy, PoolingStrategy};
use commonwealth_state::inference_plan::InferencePlan;
use commonwealth_state::peer_preferences::{peer_preference, PeerPreference, PeerPreferenceStore};
use commonwealth_state::MeshStore;
use serde_json::{json, Value};

use super::{router, LedgerDoors};

const DAEMON: u128 = 0xDAE;

async fn serve(store: MeshStore) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let forever = std::future::pending::<()>();
        host_kit::shell::serve([listener], vec![router(LedgerDoors::new(store))], forever)
            .await
            .unwrap();
    });
    format!("http://{addr}")
}

async fn get(base: &str, path: &str) -> Value {
    let resp = reqwest::get(format!("{base}{path}")).await.unwrap();
    assert!(resp.status().is_success(), "{path}: {}", resp.status());
    resp.json().await.unwrap()
}

async fn post(base: &str, path: &str, body: Value) -> Value {
    let resp = reqwest::Client::new()
        .post(format!("{base}{path}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "{path}: {}", resp.status());
    resp.json().await.unwrap()
}

fn model(id: u128) -> ModelInfo {
    ModelInfo {
        id: ModelId::from_u128(id),
        name: "m".into(),
        repo: "test/m".into(),
        file: "m.gguf".into(),
        size_bytes: 1,
        total_layers: 1,
        architecture: ModelArchitecture::Qwen,
        available_on: HashMap::new(),
        oicp_capabilities: Default::default(),
        quantization: "Q4_K_M".into(),
        min_memory_gb: 0,
        preferred_memory_gb: 0,
        supports_parallel_instances: false,
        supports_pipeline_shard: false,
    }
}

#[tokio::test]
async fn a_contribution_written_through_the_door_reads_back_under_the_daemons_id() {
    let store = MeshStore::in_memory().unwrap();
    let base = serve(store.clone()).await;
    let peer = NodeId::from_u128(7);
    let kind = LedgerEventKind::InferenceServed {
        for_node: peer,
        model_id: "m".into(),
        tokens_generated: 12,
        wall_seconds: 0.5,
    };
    post(
        &base,
        "/v1/ledger/contributions",
        json!({ "node_id": NodeId::from_u128(DAEMON), "record": kind }),
    )
    .await;

    let events: Vec<LedgerEvent> =
        serde_json::from_value(get(&base, "/v1/ledger/contributions").await).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].node_id, NodeId::from_u128(DAEMON));
    assert_eq!(events[0].kind, kind);

    let current: Vec<(NodeId, NodeContributions)> = serde_json::from_value(
        post(
            &base,
            "/v1/ledger/contributions/current",
            json!({ "peer_capabilities": [], "window_days": 30 }),
        )
        .await,
    )
    .unwrap();
    let expected = commonwealth_state::current_contributions(&store, &HashMap::new(), 30).unwrap();
    assert_eq!(current.into_iter().collect::<HashMap<_, _>>(), expected);
}

#[tokio::test]
async fn an_activity_written_through_the_door_is_in_the_current_summary() {
    let store = MeshStore::in_memory().unwrap();
    let base = serve(store.clone()).await;
    let kind = ActivityEventKind::LocalInferenceServed {
        model_id: "m".into(),
        prompt_tokens: 3,
        completion_tokens: 4,
        wall_seconds: 0.1,
    };
    post(
        &base,
        "/v1/ledger/activity",
        json!({ "node_id": NodeId::from_u128(DAEMON), "record": kind }),
    )
    .await;

    let events: Vec<ActivityEvent> =
        serde_json::from_value(get(&base, "/v1/ledger/activity").await).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].node_id, NodeId::from_u128(DAEMON));
    assert_eq!(events[0].kind, kind);
    let summary = post(
        &base,
        "/v1/ledger/activity/current",
        json!({ "window_days": 7 }),
    )
    .await;
    let expected = commonwealth_state::current_activity(&store, 7).unwrap();
    assert_eq!(summary, serde_json::to_value(&expected).unwrap());
    assert_ne!(
        expected,
        Default::default(),
        "the door's write reached the store"
    );
}

#[tokio::test]
async fn peer_preferences_list_and_get_read_the_store() {
    let store = MeshStore::in_memory().unwrap();
    let peer = NodeId::from_u128(9);
    PeerPreferenceStore::new(store.clone(), NodeId::from_u128(DAEMON))
        .set(&peer, peer_preference(0.5, Some("slow".into())).unwrap())
        .unwrap();
    let base = serve(store).await;

    let list: Vec<(NodeId, PeerPreference)> =
        serde_json::from_value(get(&base, "/v1/ledger/peer-preferences").await).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].0, peer);
    let one: Option<PeerPreference> = serde_json::from_value(
        post(
            &base,
            "/v1/ledger/peer-preferences/get",
            json!({ "peer": peer }),
        )
        .await,
    )
    .unwrap();
    assert_eq!(one, Some(list[0].1.clone()));
    let none: Option<PeerPreference> = serde_json::from_value(
        post(
            &base,
            "/v1/ledger/peer-preferences/get",
            json!({ "peer": NodeId::from_u128(10) }),
        )
        .await,
    )
    .unwrap();
    assert_eq!(none, None);
}

#[tokio::test]
async fn processed_shards_published_through_the_door_are_in_the_union() {
    let store = MeshStore::in_memory().unwrap();
    let base = serve(store.clone()).await;
    let changed = post(
        &base,
        "/v1/ledger/processed-shards",
        json!({ "corpus_id": "c", "node_id": NodeId::from_u128(DAEMON), "shards": [0, 2] }),
    )
    .await;
    assert_eq!(changed, json!(true));
    let union: BTreeSet<usize> = serde_json::from_value(
        post(
            &base,
            "/v1/ledger/processed-shards/union",
            json!({ "corpus_id": "c" }),
        )
        .await,
    )
    .unwrap();
    assert_eq!(union, BTreeSet::from([0, 2]));
    assert_eq!(
        commonwealth_state::union_processed_shards(&store, "c"),
        union
    );
}

#[tokio::test]
async fn every_inference_door_round_trips() {
    let store = MeshStore::in_memory().unwrap();
    let base = serve(store).await;
    let me = NodeId::from_u128(DAEMON);

    assert_eq!(get(&base, "/v1/ledger/inference/plan").await, Value::Null);
    post(
        &base,
        "/v1/ledger/inference/plan",
        json!({ "node_id": me, "record": InferencePlan::default() }),
    )
    .await;
    let plan: Option<InferencePlan> =
        serde_json::from_value(get(&base, "/v1/ledger/inference/plan").await).unwrap();
    assert!(plan.is_some_and(|p| p.model_plans.is_empty()));

    post(
        &base,
        "/v1/ledger/inference/model",
        json!({ "node_id": me, "record": model(1) }),
    )
    .await;
    let models: Vec<(NodeId, ModelInfo)> =
        serde_json::from_value(get(&base, "/v1/ledger/inference/models").await).unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].0, me, "the row keeps the daemon's origin");
    assert_eq!(models[0].1.id, ModelId::from_u128(1));
    let one: Option<ModelInfo> = serde_json::from_value(
        post(
            &base,
            "/v1/ledger/inference/model/get",
            json!({ "model_id": ModelId::from_u128(1) }),
        )
        .await,
    )
    .unwrap();
    assert_eq!(one.map(|m| m.id), Some(ModelId::from_u128(1)));
    let removed = post(
        &base,
        "/v1/ledger/inference/model/remove",
        json!({ "node_id": me, "model_id": ModelId::from_u128(1) }),
    )
    .await;
    assert_eq!(removed, json!(true));
    assert_eq!(get(&base, "/v1/ledger/inference/models").await, json!([]));

    post(
        &base,
        "/v1/ledger/inference/llama-address",
        json!({ "node_id": me, "model_id": ModelId::from_u128(2), "addr": "127.0.0.1:8080" }),
    )
    .await;
    let addr = post(
        &base,
        "/v1/ledger/inference/llama-address/get",
        json!({ "model_id": ModelId::from_u128(2) }),
    )
    .await;
    assert_eq!(addr, json!("127.0.0.1:8080"));

    assert_eq!(
        get(&base, "/v1/ledger/inference/embed-model").await,
        Value::Null
    );
    let embed = EmbedModelInfo {
        model_id: "e".into(),
        dimensions: 8,
        pooling: PoolingStrategy::Mean,
        normalization: NormalizationStrategy::Server,
        query_instruction_prefix: String::new(),
    };
    post(
        &base,
        "/v1/ledger/inference/embed-model",
        json!({ "node_id": me, "record": embed }),
    )
    .await;
    let back: Option<EmbedModelInfo> =
        serde_json::from_value(get(&base, "/v1/ledger/inference/embed-model").await).unwrap();
    assert_eq!(back, Some(embed));
}
