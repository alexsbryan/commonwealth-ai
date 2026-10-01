// SPDX-License-Identifier: AGPL-3.0-or-later
//! Scheduling intent/plan endpoints: scheduling lock acquisition
//! (`/internal/scheduling/intent`) and shard-plan broadcast
//! (`/internal/scheduling/plan`). Gossip, which sat beside them, is cw-rails'
//! (pb-mesh-exit-transport).

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use sovereign_mesh::ledger_port::InferencePlan;

use crate::state::AppState;

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
///
/// Peer nodes call this when they compute a new inference plan.
/// The plan is stored in the mesh store and propagated via gossip.
pub async fn scheduling_plan(
    State(state): State<AppState>,
    Json(plan): Json<InferencePlan>,
) -> StatusCode {
    if let Err(e) = state.set_inference_plan(&plan).await {
        tracing::warn!(error = %e, "scheduling_plan: inference state absent; plan not stored");
        return StatusCode::SERVICE_UNAVAILABLE;
    }
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
