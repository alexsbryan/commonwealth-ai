// SPDX-License-Identifier: AGPL-3.0-or-later
//! Agent-callable tools for recipe authoring.
//!
//! Lets the chat LLM run the same author → validate → test → fix
//! loop a human would. Every tool here is allowlisted to the
//! user's `~/.svrnmesh/recipes/` directory; the approval gate
//! sees a single [`Permission::RecipeAuthoring`] per call so the
//! operator grants "yes, recipe authoring" once rather than
//! re-approving every file write.
//!
//! Composition (intended LLM flow):
//!
//! 1. [`RegistryBrowseTool`] — survey existing recipes for examples.
//! 2. [`RecipeReadTool`] — read a known recipe to mirror its shape.
//! 3. [`RecipeWriteTool`] — draft a new recipe under
//!    `~/.svrnmesh/recipes/<id>/recipe.toml`.
//! 4. [`RecipeValidateTool`] — schema + regex compile + URL-template
//!    placeholder + for_each cross-reference checks.
//! 5. [`RecipeTestTool`] — sample acquire / extract / chunk
//!    against real source data, with section-miss reporting for
//!    iterative regex tuning.
//!
//! Once the test report is clean, the agent (or the user) calls
//! `sovereign recipe publish` to add the recipe to the local
//! registry — the publish command lives in `sovereign-cli` so the
//! agent uses [`crate::shell::ShellTool`] (or the Tauri wrapper)
//! to invoke it. The Rust-level publish surface is intentionally
//! small.

pub mod capability_request;
pub mod checkpoint;
pub mod decision_log;
pub mod http_tester;
/// JSON→TOML conversion helpers now live in the shared contract crate so both
/// the recipe-author tools and the workflow-host author tools use one
/// implementation; re-exported here at the old path for zero importer churn.
pub use sovereign_contracts::recipe::json_to_toml;
/// Path-safety sandbox check, relocated to the shared contract crate; the
/// recipe root helpers below still wrap it.
pub use sovereign_contracts::recipe::paths::assert_under_root;
pub mod probe_url;
pub mod project;
pub mod read;
pub mod recipe_project_store;
pub mod recipe_schema;
pub mod registry_browse;
pub mod research_finding;
pub mod situated_context;
pub mod test_tool;
pub mod validate;
pub mod write;
pub mod write_structured;

/// In-memory `RecipeNotes` for the tool unit tests (keeps them inside the
/// package's dependency budget — the real store adapter is exercised by the
/// sovereign-tools integration test). See the module docs.
#[cfg(test)]
mod test_support;

/// Serialize every test that mutates the process-global `HOME` env var.
///
/// `RecipeProject::new` and the registry-browse tools resolve paths from
/// `HOME` at call time; tests point each project at a tempdir HOME via
/// `std::env::set_var`. Env vars are process-global while `cargo test`
/// runs tests concurrently — an unlocked test flipping `HOME` between a
/// peer's `set_var` and its HOME-derived read makes the peer's files land
/// in a tempdir that is then dropped (observed: checkpoint's
/// `decision_frontier.json` write dying with ENOENT under full-suite
/// load). EVERY test that sets `HOME` must hold this ONE lock for its
/// whole lifetime — a module-local mutex cannot exclude sibling modules.
#[cfg(test)]
pub(crate) fn home_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

pub use capability_request::CapabilityRequestTool;
pub use checkpoint::CheckpointTool;
pub use decision_log::DecisionLogTool;
pub use http_tester::HttpRecipeTester;
pub use probe_url::{detect_pagination_hint, PaginationHint, ProbeUrlTool};
pub use project::{
    maintainer_inbox_dir, projects_root_dir, ArtifactKind, CheckpointMeta, DecisionFrontier,
    ProjectSummary, RecipeProject,
};
pub use read::RecipeReadTool;
pub use recipe_schema::recipe_json_schema;
pub use registry_browse::RegistryBrowseTool;
pub use research_finding::{
    FindingConfidence, FindingScope, ResearchFindingPayload, ResearchFindingTool,
};
pub use test_tool::RecipeTestTool;
pub use validate::RecipeValidateTool;
pub use write::RecipeWriteTool;
pub use write_structured::RecipeWriteStructuredTool;

use std::path::PathBuf;

use sovereign_contracts::error::Result;

/// Resolve the user's local recipes directory: `~/.svrnmesh/recipes/`.
/// Returns `Result` for historical reasons — the underlying accessor
/// is infallible, so this now always succeeds; kept `Result` so
/// callers don't need to change.
pub fn local_recipes_dir() -> Result<PathBuf> {
    Ok(sovereign_contracts::rebrand::svrnmesh_root().join("recipes"))
}

/// Validate that `path` is inside the local recipes dir
/// (`~/.svrnmesh/recipes/` by default). The recipe-author tools
/// refuse to read or write outside this root — it's the safety net
/// that lets us register a `RecipeAuthoring` permission instead of
/// broad `FileWrite`.
///
/// Looks up the root via [`local_recipes_dir`]. For test code that
/// wants to inject a per-test root without mutating the global
/// `HOME` env var (which races across parallel tests), use
/// [`assert_under_root`] directly.
pub fn assert_under_local_recipes(path: &std::path::Path) -> Result<PathBuf> {
    assert_under_root(path, &local_recipes_dir()?)
}

/// Resolve a recipes-dir override or fall back to the
/// HOME-derived default. Each tool stores an `Option<PathBuf>`
/// override at construction; tests use the override to point at a
/// per-test tempdir without mutating process-global `HOME`.
fn resolve_root(override_dir: Option<&PathBuf>) -> Result<PathBuf> {
    match override_dir {
        Some(p) => Ok(p.clone()),
        None => local_recipes_dir(),
    }
}

/// Translate an agent-supplied path into the on-disk recipe TOML
/// path. Bare ids (no slash, no `.toml` suffix) expand to
/// `<id>/recipe.toml` because that's the canonical published-recipe
/// layout. Used by every tool below — duplicating the logic in each
/// would just risk drift.
pub(crate) fn resolve_recipe_path(input: &str, override_dir: Option<&PathBuf>) -> Result<PathBuf> {
    let candidate: PathBuf = if input.contains('/') || input.ends_with(".toml") {
        input.into()
    } else {
        format!("{input}/recipe.toml").into()
    };
    let root = resolve_root(override_dir)?;
    assert_under_root(&candidate, &root)
}

/// Resolve the user's local workflows directory: `~/.svrnmesh/workflows/`. The
/// tools-local mirror of `sovereign_workflow_host::workflows_dir()` — both derive
/// the same `~/.svrnmesh/workflows` path, but `checkpoint.rs` lives in
/// `sovereign-tools`, which `sovereign-workflow-host` depends on, so it cannot call
/// back into the host without a dependency cycle. Same `~/.svrnmesh` root as
/// [`local_recipes_dir`].
pub fn local_workflows_dir() -> Result<PathBuf> {
    Ok(sovereign_contracts::rebrand::svrnmesh_root().join("workflows"))
}

/// Translate an agent-supplied workflow ref into its on-disk TOML path. Unlike a
/// recipe (`<id>/recipe.toml`), a workflow is a single file `<id>.toml`, so a bare
/// id expands to `<id>.toml`. Scoped to `~/.svrnmesh/workflows/` via the same
/// traversal guard as recipes.
pub(crate) fn resolve_workflow_path(
    input: &str,
    override_dir: Option<&PathBuf>,
) -> Result<PathBuf> {
    let candidate: PathBuf = if input.ends_with(".toml") {
        input.into()
    } else {
        format!("{input}.toml").into()
    };
    let root = match override_dir {
        Some(p) => p.clone(),
        None => local_workflows_dir()?,
    };
    assert_under_root(&candidate, &root)
}

/// Resolve an artifact path by kind — the single dispatch point the checkpoint
/// snapshot/restore uses so it never hardcodes "recipe". Recipe → a
/// `<id>/recipe.toml` under `~/.svrnmesh/recipes/`; Workflow → a `<id>.toml`
/// under `~/.svrnmesh/workflows/`. `override_dir` is the kind's root override
/// (tests inject a tempdir).
pub(crate) fn resolve_artifact_path(
    kind: ArtifactKind,
    input: &str,
    override_dir: Option<&PathBuf>,
) -> Result<PathBuf> {
    match kind {
        ArtifactKind::Recipe => resolve_recipe_path(input, override_dir),
        ArtifactKind::Workflow => resolve_workflow_path(input, override_dir),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_paths() {
        let root = std::path::PathBuf::from("/tmp/x/.sovereign/recipes");
        let err = assert_under_root(std::path::Path::new("../../etc/passwd"), &root).unwrap_err();
        assert!(format!("{err}").contains(".."));
    }

    #[test]
    fn rejects_path_outside_recipes_dir() {
        let root = std::path::PathBuf::from("/tmp/x/.sovereign/recipes");
        let err = assert_under_root(std::path::Path::new("/etc/passwd"), &root).unwrap_err();
        assert!(format!("{err}").contains("outside"));
    }

    #[test]
    fn relative_path_resolves_under_recipes_dir() {
        let root = std::path::PathBuf::from("/tmp/x/.sovereign/recipes");
        let p = assert_under_root(std::path::Path::new("sec-investigation/recipe.toml"), &root)
            .unwrap();
        assert!(p.ends_with("sec-investigation/recipe.toml"));
        assert!(p.starts_with(&root));
    }
}
