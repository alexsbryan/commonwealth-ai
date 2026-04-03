use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// POST /internal/gossip — member state exchange.
pub async fn gossip(
    State(_state): State<AppState>,
    Json(_payload): Json<serde_json::Value>,
) -> (StatusCode, Json<serde_json::Value>) {
    // Gossip exchange will be fully wired in when the gossip transport
    // is integrated. For now, accept and acknowledge.
    (
        StatusCode::OK,
        Json(serde_json::json!({ "status": "accepted" })),
    )
}

/// POST /internal/scheduling/intent — scheduling lock acquisition.
pub async fn scheduling_intent(
    State(_state): State<AppState>,
    Json(_payload): Json<SchedulingIntent>,
) -> (StatusCode, Json<SchedulingIntentResponse>) {
    (
        StatusCode::OK,
        Json(SchedulingIntentResponse {
            granted: true,
            leader: String::new(),
        }),
    )
}

/// POST /internal/scheduling/plan — shard plan broadcast.
pub async fn scheduling_plan(
    State(_state): State<AppState>,
    Json(_payload): Json<serde_json::Value>,
) -> StatusCode {
    // Plan broadcast will be applied by the local orchestrator.
    StatusCode::OK
}

/// POST /internal/model/transfer — peer-to-peer model file transfer.
pub async fn model_transfer(
    State(_state): State<AppState>,
    Json(_payload): Json<serde_json::Value>,
) -> StatusCode {
    // Model transfer is implemented in Phase 13 (Mesh Peering).
    StatusCode::NOT_IMPLEMENTED
}

/// POST /internal/index/transfer — peer-to-peer corpus index transfer.
pub async fn index_transfer(
    State(_state): State<AppState>,
    Json(_payload): Json<serde_json::Value>,
) -> StatusCode {
    // Index transfer is implemented in Phase 11 (Knowledge Subsystem).
    StatusCode::NOT_IMPLEMENTED
}

/// POST /internal/knowledge/search — inter-node shard query (fan-out target).
pub async fn knowledge_search(
    State(_state): State<AppState>,
    Json(_payload): Json<serde_json::Value>,
) -> (StatusCode, Json<serde_json::Value>) {
    // Knowledge search is implemented in Phase 11.
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({ "error": "knowledge search not yet implemented" })),
    )
}

/// GET /internal/latency/probe — RTT measurement endpoint.
pub async fn latency_probe() -> StatusCode {
    // Simply responding proves the node is reachable.
    // The real latency measurement uses UDP (in latency_probe module).
    StatusCode::OK
}

#[derive(Debug, Deserialize)]
pub struct SchedulingIntent {
    pub node_id: String,
    pub intent: String,
}

#[derive(Debug, Serialize)]
pub struct SchedulingIntentResponse {
    pub granted: bool,
    pub leader: String,
}
