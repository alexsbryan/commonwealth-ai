// SPDX-License-Identifier: AGPL-3.0-or-later
//! On the dialing path `/status` lists the models serve holds, from this
//! node's own manifest and with no ledger read, and no alias row
//! (pb-svrn-dials-serve; seat, reviewing c0c39be03).
//!
//! The ledger here holds a model serve does not, planned as loaded: a
//! `/status` that read the ledger would list it. The alias map is the one the
//! daemon publishes on the dialing path, from serve's residency
//! (`serve_client::served_slot_aliases`).
//!
//! After a reload the same readers name what serve holds then, because boot
//! handed them one provider cell and the reload stores into it (phase-b-28).

use std::collections::HashMap;
use std::sync::Arc;

use kernel_types::{MeshId, ModelId, NodeId};
use oicp_types::model_catalog::{ModelArchitecture, ModelInfo};
use serde_json::Value;
use sovereign_contracts::engine_state::ServedSelf;
use sovereign_contracts::oicp::ResidentSlot;
use sovereign_daemon::serve_client::{
    loopback_provider, served_slot_aliases, ServeBase, ServeBaseSource,
};
use sovereign_daemon::server::client_router;
use sovereign_daemon::state::{AppState, LocalInferenceService, ServingSeed};
use sovereign_mesh::ledger_port::{InferencePlan, ShardPlan};
use sovereign_serving_host::inference_adapter::SovereignInferenceAdapter;
use sovereign_serving_host::slot_manifest::CoreSlotManifest;

use crate::common::{member_with_last_seen, solo_mesh, spawn_router};

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
    let serve = ServeBase {
        base: "http://127.0.0.1:1".into(),
        source: ServeBaseSource::Default,
    };
    let arm = Arc::new(loopback_provider(&serve, served(), 4096));
    node_over(Arc::new(SovereignInferenceAdapter::new(
        arm,
        Arc::new(CoreSlotManifest),
    )))
}

/// A commissioned node whose local inference is `adapter`, with the alias map
/// published from `served()`.
fn node_over(adapter: Arc<dyn LocalInferenceService>) -> AppState {
    let id = NodeId::from_u128(0x3333 << 64);
    let mut members = HashMap::new();
    members.insert(
        id,
        member_with_last_seen(id, "a", 100, "127.0.0.1:9742".parse().unwrap()),
    );
    let mut mesh = solo_mesh(id, "status-from-serve");
    mesh.id = MeshId::from_u128(43);
    mesh.invite_key_hash = [7u8; 32];
    mesh.members = members;
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

/// serve after a reload: another primary and another context window.
fn served_after_reload() -> ServedSelf {
    ServedSelf {
        primary_model: "bigger".into(),
        medium_model: "bigger".into(),
        fast_model: "small".into(),
        embed_model: "emb".into(),
        resident_slots: vec![
            slot("primary", "bigger"),
            slot("fast", "small"),
            slot("embed", "emb"),
        ],
        context_size: Some(16384),
        ..ServedSelf::default()
    }
}

/// serve's reload and self-report routes: the self-report is `served()` until
/// a reload, `served_after_reload()` after it.
async fn stub_serve() -> String {
    use axum::routing::{get, post};
    use sovereign_contracts::engine_state::{EngineReloaded, RELOAD_PATH, SERVED_SELF_PATH};
    use std::sync::atomic::{AtomicBool, Ordering};
    let reloaded = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&reloaded);
    let app = axum::Router::new()
        .route(
            RELOAD_PATH,
            post(move || {
                let flag = Arc::clone(&flag);
                async move {
                    flag.store(true, Ordering::SeqCst);
                    axum::Json(EngineReloaded {
                        resident_models: vec!["bigger".into(), "small".into(), "emb".into()],
                    })
                }
            }),
        )
        .route(
            SERVED_SELF_PATH,
            get(move || {
                let reloaded = Arc::clone(&reloaded);
                async move {
                    axum::Json(if reloaded.load(Ordering::SeqCst) {
                        served_after_reload()
                    } else {
                        served()
                    })
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, app).await });
    format!("http://{addr}")
}

/// A reload on the dialing path is seen by the readers boot handed the
/// provider to, not only by the provider the reload returns (seat, reviewing
/// cfb83f03b; phase-b-28): `/v1/models`, `/status` and the peer manifest name
/// serve's new primary and no row for the old one, the aliases point at it,
/// and the adapter every turn budgets against reports the new context window.
#[tokio::test]
async fn a_reload_is_seen_by_every_reader_boot_wired() {
    use sovereign_contracts::reloadable_provider::ReloadableProvider;
    use sovereign_contracts::traits::InferenceProvider;
    use sovereign_daemon::serve_client::reload_through_serve;

    let serve = ServeBase {
        base: stub_serve().await,
        source: ServeBaseSource::Default,
    };
    let cell = Arc::new(ReloadableProvider::new(
        Arc::new(loopback_provider(&serve, served(), 4096)),
        Default::default(),
    ));
    let adapter = Arc::new(SovereignInferenceAdapter::new(
        Arc::clone(&cell) as Arc<dyn InferenceProvider>,
        Arc::new(CoreSlotManifest),
    ));
    let state = node_over(Arc::clone(&adapter) as Arc<dyn LocalInferenceService>);
    assert_eq!(adapter.effective_context_size(), Some(8192));

    let after = reload_through_serve(&serve, &cell, 4096)
        .await
        .expect("the reload reaches serve");
    state
        .slot_aliases_reader()
        .publish(served_slot_aliases(&after.resident_slots));

    let a = spawn_router(client_router(state)).await;
    let models: Value = reqwest::get(format!("http://{a}/v1/models"))
        .await
        .expect("models")
        .json()
        .await
        .expect("a models body");
    let rows = models["data"].as_array().expect("data");
    let ids: Vec<&str> = rows.iter().filter_map(|m| m["id"].as_str()).collect();
    assert!(
        ids.contains(&"bigger"),
        "/v1/models misses serve's new primary: {ids:?}"
    );
    assert!(
        !ids.contains(&"big"),
        "/v1/models names a model serve no longer holds: {ids:?}"
    );
    let primary = rows
        .iter()
        .find(|m| m["id"] == "primary")
        .expect("the primary alias row");
    assert_eq!(primary["owned_by"], "alias→bigger", "{primary}");

    let status: Value = reqwest::get(format!("http://{a}/status"))
        .await
        .expect("status")
        .json()
        .await
        .expect("a status body");
    let loaded: Vec<&str> = status["inference"]["loaded_models"]
        .as_array()
        .expect("loaded_models")
        .iter()
        .filter_map(|m| m["model"].as_str())
        .collect();
    assert!(
        loaded.contains(&"bigger") && !loaded.contains(&"big"),
        "/status names the pre-reload model: {loaded:?}"
    );

    let caps: Value = reqwest::get(format!("http://{a}/oicp/v1/capabilities"))
        .await
        .expect("capabilities")
        .json()
        .await
        .expect("a manifest body");
    let offered: Vec<&str> = caps["models"]
        .as_array()
        .expect("models")
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    assert!(
        offered.contains(&"bigger") && !offered.contains(&"big"),
        "peers are offered the pre-reload model: {offered:?}"
    );

    assert_eq!(
        adapter.effective_context_size(),
        Some(16384),
        "turns budget against the pre-reload context window"
    );
}
