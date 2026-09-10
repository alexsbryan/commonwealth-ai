// SPDX-License-Identifier: AGPL-3.0-or-later
//! Recipe-author project store over the wire — `/v1/features/projects`
//! (sv-surface D6, second half).
//!
//! `features.db` is the recipe-author project layer: one row per
//! authoring project (`id`, `title`, `charter_md`, timestamps, an
//! archive stamp). `sovereign daemon run` has opened one since
//! `daemon_cmd/mod.rs:947` and handed it to the tool bundles only, so it
//! was reachable from an agent's tools and from nowhere else. The
//! desktop opened a SECOND handle on the same file
//! (`AppState.features`) and read it from seven Tauri commands.
//!
//! These three routes serve the store's whole surface — it has exactly
//! three methods (`list`, `get`, `provision_recipe_project`) — so the
//! twin closes for every read and write that IS a store operation.
//!
//! | Route | Store method |
//! |---|---|
//! | `GET  /v1/features/projects?include_archived=` | `list` |
//! | `GET  /v1/features/projects/{id}` | `get` |
//! | `POST /v1/features/projects` | `provision_recipe_project` |
//!
//! # Which desktop commands this retires, and which it does not
//!
//! Of `recipe_author_commands.rs`'s seven, TWO are store operations and
//! are served here: `recipe_author_list_projects` (:115, `list(false)`
//! plus a per-row sidecar fold) and `recipe_author_new_project` (:177,
//! whose store half is a provision followed by a `get`).
//!
//! The other five are NOT store reads, and saying so is the point of
//! this paragraph rather than a route that quietly does half of one:
//! `recipe_author_dashboard_state` (:442),
//! `recipe_author_save_edited_toml` (:553),
//! `recipe_author_link_recent_artifact` (:678),
//! `recipe_author_restore_checkpoint` (:735) and
//! `recipe_author_build_prelude` (:804) each go through
//! `sovereign_tools::recipe_author::RecipeProject`, which composes the
//! note store AND this store AND the user's on-disk artifact tree
//! (`~/.sovereign/recipes/<id>/recipe.toml`,
//! `~/.sovereign/workflows/<id>.toml`) — reading TOML, validating it
//! through the kind's parser, writing it atomically, rendering situated
//! context. Serving those means moving the artifact-TOML read/write and
//! the prelude renderer onto the daemon, which is a rung of its own and
//! not a door over an object the daemon already holds. The store half
//! they each perform IS served here; what stays is the composition.
//!
//! Loopback posture is `reading_http`'s, unchanged.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension, Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use sovereign_store::recipe_project_store::{RecipeProjectRow, RecipeProjectStore};

use crate::daemon::EmbeddedDaemon;
use crate::loopback_guard::enforce_localhost;

// ─── The wire projection ───────────────────────────────────────

/// One recipe-author project on the wire.
///
/// Every field of `RecipeProjectRow`, which is `#[derive(Debug, Clone)]`
/// and carries no serde impls — projected here for the `NoteEntry`
/// reason, and `Deserialize` as well as `Serialize` so a caller parses
/// back into the same struct the daemon emitted instead of a twin that
/// can drift.
///
/// `charter_md` crosses whole. It is the project's brief and the
/// sidebar renders a prefix of it; clipping it here would make the
/// route a decider about a length the caller owns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectEntry {
    pub id: String,
    pub title: String,
    pub charter_md: String,
    /// Unix seconds.
    pub created_at: i64,
    pub updated_at: i64,
    /// Unix seconds when the project was archived; `None` means active.
    pub archived_at: Option<i64>,
}

impl From<RecipeProjectRow> for ProjectEntry {
    fn from(r: RecipeProjectRow) -> Self {
        Self {
            id: r.id,
            title: r.title,
            charter_md: r.charter_md,
            created_at: r.created_at,
            updated_at: r.updated_at,
            archived_at: r.archived_at,
        }
    }
}

// ─── Request / response shapes ─────────────────────────────────

/// `?include_archived=` — `list`'s one argument.
///
/// Defaults to `false`, which is what the store documents as parity
/// with the old `FeatureStore::list` and the only value the desktop ever
/// passed. Named on the wire anyway: a caller that wants the archive
/// should be able to ask, and a default nobody can override is a
/// decision hiding as a constant.
#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub include_archived: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProjectListResponse {
    pub projects: Vec<ProjectEntry>,
}

/// `POST /v1/features/projects` — `provision_recipe_project`'s three
/// arguments. The id is the CALLER's: `RecipeProject::new_with_kind`
/// mints it from the project's essence before provisioning, and a route
/// that minted its own here would be a second identity decider (ARCH
/// §7.5).
#[derive(Debug, Deserialize)]
pub struct NewProjectRequest {
    pub id: String,
    pub title: String,
    pub charter_md: String,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

// ─── Router ────────────────────────────────────────────────────

/// The recipe-author project router. Mounted unconditionally on serving
/// daemons; a commission whose `features.db` would not open answers 503
/// with that named reason, which is the fact the daemon previously only
/// wrote to a log.
pub fn features_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route(
            "/v1/features/projects",
            get(list_projects).post(new_project),
        )
        .route("/v1/features/projects/{id}", get(get_project))
        .layer(axum::middleware::from_fn(
            crate::loopback_guard::loopback_only,
        ))
        .layer(Extension(daemon))
}

// ─── Handlers ──────────────────────────────────────────────────

/// GET `/v1/features/projects?include_archived=` — every project,
/// newest-updated first. Wire form of `RecipeProjectStore::list`.
///
/// An empty list is the right answer for a fresh install — the
/// recipe-author Welcome pane branches on it to show its first-timer
/// tutorial — so this must never become a 404.
async fn list_projects(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Query(q): Query<ListQuery>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let store = match store_for(&daemon) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    match store.list(q.include_archived).await {
        Ok(rows) => {
            tracing::debug!(
                include_archived = q.include_archived,
                returned = rows.len(),
                "features_http: projects listed",
            );
            Json(ProjectListResponse {
                projects: rows.into_iter().map(ProjectEntry::from).collect(),
            })
            .into_response()
        }
        Err(e) => internal_error(&e.to_string()),
    }
}

/// GET `/v1/features/projects/{id}` — one project. Wire form of
/// `RecipeProjectStore::get`.
///
/// 404 with the id named when the row is not there. The desktop command
/// this feeds (`recipe_author_dashboard_state`) reports exactly that
/// case as "`feature_id` not found", so the distinction survives the
/// crossing rather than arriving as a blank 200.
async fn get_project(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let store = match store_for(&daemon) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    match store.get(&id).await {
        Ok(Some(row)) => Json(ProjectEntry::from(row)).into_response(),
        Ok(None) => not_found(&format!("no recipe project `{id}`")),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// POST `/v1/features/projects` — provision a project. Wire form of
/// `RecipeProjectStore::provision_recipe_project`; answers the row it
/// wrote, so a caller does not have to `GET` it back to learn the
/// timestamps.
///
/// A duplicate id, or an empty one, is the store's `InvalidInput` and
/// arrives as a 409/400 rather than a 500: "that project already
/// exists" is the caller's mistake and is actionable, which a 500 is
/// not.
async fn new_project(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(body): Json<NewProjectRequest>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let store = match store_for(&daemon) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    match store
        .provision_recipe_project(&body.id, &body.title, &body.charter_md)
        .await
    {
        Ok(row) => {
            tracing::info!(project_id = %row.id, title = %row.title,
                "features_http: recipe project provisioned");
            (StatusCode::CREATED, Json(ProjectEntry::from(row))).into_response()
        }
        Err(sovereign_store::recipe_project_store::RecipeProjectError::InvalidInput(why)) => {
            // The store folds "empty id" and "already exists" into one
            // variant. `already exists` is a CONFLICT — a retry with the
            // same body will never succeed and the caller must pick a new
            // id — while an empty id is a malformed request. Distinguished
            // on the store's own words because the variant does not
            // separate them; the structural fix is two variants there, and
            // it belongs in that crate's commit, not this one.
            let status = if why.contains("already exists") {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            };
            tracing::debug!(reason = %why, %status, "features_http: provision refused");
            error_body(status, &why)
        }
        Err(e) => internal_error(&e.to_string()),
    }
}

// ─── Helpers ───────────────────────────────────────────────────

/// The daemon's own `RecipeProjectStore`. One lookup site, so no handler
/// can reach a different `features.db` than the tool bundles write to.
fn store_for(daemon: &Arc<EmbeddedDaemon>) -> Result<Arc<RecipeProjectStore>, Response> {
    daemon.features_store().map(Arc::clone).ok_or_else(|| {
        service_unavailable("this daemon has no recipe-author store (features.db did not open)")
    })
}

fn error_body(status: StatusCode, msg: &str) -> Response {
    (
        status,
        Json(ErrorBody {
            error: msg.to_string(),
        }),
    )
        .into_response()
}

fn not_found(msg: &str) -> Response {
    error_body(StatusCode::NOT_FOUND, msg)
}

fn internal_error(msg: &str) -> Response {
    error_body(StatusCode::INTERNAL_SERVER_ERROR, msg)
}

fn service_unavailable(msg: &str) -> Response {
    error_body(StatusCode::SERVICE_UNAVAILABLE, msg)
}
