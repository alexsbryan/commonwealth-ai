// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one decider for where code's notes store lives (pb-notes-verbs,
//! phase-b-33 item 2). Every cli-dev reader and writer of notes.db asks here;
//! `tests/notes_store_e2e.rs` counts the `join("notes.db")` sites at one.
//!
//! Code's data root by default, which is the store the `/mcp` `notes` tool
//! serves. A per-repo store is reached only by naming it (`--data-dir`). There
//! is no pointer file and no cwd walk: a resolver that answers differently by
//! working directory answers confidently from the wrong store.

use std::path::{Path, PathBuf};

/// `<data_dir>/notes.db` when the caller names a directory, else
/// `<code's data root>/notes.db`. Existence is the caller's question.
pub(crate) fn find_notes_db(data_dir: Option<&Path>) -> PathBuf {
    let base = match data_dir {
        Some(dir) => dir.to_path_buf(),
        None => sovereign_cli_base::dirs::sovereign_root(),
    };
    let path = base.join("notes.db");
    tracing::debug!(
        target: "cli_dev.notes",
        path = %path.display(),
        named = data_dir.is_some(),
        "notes store resolved"
    );
    path
}
