// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gossip exchange + scheduling intent/plan endpoints.
//!
//! These three handlers form the cluster-coordination layer of the
//! internal API: pairwise mesh-state convergence (`/internal/gossip`),
//! scheduling lock acquisition (`/internal/scheduling/intent`), and
//! shard-plan broadcast (`/internal/scheduling/plan`).

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use commonwealth_inference::inference_plan::InferencePlan;

use crate::state::AppState;

use super::MeshWire;

/// POST /internal/gossip — pairwise mesh-state exchange.
///
/// Symmetric: caller sends us their `Mesh` view, we merge it into ours
/// via `Mesh::merge_from` (per-member `last_seen` last-writer-wins),
/// and reply with our now-updated snapshot so the caller can merge it
/// in turn. After one round both sides have converged on the pairwise
/// union.
///
/// Rejects with 401 when the incoming `Mesh` has a different `mesh_id`
/// or `join_key_hash` — the auth boundary. Any member with the join
/// key can gossip freely; outsiders can't inject.
pub async fn gossip(
    State(state): State<AppState>,
    Json(req): Json<GossipRequest>,
) -> Result<Json<GossipResponse>, (StatusCode, Json<GossipRejection>)> {
    let incoming = req.mesh.into_mesh();
    let self_node_id = *state.inner.self_node_id_swap.load_full().as_ref();
    let mut mesh = state.inner.mesh.write().await;
    let report = mesh.merge_from(self_node_id, &incoming);

    if report.rejected {
        tracing::warn!("gossip: rejected — mesh_id or join_key_hash mismatch");
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(GossipRejection {
                reason: "mesh_id or join_key_hash does not match".into(),
            }),
        ));
    }

    if report.added > 0 || report.updated > 0 {
        // Info only when a NEW member was added. `updated > 0`
        // alone is the routine last_seen refresh that fires every
        // 10s — noisy heartbeat, not an event. Debug-level so
        // operators can still see it with a tighter filter.
        if report.added > 0 {
            tracing::info!(
                added = report.added,
                updated = report.updated,
                members = mesh.members.len(),
                "gossip: member added via incoming delta"
            );
        } else {
            tracing::debug!(
                updated = report.updated,
                members = mesh.members.len(),
                "gossip: merged incoming delta (last_seen refresh)"
            );
        }
        // Persist immediately on any added/updated member. The
        // gossip loop re-persists on its own cadence too, but that
        // leaves a 10s window where the founder could restart and
        // forget a newly-admitted joiner. Only fire on actual
        // deltas — no point re-writing mesh.json for a last_seen
        // bump that changed nothing structural.
        if let Some(hook) = state.inner.on_mesh_mutation.as_ref() {
            hook(&mesh, self_node_id);
        }
    }

    Ok(Json(GossipResponse {
        mesh: MeshWire::from(&*mesh),
    }))
}

#[derive(Debug, Deserialize)]
pub struct GossipRequest {
    pub mesh: MeshWire,
}

#[derive(Debug, Serialize)]
pub struct GossipResponse {
    pub mesh: MeshWire,
}

#[derive(Debug, Serialize)]
pub struct GossipRejection {
    pub reason: String,
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
///
/// Peer nodes call this when they compute a new inference plan.
/// The plan is stored in MeshStore and propagated via gossip.
pub async fn scheduling_plan(
    State(state): State<AppState>,
    Json(plan): Json<InferencePlan>,
) -> StatusCode {
    state.inner.inference_store.set_plan(&plan);
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
