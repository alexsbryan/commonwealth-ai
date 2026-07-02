// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tauri commands powering the desktop **Recipe Author Workspace**
//! (M2). The workspace is a separate two-panel surface (conversation
//! ⅔, dashboard ⅓) that lets a non-technical domain expert build a
//! svrnmesh corpus + investigation schema by conversation, while
//! every meaningful agent / partner action surfaces on a live
//! dashboard.
//!
//! These commands are deliberately *coarse*. The dashboard reads as
//! one unit (`recipe_author_dashboard_state`) rather than per-card —
//! the cards are presentational, the data shape is a single struct.
//! Mutations are explicit (`new_project`, `restore_checkpoint`).
//! Chat send/receive reuses the existing `send_message_stream`
//! command + `message-chunk` / `message-complete` events.
//!
//! Backend handles (`Arc<NoteStore>`, `Arc<RecipeProjectStore>`) live on
//! `AppState` and are populated during bootstrap. When either is
//! missing (early boot, IO failure on `notes.db` / `features.db`),
//! every command in this module returns a structured error string
//! the frontend can surface as a "workspace unavailable" banner.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::State;

use corpus_engine::Recipe;
use sovereign_workflow::Workflow;
use sovereign_store::recipe_project_store::{RecipeProjectRow, RecipeProjectStore};
use corpus_engine_notes::{NoteRow, NoteScope, NoteStore, ScopeFilter};
use sovereign_tools::recipe_author::{
    self, checkpoint::restore_checkpoint as do_restore_checkpoint, ArtifactKind, CheckpointMeta,
    ProjectSummary, RecipeProject,
};

use crate::state::AppState;

/// Pull `notes` + `features` handles off `AppState`. Returns a
/// stringified error suitable for direct `.map_err(...)?` in the
/// command bodies — the frontend renders these as toast text.
async fn handles(state: &Arc<AppState>) -> Result<(Arc<NoteStore>, Arc<RecipeProjectStore>), String> {
    let notes = state
        .notes
        .read()
        .await
        .as_ref()
        .map(Arc::clone)
        .ok_or_else(|| {
            "Recipe Author workspace unavailable: notes.db is not open. \
             Try restarting the desktop or check the daemon log."
                .to_string()
        })?;
    let features = state
        .features
        .read()
        .await
        .as_ref()
        .map(Arc::clone)
        .ok_or_else(|| {
            "Recipe Author workspace unavailable: features.db is not open. \
             Try restarting the desktop or check the daemon log."
                .to_string()
        })?;
    Ok((notes, features))
}

// ─── Project summary (sidebar) ───────────────────────────────

/// One row in the workspace's project sidebar. Mirrors
/// `RecipeProject::ProjectSummary` plus a `feature_id` and the
/// charter excerpt the sidebar renders as a hover tooltip.
#[derive(Debug, Clone, Serialize)]
pub struct RecipeProjectListEntry {
    pub feature_id: String,
    pub title: String,
    pub charter_excerpt: String,
    /// `"recipe"` or `"workflow"` — so the sidebar can tag the project's kind.
    pub artifact_kind: ArtifactKind,
    pub recipe_id: Option<String>,
    pub current_sample_size: Option<u64>,
    pub last_test_status: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl RecipeProjectListEntry {
    fn from_row_and_summary(row: &RecipeProjectRow, summary: ProjectSummary) -> Self {
        // Take first ~200 chars of the charter for the sidebar tooltip
        // — full charter is in the dashboard CharterSummary card.
        let mut excerpt = row.charter_md.chars().take(200).collect::<String>();
        if row.charter_md.chars().count() > 200 {
            excerpt.push('…');
        }
        Self {
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
}

#[tauri::command]
pub async fn recipe_author_list_projects(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<RecipeProjectListEntry>, String> {
    // Dev: SOVEREIGN_DEV_FORCE_FIRST_RUN hides real projects (in-memory
    // only — they stay on disk) so the recipe-author Welcome shows its
    // first-timer tutorial CTA, replaying the onboarding surface.
    if crate::dev_flags::force_first_run() {
        return Ok(Vec::new());
    }
    let (notes, features) = handles(&state).await?;
    let all = features
        .list(false)
        .await
        .map_err(|e| format!("recipe_author_list_projects: {e}"))?;

    let mut out = Vec::new();
    for row in all {
        // Try to load the project's sidecar summary. A failure here is
        // not fatal — the row exists, surface it with a default
        // summary so the user can still pick it.
        let summary =
            match RecipeProject::load(&row.id, Arc::clone(&notes), Arc::clone(&features)).await {
                Ok(p) => p.read_summary().unwrap_or_else(|_| default_summary(&row)),
                Err(_) => default_summary(&row),
            };
        out.push(RecipeProjectListEntry::from_row_and_summary(&row, summary));
    }
    // Newest first.
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(out)
}

fn default_summary(row: &RecipeProjectRow) -> ProjectSummary {
    ProjectSummary {
        feature_id: row.id.clone(),
        title: row.title.clone(),
        // Fallback summary when the sidecar can't be read — default to Recipe
        // (the only kind the desktop drives today; Layer B makes this kind-aware).
        artifact_kind: ArtifactKind::Recipe,
        recipe_id: None,
        current_sample_size: None,
        last_test_status: None,
        last_test_at: None,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

// ─── Project create ───────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct NewProjectRequest {
    pub title: String,
    pub charter_md: String,
    /// What the project authors. `#[serde(default)]` → an existing frontend that
    /// omits it creates a `Recipe` project, unchanged; a workflow-author surface
    /// passes `"workflow"`.
    #[serde(default)]
    pub artifact_kind: ArtifactKind,
}

#[tauri::command]
pub async fn recipe_author_new_project(
    state: State<'_, Arc<AppState>>,
    req: NewProjectRequest,
) -> Result<RecipeProjectListEntry, String> {
    let (notes, features) = handles(&state).await?;
    let title = req.title.trim();
    if title.is_empty() {
        return Err("title cannot be empty".into());
    }
    let project = RecipeProject::new_with_kind(
        title,
        &req.charter_md,
        req.artifact_kind,
        Arc::clone(&notes),
        Arc::clone(&features),
    )
    .await
    .map_err(|e| format!("recipe_author_new_project: {e}"))?;

    // Re-read the RecipeProjectRow + summary so the sidebar entry mirrors
    // exactly what `list_projects` would render after a refresh.
    let row = features
        .get(project.feature_id())
        .await
        .map_err(|e| format!("recipe_author_new_project: get row: {e}"))?
        .ok_or_else(|| {
            "recipe_author_new_project: project RecipeProjectRow vanished after creation".to_string()
        })?;
    let summary = project
        .read_summary()
        .unwrap_or_else(|_| default_summary(&row));
    Ok(RecipeProjectListEntry::from_row_and_summary(&row, summary))
}

// ─── Dashboard state (the one big read) ───────────────────────

/// Per-decision-log entry rendered by `DecisionFeed`. `payload` is
/// the parsed `payload_json` so the frontend doesn't reparse — `null`
/// for legacy rows. `attribution` / `decision_kind` are extracted
/// out of the payload for direct rendering when present.
#[derive(Debug, Clone, Serialize)]
pub struct DashboardNoteEntry {
    pub id: String,
    pub kind: String,
    pub content: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

impl From<NoteRow> for DashboardNoteEntry {
    fn from(row: NoteRow) -> Self {
        let payload = row
            .payload_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());
        let decision_kind = payload
            .as_ref()
            .and_then(|v| v.get("decision_kind"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let attribution = payload
            .as_ref()
            .and_then(|v| v.get("attribution"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        Self {
            id: row.id,
            kind: row.kind,
            content: row.content,
            created_at: row.created_at,
            decision_kind,
            attribution,
            payload,
        }
    }
}

/// Result of running the on-disk `recipe.toml` through
/// `Recipe::from_toml`. The dashboard surfaces this as the
/// `RecipeValidationCard` so a partner sees "your recipe doesn't
/// parse — here's why" front-and-center, not buried in agent logs.
///
/// `errors` carries the error text already-translated by
/// [`corpus_engine::recipe::translate_parse_error`] — section names,
/// suggested fixes, allowed-variant lists. Render verbatim.
#[derive(Debug, Clone, Serialize)]
pub struct RecipeValidationReport {
    /// `true` when the recipe parsed and the engine accepted it.
    pub ok: bool,
    /// Human-readable failure messages (one per error). Empty when
    /// `ok == true` or when there's no recipe to validate yet.
    pub errors: Vec<String>,
    /// `true` when there is no recipe to validate (project hasn't
    /// drafted one yet). Lets the UI distinguish "nothing yet" from
    /// "we tried and it failed".
    pub no_recipe: bool,
    /// `true` when the recipe parsed AND its enrichment will produce graph
    /// atoms (enabled `atlas`/`investigation`). `false` for a valid recipe
    /// whose enrichment is off or `field_model` (it would build to ZERO
    /// atoms). Meaningless when `ok == false`. Drives the readiness pill so a
    /// novice sees "this won't enrich" before the build, not after.
    pub enrichment_ready: bool,
}

/// The single struct the dashboard reads on every poll. Coarse on
/// purpose — the cards are pure presentation over slices of this.
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

/// Validate artifact TOML into the dashboard's [`RecipeValidationReport`] shape,
/// dispatched by [`ArtifactKind`]: a recipe via `Recipe::from_toml` (carrying the
/// translated parse guidance + enrichment readiness), a workflow via
/// `Workflow::parse` (syntax, duplicate step ids, step cycles). `enrichment_ready`
/// is recipe-only — always `false` for a workflow. Single source of truth for "did
/// this artifact parse?" used by both the dashboard poll and the in-app TOML save
/// so the partner sees identical verdicts whether the agent or they authored it.
fn validate_artifact_toml(
    kind: ArtifactKind,
    artifact_toml: Option<&str>,
) -> RecipeValidationReport {
    let toml_str = match artifact_toml {
        None => {
            return RecipeValidationReport {
                ok: false,
                errors: Vec::new(),
                no_recipe: true,
                enrichment_ready: false,
            }
        }
        Some(t) => t,
    };
    match kind {
        ArtifactKind::Recipe => match Recipe::from_toml(toml_str) {
            Ok(recipe) => RecipeValidationReport {
                ok: true,
                errors: Vec::new(),
                no_recipe: false,
                enrichment_ready: recipe.produces_enriched_atoms(),
            },
            Err(e) => RecipeValidationReport {
                ok: false,
                errors: split_parse_errors(&e.to_string()),
                no_recipe: false,
                enrichment_ready: false,
            },
        },
        ArtifactKind::Workflow => match Workflow::parse(toml_str) {
            Ok(_) => RecipeValidationReport {
                ok: true,
                errors: Vec::new(),
                no_recipe: false,
                enrichment_ready: false,
            },
            Err(e) => RecipeValidationReport {
                ok: false,
                errors: split_parse_errors(&e.to_string()),
                no_recipe: false,
                enrichment_ready: false,
            },
        },
    }
}

/// Split a parser error into discrete guidance rows. Recipe errors carry
/// blank-line-separated blocks from `translate_parse_error` (missing-section
/// guidance, allowed-variant lists) — render each intact; a workflow parse error
/// is a single line. Empty message → one fallback row.
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

/// Resolve the on-disk artifact TOML path for a project's `(kind, id)`: a recipe at
/// `~/.sovereign/recipes/<id>/recipe.toml`, a workflow at
/// `~/.sovereign/workflows/<id>.toml`. `None` when the home dir can't be resolved.
fn artifact_toml_path(kind: ArtifactKind, artifact_id: &str) -> Option<std::path::PathBuf> {
    match kind {
        ArtifactKind::Recipe => recipe_author::local_recipes_dir()
            .ok()
            .map(|r| r.join(artifact_id).join("recipe.toml")),
        ArtifactKind::Workflow => recipe_author::local_workflows_dir()
            .ok()
            .map(|r| r.join(format!("{artifact_id}.toml"))),
    }
}

#[tauri::command]
pub async fn recipe_author_dashboard_state(
    state: State<'_, Arc<AppState>>,
    feature_id: String,
) -> Result<RecipeAuthorDashboardState, String> {
    let (notes, features) = handles(&state).await?;
    let project = RecipeProject::load(&feature_id, Arc::clone(&notes), Arc::clone(&features))
        .await
        .map_err(|e| format!("recipe_author_dashboard_state: {e}"))?;

    let row = features
        .get(&feature_id)
        .await
        .map_err(|e| format!("recipe_author_dashboard_state: get row: {e}"))?
        .ok_or_else(|| {
            format!("recipe_author_dashboard_state: feature_id `{feature_id}` not found")
        })?;
    let summary = project
        .read_summary()
        .unwrap_or_else(|_| default_summary(&row));

    // Resolve the artifact TOML on disk if the project has an id yet — recipe.toml
    // or workflow.toml by the project's kind. (`recipe_id` carries the artifact id
    // for both kinds; the DTO field names stay recipe-shaped for frontend compat,
    // with `artifact_kind` added so the UI can branch.)
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

    // Pull recent feature-scoped notes once and partition by kind.
    // 200 is generous — the dashboard cards each take their own
    // sub-slice. Newest first.
    let scope = ScopeFilter {
        scopes: vec![NoteScope::Feature],
        feature_id: Some(feature_id.clone()),
    };
    let raw = notes
        .read_notes_scoped(None, &[], &[], &[], 200, false, &scope)
        .await
        .map_err(|e| format!("recipe_author_dashboard_state: read notes: {e}"))?;

    let mut decisions = Vec::new();
    let mut research_findings = Vec::new();
    let mut capability_requests = Vec::new();
    let mut recipe_issues = Vec::new();
    let mut deferred_questions = Vec::new();
    for row in raw {
        match row.kind.as_str() {
            "decision" => decisions.push(DashboardNoteEntry::from(row)),
            "research_finding" => research_findings.push(DashboardNoteEntry::from(row)),
            "capability_request" => capability_requests.push(DashboardNoteEntry::from(row)),
            "recipe_issue" => recipe_issues.push(DashboardNoteEntry::from(row)),
            "deferred_question" => deferred_questions.push(DashboardNoteEntry::from(row)),
            // Other kinds (`checkpoint`, `checkpoint_restored`) are
            // surfaced via the checkpoints list itself, so we drop
            // them here to keep the per-card slices focused.
            _ => {}
        }
    }

    let checkpoints = project
        .list_checkpoints()
        .map_err(|e| format!("recipe_author_dashboard_state: list checkpoints: {e}"))?;

    let validation = validate_artifact_toml(summary.artifact_kind, recipe_toml.as_deref());

    Ok(RecipeAuthorDashboardState {
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
    })
}

// ─── In-app TOML editing (Phase B) ───────────────────────────

/// Validate + atomically save a hand-edited `recipe.toml` for a recipe-author
/// project, returning the same [`RecipeValidationReport`] the dashboard shows.
///
/// Reuses the engine's `Recipe::from_toml` (same verdict as the dashboard) and
/// the recipe-author write convention (`.toml.part` → atomic rename) so a manual
/// edit is indistinguishable on disk from an agent-authored one — the agent
/// picks it up next turn via `recipe_author_build_prelude`'s disk re-read, no
/// agent change needed.
///
/// **Validate-first, write-only-if-valid:** a recipe that doesn't parse is NEVER
/// persisted (it would break the build + the agent's prelude). The editor keeps
/// the in-flight text client-side, so the partner fixes the error and re-saves;
/// `ok == false` carries the parse errors to render inline.
#[tauri::command]
pub async fn recipe_author_save_edited_toml(
    state: State<'_, Arc<AppState>>,
    feature_id: String,
    edited_toml: String,
) -> Result<RecipeValidationReport, String> {
    let (notes, features) = handles(&state).await?;
    let project = RecipeProject::load(&feature_id, Arc::clone(&notes), Arc::clone(&features))
        .await
        .map_err(|e| format!("recipe_author_save_edited_toml: {e}"))?;
    let summary = project
        .read_summary()
        .map_err(|e| format!("recipe_author_save_edited_toml: read summary: {e}"))?;
    let kind = summary.artifact_kind;
    let artifact_id = summary.recipe_id.ok_or_else(|| {
        format!(
            "this project has no {} yet — draft one with the agent before editing",
            kind.label()
        )
    })?;

    // Validate against the SAME parser the dashboard uses (by kind). Do not write
    // an artifact that fails to parse.
    let report = validate_artifact_toml(kind, Some(&edited_toml));
    if !report.ok {
        return Ok(report);
    }

    // Atomic write to the kind's on-disk path (`.part` → rename), mirroring the
    // structured-write tools so an agent write and a manual edit are identical on
    // disk — the agent picks it up next turn via the prelude's disk re-read.
    let path = artifact_toml_path(kind, &artifact_id).ok_or_else(|| {
        "recipe_author_save_edited_toml: cannot locate the artifact directory (HOME unset)"
            .to_string()
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!("recipe_author_save_edited_toml: create {}: {e}", parent.display())
        })?;
    }
    let part = path.with_extension("toml.part");
    std::fs::write(&part, edited_toml.as_bytes())
        .map_err(|e| format!("recipe_author_save_edited_toml: write {}: {e}", part.display()))?;
    std::fs::rename(&part, &path).map_err(|e| {
        format!(
            "recipe_author_save_edited_toml: rename {} → {}: {e}",
            part.display(),
            path.display()
        )
    })?;
    tracing::info!(
        feature_id = %feature_id,
        artifact_id = %artifact_id,
        kind = kind.label(),
        "recipe_author_save_edited_toml wrote hand-edited artifact TOML"
    );
    Ok(report)
}

// ─── Link a freshly-authored artifact to the project ─────────

/// The id of the newest artifact under `dir` whose TOML was (re)written at/after
/// `since_unix` — i.e. the one the agent just wrote this turn. `None` when nothing
/// was written in the window (so a turn with no draft never mislinks). A recipe
/// lives at `<dir>/<id>/recipe.toml`, a workflow at `<dir>/<id>.toml`.
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

/// After an authoring turn, register the artifact the agent just wrote onto the
/// project's summary (`recipe_id`), so the dashboard surfaces it. The shared agent
/// loop writes the TOML but can't touch the project model (`RecipeProject` is a
/// downstream crate), and the CLI live-trial's "writing the TOML IS the
/// registration" linkage never reached the desktop — this closes that gap on the
/// desktop side. `since_unix` is the turn's start time, so only an artifact written
/// THIS turn is linked (a chat-only turn links nothing). Returns the linked id, or
/// `None` when the turn wrote no artifact. Idempotent + cheap; the chat surface
/// calls it on every turn-complete.
#[tauri::command]
pub async fn recipe_author_link_recent_artifact(
    state: State<'_, Arc<AppState>>,
    feature_id: String,
    since_unix: i64,
) -> Result<Option<String>, String> {
    let (notes, features) = handles(&state).await?;
    let project = RecipeProject::load(&feature_id, Arc::clone(&notes), Arc::clone(&features))
        .await
        .map_err(|e| format!("recipe_author_link_recent_artifact: {e}"))?;
    let mut summary = project
        .read_summary()
        .map_err(|e| format!("recipe_author_link_recent_artifact: read summary: {e}"))?;

    let dir = match summary.artifact_kind {
        ArtifactKind::Recipe => recipe_author::local_recipes_dir(),
        ArtifactKind::Workflow => recipe_author::local_workflows_dir(),
    }
    .map_err(|e| format!("recipe_author_link_recent_artifact: locate dir: {e}"))?;

    let Some(id) = find_recent_artifact(summary.artifact_kind, &dir, since_unix) else {
        return Ok(None);
    };

    if summary.recipe_id.as_deref() != Some(id.as_str()) {
        summary.recipe_id = Some(id.clone());
        summary.updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        project
            .write_summary(&summary)
            .map_err(|e| format!("recipe_author_link_recent_artifact: write summary: {e}"))?;
        tracing::info!(
            feature_id = %feature_id,
            artifact_id = %id,
            kind = summary.artifact_kind.label(),
            "linked freshly-authored artifact to project"
        );
    }
    Ok(Some(id))
}

// ─── Restore checkpoint ──────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct RestoreCheckpointRequest {
    pub feature_id: String,
    pub checkpoint_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RestoreCheckpointOutcome {
    pub new_checkpoint_id: String,
    pub source_checkpoint_id: String,
}

#[tauri::command]
pub async fn recipe_author_restore_checkpoint(
    state: State<'_, Arc<AppState>>,
    req: RestoreCheckpointRequest,
) -> Result<RestoreCheckpointOutcome, String> {
    let (notes, features) = handles(&state).await?;
    let project = RecipeProject::load(&req.feature_id, Arc::clone(&notes), Arc::clone(&features))
        .await
        .map_err(|e| format!("recipe_author_restore_checkpoint: {e}"))?;

    // The summary holds the optional artifact id (`recipe_id` carries it for both
    // kinds) we need to overwrite the live recipe.toml / workflow.toml from the
    // snapshot. Best-effort — if the project hasn't drafted an artifact yet,
    // `do_restore_checkpoint` just lays down a restore-anchor checkpoint without
    // touching disk. It resolves the write path by the project's kind internally.
    let artifact_id = project.read_summary().ok().and_then(|s| s.recipe_id);

    // Stable session id so the resulting `kind=checkpoint_restored`
    // note attributes the act to the desktop workspace rather than to
    // a random uuid that's hard to filter on later.
    let session_id = format!("desktop-recipe-author-{}", req.feature_id);

    let outcome = do_restore_checkpoint(
        &project,
        &req.checkpoint_id,
        artifact_id.as_deref(),
        None,
        &session_id,
    )
    .await
    .map_err(|e| format!("recipe_author_restore_checkpoint: {e}"))?;

    Ok(RestoreCheckpointOutcome {
        new_checkpoint_id: outcome.checkpoint_id,
        source_checkpoint_id: req.checkpoint_id,
    })
}

// ─── Workspace activation (skill toggle) ─────────────────────

/// Mark the desktop as "user is in the recipe-author workspace" by
/// flipping `recipe-author` into `active_skills` (or out of it on
/// exit). The runtime's `primary_skill_id_for_conversation` prefers
/// LocalOnly skills, and the recipe-author skill is `local_only`, so
/// any conversation started while the workspace is open is tagged
/// Build the per-turn situated-context preamble for a Recipe Author
/// conversation. Diagnosed 2026-05-23: the desktop chat surface was
/// dispatching raw user messages through `send_message_stream`,
/// giving the agent no signal about which project was active. The
/// agent would respond by asking the user to paste the recipe TOML
/// + validation errors because it had no other way to see them
/// — even though `RecipeProject`, the recipe.toml on disk, and the
/// validation tool were all reachable from the same process.
///
/// This command renders the M1-CLI-equivalent splice block:
///
///   - `[Project state]` — charter, corpus state, recent decisions,
///     outstanding issues, capability requests (from
///     `recipe_author::situated_context::render`).
///   - `[Current recipe TOML]` — fenced TOML block when
///     `<recipes_dir>/<recipe_id>/recipe.toml` exists, or a note
///     that the recipe hasn't been drafted yet.
///   - `[Latest validation]` — inline-validated by parsing the same
///     TOML and surfacing the first errors. Empty when no recipe.
///
/// Frontend calls this before every turn and concatenates the block
/// with the user's message: `{block}\n[Partner says]\n{user_text}`.
/// Cheap (~5KB block, no network) so re-running every turn is fine
/// and keeps the agent's view of project state fresh.
#[tauri::command]
pub async fn recipe_author_build_prelude(
    state: State<'_, Arc<AppState>>,
    feature_id: String,
) -> Result<String, String> {
    let (notes, features) = handles(&state).await?;
    let project =
        recipe_author::RecipeProject::load(&feature_id, Arc::clone(&notes), Arc::clone(&features))
            .await
            .map_err(|e| format!("Recipe Author: load project '{feature_id}' failed: {e}"))?;

    let situated = recipe_author::situated_context::render(&project)
        .await
        .map_err(|e| format!("Recipe Author: render situated context failed: {e}"))?;

    // Recipe TOML — resolve via the project's recipe_id (None until
    // the agent's first `recipe_write_structured` call). When None
    // we tell the agent explicitly so it knows to draft from scratch
    // rather than assume one exists.
    let summary = project
        .read_summary()
        .map_err(|e| format!("Recipe Author: read summary failed: {e}"))?;
    // Resolve + render the artifact TOML by kind (`recipe.toml` under recipes/, or
    // `workflow.toml` under workflows/). `recipe_id` carries the artifact id for
    // both. Kept on `sovereign_root_dir()` (honours SOVEREIGN_HOME) as before.
    let label = summary.artifact_kind.label();
    let (recipe_block, validation_block) = match &summary.recipe_id {
        Some(artifact_id) => {
            let path = match summary.artifact_kind {
                ArtifactKind::Recipe => sovereign_root_dir()
                    .join("recipes")
                    .join(artifact_id)
                    .join("recipe.toml"),
                ArtifactKind::Workflow => sovereign_root_dir()
                    .join("workflows")
                    .join(format!("{artifact_id}.toml")),
            };
            match std::fs::read_to_string(&path) {
                Ok(toml) => {
                    let validation = inline_validate_artifact(summary.artifact_kind, &toml);
                    let block = format!(
                        "\n[Current {label} TOML]\nPath: {}\n```toml\n{}\n```\n",
                        path.display(),
                        toml.trim_end(),
                    );
                    (block, validation)
                }
                Err(e) => (
                    format!(
                        "\n[Current {label} TOML]\nNot readable at {}: {e}\n",
                        path.display()
                    ),
                    String::new(),
                ),
            }
        }
        None => (
            format!(
                "\n[Current {label} TOML]\n(no {label} drafted yet — use \
                 `{label}_write_structured` to create one)\n"
            ),
            String::new(),
        ),
    };

    let block =
        format!("[Project state]\n{situated}{recipe_block}{validation_block}\n[Partner says]\n");
    Ok(block)
}

/// Inline TOML-parse validation by kind. Mirrors what `RecipeValidateTool` /
/// `WorkflowValidateTool` produce but runs in-process so the prelude doesn't dance
/// around the tool dispatcher. Returns an empty string when the artifact parses
/// cleanly — the agent doesn't need to see a "passes" notice every turn.
fn inline_validate_artifact(kind: ArtifactKind, toml: &str) -> String {
    match kind {
        ArtifactKind::Recipe => match toml::from_str::<corpus_engine::Recipe>(toml) {
            Ok(_) => String::new(),
            Err(e) => format!("\n[Latest validation]\nRecipe does NOT parse. First error:\n{e}\n"),
        },
        ArtifactKind::Workflow => match Workflow::parse(toml) {
            Ok(_) => String::new(),
            Err(e) => {
                format!("\n[Latest validation]\nWorkflow does NOT parse. First error:\n{e}\n")
            }
        },
    }
}

fn sovereign_root_dir() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("SOVEREIGN_HOME") {
        return std::path::PathBuf::from(p);
    }
    if let Some(home) = dirs::home_dir() {
        return home.join(".sovereign");
    }
    std::path::PathBuf::from(".sovereign")
}
