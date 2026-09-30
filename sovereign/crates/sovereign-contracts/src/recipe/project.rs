// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recipe-author project's vocabulary, as a program that does not link
//! `sovereign-recipe-author` reads it (pb-ingest-rehome-daemon): the store row
//! and the on-disk summary. Both moved here from that crate, which re-exports
//! them at their old paths, as are the capability-request inbox file and its
//! directory (pb-ingest-rehome).
//!
//! And the port itself: [`RecipeProjectPort`] is how svrn reaches the
//! recipe-project store and the project model, which are ingest's
//! (sovereign-recipe-author implements it). svrn hands in its notes and
//! gets back a [`RecipeAuthoring`]; svrn alone has none, and says so by name.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::daemon_wire::{ArtifactKind, CheckpointMeta};
use crate::error::Result;
use crate::tool_bundle::ToolBundle;

/// One loaded recipe-author project. The implementor is the project model
/// (`sovereign_recipe_author::RecipeProject`).
#[async_trait]
pub trait RecipeProjectHandle: Send + Sync {
    /// The project's `feature_id`.
    fn feature_id(&self) -> &str;
    /// The project's title.
    fn title(&self) -> &str;
    /// The project's directory on this host.
    fn project_dir(&self) -> &std::path::Path;
    /// The on-disk sidecar summary.
    fn read_summary(&self) -> Result<ProjectSummary>;
    /// Replace the on-disk sidecar summary.
    fn write_summary(&self, summary: &ProjectSummary) -> Result<()>;
    /// The project's checkpoints, oldest first.
    fn list_checkpoints(&self) -> Result<Vec<CheckpointMeta>>;
    /// Restore `checkpoint_id` over the artifact `artifact_id` (when one is
    /// linked) and lay down the restore-anchor checkpoint; its id.
    async fn restore_checkpoint(
        &self,
        checkpoint_id: &str,
        artifact_id: Option<&str>,
        session_id: &str,
    ) -> Result<String>;
    /// The per-turn situated-context block.
    async fn situated_context(&self) -> Result<String>;
}

/// The recipe-project store and project model, as svrn reaches them.
#[async_trait]
pub trait RecipeProjectPort: Send + Sync {
    /// Every project row, newest-updated first.
    async fn list(&self, include_archived: bool) -> Result<Vec<RecipeProjectRow>>;
    /// One project row, or `None`.
    async fn get(&self, id: &str) -> Result<Option<RecipeProjectRow>>;
    /// Insert a row under a caller-supplied id. An empty or taken id is
    /// `Error::InvalidInput` with the store's own words.
    async fn provision(&self, id: &str, title: &str, charter_md: &str) -> Result<RecipeProjectRow>;
    /// Provision a project and lay down its artifact tree; the id is minted
    /// from the project's essence by the implementor.
    async fn create(
        &self,
        title: &str,
        charter_md: &str,
        kind: ArtifactKind,
    ) -> Result<Box<dyn RecipeProjectHandle>>;
    /// Load an existing project.
    async fn load(&self, feature_id: &str) -> Result<Box<dyn RecipeProjectHandle>>;
    /// The root a kind's artifacts live under on this host; `None` when it
    /// cannot be resolved.
    fn artifact_root(&self, kind: ArtifactKind) -> Option<PathBuf>;
}

/// Recipe authoring as ingest composes it over svrn's notes: the project
/// port, when the store opened, and the recipe-authoring tool bundle.
pub struct RecipeAuthoring {
    /// The port, or why the store did not open.
    pub projects: std::result::Result<Arc<dyn RecipeProjectPort>, String>,
    /// The recipe-authoring tools; without a store they report the
    /// store-backed ones withheld.
    pub tools: Box<dyn ToolBundle>,
}

/// One row of the `recipe_projects` table. Field names match the subset of
/// the former `FeatureRow` the recipe-author surface actually read, so the
/// migration was a type swap, not a field rename.
#[derive(Debug, Clone)]
pub struct RecipeProjectRow {
    /// The project's `feature_id`.
    pub id: String,
    /// The project's title.
    pub title: String,
    /// The partner's charter, whole.
    pub charter_md: String,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds.
    pub updated_at: i64,
    /// Unix seconds when archived; `None` means active.
    pub archived_at: Option<i64>,
}

/// On-disk per-project summary, kept small so `RecipeProject::load`
/// doesn't have to walk the whole project directory. Updated by
/// tools that change state — `recipe_id` is set when a recipe is
/// first written, `last_test_*` after each `RecipeTestTool` run,
/// `current_sample_size` as the agent climbs the 50→200→1000→full
/// progression.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSummary {
    /// The project's `feature_id`.
    pub feature_id: String,
    /// The project's title.
    pub title: String,
    /// What this project authors (recipe vs workflow). `#[serde(default)]` →
    /// pre-tag `project.json` files decode as `Recipe`.
    #[serde(default)]
    pub artifact_kind: ArtifactKind,
    /// Recipe id under `~/.svrnmesh/recipes/<recipe_id>/`. `None`
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
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds.
    pub updated_at: i64,
}

/// The port's double, for a svrn test that drives its own routes (FIVE_PROGRAMS
/// "Where a cross-program test lives"; off by default like `notes::fixtures`).
/// The store and model behaviour it stands in for is proven on the
/// implementor, sovereign-recipe-author's `port::ProjectStorePort`.
///
/// It keeps rows and summaries in memory, because a round trip is the whole
/// contract of those methods and a test reads it back; a provision refuses an
/// empty or taken id in the store's words. Everything else never answers
/// success-shaped (principle 6): `restore_checkpoint` refuses naming itself,
/// and `artifact_root` panics unless the test gave one.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::error::Error;

    fn unprogrammed(method: &str) -> String {
        format!("RecipeProjectsDouble::{method}: not programmed by this test")
    }

    type Summaries = Arc<Mutex<HashMap<String, ProjectSummary>>>;

    /// The recipe-project port's double.
    #[derive(Default)]
    pub struct RecipeProjectsDouble {
        rows: Mutex<Vec<RecipeProjectRow>>,
        summaries: Summaries,
        artifact_root: Option<PathBuf>,
    }

    impl RecipeProjectsDouble {
        /// An empty store.
        pub fn new() -> Self {
            Self::default()
        }

        /// Artifacts live under `root`: recipes in `recipes/`, workflows in
        /// `workflows/`, the layout the implementor keeps under svrn's root.
        pub fn with_artifact_root(mut self, root: PathBuf) -> Self {
            self.artifact_root = Some(root);
            self
        }

        fn handle(&self, row: &RecipeProjectRow) -> Box<dyn RecipeProjectHandle> {
            Box::new(DoubleProject {
                feature_id: row.id.clone(),
                title: row.title.clone(),
                summaries: Arc::clone(&self.summaries),
            })
        }
    }

    struct DoubleProject {
        feature_id: String,
        title: String,
        summaries: Summaries,
    }

    #[async_trait]
    impl RecipeProjectHandle for DoubleProject {
        fn feature_id(&self) -> &str {
            &self.feature_id
        }

        fn title(&self) -> &str {
            &self.title
        }

        fn project_dir(&self) -> &std::path::Path {
            panic!("{}", unprogrammed("project_dir"))
        }

        fn read_summary(&self) -> Result<ProjectSummary> {
            self.summaries
                .lock()
                .expect("summaries lock")
                .get(&self.feature_id)
                .cloned()
                .ok_or_else(|| Error::InvalidInput(format!("no summary for `{}`", self.feature_id)))
        }

        fn write_summary(&self, summary: &ProjectSummary) -> Result<()> {
            self.summaries
                .lock()
                .expect("summaries lock")
                .insert(self.feature_id.clone(), summary.clone());
            Ok(())
        }

        fn list_checkpoints(&self) -> Result<Vec<CheckpointMeta>> {
            Ok(Vec::new())
        }

        async fn restore_checkpoint(
            &self,
            _checkpoint_id: &str,
            _artifact_id: Option<&str>,
            _session_id: &str,
        ) -> Result<String> {
            Err(Error::InvalidInput(unprogrammed("restore_checkpoint")))
        }

        async fn situated_context(&self) -> Result<String> {
            Ok(format!("[Charter] {}\n", self.title))
        }
    }

    #[async_trait]
    impl RecipeProjectPort for RecipeProjectsDouble {
        async fn list(&self, _include_archived: bool) -> Result<Vec<RecipeProjectRow>> {
            let mut rows = self.rows.lock().expect("rows lock").clone();
            rows.reverse();
            Ok(rows)
        }

        async fn get(&self, id: &str) -> Result<Option<RecipeProjectRow>> {
            let rows = self.rows.lock().expect("rows lock");
            Ok(rows.iter().find(|r| r.id == id).cloned())
        }

        async fn provision(
            &self,
            id: &str,
            title: &str,
            charter_md: &str,
        ) -> Result<RecipeProjectRow> {
            if id.is_empty() {
                return Err(Error::InvalidInput(
                    "recipe project id cannot be empty".into(),
                ));
            }
            let mut rows = self.rows.lock().expect("rows lock");
            if rows.iter().any(|r| r.id == id) {
                return Err(Error::InvalidInput(format!(
                    "recipe project '{id}' already exists"
                )));
            }
            let at = sovereign_time::unix_now();
            let row = RecipeProjectRow {
                id: id.into(),
                title: title.into(),
                charter_md: charter_md.into(),
                created_at: at,
                updated_at: at,
                archived_at: None,
            };
            rows.push(row.clone());
            Ok(row)
        }

        async fn create(
            &self,
            title: &str,
            charter_md: &str,
            kind: ArtifactKind,
        ) -> Result<Box<dyn RecipeProjectHandle>> {
            let id = format!("double-{}", self.rows.lock().expect("rows lock").len() + 1);
            let row = self.provision(&id, title, charter_md).await?;
            self.summaries.lock().expect("summaries lock").insert(
                id.clone(),
                ProjectSummary {
                    feature_id: id,
                    title: title.into(),
                    artifact_kind: kind,
                    recipe_id: None,
                    current_sample_size: None,
                    last_test_status: None,
                    last_test_at: None,
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                },
            );
            Ok(self.handle(&row))
        }

        async fn load(&self, feature_id: &str) -> Result<Box<dyn RecipeProjectHandle>> {
            match self.get(feature_id).await? {
                Some(row) => Ok(self.handle(&row)),
                None => Err(Error::InvalidInput(format!(
                    "no recipe-author project with feature_id `{feature_id}`"
                ))),
            }
        }

        fn artifact_root(&self, kind: ArtifactKind) -> Option<PathBuf> {
            let root = self
                .artifact_root
                .as_ref()
                .unwrap_or_else(|| panic!("{}", unprogrammed("artifact_root")));
            Some(match kind {
                ArtifactKind::Recipe => root.join("recipes"),
                ArtifactKind::Workflow => root.join("workflows"),
            })
        }
    }
}

/// Persisted shape of a capability request. Kept compatible with
/// `serde_json::from_str` so the maintainer inbox CLI can read
/// without depending on this crate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityRequest {
    /// The request's id; its inbox file is `<request_id>.json`.
    pub request_id: String,
    /// The project that raised it.
    pub feature_id: String,
    /// That project's title.
    pub project_title: String,
    /// The format or source the extractors could not handle.
    pub format_or_source: String,
    /// The agent's analysis of the gap.
    pub analysis: String,
    /// Extractors the agent tried first.
    pub existing_extractors_tried: Vec<String>,
    /// How each attempt failed.
    pub failure_modes: Vec<String>,
    /// The recipe as it stood, when one was drafted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe_state_path: Option<String>,
    /// The recipe sections the gap blocks.
    pub blocked_recipe_parts: Vec<String>,
    /// Submission status. v1 ships only `submitted` from the agent;
    /// the maintainer flips this to `in_progress` / `resolved` /
    /// `won't_fix` out-of-band by editing the inbox file.
    pub status: String,
    /// RFC 3339.
    pub created_at: String,
}

/// Default global maintainer inbox directory (created on demand).
/// CapabilityRequestTool mirrors per-project requests into this
/// directory so the maintainer can `sovereign maintainer inbox` to
/// page through every project's pending requests at once.
pub const MAINTAINER_INBOX_SUBPATH: &str = "capability-requests/inbox";

/// Resolve `~/.svrnmesh/capability-requests/inbox/`.
pub fn maintainer_inbox_dir() -> Result<PathBuf> {
    Ok(crate::rebrand::svrnmesh_root().join(MAINTAINER_INBOX_SUBPATH))
}
