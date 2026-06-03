//! `RecipeProject` — the recipe-author "project" model.
//!
//! A project is the unit of work the partner builds end to end —
//! one corpus + (optionally) one investigation schema. Spec §4.1
//! says project state should reuse the existing `NoteStore` and
//! `FeatureStore` "with extended kinds," so this data layer is
//! deliberately thin: a `FeatureRow` (state = `RecipeAuthoring`)
//! holds the charter and gives every feature-scoped note a stable
//! `feature_id` to anchor to, and a sidecar directory under
//! `~/.sovereign/recipe-projects/<feature_id>/` carries the
//! recipe-shaped state that doesn't fit a SQLite row (the recipe
//! TOML lives separately under `~/.sovereign/recipes/<id>/`,
//! addressed by `recipe_id`).
//!
//! On-disk layout:
//!
//! ```text
//! ~/.sovereign/recipe-projects/<feature_id>/
//!   project.json                   summary (recipe_id, sample size, last test)
//!   checkpoints/<ts>-<slug>/       per checkpoint
//!     meta.json                    name, summary, trigger, restored_from?
//!     recipe.toml                  snapshot
//!     decision_frontier.json       last NoteStore note id at write time
//!   capability-requests/<ts>.json  per request (also mirrored to global inbox)
//!   research/<ts>.json             web-search findings with authority tag
//! ```
//!
//! Tools (`CheckpointTool`, `DecisionLogTool`, `CapabilityRequestTool`)
//! all funnel through this layer rather than touching paths
//! themselves. Single source of truth for layout makes it cheap to
//! relocate (e.g. behind a configurable `SOVEREIGN_RECIPE_PROJECTS_DIR`)
//! later.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use corpus_engine_atos::{FeatureRow, FeatureState, FeatureStore};
use corpus_engine_notes::{NoteRow, NoteScope, NoteStore, ScopeFilter};
use sovereign_core::error::{Error, Result};

/// Wrap an `std::io::Error` into a `sovereign_core::Error` carrying
/// the path that produced it. Matches the convention in
/// `recipe_author/{read,write}.rs` of using `InvalidInput` for
/// path-scoped IO failures so the agent can read and react.
fn io_err<P: AsRef<Path>>(op: &str, path: P, e: std::io::Error) -> Error {
    Error::InvalidInput(format!("{op} {}: {e}", path.as_ref().display()))
}

/// Bridge a `corpus_engine::Error` into a `sovereign_core::Error`.
/// The two crates' error enums are intentionally disjoint; everything
/// from corpus-engine surfaces as a `Storage` failure here, which is
/// the closest matching variant.
fn ce_err(e: corpus_engine::Error) -> Error {
    Error::Storage(e.to_string())
}

/// Same shape for `corpus-engine-atos::Error` (FeatureStore + plan_items
/// + design_signals carved out 2026-05-23). FeatureStore call sites
/// bubble the atos error type now.
fn ce_atos_err(e: corpus_engine_atos::Error) -> Error {
    Error::Storage(e.to_string())
}

/// Same shape for `corpus-engine-notes::Error` (NoteStore carved out
/// 2026-05-23, step 3). NoteStore call sites bubble this error type.
fn ce_notes_err(e: corpus_engine_notes::Error) -> Error {
    Error::Storage(e.to_string())
}

/// Default global maintainer inbox directory (created on demand).
/// CapabilityRequestTool mirrors per-project requests into this
/// directory so the maintainer can `sovereign maintainer inbox` to
/// page through every project's pending requests at once.
pub const MAINTAINER_INBOX_SUBPATH: &str = "capability-requests/inbox";

/// Resolve `~/.sovereign/recipe-projects/`. Tools and CLI go through
/// this rather than building paths inline.
pub fn projects_root_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".sovereign").join("recipe-projects"))
        .ok_or_else(|| {
            Error::InvalidInput("HOME not set; cannot locate ~/.sovereign/recipe-projects/".into())
        })
}

/// Resolve `~/.sovereign/capability-requests/inbox/`.
pub fn maintainer_inbox_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(|h| {
            PathBuf::from(h)
                .join(".sovereign")
                .join(MAINTAINER_INBOX_SUBPATH)
        })
        .ok_or_else(|| {
            Error::InvalidInput(
                "HOME not set; cannot locate ~/.sovereign/capability-requests/inbox/".into(),
            )
        })
}

/// On-disk per-project summary, kept small so `RecipeProject::load`
/// doesn't have to walk the whole project directory. Updated by
/// tools that change state — `recipe_id` is set when a recipe is
/// first written, `last_test_*` after each `RecipeTestTool` run,
/// `current_sample_size` as the agent climbs the 50→200→1000→full
/// progression.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub feature_id: String,
    pub title: String,
    /// Recipe id under `~/.sovereign/recipes/<recipe_id>/`. `None`
    /// before the agent has drafted a recipe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe_id: Option<String>,
    /// Current sample size in the test progression. `None` until the
    /// first test run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_sample_size: Option<u64>,
    /// Pass/fail summary of the most recent `recipe_test` run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_test_status: Option<String>,
    /// Human-readable timestamp (RFC 3339) of the most recent test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_test_at: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Decision-log frontier captured at checkpoint time. Restoring a
/// checkpoint preserves the decision and research logs (spec §4.2)
/// — this id is the audit-trail marker that lets the dashboard
/// render "everything before this id led up to checkpoint X."
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionFrontier {
    /// Latest NoteStore note id at scope=feature, feature_id=this
    /// project, at the moment of snapshot. May be `None` for the
    /// project-creation checkpoint, before any decision has landed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_note_id: Option<String>,
    /// Total feature-scoped note count at snapshot time. Cheap
    /// "size of the log so far" indicator for the dashboard.
    pub note_count: usize,
}

/// On-disk metadata for one checkpoint. The `restored_from` field
/// is set on checkpoints created by `RecipeProject::restore`; the
/// dashboard renders these with a "↳ restored from <name>" marker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointMeta {
    /// Stable id (the directory basename: `<ts>-<slug>`).
    pub checkpoint_id: String,
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

/// Live handle on a recipe-author project. Cheap to construct;
/// every disk-touching method is async and goes through `tokio::fs`.
pub struct RecipeProject {
    feature_id: String,
    title: String,
    project_dir: PathBuf,
    notes: Arc<NoteStore>,
    features: Arc<FeatureStore>,
}

impl RecipeProject {
    /// Provision a fresh project. Allocates a v4 UUID feature_id,
    /// writes the FeatureRow at state=`RecipeAuthoring`, lays down
    /// the on-disk directory + summary, and returns the live handle.
    pub async fn new(
        title: &str,
        charter_md: &str,
        notes: Arc<NoteStore>,
        features: Arc<FeatureStore>,
    ) -> Result<Self> {
        let feature_id = uuid::Uuid::new_v4().to_string();
        features
            .provision_recipe_project(&feature_id, title, charter_md)
            .await
            .map_err(ce_atos_err)?;

        let project_dir = Self::project_dir_for(&feature_id)?;
        std::fs::create_dir_all(&project_dir)
            .map_err(|e| io_err("create_dir_all", &project_dir, e))?;
        let checkpoints = project_dir.join("checkpoints");
        std::fs::create_dir_all(&checkpoints)
            .map_err(|e| io_err("create_dir_all", &checkpoints, e))?;
        let cap = project_dir.join("capability-requests");
        std::fs::create_dir_all(&cap).map_err(|e| io_err("create_dir_all", &cap, e))?;
        let research = project_dir.join("research");
        std::fs::create_dir_all(&research).map_err(|e| io_err("create_dir_all", &research, e))?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let summary = ProjectSummary {
            feature_id: feature_id.clone(),
            title: title.into(),
            recipe_id: None,
            current_sample_size: None,
            last_test_status: None,
            last_test_at: None,
            created_at: now,
            updated_at: now,
        };
        Self::write_summary_to_dir(&project_dir, &summary)?;

        Ok(Self {
            feature_id,
            title: title.into(),
            project_dir,
            notes,
            features,
        })
    }

    /// Load an existing project by feature_id. Returns
    /// [`Error::InvalidInput`] if no FeatureRow exists or the row
    /// isn't in the `RecipeAuthoring` state.
    pub async fn load(
        feature_id: &str,
        notes: Arc<NoteStore>,
        features: Arc<FeatureStore>,
    ) -> Result<Self> {
        let row = features
            .get(feature_id)
            .await
            .map_err(ce_atos_err)?
            .ok_or_else(|| {
                Error::InvalidInput(format!(
                    "no recipe-author project with feature_id `{feature_id}`"
                ))
            })?;
        if FeatureState::parse(&row.state) != Some(FeatureState::RecipeAuthoring) {
            return Err(Error::InvalidInput(format!(
                "feature `{feature_id}` is not a recipe-author project (state = {})",
                row.state
            )));
        }
        let project_dir = Self::project_dir_for(feature_id)?;
        // `load` is also used right after a fresh provision in tests
        // that want to round-trip through disk — be tolerant of a
        // not-yet-created project dir and lay it down on demand.
        if !project_dir.exists() {
            std::fs::create_dir_all(&project_dir)
                .map_err(|e| io_err("create_dir_all", &project_dir, e))?;
            let checkpoints = project_dir.join("checkpoints");
            std::fs::create_dir_all(&checkpoints)
                .map_err(|e| io_err("create_dir_all", &checkpoints, e))?;
            let cap = project_dir.join("capability-requests");
            std::fs::create_dir_all(&cap).map_err(|e| io_err("create_dir_all", &cap, e))?;
            let research = project_dir.join("research");
            std::fs::create_dir_all(&research)
                .map_err(|e| io_err("create_dir_all", &research, e))?;
        }
        Ok(Self {
            feature_id: row.id,
            title: row.title,
            project_dir,
            notes,
            features,
        })
    }

    pub fn feature_id(&self) -> &str {
        &self.feature_id
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    pub fn checkpoints_dir(&self) -> PathBuf {
        self.project_dir.join("checkpoints")
    }

    pub fn capability_requests_dir(&self) -> PathBuf {
        self.project_dir.join("capability-requests")
    }

    pub fn research_dir(&self) -> PathBuf {
        self.project_dir.join("research")
    }

    /// Read the persisted summary. Returns a default summary anchored
    /// to this feature_id when the file is missing — the agent's
    /// first interaction may pre-date the on-disk write.
    pub fn read_summary(&self) -> Result<ProjectSummary> {
        let path = self.project_dir.join("project.json");
        if !path.exists() {
            return Ok(ProjectSummary {
                feature_id: self.feature_id.clone(),
                title: self.title.clone(),
                recipe_id: None,
                current_sample_size: None,
                last_test_status: None,
                last_test_at: None,
                created_at: 0,
                updated_at: 0,
            });
        }
        let text = std::fs::read_to_string(&path).map_err(|e| io_err("read", &path, e))?;
        serde_json::from_str(&text)
            .map_err(|e| Error::InvalidInput(format!("malformed project.json: {e}")))
    }

    /// Replace the persisted summary atomically (write-then-rename).
    pub fn write_summary(&self, summary: &ProjectSummary) -> Result<()> {
        Self::write_summary_to_dir(&self.project_dir, summary)
    }

    fn write_summary_to_dir(dir: &Path, summary: &ProjectSummary) -> Result<()> {
        let path = dir.join("project.json");
        let part = path.with_extension("json.part");
        let bytes = serde_json::to_vec_pretty(summary).map_err(|e| {
            Error::InvalidInput(format!("failed to serialise project summary: {e}"))
        })?;
        std::fs::write(&part, &bytes).map_err(|e| io_err("write", &part, e))?;
        std::fs::rename(&part, &path).map_err(|e| io_err("rename to", &path, e))
    }

    /// List the project's checkpoints in chronological order
    /// (oldest first — directory basenames sort lexicographically
    /// because they're prefixed with the unix timestamp).
    pub fn list_checkpoints(&self) -> Result<Vec<CheckpointMeta>> {
        let dir = self.checkpoints_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|e| io_err("read_dir", &dir, e))?
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.path())
            .collect();
        entries.sort();
        let mut out = Vec::with_capacity(entries.len());
        for path in entries {
            let meta_path = path.join("meta.json");
            if !meta_path.exists() {
                continue;
            }
            let text =
                std::fs::read_to_string(&meta_path).map_err(|e| io_err("read", &meta_path, e))?;
            let meta: CheckpointMeta = serde_json::from_str(&text).map_err(|e| {
                Error::InvalidInput(format!(
                    "malformed checkpoint meta at {}: {e}",
                    meta_path.display()
                ))
            })?;
            out.push(meta);
        }
        Ok(out)
    }

    /// Read the recipe TOML snapshot stored inside a checkpoint.
    /// Returns [`Error::InvalidInput`] when the checkpoint id is
    /// unknown or the snapshot is missing.
    pub fn read_checkpoint_recipe(&self, checkpoint_id: &str) -> Result<String> {
        let path = self
            .checkpoints_dir()
            .join(checkpoint_id)
            .join("recipe.toml");
        if !path.exists() {
            return Err(Error::InvalidInput(format!(
                "checkpoint `{checkpoint_id}` has no recipe.toml snapshot"
            )));
        }
        std::fs::read_to_string(&path).map_err(|e| io_err("read", &path, e))
    }

    /// Read the decision-log frontier captured by a checkpoint.
    pub fn read_checkpoint_frontier(&self, checkpoint_id: &str) -> Result<DecisionFrontier> {
        let path = self
            .checkpoints_dir()
            .join(checkpoint_id)
            .join("decision_frontier.json");
        if !path.exists() {
            return Err(Error::InvalidInput(format!(
                "checkpoint `{checkpoint_id}` has no decision_frontier.json"
            )));
        }
        let text = std::fs::read_to_string(&path).map_err(|e| io_err("read", &path, e))?;
        serde_json::from_str(&text)
            .map_err(|e| Error::InvalidInput(format!("malformed decision_frontier.json: {e}")))
    }

    /// Recent feature-scoped notes for this project, newest first.
    /// Used by the situated-context renderer and the dashboard's
    /// decision feed.
    pub async fn recent_feature_notes(&self, limit: usize) -> Result<Vec<NoteRow>> {
        let scope = ScopeFilter {
            scopes: vec![NoteScope::Feature],
            feature_id: Some(self.feature_id.clone()),
        };
        self.notes
            .read_notes_scoped(None, &[], &[], &[], limit, false, &scope)
            .await
            .map_err(ce_notes_err)
    }

    /// FeatureStore handle access for sibling tools that need to
    /// re-read state (e.g. checkpoint restore touching the title).
    pub fn features(&self) -> &Arc<FeatureStore> {
        &self.features
    }

    /// NoteStore handle for sibling tools.
    pub fn notes(&self) -> &Arc<NoteStore> {
        &self.notes
    }

    /// Resolve the on-disk project directory for `feature_id`.
    /// Public so tools doing one-shot reads (e.g. a CLI dump command)
    /// don't need a live `RecipeProject`.
    pub fn project_dir_for(feature_id: &str) -> Result<PathBuf> {
        Ok(projects_root_dir()?.join(feature_id))
    }
}

/// Convenience helper for tools that have a feature_id and need to
/// fetch the underlying FeatureRow without going through the full
/// `RecipeProject::load` (which also enforces state validation —
/// that's the right behaviour for tool entry points but the wrong
/// shape for read-only situated-context renders).
pub async fn feature_row_for(
    feature_id: &str,
    features: &FeatureStore,
) -> Result<Option<FeatureRow>> {
    features.get(feature_id).await.map_err(ce_atos_err)
}
