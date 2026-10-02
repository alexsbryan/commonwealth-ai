// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one merge both gossip directions run (the round's reply in
//! [`super::exchange`], the inbound round in `crate::internal::gossip`), and
//! what a merge records and wakes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::{GossipAuth, MergeReport, Mesh, NodeStatus};
use tokio::sync::Notify;

/// Each peer's credential generation as this process last merged it:
/// `true` post-split, `false` pre-split, absent when not merged since start.
/// In memory on purpose — every restart empties it, which is why rotate runs
/// one confirmation round before it refuses on an absence
/// (`crate::membership::rotate`).
pub type SplitGenerations = Arc<Mutex<HashMap<NodeId, bool>>>;

/// `peer`'s generation as last merged, or `None` when not since start.
pub fn split_generation_of(split: &SplitGenerations, peer: NodeId) -> Option<bool> {
    split
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&peer)
        .copied()
}

/// Merge `incoming` from `sender` into `mesh`. An authorized merge records
/// the sender's credential generation (`MergeReport::peer_pre_split`: it
/// offered neither a proof nor a secret) for rotate's pre-split guard — the
/// daemon's `observe_peer_split_generation`, fed from both directions as
/// there. And a member this node held Offline that is Online after the merge
/// wakes the ring round, so what was written while it was away travels now,
/// not at the next sixty-second tick (the daemon's "peer back Online" nudge;
/// successor `ring_round.rs`
/// `a_write_made_while_the_peer_was_offline_travels_when_it_returns`).
pub(crate) fn merge_round(
    mesh: &mut Mesh,
    self_id: NodeId,
    incoming: &Mesh,
    auth: &GossipAuth,
    sender: Option<NodeId>,
    split: &SplitGenerations,
    ring_nudge: &Notify,
) -> MergeReport {
    let offline: Vec<NodeId> = mesh
        .members
        .values()
        .filter(|m| m.node_id != self_id && m.status == NodeStatus::Offline)
        .map(|m| m.node_id)
        .collect();
    let report = mesh.merge_from_authenticated(self_id, incoming, auth);
    if report.rejected() {
        return report;
    }
    if let Some(peer) = sender {
        let post_split = !report.peer_pre_split();
        split
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(peer, post_split);
        tracing::debug!(target: "gossip", peer = %peer, post_split,
            "gossip: the sender's credential generation recorded");
    }
    let back: Vec<&str> = offline
        .iter()
        .filter_map(|id| mesh.members.get(id))
        .filter(|m| m.status == NodeStatus::Online)
        .map(|m| m.name.as_str())
        .collect();
    if !back.is_empty() {
        tracing::info!(target: "gossip", back = ?back,
            "gossip: peer back Online — the ring round is woken");
        ring_nudge.notify_one();
    }
    report
}
