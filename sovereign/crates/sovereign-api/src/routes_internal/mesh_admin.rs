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
    let Some(service) = state.inner.serving.local_inference.as_ref() else {
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
        .map_err(|e| format!("{e}"))
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
    use commonwealth_core::model::{ModelArchitecture, ModelInfo};
    use oicp_types::CapabilityProfile;

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
    state.inner.serving.inference_store.set_model_info(&info);
}

fn deregister_extras_from_store(state: &AppState, model_id_str: &str) -> bool {
    // Look up the existing entry by advertised name, then remove
    // by ModelId. We don't have the original path here (the
    // `unload` request only carries the slot name), so we can't
    // recompute the deterministic id directly — name lookup is the
    // right path, and matches how `/v1/models` exposes the entries.
    let models = state.inner.serving.inference_store.list_models();
    let target = models
        .into_iter()
        .find(|(_, info)| info.name == model_id_str);
    if let Some((id, _)) = target {
        state.inner.serving.inference_store.remove_model_info(id);
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
    let Some(service) = state.inner.serving.local_inference.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "no local inference service is bound".to_string(),
        ));
    };
    match service
        .unload_extra_slot(&req.slot_name)
        .map_err(|e| format!("{e}"))
    {
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
/// That mounting claim was FALSE from the day it was written until
/// 2026-07-27 — the route lived on `internal_router` (`:9742`), so
/// every caller's POST 404'd and was swallowed as a no-op. If you
/// are reading this doc to learn where the route is, verify it in
/// `server::client_router` rather than trusting this sentence.
///
/// Wired into the desktop app's window-focus / chat-mount events
/// so the slot is hot by the time the user hits send. Without
/// this, every conversation that pauses past `primary_idle_secs`
/// (default 300s — `setup_config::default_primary_idle_secs`)
/// re-pays the full model load on the next turn — 10–20s on
/// Metal, and 57–95s measured for an 18.5 GB primary.
pub async fn inference_warmup(
    State(state): State<AppState>,
) -> Result<Json<WarmupResponse>, (StatusCode, String)> {
    let Some(service) = state.inner.serving.local_inference.as_ref() else {
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
        .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("{e}")))?;
    let latency_ms = started.elapsed().as_millis() as u64;
    Ok(Json(WarmupResponse { latency_ms }))
}

/// `GET /internal/models/inventory` — list the currently-loaded
/// extras lineup.
pub async fn models_inventory(State(state): State<AppState>) -> Json<InventoryResponse> {
    let Some(service) = state.inner.serving.local_inference.as_ref() else {
        return Json(InventoryResponse { extras: Vec::new() });
    };
    let inventory = service.extras_inventory();
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
    /// Turns in flight (`AppStateInner::foreground_inflight`).
    pub foreground_inflight: usize,
}

/// `GET /internal/daemon/foreground_state` — read-only snapshot of
/// the foreground-yield atomics. See [`ForegroundStateResponse`].
pub async fn foreground_state(State(state): State<AppState>) -> Json<ForegroundStateResponse> {
    let active_ingests_count = state.inner.ingest.active_ingests.read().await.len();
    Json(ForegroundStateResponse {
        last_active_unix_ts: state.foreground_last_active_ts(),
        window_secs: state.yield_window_secs(),
        currently_yielding: state.should_yield_to_foreground(),
        seconds_until_idle: state.seconds_until_foreground_idle(),
        active_ingests_count,
        foreground_inflight: state.foreground_inflight(),
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
// compare against `mesh.invite_key_hash`; on match we append the new
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
//     at rest via `Mesh::invite_key_hash`.
//   - Timing-attack-resistant equality lives in `membership::verify_join_key`.

// `JoinRequest` / `JoinResponse` / `JoinRejection` are
// `commonwealth_core::mesh::wire` — one definition for every process that
// speaks them, including a package-only member that never links this crate.
// Re-exported so this module's callers are unchanged.
pub use commonwealth_core::mesh::wire::{JoinRejection, JoinRequest, JoinResponse};

/// Wire shape for the full mesh snapshot: the one projection in
/// `commonwealth_core::mesh` (it had four declarations once — see its docs).
pub use commonwealth_core::mesh::{MeshWire, SecretDisclosure};

/// POST /internal/join — verify a join_key and (on match) admit the caller.
pub async fn join(
    State(state): State<AppState>,
    Json(req): Json<JoinRequest>,
) -> Result<Json<JoinResponse>, (StatusCode, Json<JoinRejection>)> {
    let self_node_id = state.inner.fabric.identity.current();

    // Identity proof of possession — verified BEFORE taking the mesh
    // write lock. Only enforced when the joiner presents a pubkey:
    // pre-identity joiners are admitted exactly as before. A pubkey
    // with a missing/invalid proof is a loud 401 (never a silent
    // admit-without-key), because admitting an unproven key would
    // bind a transport identity the joiner may not control.
    if let Some(pubkey) = req.node_pubkey.as_ref() {
        let proven = match (req.proposed_node_id.as_ref(), req.pubkey_proof.as_deref()) {
            (Some(node_id), Some(proof)) => commonwealth_transport::identity::verify_join_proof(
                pubkey,
                node_id,
                &req.joining_node_name,
                proof,
            ),
            // The proof binds the proposed_node_id; modern joiners
            // always persist + send one. A pubkey without it (or
            // without a proof) is malformed.
            _ => false,
        };
        if !proven {
            tracing::warn!(
                joining_name = %req.joining_node_name,
                pubkey = %pubkey,
                "handshake_rejected: node_pubkey presented without a valid proof of possession"
            );
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(JoinRejection {
                    reason: "node_pubkey proof of possession missing or invalid".into(),
                }),
            ));
        }
    }

    let mut mesh = state.inner.fabric.mesh.write().await;

    // Encrypted-mesh invites are short-lived: reject a join once the TTL has
    // passed. Plaintext meshes set no expiry, so this is a no-op for them.
    //
    // The expiry is read from the MESH, not from this node's `AppState`. It
    // used to live in per-node RAM, which meant it was armed only on the node
    // that personally minted the invite and was lost entirely on restart — so
    // a joiner aimed at any other member, or at the same member after a
    // bounce, bypassed the TTL completely. Any member can admit (there is no
    // founder check here and never was), so the expiry has to travel with the
    // mesh for the check to mean anything.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if mesh.invite_expired_at(now) {
        tracing::warn!(
            joining_name = %req.joining_node_name,
            expires_at = ?mesh.invite_expires_at,
            now,
            "handshake_rejected: invite link has expired"
        );
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(JoinRejection {
                reason: "invite link has expired".into(),
            }),
        ));
    }

    match membership::accept_join_with_identity(
        &mut mesh,
        &req.join_key,
        &req.joining_node_name,
        req.joining_node_addresses,
        self_node_id,
        req.proposed_node_id,
        req.node_pubkey,
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
            if let Some(hook) = state.inner.fabric.on_mesh_mutation.as_ref() {
                hook(&mesh, self_node_id);
            }
            Ok(Json(JoinResponse {
                assigned_node_id: new_id,
                // Disclose, and now it SAYS so. A joiner has no other
                // channel to obtain the gossip credential, so withholding it
                // here would admit a node that can never authorize. This was
                // previously correct by the absence of a redaction line —
                // indistinguishable, to a reader, from the bug of forgetting
                // one.
                mesh: MeshWire::for_peer(&mesh, SecretDisclosure::Disclose),
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
#[path = "mesh_admin/tests.rs"]
mod tests;

// ─── Contribution controls live in `mesh_admin/contribution.rs` ───
// Re-exported here so `routes_internal::mesh_admin::contribution_*` and the
// DTOs keep resolving at the historical path (ARCH 8: one implementation).
pub mod contribution;
pub use contribution::*;
