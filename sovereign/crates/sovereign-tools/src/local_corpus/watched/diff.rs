//! Pure diff over the prior manifest and a fresh walk snapshot.
//!
//! `WatchedDiff` mirrors `corpus_engine::update::ManifestDiff` but
//! lives in our crate so the watched-folder code can compute the diff
//! without depending on the engine for trivial set arithmetic. The
//! one-line `into_manifest_diff` adapter sits in `apply.rs`.

use std::collections::HashMap;

use super::status::DiffSummary;
use super::walker::EntryRecord;

/// Per-doc-id diff produced by a single sweep.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WatchedDiff {
    pub added: Vec<String>,
    pub modified: Vec<String>,
    pub removed: Vec<String>,
}

impl WatchedDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.removed.is_empty()
    }

    pub fn summary(&self, live_before: usize) -> DiffSummary {
        DiffSummary {
            added: self.added.len(),
            modified: self.modified.len(),
            removed: self.removed.len(),
            live_before,
        }
    }
}

/// Compute the diff between the prior manifest (doc_id → content_hash)
/// and the fresh walk snapshot. Both inputs key on the relative path
/// (the doc_id) — see plan §4 for the doc_id derivation rationale.
///
/// Pure function: no IO, no side effects. Easy to unit-test with
/// synthetic inputs.
pub fn compute_diff(
    prior: &HashMap<String, String>,
    snapshot: &HashMap<String, EntryRecord>,
) -> WatchedDiff {
    let mut diff = WatchedDiff::default();

    for (doc_id, entry) in snapshot {
        match prior.get(doc_id.as_str()) {
            None => diff.added.push(doc_id.clone()),
            Some(prior_hash) if prior_hash != &entry.content_hash => {
                diff.modified.push(doc_id.clone());
            }
            _ => {} // unchanged
        }
    }

    for doc_id in prior.keys() {
        if !snapshot.contains_key(doc_id) {
            diff.removed.push(doc_id.clone());
        }
    }

    // Sort for determinism — makes test assertions and tracing-event
    // ordering reproducible across HashMap iteration orders.
    diff.added.sort();
    diff.modified.sort();
    diff.removed.sort();

    diff
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(hash: &str) -> EntryRecord {
        EntryRecord {
            absolute_path: PathBuf::from("/tmp/x"),
            mtime_unix: 0,
            size_bytes: 0,
            content_hash: hash.into(),
            source_root_index: 0,
            aux_paths: Vec::new(),
        }
    }

    fn snapshot(items: &[(&str, &str)]) -> HashMap<String, EntryRecord> {
        items
            .iter()
            .map(|(k, v)| (k.to_string(), entry(v)))
            .collect()
    }

    fn prior(items: &[(&str, &str)]) -> HashMap<String, String> {
        items
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn empty_when_no_changes() {
        let p = prior(&[("a", "h1"), ("b", "h2")]);
        let s = snapshot(&[("a", "h1"), ("b", "h2")]);
        assert!(compute_diff(&p, &s).is_empty());
    }

    #[test]
    fn detects_all_three_buckets() {
        let p = prior(&[("a", "h1"), ("b", "h2"), ("c", "h3")]);
        let s = snapshot(&[
            ("a", "h1"),     // unchanged
            ("b", "h2-new"), // modified
            ("d", "h4"),     // added
                             // c removed
        ]);
        let d = compute_diff(&p, &s);
        assert_eq!(d.added, vec!["d"]);
        assert_eq!(d.modified, vec!["b"]);
        assert_eq!(d.removed, vec!["c"]);
    }

    #[test]
    fn deterministic_ordering() {
        // Insert in non-sorted order; output should still be sorted.
        let p = prior(&[]);
        let s = snapshot(&[("z", "h"), ("a", "h"), ("m", "h")]);
        let d = compute_diff(&p, &s);
        assert_eq!(d.added, vec!["a", "m", "z"]);
    }

    #[test]
    fn initial_sweep_all_added() {
        let p = prior(&[]);
        let s = snapshot(&[("a", "h1"), ("b", "h2")]);
        let d = compute_diff(&p, &s);
        assert_eq!(d.added.len(), 2);
        assert!(d.modified.is_empty());
        assert!(d.removed.is_empty());
    }

    #[test]
    fn fully_emptied_all_removed() {
        let p = prior(&[("a", "h1"), ("b", "h2")]);
        let s = snapshot(&[]);
        let d = compute_diff(&p, &s);
        assert!(d.added.is_empty());
        assert!(d.modified.is_empty());
        assert_eq!(d.removed, vec!["a", "b"]);
    }
}
