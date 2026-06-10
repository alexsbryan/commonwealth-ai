// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pull-based collaborative ingest work queue.
//!
//! Two halves:
//!
//! 1. **Legacy push receiver** — `corpus_ingest_partition` accepts a
//!    statically-assigned partition from the collaborate coordinator
//!    (the `SOVEREIGN_USE_LEGACY_PARTITION=1` path).
//!
//! 2. **Pull-mode queue** — `corpus_next_unit`, `corpus_heartbeat`,
//!    and `corpus_complete_unit` form the lease/heartbeat/complete
//!    state machine that peers drive against the merge leader. The
//!    helpers `find_local_handoff_for_corpus` and `spawn_queue_merge`
//!    are exposed `pub(super)` so the coordinator (`corpus_collaborate`)
//!    can drive merges after the last unit completes.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use commonwealth_core::ids::NodeId;
use commonwealth_core::knowledge::IngestionHandoff;
use commonwealth_knowledge::shard_manager::ShardManager;

use crate::state::AppState;

use super::{IngestPartitionRequest, IngestPartitionResponse};

/// One ControlPlane base URL per peer (best transport candidate),
/// for `ShardManager::coordinate_merge`'s shard pulls. Resolved
/// through the PeerTransport seam; contacts are snapshotted out of
/// the mesh lock before resolving so the lock never spans an await.
async fn peer_control_urls(state: &AppState, local_node_id: NodeId) -> Vec<(NodeId, String)> {
    let contacts: Vec<commonwealth_transport::PeerContact> = {
        let mesh = state.inner.mesh.read().await;
        mesh.members
            .values()
            .filter(|m| m.node_id != local_node_id)
            .map(commonwealth_transport::peer_contact)
            .collect()
    };
    let transport = state.peer_transport();
    let mut urls = Vec::with_capacity(contacts.len());
    for contact in &contacts {
        if let Some(ep) = transport
            .endpoints(contact, commonwealth_transport::TrafficClass::ControlPlane)
            .await
            .into_iter()
            .next()
        {
            urls.push((contact.node_id, ep.base_url));
        }
    }
    urls
}

pub async fn corpus_ingest_partition(
    State(state): State<AppState>,
    Json(req): Json<IngestPartitionRequest>,
) -> (StatusCode, Json<IngestPartitionResponse>) {
    let _engine = match &state.inner.corpus_engine {
        Some(e) => e.clone(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(IngestPartitionResponse {
                    accepted: false,
                    reason: Some("no corpus engine available on this node".into()),
                }),
            );
        }
    };

    // Validate embed model compatibility. If no embed model info is stored,
    // this node hasn't completed bootstrap (or has no embed model configured)
    // and cannot safely accept a partition — return 503 so the coordinator
    // skips us rather than assigning work we can't do.
    let Some(local_embed_model) = state.inner.inference_store.get_local_embed_model() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(IngestPartitionResponse {
                accepted: false,
                reason: Some(
                    "embed model not configured on this node — cannot accept partition".into(),
                ),
            }),
        );
    };

    if local_embed_model != req.embed_model {
        tracing::warn!(
            local = ?local_embed_model,
            requested = ?req.embed_model,
            "ingest_partition: embed model mismatch — refusing"
        );
        return (
            StatusCode::CONFLICT,
            Json(IngestPartitionResponse {
                accepted: false,
                reason: Some(format!(
                    "embed model mismatch: local={} requested={}",
                    local_embed_model.model_id, req.embed_model.model_id
                )),
            }),
        );
    }

    let corpus_id = req.corpus_id.clone();
    let recipe_id = req.recipe_id.clone();
    let file_indices = req.file_indices.clone();
    let article_range = req.article_range;
    let handoff_id = req.handoff_id;
    let local_node_id = *state.inner.self_node_id_swap.load_full().as_ref();
    let engine = _engine.clone();
    let mesh_store = Arc::clone(&state.inner.mesh_store);
    let state_clone = state.clone();
    // Snapshot peer base URLs now — we can't hold the mesh lock across an async task.
    let peer_urls: Vec<(NodeId, String)> = peer_control_urls(&state, local_node_id).await;

    // Guard: insert into active_ingests BEFORE spawning so there is no
    // window between the 202 response and the task's first async yield
    // where a concurrent call (another ingest_partition request or the
    // auto-collaborate loop's spawn_local_ingest) could race past the
    // guard and start a second task on the same LanceDB partition.  Two
    // concurrent tasks writing the same partition double GPU memory
    // pressure and reliably trigger the Metal backend's
    // `ggml_metal_buffer_set_tensor: buf_src = NULL` abort.
    {
        let mut active = state.inner.active_ingests.write().await;
        if active.contains(&corpus_id) {
            tracing::info!(
                corpus = %corpus_id,
                handoff = %handoff_id,
                "ingest_partition: corpus already active — refusing duplicate task"
            );
            return (
                StatusCode::CONFLICT,
                Json(IngestPartitionResponse {
                    accepted: false,
                    reason: Some(format!(
                        "ingest for corpus '{}' is already running on this node",
                        corpus_id
                    )),
                }),
            );
        }
        active.insert(corpus_id.clone());
    }

    let output_path = engine
        .index_dir()
        .join(format!("{corpus_id}-partition-{local_node_id}"));

    tracing::info!(
        corpus = %corpus_id,
        recipe = %recipe_id,
        handoff = %handoff_id,
        files = file_indices.len(),
        output = %output_path.display(),
        "ingest_partition: starting ingestion for assigned files"
    );

    // Spawn ingestion asynchronously — 202 Accepted returns immediately.
    // Legacy static-partition path: no work-queue unit_id to stamp chunks
    // with. Pull-based peers invoke `ingest_with_overrides` directly with
    // `Some(unit_id)` from their own pull loop (see sovereign-mesh::auto_ingest).
    tokio::spawn(async move {
        let ingest_result = engine
            .ingest_with_overrides(
                &recipe_id,
                Some(file_indices),
                article_range,
                &output_path,
                None,
                None,
            )
            .await;
        state_clone
            .inner
            .active_ingests
            .write()
            .await
            .remove(&corpus_id);

        match ingest_result {
            Ok(result) => {
                tracing::info!(
                    corpus = %corpus_id,
                    handoff = %handoff_id,
                    chunks = result.chunks_created,
                    "ingest_partition: complete — triggering merge check"
                );
                // Attach the daemon-scoped emitter so successful
                // `fetch_remote_shard` calls write `ShardTransferred`
                // events to the dimensional ledger. Without this, the
                // merge step is invisible to peers (only the merge
                // leader observes pull completion). The emit is
                // attributed to the peer that shipped the bytes —
                // see `aggregate` for the pull-emission convention.
                let shard_mgr = ShardManager::new(
                    Arc::clone(&engine),
                    engine.index_dir().to_path_buf(),
                    mesh_store,
                )
                .with_emitter(state_clone.inner.contribution_emitter.clone());
                match shard_mgr
                    .coordinate_merge(handoff_id, local_node_id, &peer_urls)
                    .await
                {
                    Ok(Some(info)) => tracing::info!(
                        corpus = %corpus_id,
                        chunks = info.chunk_count,
                        "ingest_partition: merge complete"
                    ),
                    Ok(None) => tracing::info!(
                        corpus = %corpus_id,
                        "ingest_partition: not merge leader — leader will complete merge"
                    ),
                    Err(e) => tracing::error!(
                        corpus = %corpus_id,
                        error = %e,
                        "ingest_partition: merge failed"
                    ),
                }
            }
            Err(e) => tracing::error!(
                corpus = %corpus_id,
                handoff = %handoff_id,
                error = %e,
                "ingest_partition: failed"
            ),
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(IngestPartitionResponse {
            accepted: true,
            reason: None,
        }),
    )
}

// ── Pull-based work queue endpoints ────────────────────────
//
// Replaces the fire-and-forget `ingest_partition` dispatch with a
// coordinator-held queue that peers pull from one unit at a time.
// See `commonwealth-knowledge::work_queue` for the full design.
// Peers call these three endpoints in a loop:
//   1. `next_unit` — lease the next unit; 204 when queue drained
//   2. `heartbeat` — refresh lease every LEASE_MS / 3 while ingesting
//   3. `complete_unit` — report outcome (Complete or Failed)

/// POST /internal/corpus/next_unit — lease the next work unit.
pub async fn corpus_next_unit(
    State(state): State<AppState>,
    Json(req): Json<NextUnitRequest>,
) -> (StatusCode, Json<NextUnitResponse>) {
    use commonwealth_knowledge::QueueError;
    let handoff_id = req.handoff_id;
    let peer_id = req.peer_id;
    match state
        .inner
        .work_queue
        .next_unit(&req.handoff_id, req.peer_id)
        .await
    {
        Ok(Some(leased)) => {
            tracing::info!(
                handoff = %handoff_id,
                peer = %peer_id,
                unit_id = leased.unit_id,
                lease_expires_at_ms = leased.lease_expires_at_ms,
                "next_unit: leased unit to peer"
            );
            (
                StatusCode::OK,
                Json(NextUnitResponse::Leased {
                    unit_id: leased.unit_id,
                    unit: leased.unit,
                    lease_expires_at_ms: leased.lease_expires_at_ms,
                }),
            )
        }
        Ok(None) => {
            // Queue empty — include the current phase so the peer can
            // distinguish "wait, more work is coming (Draining / Open)"
            // from "done, move to merge (Merging / Complete)".
            let phase = state
                .inner
                .work_queue
                .snapshot(&req.handoff_id)
                .await
                .map(|q| q.phase)
                .unwrap_or(commonwealth_core::knowledge::HandoffPhase::Complete);
            (
                StatusCode::NO_CONTENT,
                Json(NextUnitResponse::Empty { phase }),
            )
        }
        Err(QueueError::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(NextUnitResponse::Error {
                error: format!("no queue registered for handoff {}", req.handoff_id),
            }),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(NextUnitResponse::Error {
                error: format!("queue error: {e:?}"),
            }),
        ),
    }
}

/// POST /internal/corpus/heartbeat — extend a lease.
///
/// Returns 410 Gone when the reaper reclaimed the lease (peer must
/// abort the in-flight unit via its `CancellationFlag`).
pub async fn corpus_heartbeat(
    State(state): State<AppState>,
    Json(req): Json<HeartbeatRequest>,
) -> (StatusCode, Json<HeartbeatResponseBody>) {
    use commonwealth_knowledge::{HeartbeatResult, QueueError};
    let handoff_id = req.handoff_id;
    let peer_id = req.peer_id;
    let unit_id = req.unit_id;
    match state
        .inner
        .work_queue
        .heartbeat(&req.handoff_id, req.peer_id, req.unit_id)
        .await
    {
        Ok(HeartbeatResult::Renewed { expires_at_ms }) => {
            tracing::debug!(
                handoff = %handoff_id,
                peer = %peer_id,
                unit_id = unit_id,
                "heartbeat: lease renewed"
            );
            (
                StatusCode::OK,
                Json(HeartbeatResponseBody::Renewed {
                    lease_expires_at_ms: expires_at_ms,
                }),
            )
        }
        Ok(HeartbeatResult::Reclaimed) => {
            tracing::warn!(
                handoff = %handoff_id,
                peer = %peer_id,
                unit_id = unit_id,
                "heartbeat: lease reclaimed — peer must abort"
            );
            (
                StatusCode::GONE,
                Json(HeartbeatResponseBody::Reclaimed {
                    reason: "lease was reclaimed; abort current unit".into(),
                }),
            )
        }
        Err(QueueError::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(HeartbeatResponseBody::Reclaimed {
                reason: "handoff not found".into(),
            }),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(HeartbeatResponseBody::Reclaimed {
                reason: format!("queue error: {e:?}"),
            }),
        ),
    }
}

/// POST /internal/corpus/complete_unit — report unit outcome.
///
/// Returns 409 Conflict when the peer no longer holds the lease
/// (the reaper already requeued it; the peer's local output may
/// overlap with another peer's work — the merge step dedupes by
/// content_hash + unit_id).
///
/// **Phase-transition wiring:** when `complete_unit` returns
/// `HandoffPhase::Merging`, this is the last unit landing on the
/// coordinator — the queue is fully drained and ready to merge.
/// `WorkQueueManager::complete_unit` left this wiring as "the actual
/// transition to Merging wakes the merge task — that wiring lives in
/// the caller" (see `work_queue.rs:354-355`); without it, queue-mode
/// collaborative ingests stall here forever, with peers logging
/// "deferring index build to merge leader" and the leader's daemon
/// quietly forgetting to do it. Spawn `coordinate_merge` so the
/// 200 returns immediately and the merge runs in the background.
pub async fn corpus_complete_unit(
    State(state): State<AppState>,
    Json(req): Json<CompleteUnitRequest>,
) -> (StatusCode, Json<CompleteUnitResponse>) {
    use commonwealth_core::knowledge::{CompleteOutcome, HandoffPhase};
    use commonwealth_knowledge::QueueError;
    let handoff_id = req.handoff_id;
    let peer_id = req.peer_id;
    let unit_id = req.unit_id;
    let outcome_dbg = format!("{:?}", req.outcome);
    match state
        .inner
        .work_queue
        .complete_unit(
            &req.handoff_id,
            req.peer_id,
            req.unit_id,
            match req.outcome {
                CompleteOutcome::Complete => CompleteOutcome::Complete,
                CompleteOutcome::Failed => CompleteOutcome::Failed,
            },
            req.reason.clone(),
        )
        .await
    {
        Ok(phase) => {
            tracing::info!(
                handoff = %handoff_id,
                peer = %peer_id,
                unit_id = unit_id,
                outcome = %outcome_dbg,
                phase = ?phase,
                "complete_unit: unit completed by peer"
            );
            if matches!(phase, HandoffPhase::Merging) {
                spawn_queue_merge(state.clone(), handoff_id);
            }
            (StatusCode::OK, Json(CompleteUnitResponse::Ok { phase }))
        }
        Err(QueueError::LeaseReclaimed) => (
            StatusCode::CONFLICT,
            Json(CompleteUnitResponse::Error {
                error: "lease was already reclaimed by the reaper".into(),
            }),
        ),
        Err(QueueError::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(CompleteUnitResponse::Error {
                error: format!("no queue registered for handoff {}", req.handoff_id),
            }),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(CompleteUnitResponse::Error {
                error: format!("queue error: {e:?}"),
            }),
        ),
    }
}

/// Find the local node's most recent queue-mode `IngestionHandoff`
/// blob for `corpus_id` in mesh_store, restricted to ones where this
/// node is the `merge_leader`.
///
/// Used by `corpus_collaborate` to recover a stranded queue after a
/// coordinator restart wiped its in-memory `WorkQueueManager`. The
/// blob itself survives because gossip's mesh_store replication
/// re-implants it from peers (`gossip.rs:406-494`), so the handoff_id
/// + merge_leader are still discoverable; only the live queue state
/// (units, leases, phase) is gone.
///
/// Returns `None` if no matching handoff exists or if every candidate
/// names a different node as merge leader (in which case the actual
/// leader is responsible for re-firing the merge, not us).
pub fn find_local_handoff_for_corpus(
    state: &AppState,
    corpus_id: &str,
    self_id: NodeId,
) -> Option<IngestionHandoff> {
    let entries = state
        .inner
        .mesh_store
        .scan("corpus-engine", "handoff:")
        .ok()?;
    let mut best: Option<IngestionHandoff> = None;
    for entry in entries {
        let Ok(handoff): std::result::Result<IngestionHandoff, _> =
            serde_json::from_slice(&entry.value)
        else {
            continue;
        };
        if handoff.corpus_id != corpus_id {
            continue;
        }
        if handoff.merge_leader != Some(self_id) {
            continue;
        }
        // Prefer the most-recently-updated handoff if there are
        // somehow several (e.g. a previous collaborate dispatch left
        // a stale blob alongside a fresher one).
        match &best {
            Some(b) if b.updated_at >= handoff.updated_at => {}
            _ => best = Some(handoff),
        }
    }
    best
}

/// Spawn `ShardManager::coordinate_merge` in the background after a
/// queue-mode handoff transitions to `Merging`. Runs on the
/// coordinator's daemon (the merge leader for queue-mode handoffs;
/// see `IngestionHandoff::new_queue` in routes_internal.rs:248
/// where merge_leader is set to `self_id`). Errors are logged and
/// swallowed — the response to `complete_unit` already returned 200,
/// and the operator can retry by re-issuing collaborative ingest.
pub fn spawn_queue_merge(state: AppState, handoff_id: commonwealth_core::ids::HandoffId) {
    tokio::spawn(async move {
        let engine = match state.inner.corpus_engine.as_ref() {
            Some(e) => Arc::clone(e),
            None => {
                tracing::warn!(
                    handoff = %handoff_id,
                    "complete_unit→merge: no corpus engine on coordinator — skipping merge"
                );
                return;
            }
        };
        let mesh_store = Arc::clone(&state.inner.mesh_store);
        let local_node_id = *state.inner.self_node_id_swap.load_full().as_ref();
        let peer_urls: Vec<(NodeId, String)> = peer_control_urls(&state, local_node_id).await;

        let shard_mgr = ShardManager::new(
            Arc::clone(&engine),
            engine.index_dir().to_path_buf(),
            mesh_store,
        )
        .with_emitter(state.inner.contribution_emitter.clone())
        .with_work_queue(Arc::clone(&state.inner.work_queue));

        match shard_mgr
            .coordinate_merge(handoff_id, local_node_id, &peer_urls)
            .await
        {
            Ok(Some(info)) => tracing::info!(
                handoff = %handoff_id,
                chunks = info.chunk_count,
                "complete_unit→merge: queue-mode merge complete"
            ),
            Ok(None) => tracing::info!(
                handoff = %handoff_id,
                "complete_unit→merge: not the merge leader (unexpected on coordinator) — no-op"
            ),
            Err(e) => tracing::error!(
                handoff = %handoff_id,
                error = %e,
                "complete_unit→merge: queue-mode merge failed"
            ),
        }
    });
}

// ── Pull-based queue request/response types ────────────────

#[derive(Debug, Deserialize)]
pub struct NextUnitRequest {
    pub handoff_id: commonwealth_core::ids::HandoffId,
    pub peer_id: NodeId,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum NextUnitResponse {
    Leased {
        unit_id: commonwealth_core::knowledge::UnitId,
        unit: commonwealth_core::knowledge::WorkUnit,
        lease_expires_at_ms: u64,
    },
    Empty {
        phase: commonwealth_core::knowledge::HandoffPhase,
    },
    Error {
        error: String,
    },
}

#[derive(Debug, Deserialize)]
pub struct HeartbeatRequest {
    pub handoff_id: commonwealth_core::ids::HandoffId,
    pub peer_id: NodeId,
    pub unit_id: commonwealth_core::knowledge::UnitId,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum HeartbeatResponseBody {
    Renewed { lease_expires_at_ms: u64 },
    Reclaimed { reason: String },
}

#[derive(Debug, Deserialize)]
pub struct CompleteUnitRequest {
    pub handoff_id: commonwealth_core::ids::HandoffId,
    pub peer_id: NodeId,
    pub unit_id: commonwealth_core::knowledge::UnitId,
    pub outcome: commonwealth_core::knowledge::CompleteOutcome,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum CompleteUnitResponse {
    Ok {
        phase: commonwealth_core::knowledge::HandoffPhase,
    },
    Error {
        error: String,
    },
}
