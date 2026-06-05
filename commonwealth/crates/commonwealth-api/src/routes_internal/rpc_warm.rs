//! Worker side of distributed-inference auto-warm orchestration.
//!
//! `POST /internal/rpc-warm` — a host that is about to distribute a large
//! primary across the mesh calls this on each worker to make the worker seed its
//! RPC tensor cache with ITS shard of the model. Once warm, the host's `-ot`
//! load against that worker is all `SET_TENSOR_HASH` cache hits, so it never
//! streams a large weight share (the upload deadlock the whole feature exists to
//! avoid). See `sovereign-mesh::rpc_warm_http` for the impl + the wire types, and
//! `docs/RPC_DISTRIBUTED_INFERENCE.md` for the end-to-end flow.
//!
//! On the **internal port** (`:9742`, tailnet-only) — same trust boundary as
//! `/internal/v1/models/file/{name}` (the GGUF the worker fetches from here when
//! it doesn't already hold the model). The handler is intentionally thin: it
//! resolves `model_id` → the worker's local GGUF via the servable allowlist (the
//! one surface that knows which files this node has on disk) and hands the opaque
//! request body + that path to the injected [`RpcShardWarmer`]. The plan/tensor
//! types live in sovereign-mesh, so this crate stays free of them.

use std::path::PathBuf;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::state::AppState;

/// Find this node's local copy of `model_id`: first an exact servable-allowlist
/// entry (a configured slot), then the same filename in any servable model's
/// DIRECTORY — so a worker that holds the GGUF on disk (even when it isn't a
/// configured slot) warms from it instead of re-fetching. Returns `None` when the
/// node genuinely doesn't have it (the warmer then fetches). Path-safety: a
/// `model_id` containing a path separator is rejected — it's matched as a bare
/// file name only, never a traversal.
fn resolve_local_model(state: &AppState, model_id: &str) -> Option<PathBuf> {
    if model_id.is_empty() || model_id.contains('/') || model_id.contains('\\') {
        return None;
    }
    let allow = state.inner.servable_model_files.load();
    // 1. Exact slot match.
    if let Some(p) = allow
        .iter()
        .find(|p| p.file_name().and_then(|n| n.to_str()) == Some(model_id))
    {
        return Some(p.clone());
    }
    // 2. Same filename in any servable model's directory (on disk, not a slot).
    let mut seen = std::collections::HashSet::new();
    for dir in allow.iter().filter_map(|p| p.parent()) {
        if seen.insert(dir.to_path_buf()) {
            let candidate = dir.join(model_id);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// `POST /internal/rpc-warm`. Body is an opaque `RpcWarmShardRequest` (defined in
/// sovereign-mesh). `503` when this node has no warmer (not an inference worker);
/// `500` with `{ "error": … }` when warming fails; `200` with the warmer's stats
/// on success.
pub async fn rpc_warm(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(warmer) = state.inner.rpc_shard_warmer.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "this node has no RPC shard warmer (not an inference worker)"
            })),
        )
            .into_response();
    };

    // Resolve `model_id` → this node's local copy of the GGUF via the servable
    // allowlist — the same `file_name()` match `serve_model_file` uses. `None`
    // when the node doesn't hold the model (the warmer then fetches it, or
    // range-fetches its shard for the byte-range path).
    let local_model_path = body
        .get("model_id")
        .and_then(Value::as_str)
        .and_then(|id| resolve_local_model(&state, id));

    match warmer.warm_shard(body, local_model_path).await {
        Ok(stats) => (StatusCode::OK, Json(stats)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e })),
        )
            .into_response(),
    }
}
