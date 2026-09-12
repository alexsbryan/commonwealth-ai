// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recipe-author **project composition** over the wire —
//! `/v1/recipe-projects` (sv-surface D8, third family).
//!
//! All seven `recipe_author_commands.rs` commands, each composing
//! `sovereign_tools::recipe_author::RecipeProject` over three roots the
//! daemon already owns: the note store, the feature store, and the artifact
//! tree under `svrnmesh_root()` resolved IN THIS PROCESS. No path crosses the
//! wire and no root is a parameter — a caller names a `feature_id`.
//! `features_http` keeps its three store routes; nothing here re-derives
//! them. The TOML writes land under that artifact root, never a user-picked
//! path, and validate before they persist. One decider folded in on the way:
//! [`artifact_toml_path`], where the desktop had two spellings.
//!
//! Loopback posture is `reading_http`'s, unchanged.
//!
//! Could NOT cross, named (ARCH §18.3):
//! - **Workflow-kind validation** — rung 5 settled that sovereign-mesh takes
//!   no studio workflow dep, so a workflow TOML is reported UNJUDGED
//!   (`validation: None` + `validation_unavailable`) and `PUT …/toml` on one
//!   answers 501 rather than writing bytes nothing read.
//! - **`SOVEREIGN_DEV_FORCE_FIRST_RUN`** — a UI replay affordance that stays
//!   in front of the call; a daemon hiding real projects from every client
//!   because one wanted an onboarding screen would be lying to the others.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Extension, Path as AxumPath};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use corpus_engine::Recipe;
use sovereign_contracts::recipe::notes::{Note, NoteScope, RecipeNotes, ScopeFilter};
use sovereign_store::recipe_project_store::{RecipeProjectRow, RecipeProjectStore};
use sovereign_tools::recipe_author::{
    self, checkpoint::restore_checkpoint as do_restore_checkpoint, ArtifactKind, CheckpointMeta,
    ProjectSummary, RecipeProject,
};
use sovereign_tools::recipe_notes_adapter::NoteStoreRecipeNotes;

use crate::daemon::EmbeddedDaemon;
use crate::http_response::{internal_error, not_found, Absence};
use crate::loopback_guard::{LocalOnly, LoopbackRouter};

/// Why a workflow project's TOML carries no verdict from this host.
/// Stated once and sent verbatim wherever the absence surfaces.
const WORKFLOW_UNJUDGED: &str =
    "this host does not link the workflow parser (sovereign-mesh takes no studio workflow dep — \
     sv-surface rung 5), so a workflow artifact's TOML is not judged here. No verdict is inferred.";

// ─── Wire types ────────────────────────────────────────────────

/// One row in a project sidebar: the store row plus the sidecar summary,
/// plus the charter excerpt the sidebar renders as a tooltip.
///
/// The wire shapes — defined in `sovereign_contracts::daemon_wire` (svt-3)
/// so the desktop parses them without linking this crate, re-exported
/// here so the routes, their tests and the CLI keep naming this path.
/// `ArtifactKind` and `CheckpointMeta` came down with them.
pub use sovereign_contracts::daemon_wire::{
    DashboardNoteEntry, RecipeAuthorDashboardState, RecipeProjectListEntry, RecipeValidationReport,
    RestoreCheckpointOutcome,
};

/// The sidebar row for a project. The 200-char excerpt is the SIDEBAR's,
/// and it stays a host decision on purpose — unlike `ProjectEntry.charter_md`,
/// which crosses whole because that route serves the row. Two routes, two
/// questions: this one answers "what does the sidebar draw".
///
/// A free function rather than `RecipeProjectListEntry::from_row_and_summary`
/// because the type is foreign now (orphan rule) — same one implementation,
/// same two callers.
fn list_entry_from_row_and_summary(
    row: &RecipeProjectRow,
    summary: ProjectSummary,
) -> RecipeProjectListEntry {
    let mut excerpt = row.charter_md.chars().take(200).collect::<String>();
    if row.charter_md.chars().count() > 200 {
        excerpt.push('…');
    }
    RecipeProjectListEntry {
        feature_id: row.id.clone(),
        title: row.title.clone(),
        charter_excerpt: excerpt,
        artifact_kind: summary.artifact_kind,
        recipe_id: summary.recipe_id,
        current_sample_size: summary.current_sample_size,
        last_test_status: summary.last_test_status,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

/// "Nothing drafted" — no artifact to validate yet, as distinct from "we
/// tried and it failed".
fn validation_nothing_drafted() -> RecipeValidationReport {
    RecipeValidationReport {
        ok: false,
        errors: Vec::new(),
        no_recipe: true,
        enrichment_ready: false,
        warnings: Vec::new(),
        notes: Vec::new(),
    }
}

/// A blocking verdict: the artifact was judged and refused.
fn validation_failed(errors: Vec<String>) -> RecipeValidationReport {
    RecipeValidationReport {
        ok: false,
        errors,
        no_recipe: false,
        enrichment_ready: false,
        warnings: Vec::new(),
        notes: Vec::new(),
    }
}

/// `POST /v1/recipe-projects` — provision a project and lay down its
/// artifact tree.
#[derive(Debug, Deserialize)]
pub struct NewProjectRequest {
    pub title: String,
    pub charter_md: String,
    /// `#[serde(default)]` → a caller that omits it creates a `Recipe`
    /// project, which is what every caller did before the tag existed.
    #[serde(default)]
    pub artifact_kind: ArtifactKind,
}

/// `PUT /v1/recipe-projects/{id}/toml` — validate, then write if valid.
#[derive(Debug, Deserialize)]
pub struct SaveTomlRequest {
    pub edited_toml: String,
}

/// `POST /v1/recipe-projects/{id}/link-recent-artifact`.
#[derive(Debug, Deserialize)]
pub struct LinkRecentRequest {
    /// The turn's start time. Only an artifact written at or after it is
    /// linked, so a chat-only turn links nothing.
    pub since_unix: i64,
}

/// The artifact id that was linked, or `null` when the turn wrote none.
/// An explicit null, not an absent key: an absent key is
/// indistinguishable from an old host (ARCH §18.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkRecentResponse {
    pub artifact_id: Option<String>,
}

/// The per-turn situated-context preamble.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreludeResponse {
    pub prelude: String,
}

// ─── Router ────────────────────────────────────────────────────

/// The recipe-author project router. Mounted unconditionally on serving
/// daemons; a commission missing either store answers 503 naming WHICH,
/// because "notes.db would not open" and "features.db would not open"
/// have different fixes.
pub fn recipe_project_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/v1/recipe-projects", get(list_projects).post(new_project))
        .route(
            "/v1/recipe-projects/{feature_id}/dashboard",
            get(dashboard_state),
        )
        .route(
            "/v1/recipe-projects/{feature_id}/toml",
            put(save_edited_toml),
        )
        .route(
            "/v1/recipe-projects/{feature_id}/link-recent-artifact",
            post(link_recent_artifact),
        )
        .route(
            "/v1/recipe-projects/{feature_id}/checkpoints/{checkpoint_id}/restore",
            post(restore_checkpoint),
        )
        .route("/v1/recipe-projects/{feature_id}/prelude", get(prelude))
        .localhost_only_with(daemon)
}

// ─── Handlers ──────────────────────────────────────────────────

/// GET `/v1/recipe-projects` — every project with its sidecar summary,
/// newest-updated first.
///
/// A row whose sidecar will not load is still LISTED, with a default
/// summary, exactly as the desktop did: the project exists and the
/// operator must be able to pick it. That is a degraded row, not a
/// substituted fact — `artifact_kind` defaults to `Recipe` and the
/// summary-only fields come back `null`, which is what "not read" looks
/// like on this payload.
async fn list_projects(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> Result<Response, Absence> {
    let (notes, features) = handles(&daemon)?;
    let rows = match features.list(false).await {
        Ok(r) => r,
        Err(e) => return Ok(internal_error(&format!("list projects: {e}"))),
    };
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let summary =
            match RecipeProject::load(&row.id, Arc::clone(&notes), Arc::clone(&features)).await {
                Ok(p) => p.read_summary().unwrap_or_else(|_| default_summary(&row)),
                Err(_) => default_summary(&row),
            };
        out.push(list_entry_from_row_and_summary(&row, summary));
    }
    out.sort_by_key(|e| std::cmp::Reverse(e.updated_at));
    tracing::debug!(returned = out.len(), "recipe_project_http: projects listed");
    Ok(Json(out).into_response())
}

/// POST `/v1/recipe-projects` — provision the row AND the artifact tree,
/// then answer the sidebar entry a refresh would render.
///
/// The id is minted by `RecipeProject::new_with_kind` from the project's
/// essence, here, on the host that provisions — one identity decider
/// (ARCH §7.5). `features_http`'s `POST` takes a caller-supplied id
/// precisely because it is the STORE's door and this is the composition's.
async fn new_project(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(body): Json<NewProjectRequest>,
) -> Result<Response, Absence> {
    let (notes, features) = handles(&daemon)?;
    let title = body.title.trim();
    if title.is_empty() {
        return Err(Absence::invalid("title cannot be empty"));
    }
    let project = match RecipeProject::new_with_kind(
        title,
        &body.charter_md,
        body.artifact_kind,
        Arc::clone(&notes),
        Arc::clone(&features),
    )
    .await
    {
        Ok(p) => p,
        Err(e) => return Ok(internal_error(&format!("new project: {e}"))),
    };
    // Re-read the row + summary so the entry mirrors exactly what a
    // subsequent list would render.
    let row = match features.get(project.feature_id()).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return Ok(internal_error("project row vanished after creation"));
        }
        Err(e) => return Ok(internal_error(&format!("get row: {e}"))),
    };
    let summary = project
        .read_summary()
        .unwrap_or_else(|_| default_summary(&row));
    tracing::info!(feature_id = %row.id, title = %row.title,
        kind = body.artifact_kind.label(), "recipe_project_http: project provisioned");
    Ok((
        StatusCode::CREATED,
        Json(list_entry_from_row_and_summary(&row, summary)),
    )
        .into_response())
}

/// GET `/v1/recipe-projects/{feature_id}/dashboard` — the one big read.
async fn dashboard_state(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    AxumPath(feature_id): AxumPath<String>,
) -> Result<Response, Absence> {
    let (notes, features) = handles(&daemon)?;
    let project = load_project(&feature_id, &notes, &features).await?;
    let row = match features.get(&feature_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return Ok(not_found(&format!("no recipe project `{feature_id}`"))),
        Err(e) => return Ok(internal_error(&format!("get row: {e}"))),
    };
    let summary = project
        .read_summary()
        .unwrap_or_else(|_| default_summary(&row));

    let (recipe_path, recipe_toml) = match summary.recipe_id.as_deref() {
        Some(aid) => match artifact_toml_path(summary.artifact_kind, aid) {
            Some(path) => {
                let text = std::fs::read_to_string(&path).ok();
                (Some(path.to_string_lossy().into_owned()), text)
            }
            None => (None, None),
        },
        None => (None, None),
    };

    // Feature-scoped notes once, partitioned by kind. 200 is generous —
    // each card takes its own sub-slice. Newest first.
    let scope = ScopeFilter {
        scopes: vec![NoteScope::Feature],
        feature_id: Some(feature_id.clone()),
    };
    let raw = match notes
        .read_notes_scoped(None, &[], &[], &[], 200, false, &scope)
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(internal_error(&format!("read notes: {e}"))),
    };
    let mut decisions = Vec::new();
    let mut research_findings = Vec::new();
    let mut capability_requests = Vec::new();
    let mut recipe_issues = Vec::new();
    let mut deferred_questions = Vec::new();
    for note in raw {
        match note.kind.as_str() {
            "decision" => decisions.push(DashboardNoteEntry::from(note)),
            "research_finding" => research_findings.push(DashboardNoteEntry::from(note)),
            "capability_request" => capability_requests.push(DashboardNoteEntry::from(note)),
            "recipe_issue" => recipe_issues.push(DashboardNoteEntry::from(note)),
            "deferred_question" => deferred_questions.push(DashboardNoteEntry::from(note)),
            // `checkpoint` / `checkpoint_restored` surface through the
            // checkpoint list itself; dropping them keeps each card's
            // slice focused.
            _ => {}
        }
    }

    let checkpoints = match project.list_checkpoints() {
        Ok(c) => c,
        Err(e) => return Ok(internal_error(&format!("list checkpoints: {e}"))),
    };

    let (validation, validation_unavailable) =
        validate_artifact_toml(summary.artifact_kind, recipe_toml.as_deref());

    tracing::debug!(
        %feature_id,
        kind = summary.artifact_kind.label(),
        checkpoints = checkpoints.len(),
        judged = validation.is_some(),
        "recipe_project_http: dashboard served",
    );
    Ok(Json(RecipeAuthorDashboardState {
        feature_id,
        title: row.title,
        charter_md: row.charter_md,
        artifact_kind: summary.artifact_kind,
        recipe_id: summary.recipe_id,
        recipe_path,
        recipe_toml,
        current_sample_size: summary.current_sample_size,
        last_test_status: summary.last_test_status,
        last_test_at: summary.last_test_at,
        created_at: row.created_at,
        updated_at: row.updated_at,
        decisions,
        research_findings,
        capability_requests,
        recipe_issues,
        deferred_questions,
        checkpoints,
        validation,
        validation_unavailable,
    })
    .into_response())
}

/// PUT `/v1/recipe-projects/{feature_id}/toml` — validate, then write.
///
/// A recipe that does not parse is answered `200` with `ok: false` and
/// the parse errors, and NOTHING is written — the editor keeps the
/// in-flight text and the author re-saves. That is a successful
/// validation with a negative verdict, not a failed request.
///
/// A workflow project is answered `501` with [`WORKFLOW_UNJUDGED`]: this
/// host cannot validate it and will not write unvalidated bytes over a
/// working artifact.
async fn save_edited_toml(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    AxumPath(feature_id): AxumPath<String>,
    Json(body): Json<SaveTomlRequest>,
) -> Result<Response, Absence> {
    let (notes, features) = handles(&daemon)?;
    let project = load_project(&feature_id, &notes, &features).await?;
    let summary = match project.read_summary() {
        Ok(s) => s,
        Err(e) => return Ok(internal_error(&format!("read summary: {e}"))),
    };
    let kind = summary.artifact_kind;
    let Some(artifact_id) = summary.recipe_id else {
        return Err(Absence::invalid(format!(
            "this project has no {} yet — draft one with the agent before editing",
            kind.label()
        )));
    };

    let (report, unavailable) = validate_artifact_toml(kind, Some(&body.edited_toml));
    let Some(report) = report else {
        return Err(Absence::unsupported(
            unavailable.unwrap_or_else(|| WORKFLOW_UNJUDGED.to_string()),
        ));
    };
    if !report.ok {
        return Ok(Json(report).into_response());
    }

    // `.part` → rename, mirroring the structured-write tools, so an agent
    // write and a hand edit are indistinguishable on disk and the prelude
    // picks either up on its next disk re-read.
    let Some(path) = artifact_toml_path(kind, &artifact_id) else {
        return Ok(internal_error(
            "cannot locate the artifact directory (no home dir)",
        ));
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Ok(internal_error(&format!("create {}: {e}", parent.display())));
        }
    }
    let part = path.with_extension("toml.part");
    if let Err(e) = std::fs::write(&part, body.edited_toml.as_bytes()) {
        return Ok(internal_error(&format!("write {}: {e}", part.display())));
    }
    if let Err(e) = std::fs::rename(&part, &path) {
        return Ok(internal_error(&format!(
            "rename {} → {}: {e}",
            part.display(),
            path.display()
        )));
    }
    tracing::info!(
        %feature_id, %artifact_id, kind = kind.label(),
        "recipe_project_http: hand-edited artifact TOML written",
    );
    Ok(Json(report).into_response())
}

/// POST `.../link-recent-artifact` — register the artifact the agent just
/// wrote onto the project's summary, so the dashboard surfaces it.
///
/// Idempotent and cheap; a chat surface calls it on every turn-complete.
async fn link_recent_artifact(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    AxumPath(feature_id): AxumPath<String>,
    Json(body): Json<LinkRecentRequest>,
) -> Result<Response, Absence> {
    let (notes, features) = handles(&daemon)?;
    let project = load_project(&feature_id, &notes, &features).await?;
    let mut summary = match project.read_summary() {
        Ok(s) => s,
        Err(e) => return Ok(internal_error(&format!("read summary: {e}"))),
    };
    let dir = match artifact_root(summary.artifact_kind) {
        Some(d) => d,
        None => {
            return Ok(internal_error(
                "cannot locate the artifact directory (no home dir)",
            ))
        }
    };
    let Some(id) = find_recent_artifact(summary.artifact_kind, &dir, body.since_unix) else {
        return Ok(Json(LinkRecentResponse { artifact_id: None }).into_response());
    };
    if summary.recipe_id.as_deref() != Some(id.as_str()) {
        summary.recipe_id = Some(id.clone());
        summary.updated_at = sovereign_core::time::unix_now();
        if let Err(e) = project.write_summary(&summary) {
            return Ok(internal_error(&format!("write summary: {e}")));
        }
        tracing::info!(%feature_id, artifact_id = %id, kind = summary.artifact_kind.label(),
            "recipe_project_http: linked freshly-authored artifact");
    }
    Ok(Json(LinkRecentResponse {
        artifact_id: Some(id),
    })
    .into_response())
}

/// POST `.../checkpoints/{checkpoint_id}/restore` — restore a snapshot
/// and lay down the restore-anchor checkpoint.
///
/// The session id is stable (`daemon-recipe-author-<feature_id>`) so the
/// resulting `kind=checkpoint_restored` note attributes the act to a
/// filterable actor rather than to a fresh uuid. The desktop's spelling
/// was `desktop-recipe-author-<id>`; NAMED DELTA — the daemon is the
/// writer now, and a note claiming the desktop wrote it would be false
/// the moment the CLI restores one.
async fn restore_checkpoint(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    AxumPath((feature_id, checkpoint_id)): AxumPath<(String, String)>,
) -> Result<Response, Absence> {
    let (notes, features) = handles(&daemon)?;
    let project = load_project(&feature_id, &notes, &features).await?;
    // Best-effort: a project with no artifact yet just gets a
    // restore-anchor checkpoint without touching disk. The write path is
    // resolved by the project's kind inside `restore_checkpoint`.
    let artifact_id = project.read_summary().ok().and_then(|s| s.recipe_id);
    let session_id = format!("daemon-recipe-author-{feature_id}");
    Ok(
        match do_restore_checkpoint(
            &project,
            &checkpoint_id,
            artifact_id.as_deref(),
            None,
            &session_id,
        )
        .await
        {
            Ok(outcome) => {
                tracing::info!(%feature_id, %checkpoint_id, new = %outcome.checkpoint_id,
                "recipe_project_http: checkpoint restored");
                Json(RestoreCheckpointOutcome {
                    new_checkpoint_id: outcome.checkpoint_id,
                    source_checkpoint_id: checkpoint_id,
                })
                .into_response()
            }
            Err(e) => checkpoint_error(&checkpoint_id, &e.to_string()),
        },
    )
}

/// GET `/v1/recipe-projects/{feature_id}/prelude` — the per-turn situated
/// context block a chat surface prepends to the partner's message.
///
/// Cheap (~5KB, no network), so re-rendering every turn is the intended
/// use and keeps the agent's view of project state fresh.
async fn prelude(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    AxumPath(feature_id): AxumPath<String>,
) -> Result<Response, Absence> {
    let (notes, features) = handles(&daemon)?;
    let project = load_project(&feature_id, &notes, &features).await?;
    let situated = match recipe_author::situated_context::render(&project).await {
        Ok(s) => s,
        Err(e) => return Ok(internal_error(&format!("render situated context: {e}"))),
    };
    let summary = match project.read_summary() {
        Ok(s) => s,
        Err(e) => return Ok(internal_error(&format!("read summary: {e}"))),
    };
    let label = summary.artifact_kind.label();
    let (artifact_block, validation_block) = match &summary.recipe_id {
        Some(artifact_id) => match artifact_toml_path(summary.artifact_kind, artifact_id) {
            Some(path) => match std::fs::read_to_string(&path) {
                Ok(toml) => (
                    format!(
                        "\n[Current {label} TOML]\nPath: {}\n```toml\n{}\n```\n",
                        path.display(),
                        toml.trim_end(),
                    ),
                    inline_validate(summary.artifact_kind, &toml),
                ),
                Err(e) => (
                    format!(
                        "\n[Current {label} TOML]\nNot readable at {}: {e}\n",
                        path.display()
                    ),
                    String::new(),
                ),
            },
            None => (
                format!("\n[Current {label} TOML]\nNo artifact directory on this host.\n"),
                String::new(),
            ),
        },
        None => (
            format!(
                "\n[Current {label} TOML]\n(no {label} drafted yet — use \
                 `{label}_write_structured` to create one)\n"
            ),
            String::new(),
        ),
    };
    let prelude =
        format!("[Project state]\n{situated}{artifact_block}{validation_block}\n[Partner says]\n");
    tracing::debug!(%feature_id, bytes = prelude.len(), "recipe_project_http: prelude rendered");
    Ok(Json(PreludeResponse { prelude }).into_response())
}

// ─── Validation (the one decider for "is this artifact valid?") ─

/// Validate artifact TOML, dispatched by [`ArtifactKind`].
///
/// Returns `(Some(report), None)` when a verdict was reached and
/// `(None, Some(reason))` when it was not. Exactly one is `Some`, which
/// is what keeps "unjudged" from collapsing into "failed" (ARCH §18.1,
/// §18.3).
///
/// The recipe arm is the SAME pass `svrn recipe validate` runs, reused
/// whole rather than re-derived: parsing is not validity, and a green
/// pill over a recipe that cannot extract what it declares is a false
/// verdict.
fn validate_artifact_toml(
    kind: ArtifactKind,
    artifact_toml: Option<&str>,
) -> (Option<RecipeValidationReport>, Option<String>) {
    let Some(toml_str) = artifact_toml else {
        // "Nothing drafted" is judgeable for either kind — no parser
        // needed to see that there is no text.
        return (Some(validation_nothing_drafted()), None);
    };
    match kind {
        ArtifactKind::Recipe => match Recipe::from_toml(toml_str) {
            Ok(recipe) => {
                let v = corpus_engine::testing::validate_recipe_offline(&recipe);
                (
                    Some(RecipeValidationReport {
                        ok: v.errors.is_empty(),
                        errors: v.errors,
                        no_recipe: false,
                        enrichment_ready: recipe.produces_enriched_atoms(),
                        warnings: v.warnings,
                        notes: v.notes,
                    }),
                    None,
                )
            }
            Err(e) => (
                Some(validation_failed(split_parse_errors(&e.to_string()))),
                None,
            ),
        },
        ArtifactKind::Workflow => (None, Some(WORKFLOW_UNJUDGED.to_string())),
    }
}

/// The prelude's inline verdict. Empty string when the artifact parses
/// cleanly (the agent does not need a "passes" notice every turn) AND
/// when this host cannot judge — the prelude is prose for a model, and
/// a paragraph about the host's crate graph is not project state. The
/// dashboard is where the unjudged fact is reported to a caller that can
/// act on it.
fn inline_validate(kind: ArtifactKind, toml: &str) -> String {
    match kind {
        ArtifactKind::Recipe => match Recipe::from_toml(toml) {
            Ok(_) => String::new(),
            Err(e) => format!("\n[Latest validation]\nRecipe does NOT parse. First error:\n{e}\n"),
        },
        ArtifactKind::Workflow => String::new(),
    }
}

/// Split a parser error into discrete guidance rows. Recipe errors carry
/// blank-line-separated blocks from `translate_parse_error` — render each
/// intact. An empty message yields one fallback row rather than an empty
/// list, so `ok: false` never arrives with nothing to show.
fn split_parse_errors(message: &str) -> Vec<String> {
    if message.is_empty() {
        return vec!["artifact failed to parse (no message)".to_string()];
    }
    message
        .split("\n\n")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

// ─── Artifact tree ─────────────────────────────────────────────

/// The root a kind's artifacts live under, on THIS host.
fn artifact_root(kind: ArtifactKind) -> Option<PathBuf> {
    match kind {
        ArtifactKind::Recipe => recipe_author::local_recipes_dir().ok(),
        ArtifactKind::Workflow => recipe_author::local_workflows_dir().ok(),
    }
}

/// The on-disk TOML for `(kind, artifact_id)`: a recipe at
/// `<root>/<id>/recipe.toml`, a workflow at `<root>/<id>.toml`.
///
/// THE one resolver — see the header on the desktop's two spellings.
fn artifact_toml_path(kind: ArtifactKind, artifact_id: &str) -> Option<PathBuf> {
    let root = artifact_root(kind)?;
    Some(match kind {
        ArtifactKind::Recipe => root.join(artifact_id).join("recipe.toml"),
        ArtifactKind::Workflow => root.join(format!("{artifact_id}.toml")),
    })
}

/// The id of the newest artifact under `dir` whose TOML was (re)written
/// at or after `since_unix` — the one the agent wrote this turn. `None`
/// when nothing was written in the window, so a turn with no draft never
/// mislinks.
fn find_recent_artifact(
    kind: ArtifactKind,
    dir: &std::path::Path,
    since_unix: i64,
) -> Option<String> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut best: Option<(String, i64)> = None;
    for entry in rd.flatten() {
        let path = entry.path();
        let (id, toml_path) = match kind {
            ArtifactKind::Recipe => {
                if !path.is_dir() {
                    continue;
                }
                let Some(id) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                (id.to_string(), path.join("recipe.toml"))
            }
            ArtifactKind::Workflow => {
                if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                    continue;
                }
                let Some(id) = path.file_stem().and_then(|n| n.to_str()) else {
                    continue;
                };
                (id.to_string(), path.clone())
            }
        };
        let Some(mt) = std::fs::metadata(&toml_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
        else {
            continue;
        };
        if mt >= since_unix && best.as_ref().map(|(_, b)| mt > *b).unwrap_or(true) {
            best = Some((id, mt));
        }
    }
    best.map(|(id, _)| id)
}

// ─── Helpers ───────────────────────────────────────────────────

/// The daemon's note + feature handles, as the composition wants them.
/// ONE lookup site, so no handler can compose over a different pair.
///
/// The 503 names WHICH store is missing: `notes.db` and `features.db` are
/// opened by different code with different failure modes, and one message
/// covering both would send an operator to the wrong file.
fn handles(
    daemon: &Arc<EmbeddedDaemon>,
) -> Result<(Arc<dyn RecipeNotes>, Arc<RecipeProjectStore>), Absence> {
    let Some(note_store) = daemon.notes_store().map(Arc::clone) else {
        return Err(Absence::unavailable(
            "this daemon has no note store (notes.db did not open) — the recipe-author \
             workspace composes over it",
        ));
    };
    let Some(features) = daemon.features_store().map(Arc::clone) else {
        return Err(Absence::unavailable(
            "this daemon has no recipe-author store (features.db did not open)",
        ));
    };
    // Wrap the concrete NoteStore in the seam adapter so the recipe-author
    // crate sees the contract, not corpus-engine.
    let notes: Arc<dyn RecipeNotes> = Arc::new(NoteStoreRecipeNotes::new(note_store));
    Ok((notes, features))
}

/// Load a project, or say which absence it is.
///
/// `RecipeProject::load` folds "no such row" and "the state machine
/// refuses this row" into one error, so an unknown id and a project in a
/// wrong state both arrive as text. The id is echoed into a 404 when the
/// row genuinely is not there, which the caller distinguishes by the
/// message; the STRUCTURAL fix is a typed error in the recipe-author
/// crate and it belongs in that crate's commit.
async fn load_project(
    feature_id: &str,
    notes: &Arc<dyn RecipeNotes>,
    features: &Arc<RecipeProjectStore>,
) -> Result<RecipeProject, Absence> {
    match RecipeProject::load(feature_id, Arc::clone(notes), Arc::clone(features)).await {
        Ok(p) => Ok(p),
        Err(e) => {
            let msg = e.to_string();
            let status = match features.get(feature_id).await {
                Ok(None) => StatusCode::NOT_FOUND,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            Err(Absence::at(
                status,
                format!("load project `{feature_id}`: {msg}"),
            ))
        }
    }
}

/// The summary a row falls back to when its sidecar will not read.
/// `Recipe` is the default kind because that is what `ArtifactKind`'s own
/// `#[default]` says and a pre-tag `project.json` decodes as.
fn default_summary(row: &RecipeProjectRow) -> ProjectSummary {
    ProjectSummary {
        feature_id: row.id.clone(),
        title: row.title.clone(),
        artifact_kind: ArtifactKind::Recipe,
        recipe_id: None,
        current_sample_size: None,
        last_test_status: None,
        last_test_at: None,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

/// A restore failure whose cause is an unknown checkpoint id is the
/// caller's stale list (404); anything else is ours.
fn checkpoint_error(checkpoint_id: &str, msg: &str) -> Response {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("not found") || lower.contains("no such file") {
        not_found(&format!("no checkpoint `{checkpoint_id}`: {msg}"))
    } else {
        internal_error(msg)
    }
}
