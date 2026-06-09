// SPDX-License-Identifier: AGPL-3.0-or-later
//! Content-addressed asset store (AD-1).
//!
//! The substrate primitive every binary-bearing vertical inherits: a
//! filesystem-backed, append-only store keyed by SHA-256 of the raw
//! bytes. The same address space holds (1) the original bytes and (2)
//! an optional **parsed form** — a typed representation a sub-extractor
//! wrote alongside the raw payload (parquet per sheet for XLSX,
//! ical for calendar, jsonl for transactions, …) so a future
//! structured-query path reads typed records without re-parsing.
//!
//! Both raw + parsed are addressed by the same sha256. The atom graph
//! holds only the prose-shaped description atom pointing at the asset;
//! see [`crate::enrichment::atlas::atoms::Asset`].
//!
//! Layout:
//! ```text
//! <corpus_index>/assets/
//!   ledger.jsonl          # append-only JSONL of LedgerEntry
//!   <hh>/<sha256>         # raw bytes (sharded by leading two hex chars)
//!   parsed/<sha256>.<ext> # optional typed cache
//! ```
//!
//! The store is **not** a sqlite table (lifecycle is file-shaped,
//! append-only); **not** a `_corpus_meta.json` extension (single
//! mutable JSON does not scale to asset counts); **not**
//! `DocumentAssetStore` (that trait is conversation-asset-scoped at
//! `sovereign-core/src/traits.rs`). It is its own thing — named after
//! the substrate primitive, not its first concrete payload.

pub mod fs;
pub mod ledger;

pub use fs::FilesystemAssetStore;
pub use ledger::{LedgerEntry, LedgerReader};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::Result;

/// Receipt returned when an asset is stored.
///
/// `newly_stored` distinguishes the first-write case from the
/// idempotent re-write: callers (the email extractor seeing the same
/// attachment under two messages, the folder walker seeing duplicates)
/// can use the flag to suppress double-counted progress events
/// without paying a separate "exists?" round-trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetReceipt {
    pub sha256: String,
    pub raw_path: PathBuf,
    pub size: u64,
    pub newly_stored: bool,
}

/// Append-only ledger + raw-byte + parsed-form storage.
///
/// All methods are sync — the store sits behind the existing engine
/// ingest pipeline which is sync-with-blocking-IO. The asynchrony seam
/// (chunker → embedder → index writer) is downstream.
pub trait AssetStore: Send + Sync {
    /// Put raw bytes into the store. Idempotent: a second call with
    /// the same bytes returns the same `sha256` and `raw_path`, sets
    /// `newly_stored = false`, and does not duplicate the ledger entry.
    ///
    /// `source_doc_id` records the document this asset was first
    /// observed inside (the email Message-ID, the source filename for
    /// folder-walk ingests). Re-observations from new docs are tracked
    /// implicitly via the standard atom-graph Attaches edges; the
    /// ledger captures only the *first* observation.
    fn put_raw(
        &self,
        bytes: &[u8],
        original_filename: Option<&str>,
        mime: Option<&str>,
        source_doc_id: &str,
    ) -> Result<AssetReceipt>;

    /// Put a typed parsed form alongside the raw bytes. `ext` is the
    /// suffix on the parsed file (e.g. `"parquet"`, `"ics"`, `"jsonl"`).
    /// Idempotent on `(sha256, ext)`. Returns the on-disk path.
    fn put_parsed(&self, sha256: &str, ext: &str, bytes: &[u8]) -> Result<PathBuf>;

    /// Update an existing ledger entry's `parsed_form` pointer.
    /// Called after [`put_parsed`] so a downstream consumer reading
    /// the ledger sees the parsed-cache path without scanning the
    /// `parsed/` directory.
    fn record_parsed_form(&self, sha256: &str, parsed_path: &Path) -> Result<()>;

    /// Look up an entry. Returns `None` if `sha256` has not been
    /// observed yet.
    fn lookup(&self, sha256: &str) -> Result<Option<LedgerEntry>>;

    /// Iterate every ledger entry currently on disk. Used by
    /// `sovereign asset ls` and by Phase 5 reconciliation stats.
    fn entries(&self) -> Result<Vec<LedgerEntry>>;

    /// Path where this sha256's raw bytes live.
    fn raw_path(&self, sha256: &str) -> PathBuf;

    /// Root directory the store writes under. Phase 4 multi-origin
    /// merge wants this to thread parquet caches through the
    /// column-aware extractor without going via the ledger.
    fn root(&self) -> &Path;
}

/// Type alias for the shared, runtime-installed asset store handle
/// the engine and extractors pass around. `Arc<dyn AssetStore>` so
/// (a) the store can be cheap-cloned across the ingest threadpool and
/// (b) tests can swap in an in-memory mock.
pub type AssetStoreHandle = Arc<dyn AssetStore>;

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store() -> (tempfile::TempDir, FilesystemAssetStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemAssetStore::new(dir.path()).unwrap();
        (dir, store)
    }

    #[test]
    fn put_raw_is_idempotent() {
        let (_d, store) = tmp_store();
        let r1 = store
            .put_raw(b"hello world", Some("hi.txt"), Some("text/plain"), "doc-1")
            .unwrap();
        let r2 = store
            .put_raw(b"hello world", Some("hi.txt"), Some("text/plain"), "doc-2")
            .unwrap();
        assert_eq!(r1.sha256, r2.sha256);
        assert_eq!(r1.raw_path, r2.raw_path);
        assert!(r1.newly_stored);
        assert!(!r2.newly_stored);
    }

    #[test]
    fn ledger_round_trip() {
        let (_d, store) = tmp_store();
        let r = store
            .put_raw(
                b"payload",
                Some("a.bin"),
                Some("application/octet-stream"),
                "doc-1",
            )
            .unwrap();
        let entry = store.lookup(&r.sha256).unwrap().expect("entry");
        assert_eq!(entry.sha256, r.sha256);
        assert_eq!(entry.original_filename.as_deref(), Some("a.bin"));
        assert_eq!(entry.size, 7);
        assert_eq!(entry.first_seen_source_doc_id, "doc-1");
        assert!(entry.parsed_form.is_none());
    }

    #[test]
    fn parsed_form_landing() {
        let (_d, store) = tmp_store();
        let r = store
            .put_raw(b"xlsx-bytes", Some("q3.xlsx"), None, "doc-1")
            .unwrap();
        let p = store.put_parsed(&r.sha256, "parquet", b"PARQ").unwrap();
        store.record_parsed_form(&r.sha256, &p).unwrap();
        let entry = store.lookup(&r.sha256).unwrap().expect("entry");
        assert_eq!(entry.parsed_form.as_deref(), Some(p.as_path()));
        assert!(p.exists());
    }

    #[test]
    fn entries_lists_every_observation() {
        let (_d, store) = tmp_store();
        store.put_raw(b"a", None, None, "d-1").unwrap();
        store.put_raw(b"b", None, None, "d-2").unwrap();
        // Idempotent re-observation must not double-count.
        store.put_raw(b"a", None, None, "d-3").unwrap();
        let entries = store.entries().unwrap();
        assert_eq!(entries.len(), 2);
    }
}
