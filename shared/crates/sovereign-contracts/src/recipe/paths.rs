// SPDX-License-Identifier: AGPL-3.0-or-later
//! Path-safety helpers shared by the recipe-authoring tools and the workflow
//! authoring surface.
//!
//! `assert_under_root` is the sandbox that lets the recipe/workflow author tools
//! register a narrow `RecipeAuthoring` permission instead of broad file access:
//! every read/write path is confirmed to sit inside the recipes root, rejecting
//! `..` traversal and absolute escapes. It lives here (in the shared contract)
//! so both the recipe-author bundle and the workflow-host author tools apply the
//! identical check without either depending on the other.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Validate that `path` resolves inside `root`, returning the absolute path.
///
/// Relative paths resolve under `root` for ergonomics (the LLM can pass
/// `sec-investigation/recipe.toml`); absolute paths are checked as-is. We cannot
/// `canonicalize` — the file may not exist yet (write path) — so components are
/// walked manually to reject `..` traversal, then the prefix is checked.
pub fn assert_under_root(path: &Path, root: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
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
             Recipe-author tools are scoped to ~/.svrnmesh/recipes/",
            absolute.display(),
            root.display(),
        )));
    }
    Ok(absolute)
}
