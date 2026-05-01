//! Per-corpus state file `_watched_folder_state.json`.
//!
//! Holds three things the engine's manifest doesn't:
//!   - the worker's status (Idle / Sweeping / Paused / Errored)
//!   - the per-file `(mtime, size)` cache for the walker fast-path
//!   - tombstones for soft-delete grace tracking
//!
//! Atomic load/save: write to a tempfile in the same directory, fsync,
//! rename. Mirrors `manager::persist_config` (line 785+).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sovereign_core::error::{Error, Result};

use super::status::WatchedFolderStatus;
use super::walker::EntryRecord;

const STATE_FILENAME: &str = "_watched_folder_state.json";

/// One state document per watched-folder corpus, persisted under
/// `{index_dir}/{corpus_id}/_watched_folder_state.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchedFolderState {
    pub corpus_id: String,
    pub schema_version: u8,
    pub status: WatchedFolderStatus,
    /// Per-file (mtime, size, content_hash) cache. Keyed on doc_id
    /// (relative path). Persisted so the walker fast-path survives a
    /// daemon restart — without it, every restart would force a full
    /// re-hash of the watched tree.
    pub entries: std::collections::HashMap<String, EntryRecord>,
    /// Soft-delete tombstones — see `soft_delete_gc.rs`.
    pub tombstones: Vec<Tombstone>,
    /// One-shot deletion-guard bypass. Set by
    /// `LocalCorpusManager::confirm_pending_deletion` so the next
    /// sweep applies pending deletions even if their ratio still
    /// trips the threshold guard. Consumed (cleared) by the worker
    /// after that one sweep so subsequent sweeps re-evaluate the
    /// guard normally. Spec §5.3: "Apply pending deletion on the
    /// next sweep" — without the bypass, a 100%-deletion scenario
    /// would re-trip the guard forever.
    #[serde(default)]
    pub bypass_guard_next_sweep: bool,
    /// Per-extension breakdown of files the walker saw but skipped
    /// because the extension wasn't in the corpus's allow-list.
    /// Refreshed every sweep. `corpus watch-status --skipped` reads
    /// this so a user dropping `.docx` files into a watched folder
    /// gets an immediate "no extractor for .docx" answer instead of
    /// silent omission. Spec §5.1.
    #[serde(default)]
    pub skipped_by_extension: std::collections::HashMap<String, usize>,
    /// Files the pre-scan classified as unindexable (corrupt,
    /// password-protected, scanned PDF without OCR enabled). Each
    /// entry carries a human-readable reason. Refreshed every
    /// sweep — files no longer present drop out. Spec §5.1's
    /// "Corrupt / failed extraction" bucket.
    #[serde(default)]
    pub failed_files: Vec<FailedFile>,
    pub last_updated_unix: u64,
}

impl WatchedFolderState {
    /// New, empty state for a freshly-registered corpus.
    pub fn fresh(corpus_id: impl Into<String>) -> Self {
        Self {
            corpus_id: corpus_id.into(),
            schema_version: 1,
            status: WatchedFolderStatus::Idle {
                last_sweep_unix: 0,
                live_docs: 0,
                tombstones: 0,
            },
            entries: Default::default(),
            tombstones: Vec::new(),
            bypass_guard_next_sweep: false,
            skipped_by_extension: Default::default(),
            failed_files: Vec::new(),
            last_updated_unix: 0,
        }
    }

    /// Read state from `{state_dir}/_watched_folder_state.json`.
    /// Missing file returns `Ok(None)` (caller decides whether to
    /// create fresh state); malformed file returns `Err` rather than
    /// silently dropping the user's tombstone history.
    pub fn load(state_dir: &Path) -> Result<Option<Self>> {
        let path = state_dir.join(STATE_FILENAME);
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| Error::Execution(format!("read {STATE_FILENAME}: {e}")))?;
        let s: Self = serde_json::from_str(&raw)
            .map_err(|e| Error::Execution(format!("parse {STATE_FILENAME}: {e}")))?;
        Ok(Some(s))
    }

    /// Write state atomically. Creates the directory if missing.
    /// Atomic semantics via tempfile-rename — partial writes never
    /// leave a corrupt sidecar behind.
    pub fn save(&self, state_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(state_dir)
            .map_err(|e| Error::Execution(format!("create state dir: {e}")))?;
        let raw = serde_json::to_string_pretty(self)
            .map_err(|e| Error::Execution(format!("serialise state: {e}")))?;
        let dir_owned = state_dir.to_path_buf();
        let temp = tempfile::NamedTempFile::new_in(&dir_owned)
            .map_err(|e| Error::Execution(format!("temp state file: {e}")))?;
        std::fs::write(temp.path(), raw.as_bytes())
            .map_err(|e| Error::Execution(format!("write state: {e}")))?;
        temp.persist(state_dir.join(STATE_FILENAME))
            .map_err(|e| Error::Execution(format!("rename state: {e}")))?;
        Ok(())
    }

    /// Convenience: state-file path under an index dir.
    pub fn path_for(state_dir: &Path) -> PathBuf {
        state_dir.join(STATE_FILENAME)
    }

    pub fn is_paused(&self) -> bool {
        self.status.is_paused()
    }

    /// Project the entries map down to `(doc_id → content_hash)` for
    /// `diff::compute_diff`.
    pub fn prior_hashes(&self) -> std::collections::HashMap<String, String> {
        self.entries
            .iter()
            .map(|(k, v)| (k.clone(), v.content_hash.clone()))
            .collect()
    }
}

/// One file the pre-scan classified as unindexable. Surfaced via
/// `corpus watch-status --failures` and the desktop "files we
/// couldn't read" panel. `first_seen_unix` marks when the failure
/// was first observed — useful for "this PDF has been broken for 3
/// weeks, do you want to delete it?" prompts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FailedFile {
    /// Relative path under the watched root (the same doc_id form
    /// the rest of the system uses).
    pub doc_id: String,
    pub absolute_path: PathBuf,
    /// One of: `"corrupt"`, `"password_protected"`,
    /// `"scanned_no_text"` — extensible.
    pub kind: String,
    /// Free-form human-readable reason. Surfaced verbatim in CLI
    /// output and the desktop status drawer.
    pub reason: String,
    pub first_seen_unix: u64,
}

/// One soft-deleted document. The chunks are already physically gone
/// from the LanceDB index (apply phase 1) — the tombstone exists so a
/// restored file with matching content_hash inside the grace window
/// can be detected and re-ingested without surfacing a deletion to
/// the user.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tombstone {
    pub doc_id: String,
    pub absolute_path: PathBuf,
    pub last_known_content_hash: String,
    pub last_known_size_bytes: u64,
    pub removed_at_unix: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn fresh_state_is_idle() {
        let s = WatchedFolderState::fresh("c1");
        assert_eq!(s.corpus_id, "c1");
        assert!(matches!(
            s.status,
            WatchedFolderStatus::Idle { live_docs: 0, .. }
        ));
        assert!(s.entries.is_empty());
        assert!(s.tombstones.is_empty());
    }

    #[test]
    fn load_returns_none_when_missing() {
        let dir = tempdir().unwrap();
        let loaded = WatchedFolderState::load(dir.path()).unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn save_then_load_round_trip() {
        let dir = tempdir().unwrap();
        let mut s = WatchedFolderState::fresh("c1");
        s.tombstones.push(Tombstone {
            doc_id: "a.md".into(),
            absolute_path: dir.path().join("a.md"),
            last_known_content_hash: "deadbeef".into(),
            last_known_size_bytes: 42,
            removed_at_unix: 12345,
        });
        s.save(dir.path()).unwrap();

        let back = WatchedFolderState::load(dir.path()).unwrap().unwrap();
        assert_eq!(back.corpus_id, "c1");
        assert_eq!(back.tombstones.len(), 1);
        assert_eq!(back.tombstones[0].last_known_content_hash, "deadbeef");
    }

    #[test]
    fn save_is_atomic_via_tempfile() {
        let dir = tempdir().unwrap();
        let s = WatchedFolderState::fresh("c1");
        s.save(dir.path()).unwrap();
        assert!(dir.path().join(STATE_FILENAME).exists());
        // Stray tempfiles from the persist-rename should not linger.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name().to_string_lossy().starts_with(".tmp")
                    || e.file_name().to_string_lossy().contains("tmp")
                        && e.file_name().to_string_lossy() != STATE_FILENAME
            })
            .collect();
        assert!(leftovers.is_empty(), "stale tempfiles: {leftovers:?}");
    }
}
