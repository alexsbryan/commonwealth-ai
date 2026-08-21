// SPDX-License-Identifier: AGPL-3.0-or-later
//! `.sovereign/` store openers shared across awareness subcommands.
//!
//! Mirrors `atos_cmd/stores.rs` but resolves *user-level* paths
//! (`~/.svrnmesh/`) rather than *project-level* (`./.sovereign/`).
//! The relational + strategic awareness pipeline writes its atlas
//! under the user's home — same place `KnowledgeViewManager` writes
//! it in production. The project-level features.db / project.toml
//! are still resolved relative to CWD because that's where ATOS
//! lives.
//!
//! `--db-path <path>` overrides the `~/.svrnmesh/` root for
//! sandboxed runs (e.g. an integration test directory).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_engine_atos::FeatureStore;
use corpus_engine_notes::NoteStore;

use super::args::parse_args;
use sovereign_cli_shared::args::Parsed;

/// Where awareness reads/writes user-level state.
///
/// Resolution order:
///   1. `--db-path <path>` flag (treats `<path>` as the `.svrnmesh/`
///      root — atoms.json lives at `<path>/indexes/...`).
///   2. `~/.svrnmesh/` (matches main.rs:482-484).
pub(super) fn sovereign_root(flags: &Parsed) -> PathBuf {
    if let Some(p) = flags
        .value("db-path")
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
    {
        return PathBuf::from(p);
    }
    sovereign_contracts::rebrand::svrnmesh_root()
}

/// Where the per-project ATOS store lives — `./.sovereign/` from CWD.
/// Mirrors `atos_cmd/stores.rs::sovereign_dir`.
pub(super) fn project_sovereign_dir() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".sovereign")
}

/// Atlas directory for a corpus view (e.g. `personal-knowledge`,
/// `conversation-history`). Caller checks `.exists()`; we don't
/// because awareness subcommands need to print "no atlas yet" rather
/// than fail.
pub(super) fn atlas_dir_for(root: &Path, view_id: &str) -> PathBuf {
    root.join("indexes").join(view_id).join("atlas")
}

/// `state.db` path inside the awareness root.
pub(super) fn state_db_path(root: &Path) -> PathBuf {
    root.join("state.db")
}

/// `notes.db` path. Notes live alongside features in the
/// per-project `.sovereign/` (mirrors how `KnowledgeViewManager` is
/// wired in main.rs:534-535).
pub(super) fn notes_db_path() -> PathBuf {
    project_sovereign_dir().join("notes.db")
}

/// `features.db` path inside the per-project `.sovereign/`.
pub(super) fn features_db_path() -> PathBuf {
    project_sovereign_dir().join("features.db")
}

/// `project.toml` path inside the per-project `.sovereign/`.
pub(super) fn project_toml_path() -> PathBuf {
    project_sovereign_dir().join("project.toml")
}

/// Open the NoteStore. Returns `None` if `notes.db` is absent so
/// awareness subcommands can still render entity lists without
/// note-count joins (a fresh `awareness` run on an empty repo).
pub(super) fn try_open_notes() -> Option<Arc<NoteStore>> {
    let path = notes_db_path();
    if !path.exists() {
        return None;
    }
    NoteStore::open(&path).ok().map(Arc::new)
}

/// Open the FeatureStore. Returns `None` if `features.db` is absent
/// (the user hasn't run `svrn atos provision`).
pub(super) fn try_open_features() -> Option<Arc<FeatureStore>> {
    let path = features_db_path();
    if !path.exists() {
        return None;
    }
    FeatureStore::open(&path).ok().map(Arc::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sovereign_root_prefers_db_path_flag() {
        let flags = vec![("db-path".into(), "/tmp/awareness-test".into())];
        assert_eq!(sovereign_root(&flags), PathBuf::from("/tmp/awareness-test"));
    }

    #[test]
    fn sovereign_root_falls_back_to_home_or_cwd() {
        let flags: Vec<(String, String)> = Vec::new();
        let resolved = sovereign_root(&flags);
        assert!(
            resolved.ends_with(".sovereign"),
            "got {}",
            resolved.display()
        );
    }

    #[test]
    fn atlas_dir_layout_matches_main_rs() {
        let dir = atlas_dir_for(Path::new("/home/u/.sovereign"), "personal-knowledge");
        assert_eq!(
            dir,
            PathBuf::from("/home/u/.sovereign/indexes/personal-knowledge/atlas")
        );
    }
}
