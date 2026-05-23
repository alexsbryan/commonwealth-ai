//! Tauri commands powering the desktop **Recipe Author Workspace**
//! (M2). The workspace is a separate two-panel surface (conversation
//! ⅔, dashboard ⅓) that lets a non-technical domain expert build a
//! Sovereign corpus + investigation schema by conversation, while
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
//! Backend handles (`Arc<NoteStore>`, `Arc<FeatureStore>`) live on
//! `AppState` and are populated during bootstrap. When either is
//! missing (early boot, IO failure on `notes.db` / `features.db`),
//! every command in this module returns a structured error string
//! the frontend can surface as a "workspace unavailable" banner.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::State;

use corpus_engine::{Recipe};
use corpus_engine_notes::{NoteRow, NoteScope, NoteStore, ScopeFilter};
use corpus_engine_atos::{FeatureRow, FeatureState, FeatureStore};
use sovereign_tools::recipe_author::{
    self, checkpoint::restore_checkpoint as do_restore_checkpoint, CheckpointMeta,
    ProjectSummary, RecipeProject,
};

use crate::state::AppState;

/// Pull `notes` + `features` handles off `AppState`. Returns a
/// stringified error suitable for direct `.map_err(...)?` in the
/// command bodies — the frontend renders these as toast text.
async fn handles(
    state: &Arc<AppState>,
) -> Result<(Arc<NoteStore>, Arc<FeatureStore>), String> {
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
    pub recipe_id: Option<String>,
    pub current_sample_size: Option<u64>,
    pub last_test_status: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl RecipeProjectListEntry {
    fn from_row_and_summary(row: &FeatureRow, summary: ProjectSummary) -> Self {
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
    let (notes, features) = handles(&state).await?;
    let all = features
        .list(false)
        .await
        .map_err(|e| format!("recipe_author_list_projects: {e}"))?;

    let mut out = Vec::new();
    for row in all {
        if FeatureState::parse(&row.state) != Some(FeatureState::RecipeAuthoring) {
            continue;
        }
        // Try to load the project's sidecar summary. A failure here is
        // not fatal — the row exists, surface it with a default
        // summary so the user can still pick it.
        let summary = match RecipeProject::load(
            &row.id,
            Arc::clone(&notes),
            Arc::clone(&features),
        )
        .await
        {
            Ok(p) => p.read_summary().unwrap_or_else(|_| default_summary(&row)),
            Err(_) => default_summary(&row),
        };
        out.push(RecipeProjectListEntry::from_row_and_summary(&row, summary));
    }
    // Newest first.
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(out)
}

fn default_summary(row: &FeatureRow) -> ProjectSummary {
    ProjectSummary {
        feature_id: row.id.clone(),
        title: row.title.clone(),
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
    let project = RecipeProject::new(
        title,
        &req.charter_md,
        Arc::clone(&notes),
        Arc::clone(&features),
    )
    .await
    .map_err(|e| format!("recipe_author_new_project: {e}"))?;

    // Re-read the FeatureRow + summary so the sidebar entry mirrors
    // exactly what `list_projects` would render after a refresh.
    let row = features
        .get(project.feature_id())
        .await
        .map_err(|e| format!("recipe_author_new_project: get row: {e}"))?
        .ok_or_else(|| {
            "recipe_author_new_project: project FeatureRow vanished after creation".to_string()
        })?;
    let summary = project.read_summary().unwrap_or_else(|_| default_summary(&row));
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
}

/// The single struct the dashboard reads on every poll. Coarse on
/// purpose — the cards are pure presentation over slices of this.
#[derive(Debug, Clone, Serialize)]
pub struct RecipeAuthorDashboardState {
    pub feature_id: String,
    pub title: String,
    pub charter_md: String,
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

#[tauri::command]
pub async fn recipe_author_dashboard_state(
    state: State<'_, Arc<AppState>>,
    feature_id: String,
) -> Result<RecipeAuthorDashboardState, String> {
    let (notes, features) = handles(&state).await?;
    let project = RecipeProject::load(
        &feature_id,
        Arc::clone(&notes),
        Arc::clone(&features),
    )
    .await
    .map_err(|e| format!("recipe_author_dashboard_state: {e}"))?;

    let row = features
        .get(&feature_id)
        .await
        .map_err(|e| format!("recipe_author_dashboard_state: get row: {e}"))?
        .ok_or_else(|| {
            format!("recipe_author_dashboard_state: feature_id `{feature_id}` not found")
        })?;
    let summary = project.read_summary().unwrap_or_else(|_| default_summary(&row));

    // Resolve recipe.toml on disk if the project has a recipe id yet.
    let (recipe_path, recipe_toml) = match summary.recipe_id.as_deref() {
        Some(rid) => match recipe_author::local_recipes_dir() {
            Ok(root) => {
                let path = root.join(rid).join("recipe.toml");
                let text = std::fs::read_to_string(&path).ok();
                (Some(path.to_string_lossy().into_owned()), text)
            }
            Err(_) => (None, None),
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

    let validation = match recipe_toml.as_deref() {
        None => RecipeValidationReport {
            ok: false,
            errors: Vec::new(),
            no_recipe: true,
        },
        Some(toml_str) => match Recipe::from_toml(toml_str) {
            Ok(_) => RecipeValidationReport {
                ok: true,
                errors: Vec::new(),
                no_recipe: false,
            },
            // The error text already carries the translate_parse_error
            // rewrite (missing-section guidance, allowed-variant lists,
            // field-named unknown-variant lines). Surface verbatim —
            // splitting on \n\n preserves multi-line guidance blocks
            // intact while still letting the UI render each error as a
            // discrete row.
            Err(e) => {
                let message = e.to_string();
                let errors = if message.is_empty() {
                    vec!["recipe failed to parse (no message)".to_string()]
                } else {
                    message
                        .split("\n\n")
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect()
                };
                RecipeValidationReport {
                    ok: false,
                    errors,
                    no_recipe: false,
                }
            }
        },
    };

    Ok(RecipeAuthorDashboardState {
        feature_id,
        title: row.title,
        charter_md: row.charter_md,
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
    let project = RecipeProject::load(
        &req.feature_id,
        Arc::clone(&notes),
        Arc::clone(&features),
    )
    .await
    .map_err(|e| format!("recipe_author_restore_checkpoint: {e}"))?;

    // The summary holds the optional recipe_id we need to overwrite
    // the live recipe.toml from the snapshot. Best-effort — if the
    // project hasn't drafted a recipe yet, restore_checkpoint just
    // lays down a restore-anchor checkpoint without touching disk.
    let recipe_id = project.read_summary().ok().and_then(|s| s.recipe_id);

    // Stable session id so the resulting `kind=checkpoint_restored`
    // note attributes the act to the desktop workspace rather than to
    // a random uuid that's hard to filter on later.
    let session_id = format!("desktop-recipe-author-{}", req.feature_id);

    let outcome = do_restore_checkpoint(
        &project,
        &req.checkpoint_id,
        recipe_id.as_deref(),
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
    let project = recipe_author::RecipeProject::load(
        &feature_id,
        Arc::clone(&notes),
        Arc::clone(&features),
    )
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
    let (recipe_block, validation_block) = match &summary.recipe_id {
        Some(recipe_id) => {
            let path = sovereign_root_dir()
                .join("recipes")
                .join(recipe_id)
                .join("recipe.toml");
            match std::fs::read_to_string(&path) {
                Ok(toml) => {
                    let validation = inline_validate_recipe(&toml);
                    let recipe = format!(
                        "\n[Current recipe TOML]\nPath: {}\n```toml\n{}\n```\n",
                        path.display(),
                        toml.trim_end(),
                    );
                    (recipe, validation)
                }
                Err(e) => (
                    format!(
                        "\n[Current recipe TOML]\nNot readable at {}: {e}\n",
                        path.display()
                    ),
                    String::new(),
                ),
            }
        }
        None => (
            "\n[Current recipe TOML]\n(no recipe drafted yet — use \
             `recipe_write_structured` to create one)\n"
                .to_string(),
            String::new(),
        ),
    };

    let block = format!(
        "[Project state]\n{situated}{recipe_block}{validation_block}\n[Partner says]\n"
    );
    Ok(block)
}

/// Inline TOML-parse validation. Mirrors what `RecipeValidateTool`
/// produces but runs in-process so the prelude doesn't need to dance
/// around the tool dispatcher. Returns an empty string when the
/// recipe parses cleanly — the agent doesn't need to see a "passes"
/// notice every turn.
fn inline_validate_recipe(toml: &str) -> String {
    match toml::from_str::<corpus_engine::Recipe>(toml) {
        Ok(_) => String::new(),
        Err(e) => format!(
            "\n[Latest validation]\nRecipe does NOT parse. First error:\n{e}\n"
        ),
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

