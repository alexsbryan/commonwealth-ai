// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recipe-author project's vocabulary, as a program that does not link
//! `sovereign-recipe-author` reads it (pb-ingest-rehome-daemon): the store row
//! and the on-disk summary. Both moved here from that crate, which re-exports
//! them at their old paths.

use serde::{Deserialize, Serialize};

use crate::daemon_wire::ArtifactKind;

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
