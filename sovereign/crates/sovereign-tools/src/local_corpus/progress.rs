// SPDX-License-Identifier: AGPL-3.0-or-later
//! Progress events for the local-corpus flows.
//!
//! One enum spans every phase in both flows (folder and vault) so the
//! Tauri layer and UI only need one listener per job. Variants that are
//! Obsidian-specific (Clustering, Snapshotting, Writing, RollingBack)
//! are declared here but only emitted from the relevant manager entry
//! points.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::pre_scanner::FileMeta;

// The DATA moved to `sovereign_contracts::daemon_wire::local_corpus::progress`
// at svt-6 (2026-09-12) and is re-exported here at its historical path, so
// `sovereign_tools::local_corpus::progress::Name` keeps resolving. What stays
// in this file is the behaviour — the part that names corpus-engine, the
// filesystem, or a process.
pub use sovereign_contracts::daemon_wire::local_corpus::progress::*;


/// JSONL staging output path helper. Kept here so both the manager and
/// test harnesses agree on the layout.
pub fn staging_jsonl_path(staging_dir: &std::path::Path, corpus_id: &str) -> PathBuf {
    staging_dir.join(format!("{corpus_id}.jsonl"))
}
