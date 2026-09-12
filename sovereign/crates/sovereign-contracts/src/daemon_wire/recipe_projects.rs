// SPDX-License-Identifier: AGPL-3.0-or-later
//! Recipe-author project wire shapes — `/v1/recipe-projects/…`
//! (`sovereign_mesh::recipe_project_http`) and the two recipe-author nouns
//! those answers carry (`sovereign_recipe_author::project::{ArtifactKind,
//! CheckpointMeta}`). Moved here at sv-surface svt-3 (2026-09-11); both
//! owners re-export at the historical paths.
//!
//! `ArtifactKind` and `CheckpointMeta` are the recipe-author's vocabulary
//! and they came down for the same reason `OriginKind` did: every wire
//! answer that carries a project tags it with its kind and lists its
//! checkpoints, so a client parsing the answer had to link the authoring
//! runtime to name a two-variant enum. `CheckpointMeta` is also the
//! on-disk `meta.json` of a checkpoint dir — one shape, written by the
//! runtime and read back by the dashboard, which is why it is not two.

use serde::{Deserialize, Serialize};

use crate::recipe::notes::Note;

/// What a recipe-author project produces — a recipe or a workflow.
///
/// `#[serde(default)]` at every carrier: a row or checkpoint written before
/// the tag existed decodes as `Recipe`, restoring byte-identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    #[default]
    /// A corpus recipe (`recipe.toml`).
    Recipe,
    /// A workflow (`workflow.toml`).
    Workflow,
}

impl ArtifactKind {
    /// The snapshot filename inside a checkpoint dir. `recipe.toml` is unchanged
    /// (existing checkpoints restore identically); `workflow.toml` is the new arm.
    pub fn snapshot_basename(self) -> &'static str {
        match self {
            ArtifactKind::Recipe => "recipe.toml",
            ArtifactKind::Workflow => "workflow.toml",
        }
    }

    /// Lowercase noun for prose surfaces (`[Current <label> TOML]`, errors).
    pub fn label(self) -> &'static str {
        match self {
            ArtifactKind::Recipe => "recipe",
            ArtifactKind::Workflow => "workflow",
        }
    }
}

/// Checkpoint metadata — the `meta.json` in a checkpoint dir and the row
/// the dashboard lists. `restored_from` is set on checkpoints created by
/// `RecipeProject::restore`; the dashboard renders these with a
/// "↳ restored from <name>" marker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointMeta {
    /// Stable id (the directory basename: `<ts>-<slug>`).
    pub checkpoint_id: String,
    /// What was snapshotted (recipe vs workflow) — so a checkpoint dir is
    /// self-describing. `#[serde(default)]` → pre-tag checkpoints decode as
    /// `Recipe` (they hold a `recipe.toml`).
    #[serde(default)]
    pub artifact_kind: ArtifactKind,
    /// Human-readable name supplied by the agent / partner.
    pub name: String,
    /// Why the checkpoint was created. Set to one of the
    /// agent-spec triggers (`auto_scale_up`, `auto_strategy_change`,
    /// `partner_request`, `project_creation`) or `restore` for
    /// restoration-anchor checkpoints.
    pub trigger: String,
    /// Optional one-paragraph summary of where the project stands.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    /// Set when this checkpoint was created by a restore from
    /// another. Carries the source checkpoint id; the dashboard
    /// uses this together with the `kind=checkpoint_restored`
    /// NoteStore entry to render the temporal narrative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restored_from: Option<String>,
    /// RFC 3339 timestamp of creation.
    pub created_at: String,
}

/// One project in the sidebar. `Deserialize` as well as `Serialize` for
/// `sovereign_mesh::features_http`'s `ProjectEntry` reason: a caller parses
/// back into the struct the daemon emitted, never into a twin that can
/// drift.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeProjectListEntry {
    /// The project's id.
    pub feature_id: String,
    /// The project's title.
    pub title: String,
    /// The charter's first 200 chars, host-trimmed for the sidebar.
    pub charter_excerpt: String,
    /// `"recipe"` or `"workflow"` — [`ArtifactKind`], reused rather than
    /// re-spelled.
    pub artifact_kind: ArtifactKind,
    /// The linked artifact's id, when one is drafted.
    pub recipe_id: Option<String>,
    /// Items in the last test sample.
    pub current_sample_size: Option<u64>,
    /// The last test's status word.
    pub last_test_status: Option<String>,
    /// Unix seconds the project was created.
    pub created_at: i64,
    /// Unix seconds the project last changed.
    pub updated_at: i64,
}

/// One decision-log entry the dashboard renders. `payload` is the parsed
/// `payload_json` so the caller does not reparse — `null` for legacy
/// rows; `decision_kind` / `attribution` are lifted out of it for direct
/// rendering when present.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardNoteEntry {
    /// Note id.
    pub id: String,
    /// Note kind (`decision`, `research_finding`, …).
    pub kind: String,
    /// The note's text.
    pub content: String,
    /// RFC 3339 creation time.
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// `payload.decision_kind`, lifted for direct rendering.
    pub decision_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// `payload.attribution`, lifted for direct rendering.
    pub attribution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// The parsed `payload_json`; `None` for legacy rows.
    pub payload: Option<serde_json::Value>,
}

impl From<Note> for DashboardNoteEntry {
    fn from(row: Note) -> Self {
        let payload = row
            .payload_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());
        let field = |k: &str| {
            payload
                .as_ref()
                .and_then(|v| v.get(k))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        Self {
            id: row.id,
            kind: row.kind,
            content: row.content,
            created_at: row.created_at,
            decision_kind: field("decision_kind"),
            attribution: field("attribution"),
            payload,
        }
    }
}

/// The verdict on an artifact's on-disk TOML.
///
/// Recipe-shaped, because the recipe arm is the one the host can judge:
/// `errors` blocks, `warnings` does not, and `notes` is neither — it is
/// what the recipe WILL do (derived ontology facets), kept as its own
/// field rather than as tagged strings inside `warnings` so no renderer
/// has to strip a prefix to tell a facet from a defect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeValidationReport {
    /// `true` when the recipe parsed and the offline pass accepted it.
    pub ok: bool,
    /// One message per blocking error, already translated by
    /// `corpus_engine::recipe::translate_parse_error`. Render verbatim.
    pub errors: Vec<String>,
    /// `true` when there is no artifact to validate yet — "nothing
    /// drafted" as distinct from "we tried and it failed".
    pub no_recipe: bool,
    /// `true` when the recipe parsed AND its enrichment will produce
    /// atoms. Meaningless when `ok == false`.
    pub enrichment_ready: bool,
    /// Findings that do not block.
    pub warnings: Vec<String>,
    /// Derived facets of a declared ontology.
    pub notes: Vec<String>,
}

/// The single payload a dashboard reads on every poll.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeAuthorDashboardState {
    /// The project's id.
    pub feature_id: String,
    /// The project's title.
    pub title: String,
    /// The whole charter, markdown.
    pub charter_md: String,
    /// What the project produces.
    pub artifact_kind: ArtifactKind,
    /// The artifact id. Named `recipe_*` for both kinds — the field names
    /// are the desktop's and `artifact_kind` is what a caller branches on.
    pub recipe_id: Option<String>,
    /// The artifact's on-disk path, when drafted.
    pub recipe_path: Option<String>,
    /// The artifact's TOML, when drafted.
    pub recipe_toml: Option<String>,
    /// Items in the current test sample.
    pub current_sample_size: Option<u64>,
    /// The last test's status word.
    pub last_test_status: Option<String>,
    /// RFC 3339 time of the last test.
    pub last_test_at: Option<String>,
    /// Unix seconds the project was created.
    pub created_at: i64,
    /// Unix seconds the project last changed.
    pub updated_at: i64,
    /// Decision-log entries.
    pub decisions: Vec<DashboardNoteEntry>,
    /// Research findings.
    pub research_findings: Vec<DashboardNoteEntry>,
    /// Capability requests.
    pub capability_requests: Vec<DashboardNoteEntry>,
    /// Recipe issues.
    pub recipe_issues: Vec<DashboardNoteEntry>,
    /// Deferred questions.
    pub deferred_questions: Vec<DashboardNoteEntry>,
    /// Every checkpoint, newest last.
    pub checkpoints: Vec<CheckpointMeta>,
    /// The verdict, or `None` when this host reached none.
    ///
    /// `None` is never "it failed" and never "it passed": exactly one of
    /// this field and `validation_unavailable` is set, and the pair is the
    /// four-verdict discipline in two fields (ARCH §18.1).
    pub validation: Option<RecipeValidationReport>,
    /// Why no verdict. `Some` iff `validation` is `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_unavailable: Option<String>,
}

/// Answer of `POST /v1/recipe-projects/{id}/checkpoints/{cp}/restore`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreCheckpointOutcome {
    /// The restoration-anchor checkpoint the restore created.
    pub new_checkpoint_id: String,
    /// The checkpoint restored from.
    pub source_checkpoint_id: String,
}
