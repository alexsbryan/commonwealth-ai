// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recipe-author project's vocabulary, as a program that does not link
//! `sovereign-recipe-author` reads it (pb-ingest-rehome-daemon): the store row
//! and the on-disk summary. Both moved here from that crate, which re-exports
//! them at their old paths.
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
