// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gossip-replicated per-peer view of which corpus shards each peer
//! has finished ingesting locally.
//!
//! Why this exists
//! ---------------
//! `corpus_engine::CorpusEngine::corpus_processed_shards` walks the
//! local `index_dir/` only — canonical `<corpus>/` and any
//! `<corpus>-partition-*/` dirs **on this machine**. That's correct
//! when the merge has already pulled peer partitions to disk, but
//! during a live collaborative ingest each peer only has its own
//! partition. Without a cross-peer view, the dispatch side
//! (`corpus_collaborate`) computes
//! `remaining = (0..shard_count) - local_processed_shards` and
//! queues units for shards another peer has already done.
//!
//! Observed in the wild on a two-peer Wikipedia ingest: 8 of 33
//! distinct shards processed twice because neither peer saw the
//! other's progress until the (broken) merge step.
//!
//! How this works
//! --------------
//! Every peer publishes its local `processed_shards` array into
//! [`MeshStore`] on each `auto_ingest` tick under
//! `app_id = "corpus-engine"`, key
//! `processed_shards:<corpus_id>:<self_node_id_hex>`. The
//! `:<node_id>` suffix keeps each peer's slot distinct — without
//! it, two peers writing the same key would collapse under LWW and
//! we'd see only the last-writer's view.
//!
//! The dispatch side calls [`union_processed_shards`] to merge
//! everyone's published view into one set, which it then subtracts
//! from `(0..shard_count)` to compute the actually-remaining work.

use std::collections::BTreeSet;

use commonwealth_core::ids::NodeId;

use crate::store::MeshStore;

/// `app_id` used when publishing or scanning processed-shards
/// announcements. Shared by `corpus-engine`'s other gossip blobs
/// (handoffs, collaborate state) — the namespace is "things the
/// corpus pipeline gossips," not "the processed-shards table
/// specifically."
pub const PROCESSED_SHARDS_APP_ID: &str = "corpus-engine";

/// Build the canonical key shape this peer publishes under for
/// `corpus_id`. The hex-encoded node id is appended so each peer
/// owns a distinct slot in `MeshStore` — see module docs for the
/// LWW-collision rationale.
pub fn processed_shards_key(corpus_id: &str, node_id: NodeId) -> String {
    let hex: String = node_id
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("processed_shards:{corpus_id}:{hex}")
}

/// Scan every `processed_shards:<corpus_id>:*` entry visible in
/// `store` (post-gossip) and union the shard indices into a single
/// set. Used by the dispatch side to compute `remaining` with full
/// peer awareness.
///
/// Failure modes are absorbed: a missing scan, an entry whose
/// payload is unparseable, etc., all degrade to "fewer entries in
/// the union" rather than panicking. The dispatch will at worst
/// over-queue (re-queue work some peer has actually done), which
/// is exactly the legacy behaviour — never under-queue (skip work
/// that nobody has done).
pub fn union_processed_shards(store: &MeshStore, corpus_id: &str) -> BTreeSet<usize> {
    let prefix = format!("processed_shards:{corpus_id}:");
    let entries = match store.scan(PROCESSED_SHARDS_APP_ID, &prefix) {
        Ok(e) => e,
        Err(_) => return BTreeSet::new(),
    };
    let mut out = BTreeSet::new();
    for entry in entries {
        let Ok(arr) = serde_json::from_slice::<Vec<usize>>(&entry.value) else {
            continue;
        };
        out.extend(arr);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn nid(byte: u8) -> NodeId {
        NodeId::from_u128(byte as u128)
    }

    #[test]
    fn key_includes_node_id_suffix() {
        // Without the per-peer suffix, two peers writing the same key
        // collapse under LWW. Pin the contract so a future "simplify"
        // can't silently regress.
        let a = processed_shards_key("wikipedia", nid(7));
        let b = processed_shards_key("wikipedia", nid(8));
        assert_ne!(a, b);
        assert!(a.starts_with("processed_shards:wikipedia:"));
        assert!(b.starts_with("processed_shards:wikipedia:"));
    }

    #[test]
    fn union_returns_empty_for_unknown_corpus() {
        let store = MeshStore::in_memory().unwrap();
        let result = union_processed_shards(&store, "nope");
        assert!(result.is_empty());
    }

    #[test]
    fn union_merges_per_peer_views_into_single_set() {
        // Pins the actual scenario from the two-peer Wikipedia ingest
        // that motivated this module. Local view sees 26 shards
        // [10..36] minus 23. linux-peer-equivalent peer published
        // [0..4, 16, 17, 18, 21, 23, 28, 31, 33, 35, 37]. Union
        // should be 33 distinct shards; missing-from-union = {5,6,7,8,9}.
        let store = MeshStore::in_memory().unwrap();
        let local_node = nid(0xb8);
        let peer_node = nid(0x44);

        let local_shards: Vec<usize> = (10..=22).chain(24..=36).collect();
        store
            .set(
                PROCESSED_SHARDS_APP_ID,
                &processed_shards_key("wikipedia", local_node),
                Bytes::from(serde_json::to_vec(&local_shards).unwrap()),
                local_node,
            )
            .unwrap();

        let peer_shards: Vec<usize> = vec![0, 1, 2, 3, 4, 16, 17, 18, 21, 23, 28, 31, 33, 35, 37];
        store
            .set(
                PROCESSED_SHARDS_APP_ID,
                &processed_shards_key("wikipedia", peer_node),
                Bytes::from(serde_json::to_vec(&peer_shards).unwrap()),
                peer_node,
            )
            .unwrap();

        let union = union_processed_shards(&store, "wikipedia");
        assert_eq!(union.len(), 33, "33 distinct shards across the two peers");

        let missing: Vec<usize> = (0..38).filter(|i| !union.contains(i)).collect();
        assert_eq!(
            missing,
            vec![5, 6, 7, 8, 9],
            "5 shards still untouched anywhere"
        );
    }

    #[test]
    fn union_skips_unparseable_entries_without_panicking() {
        // Defensive: a corrupted payload from gossip mustn't take
        // the dispatch down. We log nothing here (it's a unit test);
        // the production code path emits no log either, just degrades.
        let store = MeshStore::in_memory().unwrap();
        let node = nid(0xff);
        store
            .set(
                PROCESSED_SHARDS_APP_ID,
                &processed_shards_key("wikipedia", node),
                Bytes::from_static(b"not valid json"),
                node,
            )
            .unwrap();
        let union = union_processed_shards(&store, "wikipedia");
        assert!(union.is_empty());
    }
}
