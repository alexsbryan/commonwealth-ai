//! Agent-callable tools for recipe authoring.
//!
//! Lets the chat LLM run the same author → validate → test → fix
//! loop a human would. Every tool here is allowlisted to the
//! user's `~/.sovereign/recipes/` directory; the approval gate
//! sees a single [`Permission::RecipeAuthoring`] per call so the
//! operator grants "yes, recipe authoring" once rather than
//! re-approving every file write.
//!
//! Composition (intended LLM flow):
//!
//! 1. [`RegistryBrowseTool`] — survey existing recipes for examples.
//! 2. [`RecipeReadTool`] — read a known recipe to mirror its shape.
//! 3. [`RecipeWriteTool`] — draft a new recipe under
//!    `~/.sovereign/recipes/<id>/recipe.toml`.
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
pub mod json_to_toml;
pub mod probe_url;
pub mod project;
pub mod read;
pub mod recipe_schema;
pub mod registry_browse;
pub mod research_finding;
pub mod situated_context;
pub mod test_tool;
pub mod validate;
pub mod write;
pub mod write_structured;

pub use capability_request::CapabilityRequestTool;
pub use checkpoint::CheckpointTool;
pub use decision_log::DecisionLogTool;
pub use probe_url::{detect_pagination_hint, PaginationHint, ProbeUrlTool};
pub use project::{
    maintainer_inbox_dir, projects_root_dir, CheckpointMeta, DecisionFrontier, ProjectSummary,
    RecipeProject,
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

use sovereign_core::error::{Error, Result};

/// Resolve the user's local recipes directory:
/// `~/.sovereign/recipes/`. Returns an error when `HOME` is
/// missing — that surface lands as a tool-level failure the LLM
/// can read and react to (e.g. bail on the authoring loop) rather
/// than panicking.
pub fn local_recipes_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".sovereign").join("recipes"))
        .ok_or_else(|| {
            Error::InvalidInput(
                "HOME environment variable is not set; cannot locate \
                 ~/.sovereign/recipes/. Set HOME or pass an explicit path."
                    .into(),
            )
        })
}

/// Validate that `path` is inside the local recipes dir
/// (`~/.sovereign/recipes/` by default). The recipe-author tools
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

/// Same check as [`assert_under_local_recipes`] but with the root
/// passed explicitly. Used by the production helper above and by
/// tests that want isolation without thrashing process-global
/// `HOME`.
pub fn assert_under_root(path: &std::path::Path, root: &std::path::Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        // Relative paths resolve under the local recipes dir for
        // ergonomics: the LLM can pass `sec-investigation/recipe.toml`
        // and we normalise to `<root>/sec-investigation/recipe.toml`.
        root.join(path)
    };
    // We can't `canonicalize` — the file may not exist yet (write
    // tool path). Walk components manually to reject `..` traversal.
    if absolute
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(Error::InvalidInput(format!(
            "path `{}` contains `..`; recipe-author tools refuse \
             traversal segments",
            absolute.display()
        )));
    }
    if !absolute.starts_with(root) {
        return Err(Error::InvalidInput(format!(
            "path `{}` is outside the local recipes directory `{}`. \
             Recipe-author tools are scoped to ~/.sovereign/recipes/",
            absolute.display(),
            root.display(),
        )));
    }
    Ok(absolute)
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
