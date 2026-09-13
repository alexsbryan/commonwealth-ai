// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared, mutable-mid-scenario fault state.
//!
//! One [`FaultPolicy`] is shared (behind `Arc<RwLock<_>>`) across every node's
//! [`super::FaultTransport`] and [`super::FaultProxy`], so an event applied
//! between gossip rounds is visible to every node's next endpoint resolution
//! and every newly accepted proxy connection.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use commonwealth_core::ids::NodeId;

/// A wire-level fault on a directed `(observer -> target)` edge, enforced by
/// [`super::FaultProxy`]. Absent from the policy map means a clean wire.
#[derive(Debug, Clone, Default)]
pub struct WireFault {
    /// Delay before the proxy dials upstream — inflates time-to-first-byte.
    pub connect_delay: Duration,
    /// Accept the connection then immediately close it (no upstream dial).
    pub drop_conn: bool,
    /// Forward at most this many upstream->client bytes, then cut — models a
    /// truncated / partial response (peer died mid-stream).
    pub cut_after_bytes: Option<usize>,
    /// Cap upstream->client throughput (bytes/sec) — models slow-loris.
    pub throttle_bps: Option<u64>,
}

/// Mesh-wide fault state. Symmetric reachability blocks model partitions;
/// `down` models process crashes (unreachable from everyone); `wire` carries
/// per-directed-edge [`WireFault`]s.
#[derive(Debug, Default)]
pub struct FaultPolicy {
    partitions: HashSet<(NodeId, NodeId)>,
    down: HashSet<NodeId>,
    wire: HashMap<(NodeId, NodeId), WireFault>,
}

impl FaultPolicy {
    /// Can `from` reach `to` right now? False if `to` is down or the
    /// `(from, to)` edge is partitioned.
    pub fn reachable(&self, from: NodeId, to: NodeId) -> bool {
        !self.down.contains(&to) && !self.partitions.contains(&(from, to))
    }

    /// Sever the link between `a` and `b` in both directions.
    pub fn partition(&mut self, a: NodeId, b: NodeId) {
        self.partitions.insert((a, b));
        self.partitions.insert((b, a));
    }

    /// Restore the link between `a` and `b` in both directions.
    pub fn heal(&mut self, a: NodeId, b: NodeId) {
        self.partitions.remove(&(a, b));
        self.partitions.remove(&(b, a));
    }

    /// Mark `node` down (unreachable from every observer).
    pub fn mark_down(&mut self, node: NodeId) {
        self.down.insert(node);
    }

    /// Mark `node` back up.
    pub fn mark_up(&mut self, node: NodeId) {
        self.down.remove(&node);
    }

    /// Install a wire fault on the directed `(observer, target)` edge.
    pub fn set_wire(&mut self, observer: NodeId, target: NodeId, fault: WireFault) {
        self.wire.insert((observer, target), fault);
    }

    /// Remove any wire fault on `(observer, target)`.
    pub fn clear_wire(&mut self, observer: NodeId, target: NodeId) {
        self.wire.remove(&(observer, target));
    }

    /// The wire fault on `(observer, target)`, if any.
    pub fn wire_fault(&self, observer: NodeId, target: NodeId) -> Option<WireFault> {
        self.wire.get(&(observer, target)).cloned()
    }
}

/// Handle shared by every node's transport / proxy + the scenario driver.
pub type SharedPolicy = Arc<RwLock<FaultPolicy>>;

/// A fresh clean-wire shared policy.
pub fn shared_policy() -> SharedPolicy {
    Arc::new(RwLock::new(FaultPolicy::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nid(n: u128) -> NodeId {
        NodeId::from_u128(n)
    }

    #[test]
    fn partition_is_symmetric_and_heals() {
        let mut p = FaultPolicy::default();
        let (a, b) = (nid(1), nid(2));
        assert!(p.reachable(a, b) && p.reachable(b, a));
        p.partition(a, b);
        assert!(!p.reachable(a, b) && !p.reachable(b, a));
        p.heal(a, b);
        assert!(p.reachable(a, b) && p.reachable(b, a));
    }

    #[test]
    fn down_node_unreachable_from_everyone() {
        let mut p = FaultPolicy::default();
        let (a, b, c) = (nid(1), nid(2), nid(3));
        p.mark_down(c);
        assert!(!p.reachable(a, c) && !p.reachable(b, c));
        // c can still (in policy terms) reach others — down is about being a target.
        assert!(p.reachable(c, a));
        p.mark_up(c);
        assert!(p.reachable(a, c));
    }
}
