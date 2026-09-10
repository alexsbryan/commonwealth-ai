// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tauri commands powering the desktop **Recipe Author Workspace** (M2).
//! The workspace is a two-panel surface (conversation ⅔, dashboard ⅓)
//! that lets a non-technical domain expert build a svrnmesh corpus +
//! investigation schema by conversation, while every meaningful agent /
//! partner action surfaces on a live dashboard.
//!
//! These commands are deliberately *coarse*. The dashboard reads as one
//! unit (`recipe_author_dashboard_state`) rather than per-card — the
//! cards are presentational, the data shape is a single struct.
//!
//! # Where the project is composed (sv-surface D8)
//!
//! All seven commands hold NO `RecipeProject`, no `NoteStore` and no
//! `RecipeProjectStore`. Each is one call onto
//! `sovereign_mesh::recipe_project_http`'s `/v1/recipe-projects` routes
//! over [`ra_client`]. The composition — the row, the sidecar summary,
//! the checkpoint list, the notes partition, the artifact tree under the
//! data root — is the daemon's, over the stores it opened.
//!
//! That deletes, besides the two store handles:
//!
//!   * the artifact-path resolution, which the desktop spelled TWICE
//!     (`artifact_toml_path` for the dashboard/save, and a second
//!     `sovereign_root_dir().join("recipes")` walk in the prelude) —
//!     one decider now, on the host (ARCH §10.6).
//!   * the atomic `.toml.part` → rename write. The host writes it, UNDER
//!     ITS OWN DATA ROOT, which is the point: an attached desktop writing
//!     into its own dir put the recipe where the daemon never looks.
//!   * `find_recent_artifact`'s mtime scan, the offline validation pass,
//!     the situated-context render and the inline prelude verdict.
//!
//! # What could not cross, named rather than absorbed (ARCH §18.3)
//!
//! **Workflow-kind validation.** `Workflow::parse` is a studio dependency
//! the layer map forbids `sovereign-mesh`, so the host cannot judge a
//! workflow TOML and says so instead of guessing. Two user-visible
//! consequences, both deliberate:
//!
//!   * the dashboard's validation card for a WORKFLOW project renders the
//!     host's "not judged here" sentence as its message, where it used to
//!     render a parse verdict. It is mapped into the card's existing
//!     shape by [`unjudged_report`] because the card's props are not
//!     nullable; the sentence is the host's own words, verbatim, so what
//!     the partner reads is "nobody judged this", never "this failed".
//!   * saving a hand-edited workflow TOML is now an `Err` carrying that
//!     sentence: the host answers 501 rather than writing bytes it could
//!     not judge over a working artifact. A recipe saves exactly as
//!     before.
//!
//! **`SOVEREIGN_DEV_FORCE_FIRST_RUN`** stays desktop-side: it replays
//! this app's onboarding surface, and the daemon has no onboarding.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::State;

use sovereign_tools::recipe_author::{ArtifactKind, CheckpointMeta};

use crate::state::AppState;

/// The wire types, re-exported rather than mirrored: field-for-field what
/// this module used to declare, so the webview sees the same bytes.
pub use sovereign_mesh::recipe_project_http::{
    DashboardNoteEntry, RecipeProjectListEntry, RecipeValidationReport, RestoreCheckpointOutcome,
};

/// The client for the daemon's recipe-project surface — the same
/// `notes.db` + `features.db` this file used to reach through
/// `AppState`, reached over loopback instead (sv-surface D8).
fn ra_client(state: &AppState) -> sovereign_turn_client::TurnClient {
    sovereign_turn_client::TurnClient::new(state.client_base_url())
}

// ─── Project summary (sidebar) ───────────────────────────────

#[tauri::command]
pub async fn recipe_author_list_projects(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<RecipeProjectListEntry>, String> {
    // Dev: SOVEREIGN_DEV_FORCE_FIRST_RUN hides real projects (in-memory
    // only — they stay on disk) so the recipe-author Welcome shows its
    // first-timer tutorial CTA, replaying the onboarding surface. Stays
    // here: it is a fact about THIS app's UI, not about the store.
    if crate::dev_flags::force_first_run() {
        return Ok(Vec::new());
    }
    ra_client(&state)
        .recipe_projects::<RecipeProjectListEntry>()
        .await
        .map_err(|e| format!("recipe_author_list_projects: {e}"))
}

// ─── Project create ───────────────────────────────────────────

/// The frontend's new-project form. `Serialize` as well, because it is
/// also the route's request body — one declaration of the three fields
/// rather than a DTO and a body struct that can drift.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewProjectRequest {
    pub title: String,
    pub charter_md: String,
    /// What the project authors. `#[serde(default)]` → an existing frontend that
    /// omits it creates a `Recipe` project, unchanged; a workflow-author surface
    /// passes `"workflow"`.
    #[serde(default)]
    pub artifact_kind: ArtifactKind,
}

/// Provision the project row AND its artifact tree. The feature id is
/// minted BY THE HOST from the project's essence (ARCH §7.5), so this
/// sends no id and reads the one it gets back.
#[tauri::command]
pub async fn recipe_author_new_project(
    state: State<'_, Arc<AppState>>,
    req: NewProjectRequest,
) -> Result<RecipeProjectListEntry, String> {
    if req.title.trim().is_empty() {
        return Err("title cannot be empty".into());
    }
    ra_client(&state)
        .recipe_project_create::<NewProjectRequest, RecipeProjectListEntry>(&req)
        .await
        .map_err(|e| format!("recipe_author_new_project: {e}"))
}

// ─── Dashboard state (the one big read) ───────────────────────

/// The single struct the dashboard reads on every poll. Coarse on
/// purpose — the cards are pure presentation over slices of this.
///
/// Field-for-field the route's `RecipeAuthorDashboardState` except for
/// `validation`, which is NOT an `Option` here. The route reports
/// "unjudged" as `validation: null` + `validation_unavailable: "<why>"`,
/// and this IPC contract cannot: `ProjectDashboard.svelte` reads
/// `dashboard.validation.enrichment_ready` unguarded. [`unjudged_report`]
/// carries the host's sentence into the card instead — see the module
/// header. Making the card nullable is the owed frontend change.
#[derive(Debug, Clone, Serialize)]
pub struct RecipeAuthorDashboardState {
    pub feature_id: String,
    pub title: String,
    pub charter_md: String,
    /// `"recipe"` or `"workflow"` — lets the frontend label the workspace and
    /// branch its validation card. `recipe_*` fields below carry the artifact
    /// regardless of kind (frontend-compat field names).
    pub artifact_kind: ArtifactKind,
    pub recipe_id: Option<String>,
    pub recipe_path: Option<String>,
    pub recipe_toml: Option<String>,
    pub current_sample_size: Option<u64>,
    pub last_test_status: Option<String>,
    pub last_test_at: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub decisions: Vec<DashboardNoteEntry>,
    pub research_findings: Vec<DashboardNoteEntry>,
    pub capability_requests: Vec<DashboardNoteEntry>,
    pub recipe_issues: Vec<DashboardNoteEntry>,
    pub deferred_questions: Vec<DashboardNoteEntry>,
    pub checkpoints: Vec<CheckpointMeta>,
    pub validation: RecipeValidationReport,
}

/// The host's "I did not judge this" reason, in the card's shape.
///
/// `ok: false` is a lossy encoding of could-not-judge and is used ONLY
/// because the card has no third state (ARCH §18.1). What keeps it honest
/// is `errors` carrying the host's OWN sentence verbatim — the reader sees
/// "this host does not link the workflow parser … No verdict is inferred",
/// not a fabricated parse failure. Fabricating `ok: true` here would be
/// the green pill over an unjudged artifact that §18.3 forbids outright.
fn unjudged_report(reason: String) -> RecipeValidationReport {
    RecipeValidationReport {
        ok: false,
        errors: vec![reason],
        no_recipe: false,
        enrichment_ready: false,
        warnings: Vec::new(),
        notes: Vec::new(),
    }
}

#[tauri::command]
pub async fn recipe_author_dashboard_state(
    state: State<'_, Arc<AppState>>,
    feature_id: String,
) -> Result<RecipeAuthorDashboardState, String> {
    let d = ra_client(&state)
        .recipe_project_dashboard::<sovereign_mesh::recipe_project_http::RecipeAuthorDashboardState>(
            &feature_id,
        )
        .await
        .map_err(|e| format!("recipe_author_dashboard_state: {e}"))?;

    let validation = match (d.validation, d.validation_unavailable) {
        (Some(v), _) => v,
        (None, Some(why)) => {
            tracing::debug!(%feature_id, reason = %why, "dashboard: host reported no verdict");
            unjudged_report(why)
        }
        // Neither half set is a host that answered a shape it declares
        // impossible; say so rather than render an invented verdict.
        (None, None) => unjudged_report(
            "the host returned neither a validation verdict nor a reason for its \
             absence — this artifact has not been judged."
                .to_string(),
        ),
    };

    Ok(RecipeAuthorDashboardState {
        feature_id: d.feature_id,
        title: d.title,
        charter_md: d.charter_md,
        artifact_kind: d.artifact_kind,
        recipe_id: d.recipe_id,
        recipe_path: d.recipe_path,
        recipe_toml: d.recipe_toml,
        current_sample_size: d.current_sample_size,
        last_test_status: d.last_test_status,
        last_test_at: d.last_test_at,
        created_at: d.created_at,
        updated_at: d.updated_at,
        decisions: d.decisions,
        research_findings: d.research_findings,
        capability_requests: d.capability_requests,
        recipe_issues: d.recipe_issues,
        deferred_questions: d.deferred_questions,
        checkpoints: d.checkpoints,
        validation,
    })
}

// ─── In-app TOML editing (Phase B) ───────────────────────────

/// Validate + atomically save a hand-edited artifact TOML, returning the
/// same [`RecipeValidationReport`] the dashboard shows.
///
/// **Validate-first, write-only-if-valid** is the HOST's rule now: a
/// recipe that does not parse comes back `Ok(report)` with `ok: false`
/// and NOTHING was written — the editor keeps the in-flight text, the
/// partner fixes the error and re-saves, exactly as before.
///
/// A WORKFLOW project is an `Err` carrying the host's 501 sentence: it
/// does not link the workflow parser and will not write unjudged bytes
/// over a working artifact. That is a behaviour change and it is loud on
/// purpose — the alternative was an unvalidated write nobody could see.
#[tauri::command]
pub async fn recipe_author_save_edited_toml(
    state: State<'_, Arc<AppState>>,
    feature_id: String,
    edited_toml: String,
) -> Result<RecipeValidationReport, String> {
    ra_client(&state)
        .recipe_project_save_toml::<RecipeValidationReport>(&feature_id, &edited_toml)
        .await
        .map_err(|e| format!("recipe_author_save_edited_toml: {e}"))
}

// ─── Link a freshly-authored artifact to the project ─────────

/// After an authoring turn, register the artifact the agent just wrote
/// onto the project's summary (`recipe_id`), so the dashboard surfaces
/// it. `since_unix` is the turn's start time, so only an artifact written
/// THIS turn is linked (a chat-only turn links nothing and answers
/// `None`). Idempotent + cheap; the chat surface calls it on every
/// turn-complete.
#[tauri::command]
pub async fn recipe_author_link_recent_artifact(
    state: State<'_, Arc<AppState>>,
    feature_id: String,
    since_unix: i64,
) -> Result<Option<String>, String> {
    ra_client(&state)
        .recipe_project_link_recent(&feature_id, since_unix)
        .await
        .map_err(|e| format!("recipe_author_link_recent_artifact: {e}"))
}

// ─── Restore checkpoint ──────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct RestoreCheckpointRequest {
    pub feature_id: String,
    pub checkpoint_id: String,
}

/// Restore a snapshot and lay down the restore anchor. The host resolves
/// the artifact write path by the project's kind and attributes the
/// resulting `checkpoint_restored` note itself.
#[tauri::command]
pub async fn recipe_author_restore_checkpoint(
    state: State<'_, Arc<AppState>>,
    req: RestoreCheckpointRequest,
) -> Result<RestoreCheckpointOutcome, String> {
    ra_client(&state)
        .recipe_project_restore_checkpoint::<RestoreCheckpointOutcome>(
            &req.feature_id,
            &req.checkpoint_id,
        )
        .await
        .map_err(|e| format!("recipe_author_restore_checkpoint: {e}"))
}

// ─── Per-turn situated context ───────────────────────────────

/// Build the per-turn situated-context preamble for a Recipe Author
/// conversation: `[Project state]` (charter, corpus state, recent
/// decisions, outstanding issues, capability requests), `[Current
/// <kind> TOML]`, `[Latest validation]`, ending with `[Partner says]\n`.
///
/// Diagnosed 2026-05-23: the chat surface was dispatching raw user
/// messages, giving the agent no signal about which project was active,
/// so it asked the user to paste the TOML and the errors. The frontend
/// concatenates this block with the user's text. Cheap enough to
/// re-render every turn, which is what keeps the agent's view fresh.
#[tauri::command]
pub async fn recipe_author_build_prelude(
    state: State<'_, Arc<AppState>>,
    feature_id: String,
) -> Result<String, String> {
    ra_client(&state)
        .recipe_project_prelude(&feature_id)
        .await
        .map_err(|e| format!("Recipe Author: build prelude for '{feature_id}' failed: {e}"))
}
