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

/// Decide whether `self_id` runs the shared-model HOST role, given an optional
/// operator-designated pin (`[shared_model] host_node_id`) and the current
/// eligible-anchor set.
///
/// A pin wins ONLY while it is actually an eligible anchor — so when the pinned
/// host drops out, the role fails over to the elected leader (min `NodeId`) of
/// the survivors instead of stranding the cluster. With no pin (or a pin that
/// isn't currently eligible) it is pure election. Like [`elect_leader`] it is
/// pure-functional over the gossiped set, so every anchor converges on the same
/// host without coordination — the property that makes leaderless failover safe
/// (a minority that can't see the pin still can't distribute: the quorum gate
/// keeps it in "forming").
pub fn should_host(self_id: NodeId, pin: Option<NodeId>, eligible_anchors: &[NodeId]) -> bool {
    match pin {
        Some(p) if eligible_anchors.contains(&p) => self_id == p,
        _ => is_leader(self_id, eligible_anchors),
    }
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
    fn should_host_pin_wins_when_eligible() {
        // The operator pinned node 5 as host. While 5 is an eligible anchor it
        // hosts — even though 2 is the lower NodeId that election would pick.
        let anchors = nodes(&[2, 5, 8]);
        let pin = Some(NodeId::from_u128(5));
        assert!(
            should_host(NodeId::from_u128(5), pin, &anchors),
            "pin hosts"
        );
        assert!(
            !should_host(NodeId::from_u128(2), pin, &anchors),
            "low id yields to pin"
        );
        assert!(!should_host(NodeId::from_u128(8), pin, &anchors));
    }

    #[test]
    fn should_host_fails_over_to_election_when_pin_absent() {
        // The pinned node 5 dropped out (not in the eligible set) → the role
        // fails over to the elected leader (min) of the survivors, here node 2.
        let survivors = nodes(&[2, 8]);
        let pin = Some(NodeId::from_u128(5));
        assert!(
            should_host(NodeId::from_u128(2), pin, &survivors),
            "elected leader hosts"
        );
        assert!(!should_host(NodeId::from_u128(8), pin, &survivors));
    }

    #[test]
    fn should_host_pure_election_when_no_pin() {
        let anchors = nodes(&[2, 8]);
        assert!(should_host(NodeId::from_u128(2), None, &anchors));
        assert!(!should_host(NodeId::from_u128(8), None, &anchors));
    }

    /// A MESH OF ONE. This is the case the whole local-only daemon rests on
    /// (cw-lift 3b): the degenerate roster must produce a CORRECT answer, not
    /// a skipped one, because that is what lets the same code path serve a
    /// solo node and a fleet with no `if local { … }` branch anywhere.
    ///
    /// All three pin shapes are asserted, because the pin arm is where a
    /// "nobody hosts" answer could hide: with no pin it is election over a
    /// one-node set, with self pinned it is the pin, and with a STALE pin
    /// (an operator-designated host that is not currently eligible) it must
    /// fail over to the only survivor rather than strand the node.
    #[test]
    fn should_host_is_true_for_a_roster_of_one() {
        let me = NodeId::from_u128(4);
        let alone = [me];

        assert!(
            should_host(me, None, &alone),
            "no pin: the only anchor is the elected leader"
        );
        assert!(
            should_host(me, Some(me), &alone),
            "self-pinned and eligible: the pin is us"
        );
        assert!(
            should_host(me, Some(NodeId::from_u128(9)), &alone),
            "a pin that is not eligible fails over to election, and election \
             over a roster of one is us — a stale pin must not strand a solo node"
        );
    }

    #[test]
    fn should_host_nobody_when_no_anchors() {
        assert!(!should_host(NodeId::from_u128(2), None, &[]));
        assert!(!should_host(
            NodeId::from_u128(2),
            Some(NodeId::from_u128(2)),
            &[]
        ));
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
