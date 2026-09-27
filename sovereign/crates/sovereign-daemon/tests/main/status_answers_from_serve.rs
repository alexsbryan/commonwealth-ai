// SPDX-License-Identifier: AGPL-3.0-or-later
//! On the dialing path `/status` lists the models serve holds, from this
//! node's own manifest and with no ledger read, and no alias row
//! (pb-svrn-dials-serve; seat, reviewing c0c39be03).
//!
//! The ledger here holds a model serve does not, planned as loaded: a
//! `/status` that read the ledger would list it. The alias map is the one the
//! daemon publishes on the dialing path, from serve's residency
//! (`serve_client::served_slot_aliases`).

use std::collections::HashMap;
use std::sync::Arc;

use commonwealth_core::ids::{MeshId, ModelId, NodeId};
use commonwealth_core::mesh::Mesh;
use commonwealth_core::model::{ModelArchitecture, ModelInfo};
use serde_json::Value;
use sovereign_contracts::engine_state::ServedSelf;
use sovereign_contracts::oicp::ResidentSlot;
use sovereign_daemon::serve_client::{
    loopback_provider, served_slot_aliases, ServeBase, ServeBaseSource,
};
use sovereign_daemon::server::client_router;
use sovereign_daemon::slot_manifest::CoreSlotManifest;
use sovereign_daemon::state::{AppState, LocalInferenceService, ServingSeed};
use sovereign_mesh::inference_adapter::SovereignInferenceAdapter;
use sovereign_mesh::ledger_port::{InferencePlan, ShardPlan};

use crate::common::{member_with_last_seen, spawn_router};

fn slot(role: &str, model: &str) -> ResidentSlot {
    ResidentSlot {
        role: role.into(),
        model_id: model.into(),
        resident: true,
        size_bytes: None,
        transitioning: false,
        placement: None,
    }
}

fn served() -> ServedSelf {
    ServedSelf {
        primary_model: "big".into(),
        medium_model: "big".into(),
        fast_model: "small".into(),
        embed_model: "emb".into(),
        resident_slots: vec![
            slot("primary", "big"),
            slot("fast", "small"),
            slot("embed", "emb"),
        ],
        context_size: Some(8192),
        ..ServedSelf::default()
    }
}

fn dialing_node() -> AppState {
    let id = NodeId::from_u128(0x3333 << 64);
    let mut members = HashMap::new();
    members.insert(
        id,
        member_with_last_seen(id, "a", 100, "127.0.0.1:9742".parse().unwrap()),
    );
    let mesh = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(43),
        name: "status-from-serve".into(),
        invite_key_hash: [7u8; 32],
        invite_version: 0,
        require_encryption: false,
        members,
        peers: vec![],
    };
    let serve = ServeBase {
        base: "http://127.0.0.1:1".into(),
        source: ServeBaseSource::Default,
    };
    let arm = Arc::new(loopback_provider(&serve, served(), 4096));
    let adapter: Arc<dyn LocalInferenceService> = Arc::new(SovereignInferenceAdapter::new(
        arm,
        Arc::new(CoreSlotManifest),
    ));
    let state = AppState::new_with_serving(
        id,
        mesh,
        ServingSeed {
            local_inference: Some(adapter),
            ..Default::default()
        },
    );
    state
        .slot_aliases_reader()
        .publish(served_slot_aliases(&served().resident_slots));
    state
}

/// A model the ledger plans as loaded and serve does not hold.
async fn seed_ledger_only_model(state: &AppState) {
    let id = ModelId::from_u128(0xdead);
    state
        .register_model(ModelInfo {
            id,
            name: "ledger-only".into(),
            repo: String::new(),
            file: "ledger-only.gguf".into(),
            size_bytes: 0,
            total_layers: 0,
            architecture: ModelArchitecture::Other,
            available_on: HashMap::new(),
            oicp_capabilities: Default::default(),
            quantization: String::new(),
            min_memory_gb: 0,
            preferred_memory_gb: 0,
            supports_parallel_instances: false,
            supports_pipeline_shard: false,
        })
        .await
        .expect("ledger row");
    state
        .set_inference_plan(&InferencePlan {
            model_plans: vec![ShardPlan {
                model: id,
                entry_node: NodeId::from_u128(0x3333 << 64),
                assignments: vec![],
                estimated_tokens_per_sec: 1.0,
                estimated_ttft_ms: 1,
            }],
        })
        .await
        .expect("ledger plan");
}

#[tokio::test]
async fn status_lists_serves_models_without_the_ledger_or_an_alias_row() {
    let state = dialing_node();
    seed_ledger_only_model(&state).await;
    let a = spawn_router(client_router(state)).await;
    let status: Value = reqwest::get(format!("http://{a}/status"))
        .await
        .expect("status")
        .json()
        .await
        .expect("a status body");
    let mut models: Vec<String> = status["inference"]["loaded_models"]
        .as_array()
        .expect("loaded_models")
        .iter()
        .filter_map(|m| m["model"].as_str().map(str::to_string))
        .collect();
    models.sort();
    assert!(
        !models.iter().any(|m| m == "ledger-only"),
        "/status read the ledger: {models:?}"
    );
    assert!(
        models.iter().any(|m| m == "big") && models.iter().any(|m| m == "small"),
        "/status must list the models serve holds: {models:?}"
    );
    // serve's manifest also advertises `primary`, `commonwealth/fast`, ...;
    // every row must be a model serve holds, never one of those aliases.
    let held = ["big", "small", "emb"];
    assert!(
        models.iter().all(|m| held.contains(&m.as_str())),
        "an alias is a row of its own: {models:?}"
    );
}
