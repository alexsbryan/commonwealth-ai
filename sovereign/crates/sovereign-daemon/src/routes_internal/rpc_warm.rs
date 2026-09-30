// SPDX-License-Identifier: AGPL-3.0-or-later
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

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
// The one resolver, shared with serve's route (pb-serve-distributes-standalone).
use sovereign_contracts::rpc_warm::resolve_local_model;

use crate::state::AppState;

/// `POST /internal/rpc-warm`. Body is an opaque `RpcWarmShardRequest` (defined in
/// sovereign-mesh). `503` when this node has no warmer (not an inference worker);
/// `500` with `{ "error": … }` when warming fails; `200` with the warmer's stats
/// on success.
pub async fn rpc_warm(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(warmer) = state.inner.store.rpc_shard_warmer.clone() else {
        // No warmer in this process: serve is dialed, not hosted. Until the
        // flip registers serve's rpc-warm with cw-rails, a peer still dials
        // this route, so it goes to serve's own (pb-serve-distributes-
        // standalone), as model transfer does (`model_files`).
        return match crate::serve_client::ServingPath::decided_serve() {
            Some(Some(serve)) => forward_to_serve(&serve.base, &body).await,
            _ => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": "this node has no RPC shard warmer (not an inference worker)"
                })),
            )
                .into_response(),
        };
    };

    // Resolve `model_id` → this node's local copy of the GGUF via the servable
    // allowlist — the same `file_name()` match `serve_model_file` uses. `None`
    // when the node doesn't hold the model (the warmer then fetches it, or
    // range-fetches its shard for the byte-range path).
    let local_model_path = body.get("model_id").and_then(Value::as_str).and_then(|id| {
        resolve_local_model(&state.inner.serving.servable_model_files.current(), id)
    });

    let reach =
        crate::rpc_warm_http::warm_reach(&state, body.get("host_node_id").and_then(Value::as_str))
            .await;
    match warmer.warm_shard(body, local_model_path, reach).await {
        Ok(stats) => (StatusCode::OK, Json(stats)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e })),
        )
            .into_response(),
    }
}

/// How long a forwarded warm may take: a worker without the model fetches
/// its shard first.
const WARM_WINDOW: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// `body` to serve's `/internal/rpc-warm` at `base`, its answer relayed; an
/// unreachable serve is a 503 naming it.
async fn forward_to_serve(base: &str, body: &Value) -> Response {
    let path = "/internal/rpc-warm";
    tracing::info!(target: "serving_path", serve_base = base, "rpc-warm: forwarded to serve");
    match crate::serve_client::forward_within(
        base,
        axum::http::Method::POST,
        path,
        Some(body.to_string().into_bytes()),
        WARM_WINDOW,
    )
    .await
    {
        Ok((status, answer)) => (
            status,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            answer,
        )
            .into_response(),
        Err(why) => {
            tracing::warn!(target: "serving_path", error = %why, "rpc-warm: not forwarded to serve");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": why })),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
#[path = "rpc_warm_tests.rs"]
mod tests;
