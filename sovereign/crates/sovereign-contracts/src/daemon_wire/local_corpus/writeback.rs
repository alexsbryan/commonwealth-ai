// SPDX-License-Identifier: AGPL-3.0-or-later
//! Results of the Obsidian write-back surface: what a snapshot records,
//! what a write touched, and what a rollback or clean undid.
//!
//! Moved down from `sovereign_tools::local_corpus::writeback` at svt-6
//! (2026-09-12) and re-exported there at the historical path. Pure serde over
//! primitives: a client that only wants to SPELL one of these had to link
//! `sovereign-tools` — and through it corpus-engine, sovereign-store,
//! sovereign-atos and five more. See this module's parent for the full note.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMeta {
    pub taken_at: DateTime<Utc>,
    pub sovereign_version: u32,
    pub file_count: usize,
    pub git_commit: Option<String>,
    pub snapshot_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteBackResult {
    pub files_tagged: usize,
    pub files_skipped: Vec<FailedWrite>,
    pub index_notes_created: usize,
    pub snapshot_path: PathBuf,
    pub sovereign_version: u32,
    /// Per-note tag writes that succeeded, with the post-write
    /// `(mtime, size, content_hash)` of each touched file. The
    /// reconciliation worker uses this to patch
    /// `WatchedFolderState.entries` so the very-next sweep's
    /// fast-path treats these mtime bumps as "no real change"
    /// rather than re-extracting + re-tagging in a loop.
    ///
    /// Index notes under `<index_dir>/...` are deliberately absent —
    /// they live under a path the walker excludes globbed, so a
    /// state patch would be a no-op for them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touched_user_notes: Vec<TouchedNote>,
}

/// Result of one successful per-note tag write — the post-write
/// metadata the reconciliation worker patches back onto
/// `WatchedFolderState.entries`. Mirrors the load-bearing fields of
/// `watched::walker::EntryRecord` (mtime, size, hash) without depending
/// on the watched module — keeping `writeback` as a pure-IO leaf.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TouchedNote {
    /// Relative path under the vault root — same shape the walker uses
    /// as `doc_id` for primary-root entries.
    pub relative_path: String,
    pub absolute_path: PathBuf,
    pub mtime_unix: i64,
    pub size_bytes: u64,
    /// Lowercase hex sha256 of the post-write file contents, truncated
    /// to 16 chars — matches the walker's `EntryRecord.content_hash`
    /// shape so the worker's patch is byte-comparable on the next
    /// fast-path lookup.
    pub content_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedWrite {
    pub relative_path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackResult {
    pub files_restored: usize,
    pub files_skipped: Vec<FailedWrite>,
    pub index_notes_deleted: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanResult {
    pub tags_removed_from: usize,
    pub index_notes_deleted: usize,
}
