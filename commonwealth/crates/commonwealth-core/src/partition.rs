// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic partitioning primitives shared across replicated-state
//! daemons (inference scheduler, freshness watchers, etc.).
//!
//! Two related concerns live here:
//!
//! - **Leader election** — a per-decision leader selected by `min(NodeId)`
//!   over the current online set. No consensus, no debounce: the comparison
//!   is pure-functional over gossiped membership, so every node converges
//!   on the same leader without coordination.
//! - **Owner partitioning** — for work that must be assigned to exactly one
//!   node per key (e.g. "which node refreshes this article today"), we use
//!   rendezvous hashing (highest-random-weight). When a node leaves the
//!   mesh only that node's keys reassign, unlike modulo hashing which
//!   reshuffles every key on every membership change.
//!
//! Both functions are O(N) over mesh size, which is single- to
//! low-double-digit in practice.
use crate::ids::NodeId;

/// Determine the scheduling leader among a set of online nodes.
///
/// The leader is the node with the lowest `NodeId`. Pure-functional over
/// the input slice — every node fed the same membership snapshot picks the
/// same leader without explicit coordination.
pub fn elect_leader(online_nodes: &[NodeId]) -> Option<NodeId> {
    online_nodes.iter().copied().min()
}

/// Check if a given node is the current scheduling leader.
pub fn is_leader(self_id: NodeId, online_nodes: &[NodeId]) -> bool {
    elect_leader(online_nodes) == Some(self_id)
}

/// Highest-random-weight (rendezvous) owner assignment for `key`.
///
/// Returns the candidate node that owns `key`. When a candidate is added
/// or removed, only that candidate's keys reassign; all other key→owner
/// mappings remain stable. Modulo hashing fails this property — a single
/// node leaving reshuffles every key.
pub fn rendezvous_owner(key: &str, candidates: &[NodeId]) -> Option<NodeId> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    candidates.iter().copied().max_by_key(|node| {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        node.hash(&mut hasher);
        hasher.finish()
    })
}

/// Check if `self_id` owns `key` under rendezvous partitioning.
pub fn is_owner(self_id: NodeId, key: &str, candidates: &[NodeId]) -> bool {
    rendezvous_owner(key, candidates) == Some(self_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn nodes(ns: &[u128]) -> Vec<NodeId> {
        ns.iter().copied().map(NodeId::from_u128).collect()
    }

    #[test]
    fn elect_leader_picks_lowest() {
        let n = nodes(&[5, 2, 8, 1]);
        assert_eq!(elect_leader(&n), Some(NodeId::from_u128(1)));
    }

    #[test]
    fn elect_leader_empty() {
        assert_eq!(elect_leader(&[]), None);
    }

    #[test]
    fn elect_leader_is_input_order_independent() {
        let a = nodes(&[5, 2, 3]);
        let b = nodes(&[3, 5, 2]);
        assert_eq!(elect_leader(&a), elect_leader(&b));
    }

    #[test]
    fn is_leader_returns_true_for_min() {
        let n = nodes(&[3, 1, 5]);
        assert!(is_leader(NodeId::from_u128(1), &n));
        assert!(!is_leader(NodeId::from_u128(3), &n));
        assert!(!is_leader(NodeId::from_u128(5), &n));
    }

    #[test]
    fn rendezvous_owner_is_deterministic() {
        let n = nodes(&[10, 20, 30]);
        let first = rendezvous_owner("Albert_Einstein", &n);
        for _ in 0..32 {
            assert_eq!(rendezvous_owner("Albert_Einstein", &n), first);
        }
    }

    #[test]
    fn rendezvous_owner_input_order_independent() {
        let a = nodes(&[10, 20, 30]);
        let b = nodes(&[30, 10, 20]);
        assert_eq!(
            rendezvous_owner("Donald_Trump", &a),
            rendezvous_owner("Donald_Trump", &b),
        );
    }

    #[test]
    fn rendezvous_owner_distributes_roughly_uniformly() {
        let n = nodes(&[1, 2, 3, 4, 5]);
        let mut counts: HashMap<NodeId, usize> = HashMap::new();
        for i in 0..10_000 {
            let key = format!("Article_{i:05}");
            let owner = rendezvous_owner(&key, &n).unwrap();
            *counts.entry(owner).or_default() += 1;
        }
        // 5 nodes → ~2000 each. Allow ±15% — generous to keep the test
        // robust to DefaultHasher seed changes.
        for (node, count) in &counts {
            assert!(
                (1700..=2300).contains(count),
                "node {node} owns {count} keys, expected ~2000",
            );
        }
        assert_eq!(counts.len(), 5, "every node should own at least one key");
    }

    #[test]
    fn rendezvous_only_redistributes_leaving_nodes_keys() {
        // Stability under churn — the entire premise of rendezvous over
        // modulo. Build the assignment map for 5 nodes, drop one, and
        // assert that only that node's keys move.
        let full = nodes(&[1, 2, 3, 4, 5]);
        let dropped = NodeId::from_u128(3);
        let reduced: Vec<NodeId> = full.iter().copied().filter(|id| *id != dropped).collect();

        let mut moved_from_dropped = 0;
        let mut moved_from_other = 0;
        for i in 0..2_000 {
            let key = format!("Article_{i:05}");
            let before = rendezvous_owner(&key, &full).unwrap();
            let after = rendezvous_owner(&key, &reduced).unwrap();
            if before == dropped {
                // Must have moved to one of the survivors.
                assert_ne!(before, after);
                moved_from_dropped += 1;
            } else if before != after {
                moved_from_other += 1;
            }
        }
        // No keys owned by surviving nodes should have shifted.
        assert_eq!(
            moved_from_other, 0,
            "rendezvous should not reshuffle non-leaving nodes' keys",
        );
        // Roughly 1/5 of keys were owned by the dropped node.
        assert!(
            (300..=500).contains(&moved_from_dropped),
            "expected ~400 keys to move from dropped node, got {moved_from_dropped}",
        );
    }

    #[test]
    fn is_owner_round_trips_with_rendezvous_owner() {
        let n = nodes(&[7, 8, 9]);
        let key = "Hurricane_Imelda";
        let owner = rendezvous_owner(key, &n).unwrap();
        assert!(is_owner(owner, key, &n));
        for other in n.iter().filter(|id| **id != owner) {
            assert!(!is_owner(*other, key, &n));
        }
    }

    #[test]
    fn rendezvous_returns_none_for_empty_candidates() {
        assert_eq!(rendezvous_owner("anything", &[]), None);
    }
}
