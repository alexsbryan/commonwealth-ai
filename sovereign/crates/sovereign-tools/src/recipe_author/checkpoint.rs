//! `CheckpointTool` — name a recoverable point in a recipe-author project.
//!
//! Spec §4.2 says the partner can back up to "any prior decision and
//! try a different direction without losing earlier work." A
//! checkpoint snapshots three things into a stable directory:
//!
//! 1. The current recipe TOML (so a restore puts the agent back on a
//!    known-good draft).
//! 2. The decision-log frontier — the last NoteStore note id at this
//!    project's feature scope. The restore preserves the decision
//!    and research logs (spec §4.2: "with a clear marker of where
//!    the restoration happened"), so the dashboard can render
//!    "everything before this id led up to checkpoint X."
//! 3. A small `meta.json` carrying the partner-facing name, the
//!    trigger that produced the checkpoint, an optional summary,
//!    and a `restored_from` field set when this checkpoint exists
//!    because of a `RecipeProject::restore` call.
//!
//! The tool also writes a NoteStore note `kind = "checkpoint"` so the
//! decision feed renders the checkpoint inline with the decisions
//! that surrounded it. For restore-anchor checkpoints, an additional
//! `kind = "checkpoint_restored"` note is written.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use corpus_engine::{
    FeatureStore, NoteScope, NoteSource, NoteStore, ScopeFilter,
};
use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use super::project::{
    CheckpointMeta, DecisionFrontier, RecipeProject,
};

/// Produced by `CheckpointTool::do_create`. Surfaced through both
/// the tool API and the situated-context renderer; pulling the
/// shared shape into a struct stops the JSON layout from drifting.
#[derive(Debug, Clone)]
pub struct CheckpointOutcome {
    pub checkpoint_id: String,
    pub snapshot_path: PathBuf,
}

#[derive(Default)]
pub struct CheckpointTool {
    notes: Option<Arc<NoteStore>>,
    features: Option<Arc<FeatureStore>>,
    /// Test-only override for the recipes directory so tests don't
    /// have to touch process-global `HOME`. None in production.
    recipes_dir: Option<PathBuf>,
}

impl CheckpointTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_stores(notes: Arc<NoteStore>, features: Arc<FeatureStore>) -> Self {
        Self {
            notes: Some(notes),
            features: Some(features),
            recipes_dir: None,
        }
    }

    pub fn with_recipes_dir(mut self, dir: PathBuf) -> Self {
        self.recipes_dir = Some(dir);
        self
    }
}

#[async_trait]
impl Tool for CheckpointTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "checkpoint".into(),
            name: "Checkpoint".into(),
            description:
                "Name a recoverable state of the recipe-author project. Use \
                 this when: the project is created (`project_creation`), the \
                 sample size scales up (`auto_scale_up`), the extraction \
                 strategy changes substantially (`auto_strategy_change`), or \
                 the partner asks to checkpoint (`partner_request`). The \
                 partner can later ask to back up to any checkpoint and try a \
                 different direction without losing decision and research \
                 logs."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "feature_id": {
                        "type": "string",
                        "description": "Recipe-author project id"
                    },
                    "name": {
                        "type": "string",
                        "description":
                            "Short partner-facing name (e.g. \"after switching \
                             to Ninth-Circuit-only\")"
                    },
                    "summary": {
                        "type": "string",
                        "description":
                            "One-paragraph 'where we are' note. Optional but \
                             encouraged — the partner reads this when \
                             choosing a restore target."
                    },
                    "trigger": {
                        "type": "string",
                        "enum": [
                            "project_creation", "auto_scale_up",
                            "auto_strategy_change", "partner_request"
                        ]
                    },
                    "recipe_path": {
                        "type": "string",
                        "description":
                            "Recipe id (loads <id>/recipe.toml) or relative \
                             path under ~/.sovereign/recipes/. The TOML at \
                             this path is snapshotted into the checkpoint. \
                             Omit to skip the recipe snapshot — useful for \
                             the project-creation checkpoint, before any \
                             recipe has been drafted."
                    }
                },
                "required": ["feature_id", "name", "trigger"]
            }),
            examples: vec![ToolExample {
                situation:
                    "Partner finished tuning the citation-graph schema and \
                     asked to checkpoint before exploring counsel-of-record."
                        .into(),
                call: json!({
                    "feature_id": "<project-uuid>",
                    "name": "citation-graph schema settled",
                    "trigger": "partner_request",
                    "summary": "Schema covers cite_to / cite_from edges with \
                                court / date attributes; passing 200-doc test.",
                    "recipe_path": "courtlistener-trial"
                }),
            }],
            effect: Effect::Write,
            idempotency: Idempotency::NonIdempotent,
            latency: Latency::Instant,
            scope: Scope::Persistent,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "checkpoint_id": {"type": "string"},
                    "snapshot_path": {"type": "string"}
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![Permission::RecipeAuthoring]
    }

    async fn execute(
        &self,
        params: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let notes = self.notes.as_ref().ok_or_else(|| {
            Error::InvalidInput(
                "CheckpointTool was constructed without a NoteStore".into(),
            )
        })?;
        let features = self.features.as_ref().ok_or_else(|| {
            Error::InvalidInput(
                "CheckpointTool was constructed without a FeatureStore".into(),
            )
        })?;
        let feature_id = params
            .get("feature_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::InvalidInput("CheckpointTool requires `feature_id`".into())
            })?;
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::InvalidInput("CheckpointTool requires `name`".into())
            })?;
        let trigger = params
            .get("trigger")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::InvalidInput("CheckpointTool requires `trigger`".into())
            })?;
        validate_trigger(trigger)?;
        let summary = params
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let recipe_path = params.get("recipe_path").and_then(|v| v.as_str());

        let project = RecipeProject::load(
            feature_id,
            Arc::clone(notes),
            Arc::clone(features),
        )
        .await?;

        let outcome = do_create(
            &project,
            name,
            &summary,
            trigger,
            recipe_path,
            self.recipes_dir.as_ref(),
            &ctx.conversation_id,
            None,
        )
        .await?;

        Ok(StepOutput::Json(json!({
            "checkpoint_id": outcome.checkpoint_id,
            "snapshot_path": outcome.snapshot_path.display().to_string(),
        })))
    }
}

/// Allow only the four trigger strings spec §4.2 names plus
/// `restore` (which the tool itself never accepts as input — see
/// `do_create`'s `restored_from` parameter — but which lands in
/// `meta.json` for restore-anchor checkpoints).
fn validate_trigger(s: &str) -> Result<()> {
    match s {
        "project_creation"
        | "auto_scale_up"
        | "auto_strategy_change"
        | "partner_request" => Ok(()),
        other => Err(Error::InvalidInput(format!(
            "CheckpointTool: unknown trigger `{other}`. Allowed: \
             project_creation | auto_scale_up | auto_strategy_change | \
             partner_request"
        ))),
    }
}

/// Slugify `name` into a directory-safe basename. ASCII alphanum +
/// `-`; everything else becomes `-`. Multiple `-`s collapse, leading
/// / trailing trimmed. Bounded at 48 chars so `<ts>-<slug>` fits on
/// every reasonable filesystem.
pub(crate) fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 48 {
        out.truncate(48);
        while out.ends_with('-') {
            out.pop();
        }
    }
    if out.is_empty() {
        out.push_str("checkpoint");
    }
    out
}

/// Wrap `std::io::Error` with a path-scoped `InvalidInput`. Matches
/// the convention in `project.rs` and the existing recipe-author
/// tools.
fn io_err<P: AsRef<Path>>(op: &str, path: P, e: std::io::Error) -> Error {
    Error::InvalidInput(format!("{op} {}: {e}", path.as_ref().display()))
}

/// Bridge a `corpus_engine::Error` into a `sovereign_core::Error`.
fn ce_err(e: corpus_engine::Error) -> Error {
    Error::Storage(e.to_string())
}

/// Shared checkpoint creation path. Used by both `CheckpointTool`
/// and `RecipeProject::restore`. `restored_from` is `Some(<source
/// checkpoint_id>)` only for restore-anchor checkpoints — it sets
/// the `meta.json` field and triggers an additional NoteStore note
/// `kind = "checkpoint_restored"`.
pub async fn do_create(
    project: &RecipeProject,
    name: &str,
    summary: &str,
    trigger: &str,
    recipe_path: Option<&str>,
    recipes_dir_override: Option<&PathBuf>,
    session_id: &str,
    restored_from: Option<&str>,
) -> Result<CheckpointOutcome> {
    let timestamp_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let checkpoint_id = format!("{timestamp_secs}-{}", slugify(name));
    let dir = project.checkpoints_dir().join(&checkpoint_id);
    std::fs::create_dir_all(&dir).map_err(|e| io_err("create_dir_all", &dir, e))?;

    // 1. Snapshot the recipe TOML if the agent supplied a path. The
    //    project-creation checkpoint may be empty here.
    if let Some(rpath) = recipe_path {
        let resolved =
            super::resolve_recipe_path(rpath, recipes_dir_override)?;
        match std::fs::read_to_string(&resolved) {
            Ok(content) => {
                let target = dir.join("recipe.toml");
                std::fs::write(&target, content)
                    .map_err(|e| io_err("write", &target, e))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Path resolves under ~/.sovereign/recipes/ but no
                // file exists yet — record the omission so a future
                // restore knows there's nothing to write back.
            }
            Err(e) => {
                return Err(io_err("read", &resolved, e));
            }
        }
    }

    // 2. Capture the decision-log frontier.
    let scope = ScopeFilter {
        scopes: vec![NoteScope::Feature],
        feature_id: Some(project.feature_id().to_string()),
    };
    let recent = project
        .notes()
        .read_notes_scoped(None, &[], &[], &[], 1, false, &scope)
        .await
        .map_err(ce_err)?;
    let last_note_id = recent.first().map(|n| n.id.clone());
    let all = project
        .notes()
        .read_notes_scoped(None, &[], &[], &[], 10_000, false, &scope)
        .await
        .map_err(ce_err)?;
    let frontier = DecisionFrontier {
        last_note_id,
        note_count: all.len(),
    };
    let frontier_path = dir.join("decision_frontier.json");
    let frontier_bytes = serde_json::to_vec_pretty(&frontier).map_err(|e| {
        Error::InvalidInput(format!("failed to serialise frontier: {e}"))
    })?;
    std::fs::write(&frontier_path, &frontier_bytes)
        .map_err(|e| io_err("write", &frontier_path, e))?;

    // 3. Write meta.json.
    let now_rfc = chrono::DateTime::<chrono::Utc>::from_timestamp(
        timestamp_secs as i64,
        0,
    )
    .map(|dt| dt.to_rfc3339())
    .unwrap_or_else(|| timestamp_secs.to_string());
    let meta = CheckpointMeta {
        checkpoint_id: checkpoint_id.clone(),
        name: name.to_string(),
        trigger: if restored_from.is_some() {
            "restore".to_string()
        } else {
            trigger.to_string()
        },
        summary: summary.to_string(),
        restored_from: restored_from.map(|s| s.to_string()),
        created_at: now_rfc,
    };
    let meta_path = dir.join("meta.json");
    let meta_bytes = serde_json::to_vec_pretty(&meta).map_err(|e| {
        Error::InvalidInput(format!("failed to serialise checkpoint meta: {e}"))
    })?;
    std::fs::write(&meta_path, &meta_bytes)
        .map_err(|e| io_err("write", &meta_path, e))?;

    // 4. NoteStore notes — one for the checkpoint, plus one for the
    //    restore marker when applicable.
    let payload = json!({
        "checkpoint_id": checkpoint_id,
        "name": name,
        "trigger": meta.trigger,
        "snapshot_path": dir.display().to_string(),
        "restored_from": restored_from,
    });
    project
        .notes()
        .write_note_full(
            "checkpoint",
            summary,
            Vec::new(),
            Vec::new(),
            session_id,
            NoteScope::Feature,
            Some(project.feature_id()),
            None,
            NoteSource::Agent,
            None,
            Some(&payload.to_string()),
        )
        .await
        .map_err(ce_err)?;

    if let Some(from) = restored_from {
        let restore_payload = json!({
            "from_checkpoint_id": from,
            "to_checkpoint_id": checkpoint_id,
        });
        project
            .notes()
            .write_note_full(
                "checkpoint_restored",
                &format!(
                    "Restored project state from checkpoint `{from}`."
                ),
                Vec::new(),
                Vec::new(),
                session_id,
                NoteScope::Feature,
                Some(project.feature_id()),
                None,
                NoteSource::Agent,
                None,
                Some(&restore_payload.to_string()),
            )
            .await
            .map_err(ce_err)?;
    }

    Ok(CheckpointOutcome {
        checkpoint_id,
        snapshot_path: dir,
    })
}

/// Restore a project to a prior checkpoint. Spec §4.2: the recipe
/// TOML and investigation schema reset to the snapshot; the
/// decision and research logs are preserved (a fresh
/// `kind = checkpoint_restored` note marks the restoration in the
/// log so the dashboard can render the temporal narrative).
///
/// Implemented as: overwrite `~/.sovereign/recipes/<recipe_id>/recipe.toml`
/// from the checkpoint snapshot, then create a new restore-anchor
/// checkpoint with `restored_from = source_id`.
pub async fn restore_checkpoint(
    project: &RecipeProject,
    source_checkpoint_id: &str,
    recipe_id: Option<&str>,
    recipes_dir_override: Option<&PathBuf>,
    session_id: &str,
) -> Result<CheckpointOutcome> {
    // 1. Read the snapshot TOML from the source checkpoint.
    let snapshot_text = project.read_checkpoint_recipe(source_checkpoint_id)?;

    // 2. Write it back to the active recipe path (when one is
    //    provided — early projects may not yet have a recipe id).
    if let Some(rid) = recipe_id {
        let resolved = super::resolve_recipe_path(rid, recipes_dir_override)?;
        if let Some(parent) = resolved.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| io_err("create_dir_all", parent, e))?;
        }
        let part = resolved.with_extension("toml.part");
        std::fs::write(&part, &snapshot_text)
            .map_err(|e| io_err("write", &part, e))?;
        std::fs::rename(&part, &resolved)
            .map_err(|e| io_err("rename to", &resolved, e))?;
    }

    // 3. Lay down a restore-anchor checkpoint marking the new state.
    do_create(
        project,
        &format!("restored from {source_checkpoint_id}"),
        &format!("Restored project state from checkpoint `{source_checkpoint_id}`."),
        // Trigger string is overridden by `restored_from.is_some()`
        // inside `do_create`; pass any valid value for the param check.
        "partner_request",
        recipe_id,
        recipes_dir_override,
        session_id,
        Some(source_checkpoint_id),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::FeatureStore;

    async fn fresh_project(
        recipes_dir: &Path,
    ) -> (RecipeProject, Arc<NoteStore>, Arc<FeatureStore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let notes = Arc::new(NoteStore::open(&dir.path().join("notes.db")).unwrap());
        let features =
            Arc::new(FeatureStore::open(&dir.path().join("features.db")).unwrap());
        // Per-test HOME so `RecipeProject::new` writes its sidecar dir
        // into the tempdir rather than the user's real home. Tests in
        // this module run sequentially (the assignment races other
        // suites that read HOME), so this is best-effort.
        std::env::set_var("HOME", dir.path());
        let project = RecipeProject::new(
            "trial",
            "Federal case law",
            Arc::clone(&notes),
            Arc::clone(&features),
        )
        .await
        .unwrap();
        std::fs::create_dir_all(recipes_dir).unwrap();
        (project, notes, features, dir)
    }

    #[tokio::test]
    async fn slugify_handles_punctuation() {
        assert_eq!(slugify("Citation graph: settled!"), "citation-graph-settled");
        assert_eq!(slugify("   "), "checkpoint");
        assert_eq!(slugify("CourtListener — Ninth Circuit"), "courtlistener-ninth-circuit");
    }

    #[tokio::test]
    async fn creates_checkpoint_writes_meta_and_frontier() {
        let recipes_dir = tempfile::tempdir().unwrap();
        let (project, _notes, _features, _dir) =
            fresh_project(recipes_dir.path()).await;
        let outcome = do_create(
            &project,
            "initial",
            "starting point",
            "project_creation",
            None,
            Some(&recipes_dir.path().to_path_buf()),
            "session-x",
            None,
        )
        .await
        .unwrap();
        assert!(outcome.snapshot_path.exists());
        assert!(outcome.snapshot_path.join("meta.json").exists());
        assert!(outcome.snapshot_path.join("decision_frontier.json").exists());
        assert!(!outcome.snapshot_path.join("recipe.toml").exists());

        let checkpoints = project.list_checkpoints().unwrap();
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(checkpoints[0].name, "initial");
        assert!(checkpoints[0].restored_from.is_none());
    }

    #[tokio::test]
    async fn snapshots_recipe_when_path_supplied() {
        let recipes_dir = tempfile::tempdir().unwrap();
        let recipe_subdir = recipes_dir.path().join("trial");
        std::fs::create_dir_all(&recipe_subdir).unwrap();
        std::fs::write(
            recipe_subdir.join("recipe.toml"),
            "[corpus]\nid = \"trial\"\n",
        )
        .unwrap();

        let (project, _notes, _features, _dir) =
            fresh_project(recipes_dir.path()).await;
        let outcome = do_create(
            &project,
            "after first draft",
            "drafted recipe",
            "auto_strategy_change",
            Some("trial"),
            Some(&recipes_dir.path().to_path_buf()),
            "session-x",
            None,
        )
        .await
        .unwrap();
        let snapshotted =
            std::fs::read_to_string(outcome.snapshot_path.join("recipe.toml")).unwrap();
        assert!(snapshotted.contains("\"trial\""));
    }

    #[tokio::test]
    async fn restore_writes_marker_and_resets_recipe() {
        let recipes_dir = tempfile::tempdir().unwrap();
        let recipe_subdir = recipes_dir.path().join("trial");
        std::fs::create_dir_all(&recipe_subdir).unwrap();
        std::fs::write(
            recipe_subdir.join("recipe.toml"),
            "[corpus]\nid = \"trial\"\nname = \"v1\"\n",
        )
        .unwrap();

        let (project, _notes, _features, _dir) =
            fresh_project(recipes_dir.path()).await;
        let first = do_create(
            &project,
            "v1",
            "",
            "partner_request",
            Some("trial"),
            Some(&recipes_dir.path().to_path_buf()),
            "session-x",
            None,
        )
        .await
        .unwrap();

        // Mutate the live recipe to simulate a wrong direction.
        std::fs::write(
            recipe_subdir.join("recipe.toml"),
            "[corpus]\nid = \"trial\"\nname = \"v2-wrong\"\n",
        )
        .unwrap();

        let _restore = restore_checkpoint(
            &project,
            &first.checkpoint_id,
            Some("trial"),
            Some(&recipes_dir.path().to_path_buf()),
            "session-x",
        )
        .await
        .unwrap();

        let restored_text =
            std::fs::read_to_string(recipe_subdir.join("recipe.toml")).unwrap();
        assert!(restored_text.contains("v1"));
        assert!(!restored_text.contains("v2-wrong"));

        // A `kind=checkpoint_restored` note should now exist on the
        // project's feature scope. Filter to that kind so this test
        // doesn't depend on the count of snap-checkpoint notes.
        let scope = ScopeFilter {
            scopes: vec![NoteScope::Feature],
            feature_id: Some(project.feature_id().to_string()),
        };
        let rows = project
            .notes()
            .read_notes_scoped(
                None,
                &[],
                &[],
                &["checkpoint_restored".to_string()],
                10,
                false,
                &scope,
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        let payload: serde_json::Value =
            serde_json::from_str(rows[0].payload_json.as_deref().unwrap()).unwrap();
        assert_eq!(payload["from_checkpoint_id"], first.checkpoint_id);
    }
}
