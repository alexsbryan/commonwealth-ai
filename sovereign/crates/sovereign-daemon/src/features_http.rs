// SPDX-License-Identifier: AGPL-3.0-or-later
//! Recipe-author project store over the wire — `/v1/features/projects`
//! (sv-surface D6, second half).
//!
//! Three routes for `RecipeProjectStore`'s whole surface — `list`, `get`,
//! `provision_recipe_project` — over the `features.db` handle the daemon has
//! opened since `daemon_cmd/mod.rs:947` and handed to the tool bundles only.
//! The desktop held a SECOND handle on the same file.
//!
//! Loopback posture is `reading_http`'s, unchanged.
//!
//! Does NOT cross: the other five `recipe_author_commands.rs` commands
//! compose this store AND the note store AND an on-disk artifact tree — their
//! store halves are here, the composition is `recipe_project_http`'s.

use std::sync::Arc;

use axum::extract::{Extension, Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use sovereign_store::recipe_project_store::{RecipeProjectRow, RecipeProjectStore};

use crate::daemon::EmbeddedDaemon;
use crate::http_response::{internal_error, json_error, not_found, Absence};
use crate::loopback_guard::{LocalOnly, LoopbackRouter};

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
        .localhost_only_with(daemon)
}

// ─── Handlers ──────────────────────────────────────────────────

/// GET `/v1/features/projects?include_archived=` — every project,
/// newest-updated first. Wire form of `RecipeProjectStore::list`.
///
/// An empty list is the right answer for a fresh install — the
/// recipe-author Welcome pane branches on it to show its first-timer
/// tutorial — so this must never become a 404.
async fn list_projects(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Query(q): Query<ListQuery>,
) -> Result<Response, Absence> {
    let store = store_for(&daemon)?;
    Ok(match store.list(q.include_archived).await {
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
    })
}

/// GET `/v1/features/projects/{id}` — one project. Wire form of
/// `RecipeProjectStore::get`.
///
/// 404 with the id named when the row is not there. The desktop command
/// this feeds (`recipe_author_dashboard_state`) reports exactly that
/// case as "`feature_id` not found", so the distinction survives the
/// crossing rather than arriving as a blank 200.
async fn get_project(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(id): Path<String>,
) -> Result<Response, Absence> {
    let store = store_for(&daemon)?;
    Ok(match store.get(&id).await {
        Ok(Some(row)) => Json(ProjectEntry::from(row)).into_response(),
        Ok(None) => not_found(&format!("no recipe project `{id}`")),
        Err(e) => internal_error(&e.to_string()),
    })
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
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(body): Json<NewProjectRequest>,
) -> Result<Response, Absence> {
    let store = store_for(&daemon)?;
    Ok(
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
                json_error(status, &why)
            }
            Err(e) => internal_error(&e.to_string()),
        },
    )
}

// ─── Helpers ───────────────────────────────────────────────────

/// The daemon's own `RecipeProjectStore`. One lookup site, so no handler
/// can reach a different `features.db` than the tool bundles write to.
fn store_for(daemon: &Arc<EmbeddedDaemon>) -> Result<Arc<RecipeProjectStore>, Absence> {
    daemon.features_store().map(Arc::clone).ok_or_else(|| {
        Absence::unavailable("this daemon has no recipe-author store (features.db did not open)")
    })
}
