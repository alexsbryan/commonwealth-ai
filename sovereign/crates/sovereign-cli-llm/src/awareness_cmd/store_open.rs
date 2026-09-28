// SPDX-License-Identifier: AGPL-3.0-or-later
//! `.sovereign/` store openers shared across awareness subcommands.
//!
//! Resolves *user-level* paths (`~/.svrnmesh/`) for the relational +
//! strategic awareness pipeline, which writes its atlas under the
//! user's home — same place `KnowledgeViewManager` writes it in
//! production. The relational notes are svrn's memory notes, in svrn's
//! store under the same root.
//!
//! `--db-path <path>` overrides the `~/.svrnmesh/` root for
//! sandboxed runs (e.g. an integration test directory).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sovereign_contracts::notes::AgentNotes;
use sovereign_store::sqlite::SqliteStateStore;

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

/// svrn's store under the awareness root, where the commitments the
/// relational notes count live (pb-notes-memory; they left the per-project
/// `.sovereign/notes.db`, which is the code program's).
pub(super) fn notes_db_path(root: &Path) -> PathBuf {
    root.join("sovereign.db")
}

/// Open svrn's memory notes. Returns `None` if the store is absent so
/// awareness subcommands can still render entity lists without note-count
/// joins (a fresh `awareness` run), and says why when it will not open.
pub(super) fn try_open_notes(root: &Path) -> Option<Arc<dyn AgentNotes>> {
    let path = notes_db_path(root);
    if !path.exists() {
        return None;
    }
    match SqliteStateStore::open(&path) {
        Ok(s) => Some(Arc::new(s)),
        Err(e) => {
            eprintln!("awareness: svrn's store at {} did not open ({e}); notes skipped", path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(argv: &[&str]) -> Parsed {
        parse_args(&argv.iter().map(|s| s.to_string()).collect::<Vec<_>>())
            .expect("test argv must parse against the awareness spec")
    }

    #[test]
    fn sovereign_root_prefers_db_path_flag() {
        let f = flags(&["--db-path", "/tmp/awareness-test"]);
        assert_eq!(sovereign_root(&f), PathBuf::from("/tmp/awareness-test"));
    }

    /// With no `--db-path`, the root must come FROM the shared rebrand
    /// accessor rather than be re-derived here — one accessor per path
    /// (ARCH §10.6).
    ///
    /// This assertion used to read `resolved.ends_with(".sovereign")` and it
    /// had never run: the module is `awareness`-gated, that feature did not
    /// compile from 2026-05-22 to 2026-08-21, and no gate built it. It was
    /// wrong when it was written — `.sovereign` is the LEGACY name and
    /// production returns `~/.svrnmesh`. Naming either literal is the bug:
    /// `resolve_branded_dir` legitimately returns the legacy directory when
    /// that is where the user's data still lives, so a hardcoded expectation
    /// is wrong for one user or the other. Comparing against the accessor is
    /// right for both.
    #[test]
    fn sovereign_root_falls_back_to_the_shared_rebrand_root() {
        assert_eq!(
            sovereign_root(&flags(&[])),
            sovereign_contracts::rebrand::svrnmesh_root()
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
