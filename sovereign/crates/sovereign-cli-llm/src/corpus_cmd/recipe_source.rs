// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn corpus install <recipe.toml>`'s registration step — a CALLER of
//! `corpus_engine::recipe_install`, which is where the step itself lives.
//!
//! The registration used to be written out here. `corpus-mcp` needed the same
//! step for `corpus ingest <recipe.toml>` (order ei-5b-build-verb) and cannot
//! link this crate, so the step moved down beside the registry that reads the
//! file back (ARCH §10.6: one decider, one name). What stays here is the one
//! thing that is this host's own — where THIS binary keeps its recipes dir.

use std::path::{Path, PathBuf};

pub(super) use corpus_engine::recipe_install::{looks_like_recipe_path, Registered};

/// Copy `path` into this host's recipe overrides dir under its declared id.
pub(super) fn register(path: &Path) -> Result<Registered, String> {
    corpus_engine::recipe_install::register(path, &recipes_dir())
}

/// Where the registry looks for user recipes (`registry.rs` step 1).
fn recipes_dir() -> PathBuf {
    sovereign_contracts::rebrand::svrnmesh_root().join("recipes")
}
