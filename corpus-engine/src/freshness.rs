//! Per-document index recency — a source-agnostic freshness signal.
//!
//! Every (re)index of a document stamps `source_doc_id → unix_seconds`
//! into a corpus-local `_doc_freshness.json` sidecar. The Atlas reads it
//! to bubble freshly-updated atoms to the top, so ANY source that
//! re-indexes content — the wikipedia-newsworthy watcher, watched-folder
//! edits, delta updates — makes its content "fresh" with no per-source
//! code. Freshness is an emergent property of indexing.
//!
//! Deliberately NOT folded into `VersionManifest`: that manifest
//! describes the *published dataset release* (doc → content hash); this
//! is purely local indexing state. Keeping them separate stops a
//! release-identity record from accreting per-node runtime state.
//!
//! Only the incremental (re)index paths stamp here — a fresh full
//! install leaves the map empty, so install-time documents read as
//! "baseline" (no recency) and sort *after* anything later refreshed.
//! That's the desired behaviour: "fresh" means "touched since the bulk
//! index," which is exactly what the newsworthy refresh produces.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Sidecar filename inside a corpus index directory.
pub const DOC_FRESHNESS_FILE: &str = "_doc_freshness.json";

/// Path to the freshness sidecar for a corpus index directory.
pub fn doc_freshness_path(corpus_dir: &Path) -> PathBuf {
    corpus_dir.join(DOC_FRESHNESS_FILE)
}

/// Load the `source_doc_id → unix_seconds` recency map for a corpus
/// index directory. A missing or unreadable file yields an empty map —
/// freshness is best-effort and never an error for callers.
pub fn load_doc_freshness(corpus_dir: &Path) -> HashMap<String, i64> {
    match std::fs::read(doc_freshness_path(corpus_dir)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// Stamp `source_doc_id` as (re)indexed at `at_unix`. Read-modify-write
/// with an atomic rename so a reader never sees a torn file.
///
/// Best-effort: a write failure is logged and swallowed — recording
/// recency must never block or fail the underlying (re)index.
///
/// Concurrency: two stamps racing on the *same corpus* can drop one
/// entry (whole-map last-writer-wins). The dominant producer, the
/// newsworthy watcher, refreshes serially within a tick, so this is
/// acceptable; cross-source concurrent reindex of one corpus is rare.
pub fn stamp_doc_indexed(corpus_dir: &Path, source_doc_id: &str, at_unix: i64) {
    let mut map = load_doc_freshness(corpus_dir);
    map.insert(source_doc_id.to_string(), at_unix);

    let path = doc_freshness_path(corpus_dir);
    let tmp = path.with_extension("json.tmp");
    let res = serde_json::to_vec(&map)
        .map_err(|e| e.to_string())
        .and_then(|bytes| std::fs::write(&tmp, bytes).map_err(|e| e.to_string()))
        .and_then(|_| std::fs::rename(&tmp, &path).map_err(|e| e.to_string()));
    if let Err(e) = res {
        tracing::warn!(
            error = %e,
            dir = %corpus_dir.display(),
            "doc_freshness: stamp write failed — atom recency may lag"
        );
    }
}

/// Current wall-clock in unix seconds (the stamp value used by callers).
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_doc_freshness(dir.path()).is_empty());

        stamp_doc_indexed(dir.path(), "Gaza", 1_000);
        stamp_doc_indexed(dir.path(), "Earthquake", 2_000);
        // Re-stamp updates in place, doesn't duplicate.
        stamp_doc_indexed(dir.path(), "Gaza", 3_000);

        let map = load_doc_freshness(dir.path());
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("Gaza"), Some(&3_000));
        assert_eq!(map.get("Earthquake"), Some(&2_000));
    }

    #[test]
    fn missing_file_is_empty_not_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_doc_freshness(dir.path()).is_empty());
    }
}
