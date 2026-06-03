//! Tombstone management — record, revive, expire, cap-evict.
//!
//! Soft-delete is implemented as a sidecar tombstone in
//! `WatchedFolderState`, NOT as a `deleted_at` column on LanceDB
//! chunks. The chunks themselves are physically deleted by
//! `CorpusUpdater::apply_update` phase 1; the tombstone exists so a
//! restored file with matching content_hash within the grace window
//! can be detected and re-ingested without surfacing a deletion to
//! the user. See plan §soft-delete-mechanism for the decision
//! rationale (option (c) over LanceDB schema changes).

use std::collections::HashMap;

use super::state::{Tombstone, WatchedFolderState};
use super::walker::WalkSnapshot;

/// Per-corpus cap on the number of tombstones kept in the state file.
/// Bounded to keep `_watched_folder_state.json` from growing
/// unboundedly when a user repeatedly bulk-deletes thousands of files.
/// When the cap is exceeded, oldest entries are evicted (FIFO by
/// `removed_at_unix`) and a `TombstoneEvicted` event is emitted at
/// `warn!` level — losing tombstones means losing the revival path
/// for those docs, which the operator should know about.
pub const TOMBSTONE_CAP: usize = 100_000;

/// Append a tombstone for every doc_id in `removed_doc_ids`. Looks up
/// the `(absolute_path, content_hash, size)` in the prior snapshot —
/// the doc has just been physically deleted, so the *prior* state is
/// the source of truth for the tombstone payload.
pub fn record_tombstones(
    state: &mut WatchedFolderState,
    removed_doc_ids: &[String],
    prior_snapshot: &WalkSnapshot,
    now_unix: u64,
) -> usize {
    let mut added = 0;
    for doc_id in removed_doc_ids {
        if let Some(entry) = prior_snapshot.get(doc_id) {
            state.tombstones.push(Tombstone {
                doc_id: doc_id.clone(),
                absolute_path: entry.absolute_path.clone(),
                last_known_content_hash: entry.content_hash.clone(),
                last_known_size_bytes: entry.size_bytes,
                removed_at_unix: now_unix,
            });
            added += 1;
        }
        // No-op when the prior snapshot doesn't have the doc (race
        // between two sweeps; not an error).
    }
    added
}

/// Detect revivals against the fresh walk snapshot. For each
/// tombstone whose path now exists with the same content_hash and
/// size and is still within grace, removes the tombstone and returns
/// the revived doc_id. The worker re-classifies these from
/// `unchanged` to `added` so the chunks (which were physically
/// deleted at apply time) get re-extracted.
///
/// Returns the list of revived doc_ids, in the order encountered.
pub fn detect_revivals(
    state: &mut WatchedFolderState,
    snapshot: &WalkSnapshot,
    grace_secs: u64,
    now_unix: u64,
) -> Vec<String> {
    let cutoff = now_unix.saturating_sub(grace_secs);

    let mut revived = Vec::new();
    state.tombstones.retain(|t| {
        // Out of grace? Keep it for `expire` to clean up — don't
        // double-handle.
        if t.removed_at_unix < cutoff {
            return true;
        }
        // Match by doc_id (path) AND content_hash AND size. All three
        // protect against false positives — a different file at the
        // same path is treated as a fresh add.
        match snapshot.get(&t.doc_id) {
            Some(e)
                if e.content_hash == t.last_known_content_hash
                    && e.size_bytes == t.last_known_size_bytes =>
            {
                revived.push(t.doc_id.clone());
                false // drop the tombstone
            }
            _ => true, // keep the tombstone
        }
    });
    revived
}

/// Drop tombstones older than the grace window. Returns the dropped
/// doc_ids so the worker can emit `TombstoneExpired` events for
/// observability.
pub fn expire(state: &mut WatchedFolderState, grace_secs: u64, now_unix: u64) -> Vec<String> {
    let cutoff = now_unix.saturating_sub(grace_secs);
    let mut expired = Vec::new();
    state.tombstones.retain(|t| {
        if t.removed_at_unix < cutoff {
            expired.push(t.doc_id.clone());
            false
        } else {
            true
        }
    });
    expired
}

/// Evict oldest tombstones if the per-corpus cap is exceeded. Returns
/// the number evicted; caller emits `TombstoneEvicted` at `warn!`
/// level when the count is non-zero.
pub fn enforce_cap(state: &mut WatchedFolderState) -> usize {
    if state.tombstones.len() <= TOMBSTONE_CAP {
        return 0;
    }
    // Sort by removed_at_unix ascending so older entries are at the
    // front, then drain the excess from the front. Single sort
    // tolerable per sweep — TOMBSTONE_CAP is 100k, so worst case is
    // ~100k log 100k comparisons after a bulk-delete, which is
    // single-digit milliseconds.
    state.tombstones.sort_by_key(|t| t.removed_at_unix);
    let excess = state.tombstones.len() - TOMBSTONE_CAP;
    state.tombstones.drain(0..excess);
    excess
}

/// Index tombstones by absolute path for fast revival lookup. Used by
/// the worker; exposed here because the lookup table is purely a
/// derived view of `state.tombstones`.
pub fn tombstone_index(state: &WatchedFolderState) -> HashMap<String, &Tombstone> {
    state
        .tombstones
        .iter()
        .map(|t| (t.doc_id.clone(), t))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_corpus::watched::walker::EntryRecord;
    use std::path::PathBuf;

    fn entry(path: &str, hash: &str, size: u64) -> EntryRecord {
        EntryRecord {
            absolute_path: PathBuf::from(path),
            mtime_unix: 0,
            size_bytes: size,
            content_hash: hash.into(),
            source_root_index: 0,
            aux_paths: Vec::new(),
        }
    }

    fn snapshot(items: &[(&str, EntryRecord)]) -> WalkSnapshot {
        items
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn record_tombstones_preserves_prior_metadata() {
        let mut state = WatchedFolderState::fresh("c1");
        let prior = snapshot(&[("a.md", entry("/tmp/a.md", "h1", 10))]);
        let added = record_tombstones(&mut state, &["a.md".to_string()], &prior, 100);
        assert_eq!(added, 1);
        assert_eq!(state.tombstones.len(), 1);
        let t = &state.tombstones[0];
        assert_eq!(t.last_known_content_hash, "h1");
        assert_eq!(t.last_known_size_bytes, 10);
        assert_eq!(t.removed_at_unix, 100);
    }

    #[test]
    fn detect_revivals_matches_on_hash_and_size() {
        let mut state = WatchedFolderState::fresh("c1");
        state.tombstones.push(Tombstone {
            doc_id: "a.md".into(),
            absolute_path: PathBuf::from("/tmp/a.md"),
            last_known_content_hash: "h1".into(),
            last_known_size_bytes: 10,
            removed_at_unix: 100,
        });
        let snap = snapshot(&[("a.md", entry("/tmp/a.md", "h1", 10))]);
        let revived = detect_revivals(&mut state, &snap, 1_000, 200);
        assert_eq!(revived, vec!["a.md"]);
        assert!(state.tombstones.is_empty());
    }

    #[test]
    fn detect_revivals_skips_if_hash_differs() {
        let mut state = WatchedFolderState::fresh("c1");
        state.tombstones.push(Tombstone {
            doc_id: "a.md".into(),
            absolute_path: PathBuf::from("/tmp/a.md"),
            last_known_content_hash: "h1".into(),
            last_known_size_bytes: 10,
            removed_at_unix: 100,
        });
        // Same path, different hash → not a revival, treated as add.
        let snap = snapshot(&[("a.md", entry("/tmp/a.md", "h2", 10))]);
        let revived = detect_revivals(&mut state, &snap, 1_000, 200);
        assert!(revived.is_empty());
        assert_eq!(
            state.tombstones.len(),
            1,
            "tombstone preserved when hash differs"
        );
    }

    #[test]
    fn detect_revivals_skips_when_out_of_grace() {
        let mut state = WatchedFolderState::fresh("c1");
        state.tombstones.push(Tombstone {
            doc_id: "a.md".into(),
            absolute_path: PathBuf::from("/tmp/a.md"),
            last_known_content_hash: "h1".into(),
            last_known_size_bytes: 10,
            removed_at_unix: 100,
        });
        let snap = snapshot(&[("a.md", entry("/tmp/a.md", "h1", 10))]);
        // grace=10, now=200 → cutoff=190; tombstone at 100 is past
        // cutoff → no revival, but tombstone preserved for `expire`.
        let revived = detect_revivals(&mut state, &snap, 10, 200);
        assert!(revived.is_empty());
        assert_eq!(state.tombstones.len(), 1);
    }

    #[test]
    fn expire_drops_old_and_returns_doc_ids() {
        let mut state = WatchedFolderState::fresh("c1");
        state.tombstones = vec![
            Tombstone {
                doc_id: "old.md".into(),
                absolute_path: PathBuf::from("/tmp/old.md"),
                last_known_content_hash: "h".into(),
                last_known_size_bytes: 1,
                removed_at_unix: 50,
            },
            Tombstone {
                doc_id: "fresh.md".into(),
                absolute_path: PathBuf::from("/tmp/fresh.md"),
                last_known_content_hash: "h".into(),
                last_known_size_bytes: 1,
                removed_at_unix: 200,
            },
        ];
        let expired = expire(&mut state, 100, 250);
        assert_eq!(expired, vec!["old.md"]);
        assert_eq!(state.tombstones.len(), 1);
        assert_eq!(state.tombstones[0].doc_id, "fresh.md");
    }

    #[test]
    fn enforce_cap_evicts_oldest() {
        let mut state = WatchedFolderState::fresh("c1");
        for i in 0..(TOMBSTONE_CAP + 5) {
            state.tombstones.push(Tombstone {
                doc_id: format!("doc{i}.md"),
                absolute_path: PathBuf::from(format!("/tmp/doc{i}.md")),
                last_known_content_hash: "h".into(),
                last_known_size_bytes: 1,
                removed_at_unix: i as u64, // older = lower
            });
        }
        let evicted = enforce_cap(&mut state);
        assert_eq!(evicted, 5);
        assert_eq!(state.tombstones.len(), TOMBSTONE_CAP);
        // The oldest five (doc0..=doc4) should be gone.
        assert!(state.tombstones.iter().all(|t| t.removed_at_unix >= 5));
    }

    #[test]
    fn enforce_cap_noop_under_limit() {
        let mut state = WatchedFolderState::fresh("c1");
        state.tombstones.push(Tombstone {
            doc_id: "a.md".into(),
            absolute_path: PathBuf::from("/tmp/a.md"),
            last_known_content_hash: "h".into(),
            last_known_size_bytes: 1,
            removed_at_unix: 1,
        });
        let evicted = enforce_cap(&mut state);
        assert_eq!(evicted, 0);
        assert_eq!(state.tombstones.len(), 1);
    }
}
