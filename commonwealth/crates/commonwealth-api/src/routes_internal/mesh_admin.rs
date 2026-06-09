// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-node admin endpoints: activity reporting, runtime model slot
//! management, foreground-yield introspection, mesh quiesce flag,
//! ingest budget throttle, and the join handshake.
//!
//! These handlers cluster together because they all mutate or read
//! single-node state without touching corpus-collaborate ingestion.
//! They share no execution path with each other beyond `AppState`.
//! Tests for `node_activity` and the runtime model slot endpoints
//! live at the bottom of this file.

use std::net::SocketAddr;
#[cfg(test)]
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::Mesh;
use commonwealth_discovery::membership;

use crate::state::AppState;

use super::ErrorBody;

// ── Node activity reporting ─────────────────────────────────

/// POST /internal/node/activity — sovereign-server reports coding activity level.
///
/// sovereign-server's ActivityReporter calls this after each level transition.
/// The level maps to an inference_availability weight that gossip carries to
/// peers so the scheduler routes work away from busy nodes.
///
/// Levels: "hot" (0.20) | "warm" (0.65) | "cool" (0.85) | "idle" (1.00)
pub async fn node_activity(
    State(state): State<AppState>,
    Json(payload): Json<NodeActivityPayload>,
) -> StatusCode {
    let availability = match payload.level.as_str() {
        "hot" => 0.20_f32,
        "warm" => 0.65_f32,
        "cool" => 0.85_f32,
        _ => 1.00_f32,
    };
    tracing::info!(
        level = %payload.level,
        reason = %payload.reason,
        availability,
        "node_activity: inference_availability updated"
    );
    state.update_local_availability(availability).await;
    StatusCode::NO_CONTENT
}

#[derive(Debug, Deserialize)]
pub struct NodeActivityPayload {
    pub level: String,
    pub reason: String,
}

// ── Runtime model slot management ───────────────────────────
//
// `POST /internal/models/load` and `POST /internal/models/unload`
// let the operator add or drop an extras slot without restarting the
// daemon. `GET /internal/models/inventory` lists what's loaded right
// now. These complement the static `[models.extra]` config in
// `setup_config.toml` — config-time slots get loaded at startup,
// these endpoints layer runtime mutations on top. Implementation
// gates on `LocalInferenceService` which delegates to
// `EmbeddedLlamaCpp`'s extras lock.

#[derive(Debug, Deserialize)]
pub struct LoadModelRequest {
    /// Operator-chosen slot label. Stable identifier the operator
    /// can later pass to `/internal/models/unload`. Routing uses
    /// `model_id` (gguf file stem), not this label, but the label
    /// is what shows up in inventory and logs.
    pub slot_name: String,
    /// Absolute path to the GGUF file to load.
    pub path: std::path::PathBuf,
    /// Optional context size override. Defaults to the daemon's
    /// configured `[models].context_size` (or 16384 if unset).
    /// Provided as a request-time knob for slots that need a
    /// different KV budget than the global default.
    #[serde(default)]
    pub context_size: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct LoadModelResponse {
    /// Advertised model id (gguf file stem) that callers send in
    /// `request.model` to land on this slot.
    pub model_id: String,
    pub slot_name: String,
}

#[derive(Debug, Deserialize)]
pub struct UnloadModelRequest {
    pub slot_name: String,
}

#[derive(Debug, Serialize)]
pub struct UnloadModelResponse {
    /// Advertised model id of the slot that was unloaded, or `null`
    /// if no slot with that name was loaded.
    pub model_id: Option<String>,
    pub slot_name: String,
}

#[derive(Debug, Serialize)]
pub struct InventoryEntry {
    pub slot_name: String,
    pub model_id: String,
}

#[derive(Debug, Serialize)]
pub struct InventoryResponse {
    pub extras: Vec<InventoryEntry>,
}

#[derive(Debug, Serialize)]
pub struct WarmupResponse {
    /// Wall-clock from request received to slot ready, including
    /// the no-op fast path when the slot was already warm.
    pub latency_ms: u64,
}

/// `POST /internal/models/load` — add (or replace) an extras slot.
///
/// On success, also registers a `ModelInfo` entry in the inference
/// store so the new slot shows up on `/v1/models` immediately —
/// without this the route would route correctly (via the live
/// extras map) but the slot would be invisible to clients until
/// the next daemon restart.
pub async fn models_load(
    State(state): State<AppState>,
    Json(req): Json<LoadModelRequest>,
) -> Result<Json<LoadModelResponse>, (StatusCode, String)> {
    let Some(service) = state.inner.local_inference.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "no local inference service is bound — runtime slot \
             management is only available on daemons that own their \
             local llama.cpp provider"
                .to_string(),
        ));
    };
    let ctx_size = req.context_size.unwrap_or(16_384);
    match service
        .load_extra_slot(req.slot_name.clone(), req.path.clone(), ctx_size)
        .await
    {
        Ok(model_id) => {
            // Reflect the new slot in the inference store so
            // `/v1/models` advertises it immediately.
            register_extras_in_store(&state, &req.slot_name, &req.path, model_id.as_str());
            Ok(Json(LoadModelResponse {
                model_id,
                slot_name: req.slot_name,
            }))
        }
        Err(e) => Err((StatusCode::BAD_REQUEST, e)),
    }
}

/// Compute the deterministic `ModelId` for an extras slot. Mirrors
/// the hash recipe `sovereign-mesh::daemon::register_local_model_slots`
/// uses for the static `[models.extra]` lineup, so the runtime-
/// loaded entries hash to the same id when the operator declares
/// the same `(slot_name, path)` pair statically and dynamically.
/// Keep both paths in sync.
fn compute_extras_model_id(
    slot_name: &str,
    path: &std::path::Path,
) -> commonwealth_core::ids::ModelId {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let role = format!("extras:{slot_name}");
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    let lo = h.finish();
    let mut h = DefaultHasher::new();
    role.hash(&mut h);
    path.hash(&mut h);
    let hi = h.finish();
    commonwealth_core::ids::ModelId::from_u128((u128::from(hi) << 64) | u128::from(lo))
}

fn register_extras_in_store(
    state: &AppState,
    slot_name: &str,
    path: &std::path::Path,
    model_id_str: &str,
) {
    use commonwealth_inference::model::{ModelArchitecture, ModelInfo};
    use commonwealth_inference::oicp::CapabilityProfile;

    let id = compute_extras_model_id(slot_name, path);
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let info = ModelInfo {
        id,
        name: model_id_str.to_string(),
        repo: String::new(),
        file: file_name,
        size_bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        total_layers: 0,
        architecture: ModelArchitecture::Other,
        available_on: std::collections::HashMap::new(),
        oicp_capabilities: CapabilityProfile::default(),
        quantization: String::new(),
        min_memory_gb: 0,
        preferred_memory_gb: 0,
        supports_parallel_instances: false,
        supports_pipeline_shard: false,
    };
    state.inner.inference_store.set_model_info(&info);
}

fn deregister_extras_from_store(state: &AppState, model_id_str: &str) -> bool {
    // Look up the existing entry by advertised name, then remove
    // by ModelId. We don't have the original path here (the
    // `unload` request only carries the slot name), so we can't
    // recompute the deterministic id directly — name lookup is the
    // right path, and matches how `/v1/models` exposes the entries.
    let models = state.inner.inference_store.list_models();
    let target = models
        .into_iter()
        .find(|(_, info)| info.name == model_id_str);
    if let Some((id, _)) = target {
        state.inner.inference_store.remove_model_info(id);
        true
    } else {
        false
    }
}

/// `POST /internal/models/unload` — drop an extras slot.
///
/// On success, also drops the `ModelInfo` entry from the inference
/// store so the slot stops appearing on `/v1/models`. If the slot
/// was already absent (`model_id == None` from the service), the
/// store mutation is skipped — nothing to clean up.
pub async fn models_unload(
    State(state): State<AppState>,
    Json(req): Json<UnloadModelRequest>,
) -> Result<Json<UnloadModelResponse>, (StatusCode, String)> {
    let Some(service) = state.inner.local_inference.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "no local inference service is bound".to_string(),
        ));
    };
    match service.unload_extra_slot(&req.slot_name).await {
        Ok(Some(model_id)) => {
            deregister_extras_from_store(&state, &model_id);
            Ok(Json(UnloadModelResponse {
                model_id: Some(model_id),
                slot_name: req.slot_name,
            }))
        }
        Ok(None) => Ok(Json(UnloadModelResponse {
            model_id: None,
            slot_name: req.slot_name,
        })),
        Err(e) => Err((StatusCode::BAD_REQUEST, e)),
    }
}

/// `POST /internal/inference/warmup` — eagerly load the primary
/// chat slot so the next chat-completions request doesn't pay
/// the lazy-load tax. Idempotent. Loopback-only (mounted on the
/// `:9741` client router behind the loopback guard).
///
/// Wired into the desktop app's window-focus / chat-mount events
/// so the slot is hot by the time the user hits send. Without
/// this, every conversation that pauses past `primary_idle_secs`
/// (default 60s) re-pays the full model load on the next turn —
/// 10–20s on Metal, much worse on CPU.
pub async fn inference_warmup(
    State(state): State<AppState>,
) -> Result<Json<WarmupResponse>, (StatusCode, String)> {
    let Some(service) = state.inner.local_inference.as_ref() else {
        // Standalone Commonwealth daemon path (orchestrator-spawned
        // llama-server) — no in-process slot to warm. 200 with
        // zero latency so the desktop's fire-and-forget call
        // stays a no-op rather than a noisy error.
        return Ok(Json(WarmupResponse { latency_ms: 0 }));
    };
    let started = std::time::Instant::now();
    service
        .warmup_primary()
        .await
        .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, e))?;
    let latency_ms = started.elapsed().as_millis() as u64;
    Ok(Json(WarmupResponse { latency_ms }))
}

/// `GET /internal/models/inventory` — list the currently-loaded
/// extras lineup.
pub async fn models_inventory(State(state): State<AppState>) -> Json<InventoryResponse> {
    let Some(service) = state.inner.local_inference.as_ref() else {
        return Json(InventoryResponse { extras: Vec::new() });
    };
    let inventory = service.extras_inventory().await;
    Json(InventoryResponse {
        extras: inventory
            .into_iter()
            .map(|(slot_name, model_id)| InventoryEntry {
                slot_name,
                model_id,
            })
            .collect(),
    })
}

// ── Foreground-yield introspection ──────────────────────────
//
// Operators triaging contention on a peer node need a way to see
// whether the foreground-yield mechanism is wired and what the
// current state is — without grepping daemon.log. The
// `GET /internal/daemon/foreground_state` route returns the two
// atomics and a derived `currently_yielding` flag in one shot, so
// `curl` against the daemon is enough to confirm:
//   - that the daemon was built with yield support,
//   - that the configured window is what the operator expects, and
//   - whether ingest workers should be paused right now.
//
// The shape is deliberately small: this is a debugging endpoint,
// not a gossip-grade contract.

#[derive(Debug, Serialize)]
pub struct ForegroundStateResponse {
    /// Unix-seconds of the last `chat_completions` request seen.
    /// `0` means no foreground request has hit this daemon yet.
    pub last_active_unix_ts: i64,
    /// Configured yield window. `0` disables the feature entirely;
    /// any positive value means ingest workers will pause when a
    /// chat request lands within that many seconds.
    pub window_secs: u64,
    /// Convenience flag: `true` iff a `YieldHook` polled now would
    /// return `should_yield`. Equal to
    /// `0 < window_secs && now - last_active_unix_ts < window_secs`.
    pub currently_yielding: bool,
    /// Seconds remaining in the current yield window when one is
    /// active. `None` when not yielding.
    pub seconds_until_idle: Option<u64>,
    /// Number of corpus ingests currently registered on this node —
    /// the actual workers that would be paused. Useful to sanity-check
    /// "is yield even relevant right now".
    pub active_ingests_count: usize,
}

/// `GET /internal/daemon/foreground_state` — read-only snapshot of
/// the foreground-yield atomics. See [`ForegroundStateResponse`].
pub async fn foreground_state(State(state): State<AppState>) -> Json<ForegroundStateResponse> {
    let active_ingests_count = state.inner.active_ingests.read().await.len();
    Json(ForegroundStateResponse {
        last_active_unix_ts: state.foreground_last_active_ts(),
        window_secs: state.yield_window_secs(),
        currently_yielding: state.should_yield_to_foreground(),
        seconds_until_idle: state.seconds_until_foreground_idle(),
        active_ingests_count,
    })
}

#[derive(Debug, Serialize)]
pub struct MeshQuiesceState {
    /// True when the auto-collaborate loop is suppressed: this node
    /// will not pull peer-assigned work and will not dispatch to
    /// peers on this tick.
    pub quiesced: bool,
}

#[derive(Debug, Deserialize)]
pub struct SetMeshQuiesceRequest {
    pub quiesced: bool,
}

/// `GET /internal/mesh/quiesce` — current quiesce state.
pub async fn mesh_quiesce_get(State(state): State<AppState>) -> Json<MeshQuiesceState> {
    Json(MeshQuiesceState {
        quiesced: state.mesh_quiesced(),
    })
}

/// `POST /internal/mesh/quiesce` — flip the quiesce flag at runtime.
///
/// `quiesced=true` stops new peer-pull dispatches and discoveries on
/// the next tick; in-flight ingests already running keep going until
/// they finish or the operator pauses them via
/// `/internal/corpus/pause`. `quiesced=false` rejoins the
/// collaborate loop. Idempotent — POSTing the same value twice is
/// a no-op.
pub async fn mesh_quiesce_set(
    State(state): State<AppState>,
    Json(req): Json<SetMeshQuiesceRequest>,
) -> Json<MeshQuiesceState> {
    let prev = state.mesh_quiesced();
    state.set_mesh_quiesced(req.quiesced);
    if prev != req.quiesced {
        tracing::info!(
            quiesced = req.quiesced,
            "mesh_quiesce: flipped via /internal/mesh/quiesce"
        );
    }
    Json(MeshQuiesceState {
        quiesced: req.quiesced,
    })
}

#[derive(Debug, Serialize)]
pub struct IngestBudgetState {
    /// Throttle factor in `(0.0, 1.0]`. `1.0` = full speed (no
    /// post-batch sleep). `0.5` = duty-cycle 50% (sleep equal to
    /// each batch's wall time, halving effective throughput).
    pub throttle_factor: f32,
}

#[derive(Debug, Deserialize)]
pub struct SetIngestBudgetRequest {
    pub throttle_factor: f32,
}

/// `GET /internal/ingest/budget` — current per-batch throttle factor.
pub async fn ingest_budget_get(State(state): State<AppState>) -> Json<IngestBudgetState> {
    Json(IngestBudgetState {
        throttle_factor: state.ingest_throttle_factor(),
    })
}

/// `POST /internal/ingest/budget` — set the per-batch throttle factor.
///
/// Accepts `(0.0, 1.0]`. `0.0` is rejected (use `/internal/corpus/pause`
/// to fully stop a corpus). Values >1.0 are clamped to 1.0. Returns
/// the value actually applied so callers can confirm clamping.
pub async fn ingest_budget_set(
    State(state): State<AppState>,
    Json(req): Json<SetIngestBudgetRequest>,
) -> Result<Json<IngestBudgetState>, (StatusCode, Json<ErrorBody>)> {
    match state.set_ingest_throttle_factor(req.throttle_factor) {
        Ok(applied) => {
            tracing::info!(
                throttle_factor = applied,
                "ingest_budget: throttle factor updated"
            );
            Ok(Json(IngestBudgetState {
                throttle_factor: applied,
            }))
        }
        Err(msg) => Err((StatusCode::BAD_REQUEST, Json(ErrorBody { error: msg }))),
    }
}

// ── Storage budget ───────────────────────────────────────────
//
// User-set ceiling on how much disk Sovereign is allowed to use for
// corpus storage. The desktop's Settings → Knowledge tab is the
// primary writer. Enforcement happens in
// `sovereign-mesh::capabilities::build_local_capabilities`, which
// clamps the published `free_storage_gb` to the budget remaining —
// every existing scheduler then refuses work that would push us
// over without needing to know the budget exists.

/// Wire shape for `GET /internal/storage/budget`.
///
/// `budget_bytes = None` ⇒ no budget configured (the publish path
/// reports raw free disk and nothing is clamped). Desktop reads
/// `used_bytes` and `free_disk_bytes` to render a usage bar without
/// having to walk the index directory itself.
#[derive(Debug, Serialize)]
pub struct StorageBudgetState {
    pub budget_bytes: Option<u64>,
    pub used_bytes: u64,
    pub free_disk_bytes: u64,
    /// Suggested default for first-time setup or the "Use recommended"
    /// affordance. Computed from current free disk: target 100 GiB
    /// when the disk has at least that much breathing room (≥125 GiB
    /// free), step down to 50% of free disk for tighter machines,
    /// floor at 20 GiB. Always reported so the UI can offer the
    /// affordance even when a budget is already configured.
    pub recommended_bytes: u64,
}

#[derive(Debug, Deserialize)]
pub struct SetStorageBudgetRequest {
    /// `null` (or `0`) clears the budget. Values below 1 GiB are
    /// rejected by the AppState setter.
    pub budget_bytes: Option<u64>,
}

/// Recommended baseline budget. Aim for a capable 100 GiB when there's
/// room, otherwise scale to what the disk can comfortably give up:
/// half the current free space, never less than 20 GiB. Returns 20 GiB
/// even when the disk has less than that available — the desktop UI
/// surfaces this as a recommendation, not a hard floor, and the user
/// can still pick a smaller number explicitly.
pub fn recommended_storage_budget_bytes(free_disk_bytes: u64) -> u64 {
    const TARGET: u64 = 100 * 1_073_741_824; // 100 GiB.
    const MIN_BASELINE: u64 = 20 * 1_073_741_824; // 20 GiB.
                                                  // Want ≥25 GiB headroom above the target so we don't fill the
                                                  // disk; if the user's disk has at least 125 GiB free, recommend
                                                  // the full 100 GiB. Otherwise back off to half free, floor at
                                                  // 20 GiB so the recommendation stays meaningful on small disks.
    if free_disk_bytes >= TARGET + (25 * 1_073_741_824) {
        TARGET
    } else {
        (free_disk_bytes / 2).max(MIN_BASELINE)
    }
}

fn current_free_disk_bytes() -> u64 {
    // Aggregating across all mounted disks matches what the gossiped
    // `HardwareProfile.free_storage_gb` reports — keeping the desktop
    // UI's "X of Y free" in sync with the value the scheduler sees.
    commonwealth_discovery::hardware::read_disk_free_bytes()
}

/// `GET /internal/storage/budget` — current budget, observed usage,
/// raw free-disk total, and a recommended baseline the desktop can
/// surface as a one-click default.
pub async fn storage_budget_get(State(state): State<AppState>) -> Json<StorageBudgetState> {
    let free_disk_bytes = current_free_disk_bytes();
    Json(StorageBudgetState {
        budget_bytes: state.storage_budget_bytes(),
        used_bytes: state.storage_used_bytes(),
        free_disk_bytes,
        recommended_bytes: recommended_storage_budget_bytes(free_disk_bytes),
    })
}

/// `POST /internal/storage/budget` — set or clear the budget.
///
/// Pass `{ "budget_bytes": null }` to clear (gossip then reports raw
/// free disk). Pass `{ "budget_bytes": <≥ 1 GiB> }` to set. The
/// AppState setter rejects anything tighter than 1 GiB to keep the
/// scheduler from refusing work the moment a single index file is
/// written.
pub async fn storage_budget_set(
    State(state): State<AppState>,
    Json(req): Json<SetStorageBudgetRequest>,
) -> Result<Json<StorageBudgetState>, (StatusCode, Json<ErrorBody>)> {
    if let Err(msg) = state.set_storage_budget_bytes(req.budget_bytes) {
        return Err((StatusCode::BAD_REQUEST, Json(ErrorBody { error: msg })));
    }
    let free_disk_bytes = current_free_disk_bytes();
    let applied = state.storage_budget_bytes();
    tracing::info!(
        budget_bytes = ?applied,
        used_bytes = state.storage_used_bytes(),
        free_disk_bytes,
        "storage_budget: budget updated"
    );
    Ok(Json(StorageBudgetState {
        budget_bytes: applied,
        used_bytes: state.storage_used_bytes(),
        free_disk_bytes,
        recommended_bytes: recommended_storage_budget_bytes(free_disk_bytes),
    }))
}

// ── Mesh join handshake ─────────────────────────────────────
//
// The founder (or any existing member) receives a POST from a
// would-be joiner carrying the raw `join_key`. We BLAKE3-hash it and
// compare against `mesh.join_key_hash`; on match we append the new
// member and return the full mesh snapshot so the joiner can adopt
// it locally. On mismatch we return 401 — the joiner treats this as
// "wrong mesh, try the next mDNS candidate" and moves on.
//
// Security posture (v1):
//   - Plain HTTP on the LAN. The join_key is exposed in transit to
//     anyone sniffing the local network; acceptable under the same
//     trust model as "I shared this link in a trusted chat".
//   - mesh_id in mDNS TXT is public (not secret); knowing it does
//     not grant membership. Only the raw key does, and it's hashed
//     at rest via `Mesh::join_key_hash`.
//   - Timing-attack-resistant equality lives in `membership::verify_join_key`.

#[derive(Debug, Deserialize)]
pub struct JoinRequest {
    pub join_key: String,
    pub joining_node_name: String,
    pub joining_node_addresses: Vec<SocketAddr>,
    /// Stable `NodeId` the joiner persists at
    /// `<data_dir>/node_id`. When present and not already claimed
    /// under a different name, the founder admits the joiner under
    /// this exact ID so rejoins don't leave zombies.
    ///
    /// Backward-compatible: older joiners don't send this field;
    /// `#[serde(default)]` makes the founder accept those requests
    /// unchanged.
    #[serde(default)]
    pub proposed_node_id: Option<NodeId>,
}

/// Wire shape for the full mesh snapshot. The Rust `Mesh` stores
/// members as `HashMap<NodeId, MemberRecord>`; JSON requires object
/// keys be strings, and `NodeId` serialises as a byte-array by
/// default — which crashes `serde_json` with "key must be a string".
/// We flatten to a Vec at the transport boundary, then reassemble
/// on the joiner side in `sovereign-mesh::join`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshWire {
    pub id: commonwealth_core::ids::MeshId,
    pub name: String,
    pub join_key_hash: [u8; 32],
    pub members: Vec<commonwealth_core::mesh::MemberRecord>,
    pub peers: Vec<commonwealth_core::mesh::MeshPeering>,
}

impl From<&Mesh> for MeshWire {
    fn from(m: &Mesh) -> Self {
        Self {
            id: m.id,
            name: m.name.clone(),
            join_key_hash: m.join_key_hash,
            members: m.members.values().cloned().collect(),
            peers: m.peers.clone(),
        }
    }
}

impl MeshWire {
    /// Reassemble into a `Mesh`. Callers use this on the joiner side
    /// to adopt the founder's state.
    pub fn into_mesh(self) -> Mesh {
        use std::collections::HashMap;
        let members = self
            .members
            .into_iter()
            .map(|m| (m.node_id, m))
            .collect::<HashMap<_, _>>();
        Mesh {
            id: self.id,
            name: self.name,
            join_key_hash: self.join_key_hash,
            members,
            peers: self.peers,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct JoinResponse {
    /// Freshly-assigned id for the joining node.
    pub assigned_node_id: NodeId,
    /// Full authoritative mesh snapshot. Joiner replaces its local
    /// placeholder with this so member lists, peers, and the canonical
    /// mesh_id all match the founder's view.
    pub mesh: MeshWire,
}

#[derive(Debug, Serialize)]
pub struct JoinRejection {
    pub reason: String,
}

/// POST /internal/join — verify a join_key and (on match) admit the caller.
pub async fn join(
    State(state): State<AppState>,
    Json(req): Json<JoinRequest>,
) -> Result<Json<JoinResponse>, (StatusCode, Json<JoinRejection>)> {
    let self_node_id = *state.inner.self_node_id_swap.load_full().as_ref();
    let mut mesh = state.inner.mesh.write().await;

    match membership::accept_join_with_proposed_id(
        &mut mesh,
        &req.join_key,
        &req.joining_node_name,
        req.joining_node_addresses,
        self_node_id,
        req.proposed_node_id,
    ) {
        Ok(new_id) => {
            tracing::info!(
                new_node = %new_id,
                joining_name = %req.joining_node_name,
                "handshake_accepted: admitted new mesh member"
            );
            // Persist IMMEDIATELY on join accept so the founder
            // doesn't forget this member if it restarts within the
            // 10s gossip-loop re-persist window. Hook is `None` in
            // tests and the standalone daemon, so this is a no-op
            // where persistence is managed elsewhere.
            if let Some(hook) = state.inner.on_mesh_mutation.as_ref() {
                hook(&mesh, self_node_id);
            }
            Ok(Json(JoinResponse {
                assigned_node_id: new_id,
                mesh: MeshWire::from(&*mesh),
            }))
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                joining_name = %req.joining_node_name,
                "handshake_rejected: join request denied"
            );
            Err((
                StatusCode::UNAUTHORIZED,
                Json(JoinRejection {
                    reason: e.to_string(),
                }),
            ))
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode as HttpStatus};
    use axum::routing::post;
    use axum::Router;
    use tower::ServiceExt;

    use crate::state::test_app_state;

    fn activity_router() -> (AppState, Router) {
        let state = test_app_state();
        let app = Router::new()
            .route("/internal/node/activity", post(node_activity))
            .with_state(state.clone());
        (state, app)
    }

    async fn post_activity(app: Router, level: &str, reason: &str) -> HttpStatus {
        let body = serde_json::json!({ "level": level, "reason": reason }).to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/internal/node/activity")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        response.status()
    }

    #[tokio::test]
    async fn hot_level_returns_204_no_content() {
        let (_, app) = activity_router();
        let status = post_activity(app, "hot", "tests_running").await;
        assert_eq!(status, HttpStatus::NO_CONTENT);
    }

    #[tokio::test]
    async fn hot_level_sets_availability_to_020() {
        let (state, app) = activity_router();
        post_activity(app, "hot", "tests_running").await;
        let val = *state.inner.local_inference_availability.read().await;
        assert!(
            (val - 0.20).abs() < 1e-6,
            "hot must set availability to 0.20, got {val}"
        );
    }

    #[tokio::test]
    async fn warm_level_sets_availability_to_065() {
        let (state, app) = activity_router();
        post_activity(app, "warm", "recent_edits").await;
        let val = *state.inner.local_inference_availability.read().await;
        assert!(
            (val - 0.65).abs() < 1e-6,
            "warm must set availability to 0.65, got {val}"
        );
    }

    #[tokio::test]
    async fn cool_level_sets_availability_to_085() {
        let (state, app) = activity_router();
        post_activity(app, "cool", "settling").await;
        let val = *state.inner.local_inference_availability.read().await;
        assert!(
            (val - 0.85).abs() < 1e-6,
            "cool must set availability to 0.85, got {val}"
        );
    }

    #[tokio::test]
    async fn idle_level_sets_availability_to_100() {
        // Start hot, then go idle to verify full round-trip.
        let (state, app) = activity_router();
        post_activity(app.clone(), "hot", "start").await;
        post_activity(app, "idle", "long_pause").await;
        let val = *state.inner.local_inference_availability.read().await;
        assert!(
            (val - 1.00).abs() < 1e-6,
            "idle must set availability to 1.00, got {val}"
        );
    }

    #[tokio::test]
    async fn unknown_level_defaults_to_idle() {
        let (state, app) = activity_router();
        post_activity(app, "turbo", "unknown_level").await;
        let val = *state.inner.local_inference_availability.read().await;
        assert!(
            (val - 1.00).abs() < 1e-6,
            "unknown level must default to 1.00, got {val}"
        );
    }

    // ── Runtime model slot management tests ──────────────────

    /// Stub `LocalInferenceService` that records every load/unload
    /// request and replays canned answers. Inference methods
    /// (`chat_completion`, `embed`) are stubbed to return errors
    /// since these tests only exercise the slot-management surface.
    struct StubLocalInference {
        load_calls: std::sync::Mutex<Vec<(String, std::path::PathBuf, u32)>>,
        unload_calls: std::sync::Mutex<Vec<String>>,
        load_response: Result<String, String>,
        inventory: Vec<(String, String)>,
    }

    impl StubLocalInference {
        fn new(load_response: Result<String, String>, inventory: Vec<(String, String)>) -> Self {
            Self {
                load_calls: std::sync::Mutex::new(Vec::new()),
                unload_calls: std::sync::Mutex::new(Vec::new()),
                load_response,
                inventory,
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::state::LocalInferenceService for StubLocalInference {
        async fn chat_completion(
            &self,
            _request: crate::openai_types::ChatCompletionRequest,
        ) -> Result<crate::openai_types::ChatCompletionResponse, String> {
            Err("stub".into())
        }

        async fn chat_completion_stream(
            &self,
            _request: crate::openai_types::ChatCompletionRequest,
        ) -> Result<
            std::pin::Pin<Box<dyn futures::Stream<Item = crate::openai_types::StreamFrame> + Send>>,
            String,
        > {
            Err("stub".into())
        }

        fn provider_manifest(&self) -> Option<commonwealth_inference::oicp::ProviderManifest> {
            None
        }

        async fn embed(&self, _input: &str) -> Result<Vec<f32>, String> {
            Err("stub".into())
        }

        async fn load_extra_slot(
            &self,
            slot_name: String,
            path: std::path::PathBuf,
            context_size: u32,
        ) -> Result<String, String> {
            self.load_calls
                .lock()
                .unwrap()
                .push((slot_name.clone(), path.clone(), context_size));
            self.load_response.clone()
        }

        async fn unload_extra_slot(&self, slot_name: &str) -> Result<Option<String>, String> {
            self.unload_calls.lock().unwrap().push(slot_name.into());
            // Stub returns Some(...) for any slot in inventory, None
            // otherwise.
            Ok(self
                .inventory
                .iter()
                .find(|(name, _)| name == slot_name)
                .map(|(_, mid)| mid.clone()))
        }

        async fn extras_inventory(&self) -> Vec<(String, String)> {
            self.inventory.clone()
        }
    }

    fn models_router(stub: Arc<StubLocalInference>) -> Router {
        let state = test_app_state().with_local_inference(stub);
        Router::new()
            .route("/internal/models/load", post(models_load))
            .route("/internal/models/unload", post(models_unload))
            .route(
                "/internal/models/inventory",
                axum::routing::get(models_inventory),
            )
            .with_state(state)
    }

    #[tokio::test]
    async fn models_load_returns_503_when_local_inference_absent() {
        // No `.with_local_inference(...)` → local_inference is None.
        let state = test_app_state();
        let app = Router::new()
            .route("/internal/models/load", post(models_load))
            .with_state(state);
        let body = serde_json::json!({
            "slot_name": "bulk",
            "path": "/m/x.gguf"
        })
        .to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/internal/models/load")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), HttpStatus::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn models_load_forwards_to_provider_and_returns_model_id() {
        let stub = Arc::new(StubLocalInference::new(
            Ok("Qwen3.5-9B.Q8_0".into()),
            vec![],
        ));
        let app = models_router(Arc::clone(&stub));
        let body = serde_json::json!({
            "slot_name": "bulk",
            "path": "/m/qwen.gguf",
            "context_size": 32768
        })
        .to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/internal/models/load")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), HttpStatus::OK);

        // Verify the stub recorded the call with the request fields
        // forwarded verbatim.
        let calls = stub.load_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "bulk");
        assert_eq!(calls[0].1, std::path::PathBuf::from("/m/qwen.gguf"));
        assert_eq!(calls[0].2, 32768);
    }

    #[tokio::test]
    async fn models_load_default_context_size_is_16384() {
        // Operators who omit `context_size` get the daemon-wide
        // default. Lock the contract so a future change is visible.
        let stub = Arc::new(StubLocalInference::new(Ok("m".into()), vec![]));
        let app = models_router(Arc::clone(&stub));
        let body = serde_json::json!({
            "slot_name": "bulk",
            "path": "/m/x.gguf"
        })
        .to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/internal/models/load")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let _ = app.oneshot(req).await.unwrap();
        let calls = stub.load_calls.lock().unwrap();
        assert_eq!(calls[0].2, 16_384);
    }

    #[tokio::test]
    async fn models_load_returns_400_on_provider_error() {
        // Stub provider rejects (e.g. a remote inference backend that
        // doesn't support runtime slot mutation). Handler surfaces
        // the error verbatim.
        let stub = Arc::new(StubLocalInference::new(Err("bad path".into()), vec![]));
        let app = models_router(stub);
        let body = serde_json::json!({
            "slot_name": "bulk",
            "path": "/m/x.gguf"
        })
        .to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/internal/models/load")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), HttpStatus::BAD_REQUEST);
    }

    #[tokio::test]
    async fn models_unload_returns_model_id_on_match() {
        let stub = Arc::new(StubLocalInference::new(
            Ok("ignored".into()),
            vec![("bulk".into(), "Qwen3.5-9B.Q8_0".into())],
        ));
        let app = models_router(Arc::clone(&stub));
        let body = serde_json::json!({"slot_name": "bulk"}).to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/internal/models/unload")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), HttpStatus::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["slot_name"], "bulk");
        assert_eq!(body["model_id"], "Qwen3.5-9B.Q8_0");
    }

    #[tokio::test]
    async fn models_unload_returns_null_model_id_when_slot_absent() {
        let stub = Arc::new(StubLocalInference::new(Ok("ignored".into()), vec![]));
        let app = models_router(stub);
        let body = serde_json::json!({"slot_name": "missing"}).to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/internal/models/unload")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), HttpStatus::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // serde_json renders `Option::None` as JSON null.
        assert!(body["model_id"].is_null());
    }

    #[tokio::test]
    async fn models_inventory_returns_loaded_extras() {
        let stub = Arc::new(StubLocalInference::new(
            Ok("ignored".into()),
            vec![
                ("bulk".into(), "Qwen3.5-9B.Q8_0".into()),
                ("reasoning".into(), "Qwopus3.5-27B-v3-Q6_K".into()),
            ],
        ));
        let app = models_router(stub);
        let req = Request::builder()
            .method("GET")
            .uri("/internal/models/inventory")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), HttpStatus::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let extras = body["extras"].as_array().unwrap();
        assert_eq!(extras.len(), 2);
    }

    #[tokio::test]
    async fn models_load_registers_in_inference_store_for_v1_models() {
        // After load_extra_slot succeeds, the handler also writes a
        // ModelInfo into inference_store so /v1/models advertises
        // the new slot. Without this, clients couldn't see the new
        // entry until the next daemon restart even though routing
        // would have worked.
        let stub = Arc::new(StubLocalInference::new(Ok("test-model".into()), vec![]));
        let state = test_app_state().with_local_inference(Arc::clone(&stub) as Arc<_>);
        let app = Router::new()
            .route("/internal/models/load", post(models_load))
            .with_state(state.clone());
        let body = serde_json::json!({
            "slot_name": "bulk",
            "path": "/m/test.gguf"
        })
        .to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/internal/models/load")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), HttpStatus::OK);

        // The store should now contain a ModelInfo whose name is
        // the model_id returned by the provider.
        let models = state.inner.inference_store.list_models();
        assert!(
            models.values().any(|m| m.name == "test-model"),
            "post-load: expected `test-model` in inference_store; got {:?}",
            models.values().map(|m| &m.name).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn models_unload_drops_from_inference_store() {
        // Pre-seed a store entry, then unload, assert it's gone.
        let stub = Arc::new(StubLocalInference::new(
            Ok("ignored".into()),
            vec![("bulk".into(), "Qwen3.5-9B.Q8_0".into())],
        ));
        let state = test_app_state().with_local_inference(Arc::clone(&stub) as Arc<_>);
        // Register a fake entry the way `models_load` would so we
        // can verify removal.
        register_extras_in_store(
            &state,
            "bulk",
            std::path::Path::new("/m/qwen.gguf"),
            "Qwen3.5-9B.Q8_0",
        );
        assert!(state
            .inner
            .inference_store
            .list_models()
            .values()
            .any(|m| m.name == "Qwen3.5-9B.Q8_0"));

        let app = Router::new()
            .route("/internal/models/unload", post(models_unload))
            .with_state(state.clone());
        let body = serde_json::json!({"slot_name": "bulk"}).to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/internal/models/unload")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), HttpStatus::OK);

        let models = state.inner.inference_store.list_models();
        assert!(
            !models.values().any(|m| m.name == "Qwen3.5-9B.Q8_0"),
            "post-unload: model_id should no longer be in store; got {:?}",
            models.values().map(|m| &m.name).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn models_inventory_empty_when_local_inference_absent() {
        // No local inference → empty inventory (not an error). This
        // keeps the GET route monitor-friendly: a recurring poll that
        // 200s with an empty list is easier to operate than one that
        // alternates 503/200 across daemon configurations.
        let state = test_app_state();
        let app = Router::new()
            .route(
                "/internal/models/inventory",
                axum::routing::get(models_inventory),
            )
            .with_state(state);
        let req = Request::builder()
            .method("GET")
            .uri("/internal/models/inventory")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), HttpStatus::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["extras"].as_array().unwrap().len(), 0);
    }

    // Storage-budget HTTP round-trip + helper tests live as
    // integration tests at `tests/storage_budget_route.rs` so they
    // can run independently of the (currently-broken) lib test
    // target.
}

// ─── Contribution controls ─────────────────────────────────────
//
// The Settings UI (and W3's tray menu) reads this status, sets the
// peer-inflight ceiling, and toggles the runtime pause.
//
// Routes are mounted on the client port behind the same loopback
// guard as `/internal/inference/warmup` — the operator's process
// (or the desktop talking to a child daemon) calls them locally;
// peers reaching them at all is a protocol error the guard rejects.

#[derive(Debug, Serialize)]
pub struct ContributionStatusResponse {
    /// Configured max concurrent peer requests. `usize::MAX`
    /// serialises as a large number — the UI displays "unlimited"
    /// when comparing to a sentinel.
    pub ceiling: usize,
    /// Live in-flight peer request count (approximate under
    /// contention — see field docs on `peer_inflight_count`).
    pub in_flight: usize,
    /// Unix-seconds expiry of the active pause, or `null` when not
    /// paused. The UI computes "Resumes at <time>" from this.
    pub paused_until: Option<i64>,
    /// Seconds until the active pause expires (null when not paused).
    pub pause_remaining_secs: Option<u64>,
    /// Whether peer requests honour the foreground-yield window.
    pub yield_peers_to_foreground: bool,
    /// Currently-yielding-to-local-user marker; `null` when not in
    /// the yield window. Lets the UI badge "yielding to chat".
    pub yielding_secs_remaining: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct SetContributionCeilingRequest {
    /// Max concurrent peer requests. `0` rejects all; `null` /
    /// missing means "unlimited" (`usize::MAX`).
    pub max: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct PauseContributionsRequest {
    /// How long the pause should last. `0` is a no-op (use
    /// `/internal/contribution/resume` to clear an active pause).
    pub duration_secs: u64,
}

/// `GET /internal/contribution/status` — snapshot for the Settings
/// panel + tray status chip. Cheap (atomic loads only).
pub async fn contribution_status(
    State(state): State<AppState>,
) -> Json<ContributionStatusResponse> {
    let paused_until_raw = state.contribution_paused_until();
    let paused_until = if paused_until_raw > 0 {
        Some(paused_until_raw)
    } else {
        None
    };
    Json(ContributionStatusResponse {
        ceiling: state.contribution_max_peer_inflight(),
        in_flight: state.peer_inflight_count(),
        paused_until,
        pause_remaining_secs: state.seconds_until_unpaused(),
        yield_peers_to_foreground: state.yield_peers_to_foreground(),
        yielding_secs_remaining: state.seconds_until_foreground_idle(),
    })
}

/// `POST /internal/contribution/ceiling` — set the peer-inflight
/// cap. `null` / missing `max` maps to unlimited.
pub async fn contribution_ceiling_set(
    State(state): State<AppState>,
    Json(req): Json<SetContributionCeilingRequest>,
) -> Json<ContributionStatusResponse> {
    let new_cap = req.max.unwrap_or(usize::MAX);
    let prev = state.contribution_max_peer_inflight();
    state.set_contribution_max_peer_inflight(new_cap);
    if prev != new_cap {
        tracing::info!(
            previous = prev,
            new = new_cap,
            "contribution: peer-inflight ceiling updated"
        );
    }
    contribution_status(State(state)).await
}

/// `POST /internal/contribution/pause` — pause for N seconds.
/// Idempotent; subsequent calls reset the expiry. The tray's "Pause
/// for 15min" / "1hr" submenu items call this.
pub async fn contribution_pause(
    State(state): State<AppState>,
    Json(req): Json<PauseContributionsRequest>,
) -> Json<ContributionStatusResponse> {
    if req.duration_secs == 0 {
        // No-op; surface current status. Avoids a confused state
        // where `paused_until` gets set to now and immediately
        // expires.
        return contribution_status(State(state)).await;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let expiry = now.saturating_add(req.duration_secs as i64);
    state.set_contribution_paused_until(expiry);
    tracing::info!(
        duration_secs = req.duration_secs,
        expiry_unix = expiry,
        "contribution: paused via /internal/contribution/pause"
    );
    contribution_status(State(state)).await
}

/// `POST /internal/contribution/resume` — clear an active pause.
/// No body required.
pub async fn contribution_resume(
    State(state): State<AppState>,
) -> Json<ContributionStatusResponse> {
    if state.contribution_paused_until() != 0 {
        state.set_contribution_paused_until(0);
        tracing::info!("contribution: resumed via /internal/contribution/resume");
    }
    contribution_status(State(state)).await
}

#[derive(Debug, Deserialize)]
pub struct RecentContributionsParams {
    /// Max events to return. Defaults to 20; capped at 200 to bound
    /// the response size (the full ledger can have many thousands of
    /// events on a long-running node).
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct RecentContributionsResponse {
    /// Most recent ledger events, sorted by timestamp DESC. Each
    /// entry is a full `LedgerEvent` (origin node + timestamp + kind);
    /// the UI is responsible for friendly-name resolution and any
    /// formatting beyond the raw fact.
    pub events: Vec<commonwealth_core::contributions::LedgerEvent>,
}

/// `GET /internal/contribution/recent` — recent ledger events, newest
/// first. Powers the W3 contribution-panel "served feed" without
/// forcing the UI to aggregate across the full per-node window. Cheap:
/// reads the MeshStore once and sorts in-memory.
pub async fn contribution_recent(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<RecentContributionsParams>,
) -> Result<Json<RecentContributionsResponse>, (StatusCode, String)> {
    let limit = params.limit.unwrap_or(20).min(200);
    let entries = state
        .inner
        .mesh_store
        .scan(commonwealth_state::CONTRIBUTIONS_APP_ID, "")
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("contribution_recent: scan failed: {e}"),
            )
        })?;
    let mut events: Vec<commonwealth_core::contributions::LedgerEvent> = entries
        .into_iter()
        .filter_map(|e| serde_json::from_slice(e.value.as_ref()).ok())
        .collect();
    // Newest first. `LedgerEvent.timestamp` is unix-seconds; ties
    // are broken by stable_sort order, which is fine for UI display.
    events.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    events.truncate(limit);
    Ok(Json(RecentContributionsResponse { events }))
}

// ── Dimensional per-node contributions (Mesh Health Members panel) ──
//
// Mirror of the Tauri-side `NodeContributionsDto` the desktop's
// `mesh_get_contributions` returns in Local mode. Field names are
// frozen — the desktop deserializes against this exact shape in
// Attach mode, so renaming a field here without updating the
// desktop side blanks the Members ledger.

#[derive(Debug, Clone, Serialize)]
pub struct CorpusHostingView {
    pub corpus_id: String,
    pub corpus_name: String,
    pub size_gb: f64,
    pub queries_served: u64,
    pub is_sole_host: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeContributionsView {
    pub node_id: String,
    pub window_days: u32,
    pub inference_served_requests: u64,
    pub inference_served_tokens: u64,
    pub inference_served_wall_seconds: f64,
    pub inference_consumed_requests: u64,
    pub inference_consumed_tokens: u64,
    pub corpora_hosted: Vec<CorpusHostingView>,
    pub bytes_served: u64,
    pub bytes_received: u64,
}

/// `GET /internal/contribution/view` — aggregated per-node contributions
/// over the default 30-day window, one entry per peer the local
/// MeshStore has ledger events about. Powers the Mesh → Members
/// section of the desktop Settings panel in Attach mode, where the
/// Tauri shell can't reach the daemon's in-process `AppState`
/// directly.
pub async fn contribution_view(
    State(state): State<AppState>,
) -> Result<Json<Vec<NodeContributionsView>>, (StatusCode, String)> {
    let caps_map: std::collections::HashMap<
        NodeId,
        commonwealth_core::capabilities::NodeCapabilities,
    > = {
        let mesh_view = state.inner.mesh.read().await;
        mesh_view
            .members
            .iter()
            .map(|(id, member)| (*id, member.capabilities.clone()))
            .collect()
    };
    let map = commonwealth_state::current_contributions(
        &state.inner.mesh_store,
        &caps_map,
        commonwealth_core::contributions::DEFAULT_WINDOW_DAYS,
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("contribution_view: aggregate failed: {e}"),
        )
    })?;
    let mut out: Vec<NodeContributionsView> = map
        .into_iter()
        .map(|(node_id, c)| NodeContributionsView {
            node_id: node_id
                .as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            window_days: c.window_days,
            inference_served_requests: c.inference_served.requests,
            inference_served_tokens: c.inference_served.total_tokens_generated,
            inference_served_wall_seconds: c.inference_served.wall_seconds,
            inference_consumed_requests: c.inference_consumed.requests,
            inference_consumed_tokens: c.inference_consumed.total_tokens_generated,
            corpora_hosted: c
                .corpora_hosted
                .into_iter()
                .map(|h| CorpusHostingView {
                    corpus_id: h.corpus_id,
                    corpus_name: h.corpus_name,
                    size_gb: h.size_gb,
                    queries_served: h.queries_served,
                    is_sole_host: h.is_sole_host,
                })
                .collect(),
            bytes_served: c.bytes_served,
            bytes_received: c.bytes_received,
        })
        .collect();
    out.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    Ok(Json(out))
}

// ── Local Activity ledger (Activity & Sharing surface) ──────────
//
// The activity counterpart to the contribution handlers above. Reads
// the gossip-excluded `activity-private` namespace and folds in this
// node's *own* mesh-contribution totals, so one response answers
// "what has my daemon been doing — for me, and for the mesh?" — the
// glassbox view that works even for a mesh of one.

#[derive(Debug, Deserialize)]
pub struct ActivitySummaryParams {
    /// Lookback window in days. Defaults to the activity ledger's
    /// 7-day window; capped at 365.
    pub window_days: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct ActivitySummaryResponse {
    #[serde(flatten)]
    pub activity: commonwealth_core::activity::ActivitySummary,
    // Folded-in mesh contribution: what THIS node provided to peers
    // (the gossiped contribution ledger, over its own 30-day window).
    pub peer_inference_served_requests: u64,
    pub peer_inference_served_tokens: u64,
    pub peer_knowledge_queries_served: u64,
    pub peer_bytes_served: u64,
    pub peer_bytes_received: u64,
}

/// `GET /internal/activity/summary` — the local Activity rollup plus
/// this node's mesh-contribution totals. Powers the "all on this
/// machine" totals card in Settings → Activity & Sharing.
pub async fn activity_summary(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<ActivitySummaryParams>,
) -> Result<Json<ActivitySummaryResponse>, (StatusCode, String)> {
    let window_days = params
        .window_days
        .unwrap_or(commonwealth_core::activity::DEFAULT_ACTIVITY_WINDOW_DAYS)
        .min(365);
    let activity = commonwealth_state::current_activity(&state.inner.mesh_store, window_days)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("activity_summary: aggregate failed: {e}"),
            )
        })?;

    // Fold in this node's own contribution totals. Self-origin
    // contribution events land on the self node's `NodeContributions`,
    // so the self entry is exactly "what I served to / received from
    // the mesh."
    let self_id = state.inner.contribution_emitter.self_node_id();
    let caps_map: std::collections::HashMap<
        NodeId,
        commonwealth_core::capabilities::NodeCapabilities,
    > = {
        let mesh_view = state.inner.mesh.read().await;
        mesh_view
            .members
            .iter()
            .map(|(id, member)| (*id, member.capabilities.clone()))
            .collect()
    };
    let contrib = commonwealth_state::current_contributions(
        &state.inner.mesh_store,
        &caps_map,
        commonwealth_core::contributions::DEFAULT_WINDOW_DAYS,
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("activity_summary: contribution aggregate failed: {e}"),
        )
    })?;
    let self_c = contrib.get(&self_id);

    Ok(Json(ActivitySummaryResponse {
        activity,
        peer_inference_served_requests: self_c.map(|c| c.inference_served.requests).unwrap_or(0),
        peer_inference_served_tokens: self_c
            .map(|c| c.inference_served.total_tokens_generated)
            .unwrap_or(0),
        peer_knowledge_queries_served: self_c
            .map(|c| {
                c.corpora_hosted
                    .iter()
                    .map(|h| h.queries_served)
                    .sum::<u64>()
            })
            .unwrap_or(0),
        peer_bytes_served: self_c.map(|c| c.bytes_served).unwrap_or(0),
        peer_bytes_received: self_c.map(|c| c.bytes_received).unwrap_or(0),
    }))
}

#[derive(Debug, Serialize)]
pub struct ActivityRecentResponse {
    /// Most recent local activity events, newest first. Each entry is
    /// a full `ActivityEvent`; the UI formats the friendly summary.
    pub events: Vec<commonwealth_core::activity::ActivityEvent>,
}

/// `GET /internal/activity/recent` — recent local activity events,
/// newest first. Powers the unified feed in Activity & Sharing
/// (interleaved client-side with the contribution feed).
pub async fn activity_recent(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<RecentContributionsParams>,
) -> Result<Json<ActivityRecentResponse>, (StatusCode, String)> {
    let limit = params.limit.unwrap_or(20).min(200);
    let entries = state
        .inner
        .mesh_store
        .scan(commonwealth_state::ACTIVITY_APP_ID, "")
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("activity_recent: scan failed: {e}"),
            )
        })?;
    let mut events: Vec<commonwealth_core::activity::ActivityEvent> = entries
        .into_iter()
        .filter_map(|e| serde_json::from_slice(e.value.as_ref()).ok())
        .collect();
    events.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    events.truncate(limit);
    Ok(Json(ActivityRecentResponse { events }))
}
