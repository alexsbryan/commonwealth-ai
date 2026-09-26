// SPDX-License-Identifier: AGPL-3.0-or-later
//! The routes of served model kinds (`sovereign_inference::served_kind`),
//! mounted from the registry rather than one arm per kind. Each route
//! answers against this daemon's local inference service.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{post, MethodRouter};
use axum::Json;
use sovereign_contracts::traits::InferenceProvider;
use sovereign_inference::served_kind::{self, KindRoute, KindServeError, ServedKind};
use tracing::{debug, warn};

use crate::openai_types::ErrorResponse;
use crate::state::AppState;

/// `(path, handler)` for every registered kind with a route. A kind whose
/// route is a named absence mounts nothing, and says why at debug.
pub fn served_kind_routes() -> Vec<(&'static str, MethodRouter<AppState>)> {
    served_kind::served_kinds()
        .into_iter()
        .filter_map(|kind| match kind.route {
            KindRoute::Served { path, .. } => {
                debug!(target: "served_kind", kind = kind.role, path, "mounting served kind route");
                let handler = post(
                    move |State(state): State<AppState>, Json(body): Json<serde_json::Value>| {
                        serve_kind(kind, state, body)
                    },
                );
                Some((path, handler))
            }
            KindRoute::Absent { reason } => {
                debug!(target: "served_kind", kind = kind.role, reason, "served kind has no route");
                None
            }
        })
        .collect()
}

fn error(status: StatusCode, message: String, code: &str) -> Response {
    (
        status,
        Json(serde_json::to_value(ErrorResponse::new(message, code)).unwrap_or_default()),
    )
        .into_response()
}

async fn serve_kind(kind: ServedKind, state: AppState, body: serde_json::Value) -> Response {
    let KindRoute::Served { serve, .. } = kind.route else {
        return error(
            StatusCode::NOT_FOUND,
            format!("served kind `{}` has no route", kind.role),
            "not_found",
        );
    };
    let Some(service) = state.inner.serving.local_inference.as_ref() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "this daemon has no local inference backend to serve `{}`",
                kind.role
            ),
            "no_local_inference_backend",
        );
    };
    let provider: Arc<dyn InferenceProvider> = Arc::clone(service) as Arc<dyn InferenceProvider>;
    match serve(provider, body).await {
        Ok(value) => {
            debug!(target: "served_kind", kind = kind.role, "served kind request answered");
            Json(value).into_response()
        }
        Err(KindServeError::BadRequest(message)) => {
            error(StatusCode::BAD_REQUEST, message, "invalid_request_error")
        }
        Err(KindServeError::Backend(message)) => {
            warn!(target: "served_kind", kind = kind.role, error = %message, "served kind request failed");
            error(StatusCode::SERVICE_UNAVAILABLE, message, "backend_error")
        }
    }
}
