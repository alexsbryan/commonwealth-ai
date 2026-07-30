// SPDX-License-Identifier: AGPL-3.0-or-later
//! Canonical filesystem paths that every subcommand agrees on.
//!
//! Before this module there were three `default_data_dir()` bodies
//! scattered across `project_cmd.rs`, `code_cmd.rs`, and
//! `setup_config.rs` that each returned a *different* path
//! (`~/.sovereign`, `~/.sovereign/indexes`, and the conflation of the
//! two). Pulling them here makes the layout obvious and ensures a
//! change like "move indexes under XDG_DATA_HOME" only needs to land
//! in one place.
//!
//! All functions fall back to `.` if the home directory can't be
//! resolved — that matches the prior `unwrap_or_else(|| std::path::Path::new("."))`
//! pattern callers relied on.

use std::path::PathBuf;

/// Root of the per-user data directory. Every mutable piece of
/// sovereign state lives underneath: models, corpora, mesh.json,
/// notes.db, per-project indexes, logs. Delegates to the rebrand-aware
/// SSOT (`sovereign_contracts::rebrand`) — prefer `~/.svrnmesh`, fall
/// back to a populated legacy `~/.sovereign`.
pub fn sovereign_root() -> PathBuf {
    sovereign_contracts::rebrand::svrnmesh_root()
}

/// `<root>/work-atlas.toml` — pass-through to the SSOT accessor for
/// crates (sovereign-cli-dev) that depend on this crate but not on
/// sovereign-contracts directly.
pub fn work_atlas_toml() -> PathBuf {
    sovereign_contracts::rebrand::work_atlas_toml()
}

/// Where per-project code intelligence indexes live
/// (`~/.sovereign/indexes/<corpus-id>/`). The path that
/// `svrn project init` writes into and `svrn project serve`
/// reads from.
pub fn sovereign_indexes() -> PathBuf {
    sovereign_root().join("indexes")
}

/// Where installed third-party mesh apps live (`~/.sovereign/meshapps/<id>/`),
/// alongside a shared `_sdk/` and the local `registry.toml` of published apps.
/// `svrn meshapp install` unpacks here; `meshapp dev <id>` runs from here.
pub fn sovereign_meshapps() -> PathBuf {
    sovereign_root().join("meshapps")
}

/// Where the embedded Commonwealth mesh persists its `mesh.json` —
/// shared with `sovereign-desktop` so a mesh created from either
/// surface is picked up by the other. Intentionally uses the
/// platform-native data dir rather than `sovereign_root()` so it
/// matches the desktop app's storage. Delegates to the SSOT.
pub fn mesh_data_dir() -> PathBuf {
    sovereign_contracts::rebrand::mesh_data_dir()
}

/// Same shape as `project_cmd::default_data_dir` before the split:
/// returns `None` when home-dir resolution fails, so existing
/// `.or_else(default_data_dir)` callers don't need to change.
pub fn default_data_dir() -> Option<PathBuf> {
    let p = sovereign_indexes();
    if p == std::path::Path::new(".") {
        None
    } else {
        Some(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sovereign_root_uses_brand_dir() {
        let p = sovereign_root();
        assert!(
            p.ends_with(".svrnmesh") || p.ends_with(".sovereign") || p == std::path::Path::new("."),
            "unexpected root: {}",
            p.display()
        );
    }

    #[test]
    fn indexes_nested_under_root() {
        let root = sovereign_root();
        let idx = sovereign_indexes();
        assert!(
            idx.starts_with(&root),
            "{} !startsWith {}",
            idx.display(),
            root.display()
        );
        assert!(idx.ends_with("indexes"));
    }

    #[test]
    fn mesh_data_dir_uses_brand_dir() {
        let p = mesh_data_dir();
        assert!(p.ends_with("svrnmesh") || p.ends_with("sovereign"));
    }
}
