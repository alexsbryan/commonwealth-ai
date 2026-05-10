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
//! resolved — that matches the prior `unwrap_or_else(|| PathBuf::from("."))`
//! pattern callers relied on.

use std::path::PathBuf;

/// Root of the per-user data directory. Every mutable piece of
/// sovereign state lives underneath: models, corpora, mesh.json,
/// notes.db, per-project indexes, logs.
pub fn sovereign_root() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".sovereign"))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Where per-project code intelligence indexes live
/// (`~/.sovereign/indexes/<corpus-id>/`). The path that
/// `sovereign project init` writes into and `sovereign project serve`
/// reads from.
pub fn sovereign_indexes() -> PathBuf {
    sovereign_root().join("indexes")
}

/// Where the embedded Commonwealth mesh persists its `mesh.json` —
/// shared with `sovereign-desktop` so a mesh created from either
/// surface is picked up by the other. Intentionally uses
/// `dirs::data_dir()` (platform-native data dir) rather than our
/// `sovereign_root()` so it matches the desktop app's storage.
pub fn mesh_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sovereign")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sovereign_root_ends_in_sovereign() {
        let p = sovereign_root();
        assert!(
            p.ends_with(".sovereign") || p == PathBuf::from("."),
            "unexpected root: {}",
            p.display()
        );
    }

    #[test]
    fn indexes_nested_under_root() {
        let root = sovereign_root();
        let idx = sovereign_indexes();
        assert!(idx.starts_with(&root), "{} !startsWith {}", idx.display(), root.display());
        assert!(idx.ends_with("indexes"));
    }

    #[test]
    fn mesh_data_dir_ends_with_sovereign() {
        // `dirs::data_dir()` differs per-platform; we only guarantee
        // the final segment is the sovereign namespace.
        assert!(mesh_data_dir().ends_with("sovereign"));
    }
}
