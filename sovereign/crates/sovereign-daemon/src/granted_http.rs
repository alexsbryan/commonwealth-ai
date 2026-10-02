// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reads a client makes over what it is GRANTED — the on-prem journey's
//! routes with no daemon equivalent (pb-distribution-onprem-routes, decision
//! phase-b-86: "no client: the daemon's API").
//!
//! - `GET /v1/corpora` — the installed corpora the caller's grant admits,
//!   as [`corpus_catalog_http`](crate::corpus_catalog_http)'s rows.
//! - `GET /v1/corpora/{corpus}/chunks/{chunk_id}?radius=` — the cited-passage
//!   reading window, `reading_http`'s neighbour window behind the grant.
//! - `GET /v1/tools` — the tools the turn runtime holds, each with whether
//!   the executor's approval gate would fire for it.
//!
//! The grant is the turn's own decider: `PrincipalScope::from_resolver` over
//! the Runtime's `corpus_principal`, then `admits` — `KeyedOwners`'
//! `[retrieval] corpora` on a keyed daemon, every corpus for the local owner
//! on an unkeyed one. Admission is the turn family's: loopback, or a key the
//! keyed gate admitted ([`crate::api_keys::local_or_keyed`]).

use std::sync::Arc;

use axum::extract::{Extension, Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sovereign_contracts::types::Effect;
use sovereign_core::context::PrincipalScope;

use crate::api_keys::Caller;
use crate::corpus_catalog_http::{catalog_entries, CatalogResponse};
use crate::daemon::EmbeddedDaemon;
use crate::http_response::Absence;
use crate::reading_http::NeighborQuery;

/// One tool the turn runtime holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeldTool {
    pub id: String,
    pub name: String,
    pub effect: Effect,
    /// Whether the executor asks for approval before running it — a
    /// non-empty `required_permissions` (sovereign-core `executor.rs`).
    pub requires_approval: bool,
}

/// `GET /v1/tools`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeldToolsResponse {
    pub tools: Vec<HeldTool>,
}

pub fn granted_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/v1/corpora", get(list_corpora))
        .route(
            "/v1/corpora/{corpus}/chunks/{chunk_id}",
            get(reading_window),
        )
        .route("/v1/tools", get(list_tools))
        // Loopback, or an API key the keyed gate admitted (`crate::api_keys`).
        .layer(axum::middleware::from_fn(crate::api_keys::local_or_keyed))
        .layer(Extension(daemon))
}

fn runtime_for(daemon: &EmbeddedDaemon) -> Result<&Arc<sovereign_core::runtime::Runtime>, Absence> {
    daemon.runtime().ok_or_else(|| {
        Absence::unavailable(
            "this daemon holds no turn Runtime (it was commissioned to serve nothing)",
        )
    })
}

/// The caller's corpus grant, decided the way a turn decides it.
fn grant_scope(daemon: &EmbeddedDaemon, caller: &Caller) -> Result<PrincipalScope, Absence> {
    let runtime = runtime_for(daemon)?;
    // `Caller::scope("")` is the id prefix a turn of this caller carries, which
    // is all `principal_for` reads.
    Ok(PrincipalScope::from_resolver(
        runtime.corpus_principal.as_deref(),
        &caller.scope(""),
    ))
}

/// `GET /v1/corpora` — installed catalogue rows the caller's grant admits.
async fn list_corpora(
    caller: Caller,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Result<Response, Absence> {
    let scope = grant_scope(&daemon, &caller)?;
    let rows = catalog_entries(&daemon).await?;
    let total = rows.len();
    let corpora: Vec<_> = rows
        .into_iter()
        .filter(|e| e.status == "installed" && scope.admits(&e.id))
        .collect();
    tracing::debug!(
        caller = ?caller,
        catalogue = total,
        granted = corpora.len(),
        "granted_http: granted corpora served"
    );
    Ok(Json(CatalogResponse { corpora }).into_response())
}

/// `GET /v1/corpora/{corpus}/chunks/{chunk_id}?radius=` — the reading window,
/// refused by name for a corpus outside the caller's grant.
async fn reading_window(
    caller: Caller,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path((corpus, chunk_id)): Path<(String, u64)>,
    Query(NeighborQuery { radius }): Query<NeighborQuery>,
) -> Result<Response, Absence> {
    let scope = grant_scope(&daemon, &caller)?;
    if !scope.admits(&corpus) {
        tracing::info!(
            caller = ?caller,
            corpus = %corpus,
            "granted_http: reading window refused — corpus outside the caller's grant"
        );
        let who = match &caller {
            Caller::Keyed { sub } => format!("key '{sub}'"),
            Caller::Local => "this caller".to_string(),
        };
        return Err(Absence::at(
            StatusCode::FORBIDDEN,
            format!("corpus '{corpus}' is not in the corpus grant of {who}"),
        ));
    }
    tracing::debug!(caller = ?caller, corpus = %corpus, chunk_id, radius, "granted_http: reading window");
    Ok(crate::reading_http::neighbor_window(&daemon, &corpus, chunk_id, radius).await)
}

/// `GET /v1/tools` — the turn runtime's registry, with each tool's approval flag.
async fn list_tools(
    _: Caller,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Result<Response, Absence> {
    let registry = &runtime_for(&daemon)?.tools;
    let tools: Vec<HeldTool> = registry
        .descriptors()
        .into_iter()
        .map(|d| HeldTool {
            requires_approval: registry
                .get(&d.id)
                .is_ok_and(|t| !t.required_permissions().is_empty()),
            id: d.id,
            name: d.name,
            effect: d.effect,
        })
        .collect();
    tracing::debug!(tools = tools.len(), "granted_http: held tools served");
    Ok(Json(HeldToolsResponse { tools }).into_response())
}
