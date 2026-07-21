// SPDX-License-Identifier: AGPL-3.0-or-later
//! The compute child's HTTP server: an axum router that exposes an
//! `Arc<dyn InferenceProvider>` over the native wire ([`crate::wire`]).
//!
//! The child (`child_main`) loads its model into a provider, flips the
//! `ready` flag, and serves this router on `127.0.0.1:0`. The daemon's
//! `ChildProvider` (increment 6) is the client.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header::CONTENT_TYPE, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use sovereign_contracts::{CompletionRequest, Error, InferenceProvider, StreamFrame};

use crate::wire::{
    self, EmbedBatchRequest, EmbedBatchResponse, EmbedMode, EmbedRequest, EmbedResponse, HealthInfo,
    WireError, NDJSON_CONTENT_TYPE, ROUTE_COMPLETE, ROUTE_COMPLETE_STREAM, ROUTE_EMBED,
    ROUTE_EMBED_BATCH, ROUTE_HEALTH,
};

/// Static identity of the child, reported by `/health`.
#[derive(Debug, Clone)]
pub struct ChildMeta {
    /// `"generate"` | `"embed"` | `"mock"`.
    pub role: String,
    /// The resident model id (or `""` for mock / pre-load).
    pub model_id: String,
}

/// Axum state: the provider being served + the readiness flag + identity.
#[derive(Clone)]
struct ChildServerState {
    provider: Arc<dyn InferenceProvider>,
    ready: Arc<AtomicBool>,
    meta: ChildMeta,
}

/// Build the child's router. `ready` starts `false` and is flipped `true`
/// by the child once its model is loaded — until then `/health` returns
/// 503 and the supervisor holds it in `Warming`.
pub fn router(
    provider: Arc<dyn InferenceProvider>,
    ready: Arc<AtomicBool>,
    meta: ChildMeta,
) -> Router {
    let state = ChildServerState {
        provider,
        ready,
        meta,
    };
    Router::new()
        .route(ROUTE_COMPLETE, post(handle_complete))
        .route(ROUTE_COMPLETE_STREAM, post(handle_complete_stream))
        .route(ROUTE_EMBED, post(handle_embed))
        .route(ROUTE_EMBED_BATCH, post(handle_embed_batch))
        .route(ROUTE_HEALTH, get(handle_health))
        .with_state(state)
}

/// Map a contract [`Error`] to an HTTP status + wire envelope.
fn err_response(err: &Error) -> Response {
    let status = match err {
        Error::InvalidInput(_) => StatusCode::BAD_REQUEST,
        Error::ModelNotLoaded(_) | Error::ComputeUnavailable { .. } => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        Error::NotImplemented(_) => StatusCode::NOT_IMPLEMENTED,
        // 499 = client closed request (nginx convention) — a cancelled
        // generation, distinct from a 500.
        Error::Cancelled => StatusCode::from_u16(499).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(WireError::from_error(err))).into_response()
}

async fn handle_complete(
    State(st): State<ChildServerState>,
    Json(req): Json<CompletionRequest>,
) -> Response {
    match st.provider.complete(&req).await {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => err_response(&e),
    }
}

async fn handle_complete_stream(
    State(st): State<ChildServerState>,
    Json(req): Json<CompletionRequest>,
) -> Response {
    let stream = match st.provider.complete_stream_with_finish(&req).await {
        Ok(s) => s,
        Err(e) => return err_response(&e),
    };
    // Each frame → one NDJSON line. A frame that somehow fails to encode
    // becomes a terminal Error frame rather than silently truncating.
    let byte_stream = stream.map(|frame| {
        let mut line = wire::encode_frame(&frame).unwrap_or_else(|e| {
            serde_json::to_string(&StreamFrame::Error(format!("frame encode failed: {e}")))
                .unwrap_or_else(|_| "{\"Error\":\"frame encode failed\"}".to_string())
        });
        line.push('\n');
        Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(line))
    });
    (
        [(CONTENT_TYPE, NDJSON_CONTENT_TYPE)],
        Body::from_stream(byte_stream),
    )
        .into_response()
}

async fn handle_embed(
    State(st): State<ChildServerState>,
    Json(req): Json<EmbedRequest>,
) -> Response {
    let result = match req.mode {
        EmbedMode::Document => st.provider.embed(&req.input).await,
        EmbedMode::Query => st.provider.embed_query(&req.input).await,
    };
    match result {
        Ok(embedding) => Json(EmbedResponse { embedding }).into_response(),
        Err(e) => err_response(&e),
    }
}

async fn handle_embed_batch(
    State(st): State<ChildServerState>,
    Json(req): Json<EmbedBatchRequest>,
) -> Response {
    match st.provider.embed_batch(&req.inputs).await {
        Ok(embeddings) => Json(EmbedBatchResponse { embeddings }).into_response(),
        Err(e) => err_response(&e),
    }
}

async fn handle_health(State(st): State<ChildServerState>) -> Response {
    let ready = st.ready.load(Ordering::Relaxed);
    let info = HealthInfo {
        state: if ready { "ready" } else { "loading" }.to_string(),
        role: st.meta.role.clone(),
        model_id: st.meta.model_id.clone(),
    };
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(info)).into_response()
}
