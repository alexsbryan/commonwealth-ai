// SPDX-License-Identifier: AGPL-3.0-or-later
//! The typed ledger doors over this daemon's mesh store (five-programs
//! fp-78, decision five-programs-35 option (b)).
//!
//! **The writers stay in commonwealth-state; this file only serves them.**
//! Every door constructs the existing typed writer over [`KvHost::store`]
//! (`ContributionEmitter`, `ActivityEmitter`, `PeerPreferenceStore`,
//! `InferenceStateStore`, the processed-shards key) and calls the one method
//! it names, so each key scheme keeps ONE decider (ARCH 8). The bodies are
//! those types' own serde shapes from commonwealth-core / commonwealth-state —
//! no twin struct on either side of the wire.
//!
//! **The writer's node id rides in the body**, the way `KvSetBody::origin`
//! does: rails holds its own key, and the daemon's rows must keep the
//! daemon's attribution (an emitter built with rails' id would re-attribute
//! every contribution). The set is the daemon's call set and no more — a
//! method nothing calls has no door.
//!
//! Every door is POST with a JSON body except the no-argument reads, so a
//! typed id never has to survive a query-string round trip.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use host_kit::shell::RouteBundle;
use commonwealth_core::activity::ActivityEventKind;
use commonwealth_core::capabilities::NodeCapabilities;
use commonwealth_core::contributions::LedgerEventKind;
use commonwealth_core::ids::{ModelId, NodeId};
use commonwealth_core::model::ModelInfo;
use commonwealth_core::oicp::EmbedModelInfo;
use commonwealth_state::inference_plan::InferencePlan;
use commonwealth_state::store_adapter::InferenceStateStore;
use commonwealth_state::{
    ActivityEmitter, ContributionEmitter, MeshStore, PeerPreference, PeerPreferenceStore,
    PROCESSED_SHARDS_APP_ID,
};
use serde::Deserialize;
use tracing::{debug, warn};

use crate::rail::err;

/// The doors' state: the store `crate::kv::KvHost` projects and pumps.
#[derive(Clone)]
pub struct LedgerDoors {
    store: MeshStore,
}

impl LedgerDoors {
    pub fn new(store: MeshStore) -> Self {
        Self { store }
    }

    fn inference(&self, node_id: NodeId) -> InferenceStateStore {
        InferenceStateStore::new(Arc::new(self.store.clone()), node_id)
    }
}

/// Every ledger door, one of [`crate::api::bundles`].
pub fn router(doors: LedgerDoors) -> RouteBundle {
    RouteBundle::new("ledger")
        .route(
            "/v1/ledger/contributions",
            get(contribution_events).post(contribution_record),
        )
        .route(
            "/v1/ledger/contributions/current",
            post(contributions_current),
        )
        .route(
            "/v1/ledger/activity",
            get(activity_events).post(activity_record),
        )
        .route("/v1/ledger/activity/current", post(activity_current))
        .route("/v1/ledger/peer-preferences", get(peer_preferences_list))
        .route("/v1/ledger/peer-preferences/get", post(peer_preference_get))
        .route("/v1/ledger/peer-preferences/set", post(peer_preference_set))
        .route(
            "/v1/ledger/peer-preferences/clear",
            post(peer_preference_clear),
        )
        .route(
            "/v1/ledger/processed-shards",
            post(processed_shards_publish),
        )
        .route(
            "/v1/ledger/processed-shards/union",
            post(processed_shards_union),
        )
        .route(
            "/v1/ledger/inference/plan",
            get(inference_plan_get).post(inference_plan_set),
        )
        .route("/v1/ledger/inference/models", get(inference_models))
        .route("/v1/ledger/inference/model/get", post(inference_model_get))
        .route("/v1/ledger/inference/model", post(inference_model_set))
        .route(
            "/v1/ledger/inference/model/remove",
            post(inference_model_remove),
        )
        .route(
            "/v1/ledger/inference/llama-address/get",
            post(inference_llama_address_get),
        )
        .route(
            "/v1/ledger/inference/llama-address",
            post(inference_llama_address_set),
        )
        .route(
            "/v1/ledger/inference/embed-model",
            get(inference_embed_model_get).post(inference_embed_model_set),
        )
        .with_state(doors)
}

fn store_error(door: &str, e: commonwealth_state::Error) -> Response {
    warn!(target: "rails", door, error = %e, "ledger: store refused");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("ledger {door}: {e}"),
    )
}

// ── Bodies: the writer's node id beside the record type ─────

#[derive(Debug, Deserialize)]
pub struct Written<T> {
    pub node_id: NodeId,
    pub record: T,
}

#[derive(Debug, Deserialize)]
pub struct CurrentContributions {
    /// Pairs rather than a map: a `NodeId` key does not have to survive
    /// being a JSON object key.
    pub peer_capabilities: Vec<(NodeId, NodeCapabilities)>,
    pub window_days: u32,
}

#[derive(Debug, Deserialize)]
pub struct WindowDays {
    pub window_days: u32,
}

#[derive(Debug, Deserialize)]
pub struct Peer {
    pub peer: NodeId,
}

#[derive(Debug, Deserialize)]
pub struct PeerPreferenceSet {
    pub node_id: NodeId,
    pub peer: NodeId,
    pub pref: PeerPreference,
}

#[derive(Debug, Deserialize)]
pub struct ProcessedShards {
    pub corpus_id: String,
    pub node_id: NodeId,
    pub shards: Vec<usize>,
}

#[derive(Debug, Deserialize)]
pub struct Corpus {
    pub corpus_id: String,
}

#[derive(Debug, Deserialize)]
pub struct Model {
    pub model_id: ModelId,
}

#[derive(Debug, Deserialize)]
pub struct NodeModel {
    pub node_id: NodeId,
    pub model_id: ModelId,
}

#[derive(Debug, Deserialize)]
pub struct LlamaAddress {
    pub node_id: NodeId,
    pub model_id: ModelId,
    pub addr: String,
}

// ── Contributions ────────────────────────────────────────────

/// POST /v1/ledger/contributions — `ContributionEmitter::record`. A store
/// failure is the emitter's own `warn`, exactly as in-process.
async fn contribution_record(
    State(d): State<LedgerDoors>,
    Json(body): Json<Written<LedgerEventKind>>,
) -> Json<()> {
    debug!(target: "rails", node = %body.node_id, "ledger: contribution recorded");
    ContributionEmitter::new(d.store.clone(), body.node_id).record(body.record);
    Json(())
}

/// GET /v1/ledger/contributions — `ContributionEmitter::events`.
async fn contribution_events(State(d): State<LedgerDoors>) -> Response {
    // The node id is not read by `events`; any id reads the same scan.
    match ContributionEmitter::new(d.store.clone(), NodeId::from_u128(0)).events() {
        Ok(events) => Json(events).into_response(),
        Err(e) => store_error("contributions", e),
    }
}

/// POST /v1/ledger/contributions/current — `current_contributions`.
async fn contributions_current(
    State(d): State<LedgerDoors>,
    Json(body): Json<CurrentContributions>,
) -> Response {
    let caps: HashMap<NodeId, NodeCapabilities> = body.peer_capabilities.into_iter().collect();
    match commonwealth_state::current_contributions(&d.store, &caps, body.window_days) {
        Ok(map) => Json(map.into_iter().collect::<Vec<_>>()).into_response(),
        Err(e) => store_error("contributions/current", e),
    }
}

// ── Activity ─────────────────────────────────────────────────

/// POST /v1/ledger/activity — `ActivityEmitter::record`.
async fn activity_record(
    State(d): State<LedgerDoors>,
    Json(body): Json<Written<ActivityEventKind>>,
) -> Json<()> {
    debug!(target: "rails", node = %body.node_id, "ledger: activity recorded");
    ActivityEmitter::new(d.store.clone(), body.node_id).record(body.record);
    Json(())
}

/// GET /v1/ledger/activity — `ActivityEmitter::events`.
async fn activity_events(State(d): State<LedgerDoors>) -> Response {
    // The node id is not read by `events`; any id reads the same scan.
    match ActivityEmitter::new(d.store.clone(), NodeId::from_u128(0)).events() {
        Ok(events) => Json(events).into_response(),
        Err(e) => store_error("activity", e),
    }
}

/// POST /v1/ledger/activity/current — `current_activity`.
async fn activity_current(State(d): State<LedgerDoors>, Json(body): Json<WindowDays>) -> Response {
    match commonwealth_state::current_activity(&d.store, body.window_days) {
        Ok(summary) => Json(summary).into_response(),
        Err(e) => store_error("activity/current", e),
    }
}

// ── Peer preferences ─────────────────────────────────────────

fn preferences(d: &LedgerDoors) -> PeerPreferenceStore {
    // `list`, `get` and `clear` never stamp the node id; `set` builds its
    // own store with the writer's.
    PeerPreferenceStore::new(d.store.clone(), NodeId::from_u128(0))
}

/// GET /v1/ledger/peer-preferences — `PeerPreferenceStore::list`.
async fn peer_preferences_list(State(d): State<LedgerDoors>) -> Response {
    match preferences(&d).list() {
        Ok(entries) => Json(entries).into_response(),
        Err(e) => store_error("peer-preferences", e),
    }
}

/// POST /v1/ledger/peer-preferences/get — `PeerPreferenceStore::get`.
async fn peer_preference_get(State(d): State<LedgerDoors>, Json(body): Json<Peer>) -> Response {
    match preferences(&d).get(&body.peer) {
        Ok(pref) => Json(pref).into_response(),
        Err(e) => store_error("peer-preferences/get", e),
    }
}

/// POST /v1/ledger/peer-preferences/set — `PeerPreferenceStore::set`,
/// stamped with the writer's node id.
async fn peer_preference_set(
    State(d): State<LedgerDoors>,
    Json(body): Json<PeerPreferenceSet>,
) -> Response {
    debug!(target: "rails", node = %body.node_id, "ledger: peer preference set");
    match PeerPreferenceStore::new(d.store.clone(), body.node_id).set(&body.peer, body.pref) {
        Ok(()) => Json(()).into_response(),
        Err(e) => store_error("peer-preferences/set", e),
    }
}

/// POST /v1/ledger/peer-preferences/clear — `PeerPreferenceStore::clear`;
/// answers whether a preference was there.
async fn peer_preference_clear(State(d): State<LedgerDoors>, Json(body): Json<Peer>) -> Response {
    match preferences(&d).clear(&body.peer) {
        Ok(cleared) => Json(cleared).into_response(),
        Err(e) => store_error("peer-preferences/clear", e),
    }
}

// ── Processed shards ─────────────────────────────────────────

/// POST /v1/ledger/processed-shards — this node's processed set for one
/// corpus, under `processed_shards_key`, the value the daemon wrote in
/// process (the JSON array). Answers whether the stored value changed.
async fn processed_shards_publish(
    State(d): State<LedgerDoors>,
    Json(body): Json<ProcessedShards>,
) -> Response {
    let key = commonwealth_state::processed_shards_key(&body.corpus_id, body.node_id);
    let payload = match serde_json::to_vec(&body.shards) {
        Ok(p) => p,
        Err(e) => {
            return err(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("processed shards are not serializable: {e}"),
            )
        }
    };
    match d
        .store
        .set(PROCESSED_SHARDS_APP_ID, &key, payload.into(), body.node_id)
    {
        Ok(changed) => Json(changed).into_response(),
        Err(e) => store_error("processed-shards", e),
    }
}

/// POST /v1/ledger/processed-shards/union — `union_processed_shards`.
async fn processed_shards_union(
    State(d): State<LedgerDoors>,
    Json(body): Json<Corpus>,
) -> Json<BTreeSet<usize>> {
    Json(commonwealth_state::union_processed_shards(
        &d.store,
        &body.corpus_id,
    ))
}

// ── Inference state ──────────────────────────────────────────
//
// `InferenceStateStore` swallows its store errors (its methods answer
// `Option`/`()`/`bool`), so these doors do too: what the daemon read in
// process is what it reads over the wire.

/// GET /v1/ledger/inference/plan — `get_plan`. Its reads never stamp the
/// node id.
async fn inference_plan_get(State(d): State<LedgerDoors>) -> Json<Option<InferencePlan>> {
    Json(d.inference(NodeId::from_u128(0)).get_plan())
}

/// POST /v1/ledger/inference/plan — `set_plan`.
async fn inference_plan_set(
    State(d): State<LedgerDoors>,
    Json(body): Json<Written<InferencePlan>>,
) -> Json<()> {
    d.inference(body.node_id).set_plan(&body.record);
    Json(())
}

/// GET /v1/ledger/inference/models — `list_models_with_origins`, the one
/// scan both `list_models` and it fold.
async fn inference_models(State(d): State<LedgerDoors>) -> Json<Vec<(NodeId, ModelInfo)>> {
    Json(d.inference(NodeId::from_u128(0)).list_models_with_origins())
}

/// POST /v1/ledger/inference/model/get — `get_model_info`.
async fn inference_model_get(
    State(d): State<LedgerDoors>,
    Json(body): Json<Model>,
) -> Json<Option<ModelInfo>> {
    Json(
        d.inference(NodeId::from_u128(0))
            .get_model_info(body.model_id),
    )
}

/// POST /v1/ledger/inference/model — `set_model_info`.
async fn inference_model_set(
    State(d): State<LedgerDoors>,
    Json(body): Json<Written<ModelInfo>>,
) -> Json<()> {
    d.inference(body.node_id).set_model_info(&body.record);
    Json(())
}

/// POST /v1/ledger/inference/model/remove — `remove_model_info`.
async fn inference_model_remove(
    State(d): State<LedgerDoors>,
    Json(body): Json<NodeModel>,
) -> Json<bool> {
    Json(d.inference(body.node_id).remove_model_info(body.model_id))
}

/// POST /v1/ledger/inference/llama-address/get — `get_llama_address`.
async fn inference_llama_address_get(
    State(d): State<LedgerDoors>,
    Json(body): Json<Model>,
) -> Json<Option<String>> {
    Json(
        d.inference(NodeId::from_u128(0))
            .get_llama_address(body.model_id),
    )
}

/// POST /v1/ledger/inference/llama-address — `set_llama_address`.
async fn inference_llama_address_set(
    State(d): State<LedgerDoors>,
    Json(body): Json<LlamaAddress>,
) -> Json<()> {
    d.inference(body.node_id)
        .set_llama_address(body.model_id, &body.addr);
    Json(())
}

/// GET /v1/ledger/inference/embed-model — `get_local_embed_model`.
async fn inference_embed_model_get(State(d): State<LedgerDoors>) -> Json<Option<EmbedModelInfo>> {
    Json(d.inference(NodeId::from_u128(0)).get_local_embed_model())
}

/// POST /v1/ledger/inference/embed-model — `set_local_embed_model`.
async fn inference_embed_model_set(
    State(d): State<LedgerDoors>,
    Json(body): Json<Written<EmbedModelInfo>>,
) -> Json<()> {
    d.inference(body.node_id)
        .set_local_embed_model(&body.record);
    Json(())
}

/// Sweep the contributions namespace on the window `commonwealth_state::
/// retention` declares — the daemon's `RetentionGc` call site, run here over
/// this store (five-programs-35: RetentionGc runs in cw-rails). Spawned by
/// [`crate::RailsDaemon::run`]; aborted with it.
pub async fn run_retention_gc(store: MeshStore) {
    let Some(gc) = commonwealth_state::RetentionGc::for_namespace(
        Arc::new(store),
        commonwealth_state::CONTRIBUTIONS_APP_ID,
        commonwealth_state::contributions::STORAGE_SNAPSHOT_INTERVAL,
    ) else {
        warn!(target: "rails", app_scope = commonwealth_state::CONTRIBUTIONS_APP_ID,
              "ledger: the contributions namespace declares no retention window, so nothing is swept");
        return;
    };
    tracing::info!(target: "rails", app_scope = commonwealth_state::CONTRIBUTIONS_APP_ID,
                   "ledger: RetentionGc started (contributions ledger)");
    // The sender lives as long as this task; abort drops both.
    let (_hold, shutdown) = tokio::sync::watch::channel(false);
    gc.run(shutdown).await;
}

#[cfg(test)]
#[path = "ledger/tests.rs"]
mod tests;
