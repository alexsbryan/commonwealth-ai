use commonwealth_core::ids::NodeId;

/// Determine the scheduling leader among a set of online nodes.
///
/// The leader is the node with the lowest NodeId. This is simple,
/// deterministic, and resolves concurrent scheduling conflicts.
pub fn elect_leader(online_nodes: &[NodeId]) -> Option<NodeId> {
    online_nodes.iter().copied().min()
}

/// Check if a given node is the current scheduling leader.
pub fn is_leader(self_id: NodeId, online_nodes: &[NodeId]) -> bool {
    elect_leader(online_nodes) == Some(self_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elect_leader_picks_lowest() {
        let nodes = vec![
            NodeId::from_u128(5),
            NodeId::from_u128(2),
            NodeId::from_u128(8),
            NodeId::from_u128(1),
        ];
        assert_eq!(elect_leader(&nodes), Some(NodeId::from_u128(1)));
    }

    #[test]
    fn elect_leader_empty() {
        assert_eq!(elect_leader(&[]), None);
    }

    #[test]
    fn elect_leader_single() {
        let nodes = vec![NodeId::from_u128(42)];
        assert_eq!(elect_leader(&nodes), Some(NodeId::from_u128(42)));
    }

    #[test]
    fn is_leader_true() {
        let nodes = vec![
            NodeId::from_u128(3),
            NodeId::from_u128(1),
            NodeId::from_u128(5),
        ];
        assert!(is_leader(NodeId::from_u128(1), &nodes));
    }

    #[test]
    fn is_leader_false() {
        let nodes = vec![
            NodeId::from_u128(3),
            NodeId::from_u128(1),
            NodeId::from_u128(5),
        ];
        assert!(!is_leader(NodeId::from_u128(3), &nodes));
    }

    #[test]
    fn leader_is_deterministic() {
        let nodes_a = vec![
            NodeId::from_u128(5),
            NodeId::from_u128(2),
            NodeId::from_u128(3),
        ];
        let nodes_b = vec![
            NodeId::from_u128(3),
            NodeId::from_u128(5),
            NodeId::from_u128(2),
        ];
        assert_eq!(elect_leader(&nodes_a), elect_leader(&nodes_b));
    }
}
