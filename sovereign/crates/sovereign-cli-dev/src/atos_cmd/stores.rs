// SPDX-License-Identifier: AGPL-3.0-or-later
//! Store-opening helpers used by every `sovereign atos` subcommand.
//!
//! The CLI deliberately reuses the same `.sovereign/` layout as
//! `sovereign project serve` — notes.db, features.db, and the
//! project-docs FTS all live in the repo-rooted directory so
//! artifacts produced by agents and operators reconcile against one
//! view of the world.
//!
//! [`open_orchestrator`] is the M3+ entry point; raw
//! [`open_feature_store`] / [`open_note_store`] are kept for the
//! small number of legacy paths that still read stores directly
//! (e.g. the post-end-milestone notes dump in `cmd_end_milestone`).

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine_atos::FeatureStore;
use corpus_engine_notes::NoteStore;

/// `.sovereign/` at the current repo root — matches where
/// `sovereign project serve` writes notes.db / features.db.
pub(super) fn sovereign_dir() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".sovereign")
}

pub(super) fn open_feature_store() -> Result<Arc<FeatureStore>, String> {
    let path = sovereign_dir().join("features.db");
    FeatureStore::open(&path)
        .map(Arc::new)
        .map_err(|e| format!("open features.db at {}: {e}", path.display()))
}

pub(super) fn open_note_store() -> Result<Arc<NoteStore>, String> {
    let path = sovereign_dir().join("notes.db");
    NoteStore::open(&path)
        .map(Arc::new)
        .map_err(|e| format!("open notes.db at {}: {e}", path.display()))
}

/// Single-call orchestrator factory. Every M3+ subcommand path goes
/// through this; the raw `open_feature_store` / `open_note_store`
/// helpers above stay for the handful of call-sites in
/// `cmd_end_milestone` and `cmd_spec_accept` that still read stores
/// directly.
pub(super) fn open_orchestrator() -> Result<Arc<sovereign_atos::LocalAtosOrchestrator>, String> {
    let features = open_feature_store()?;
    let notes = open_note_store()?;
    let mut orc = sovereign_atos::LocalAtosOrchestrator::new(features, notes);

    // Wire the project-docs store so milestone reports flow into the
    // same FTS index `project_context` searches. Opening this is
    // best-effort; if the schema is unavailable we silently degrade
    // to disk-only reports rather than failing the CLI.
    let docs_path = sovereign_dir().join("project_docs.db");
    let repo_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if let Ok(store) = corpus_engine_notes::ProjectDocsStore::open(&docs_path) {
        orc = orc.with_project_docs(Arc::new(store), repo_root);
    }

    Ok(Arc::new(orc))
}
