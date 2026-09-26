// SPDX-License-Identifier: AGPL-3.0-or-later
//! The routes of served model kinds (`sovereign_inference::served_kind`),
//! mounted from the registry through the one kind mount
//! (`sovereign_compute::server::kind_routes`). Each route answers against
//! this daemon's local inference service.

use std::sync::Arc;

use axum::routing::MethodRouter;
use sovereign_compute::server::{kind_routes, openai_refusal};
use sovereign_contracts::traits::InferenceProvider;
use sovereign_inference::served_kind::ServedKind;

use crate::state::AppState;

/// `(path, handler)` for every registered kind with a route.
pub fn served_kind_routes() -> Vec<(&'static str, MethodRouter<AppState>)> {
    kind_routes(local_provider, openai_refusal)
}

/// The daemon serves a kind against its local inference service, and names
/// the absence when it has none.
fn local_provider(
    state: &AppState,
    kind: &ServedKind,
) -> Result<Arc<dyn InferenceProvider>, String> {
    state
        .inner
        .serving
        .local_inference
        .as_ref()
        .map(|service| Arc::clone(service) as Arc<dyn InferenceProvider>)
        .ok_or_else(|| {
            format!(
                "this daemon has no local inference backend to serve `{}`",
                kind.role
            )
        })
}
