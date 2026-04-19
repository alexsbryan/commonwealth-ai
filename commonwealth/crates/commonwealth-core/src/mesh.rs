use std::collections::HashMap;
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use crate::capabilities::NodeCapabilities;
use crate::ids::{MeshId, NodeId};

/// A Commonwealth mesh — a closed group of trusted nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mesh {
    pub id: MeshId,
    pub name: String,
    /// BLAKE3 hash of the join key; raw key is never persisted.
    pub join_key_hash: [u8; 32],
    pub members: HashMap<NodeId, MemberRecord>,
    pub peers: Vec<MeshPeering>,
}

/// Record of a member node in the mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberRecord {
    pub node_id: NodeId,
    pub name: String,
    pub invited_by: NodeId,
    pub joined_at: u64,
    pub last_seen: u64,
    pub status: NodeStatus,
    pub capabilities: NodeCapabilities,
    pub addresses: Vec<SocketAddr>,
}

/// Current status of a node as observed by the mesh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Online,
    /// Under heavy local load.
    Busy,
    /// Not responding but not formally departed.
    Away,
    /// Gracefully disconnected.
    Offline,
}

/// Trust relationship with a peer mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshPeering {
    pub peer_mesh_id: MeshId,
    pub peer_mesh_name: String,
    pub trust_level: PeerTrustLevel,
    pub established_at: u64,
    pub contact_nodes: Vec<SocketAddr>,
}

/// Level of trust between peered meshes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerTrustLevel {
    /// Share model files and corpus indexes only.
    ModelAndKnowledgeSharing,
    /// Share everything plus allow overflow inference routing.
    Full,
}

/// Summary of what a `Mesh::merge_from` call did. Used for tracing
/// ("we learned about 1 new member") and test assertions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MergeReport {
    /// Members that were absent locally and got added from `other`.
    pub added: usize,
    /// Members that existed locally but were replaced by a newer
    /// record from `other` (higher `last_seen`).
    pub updated: usize,
    /// True when the merge was refused outright because `other`
    /// described a different mesh (mismatching `id` or
    /// `join_key_hash`). When set, nothing was mutated.
    pub rejected: bool,
}

impl Mesh {
    /// Merge another view of this mesh into `self`. Per-member
    /// `last_seen` acts as the Lamport-ish clock: the record with
    /// the higher timestamp wins. Own record (the caller's
    /// `self_node_id`) is *never* overwritten via gossip — we are
    /// always authoritative for our own liveness, capabilities,
    /// addresses, etc. Peers can learn about us from others, but
    /// they can't replace what we know about ourselves.
    ///
    /// Returns a [`MergeReport`] so callers can surface "added 1,
    /// updated 0" in tracing logs — useful for noticing when gossip
    /// is actually converging vs. spinning.
    ///
    /// Rejects outright when `other.id` or `other.join_key_hash`
    /// doesn't match ours — that's the auth boundary. Anyone who
    /// knows our mesh_id (public via mDNS) but not the join_key
    /// shouldn't be able to inject members into our view.
    pub fn merge_from(&mut self, self_node_id: NodeId, other: &Mesh) -> MergeReport {
        if self.id != other.id || self.join_key_hash != other.join_key_hash {
            return MergeReport {
                added: 0,
                updated: 0,
                rejected: true,
            };
        }

        let mut report = MergeReport::default();
        for (id, incoming) in &other.members {
            if *id == self_node_id {
                // Authoritative-for-self: never accept an incoming
                // record about us, regardless of its `last_seen`.
                // If a buggy peer has a stale view of us, we'll
                // correct them on our next gossip round when we
                // ship our current record in the push-pull reply.
                continue;
            }
            match self.members.get(id) {
                None => {
                    self.members.insert(*id, incoming.clone());
                    report.added += 1;
                }
                Some(existing) if incoming.last_seen > existing.last_seen => {
                    self.members.insert(*id, incoming.clone());
                    report.updated += 1;
                }
                Some(_) => {
                    // Existing is equal or newer — keep ours.
                }
            }
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_status_serde_roundtrip() {
        for status in [
            NodeStatus::Online,
            NodeStatus::Busy,
            NodeStatus::Away,
            NodeStatus::Offline,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let back: NodeStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, back);
        }
    }

    #[test]
    fn peer_trust_level_serde_roundtrip() {
        for level in [
            PeerTrustLevel::ModelAndKnowledgeSharing,
            PeerTrustLevel::Full,
        ] {
            let json = serde_json::to_string(&level).unwrap();
            let back: PeerTrustLevel = serde_json::from_str(&json).unwrap();
            assert_eq!(level, back);
        }
    }

    #[test]
    fn mesh_peering_serializes_to_json() {
        let peering = MeshPeering {
            peer_mesh_id: MeshId::from_u128(42),
            peer_mesh_name: "Mission District Co-op".into(),
            trust_level: PeerTrustLevel::Full,
            established_at: 1700000000,
            contact_nodes: vec!["10.0.1.50:9742".parse().unwrap()],
        };
        let json = serde_json::to_string(&peering).unwrap();
        let back: MeshPeering = serde_json::from_str(&json).unwrap();
        assert_eq!(back.peer_mesh_name, "Mission District Co-op");
        assert_eq!(back.trust_level, PeerTrustLevel::Full);
    }

    // ── Mesh::merge_from ──────────────────────────────────────

    use crate::capabilities::{AvailableResources, HardwareProfile};

    fn member(id: NodeId, name: &str, last_seen: u64) -> MemberRecord {
        MemberRecord {
            node_id: id,
            name: name.into(),
            invited_by: id,
            joined_at: 0,
            last_seen,
            status: NodeStatus::Online,
            capabilities: NodeCapabilities {
                hardware: HardwareProfile {
                    gpus: vec![],
                    system_ram_gb: 0,
                    cpu_cores: 0,
                    total_storage_gb: 0,
                    free_storage_gb: 0,
                    network_bandwidth_mbps: None,
                },
                available: AvailableResources::default(),
                active_processes: vec![],
                hosted_corpora: vec![],
                reported_at: last_seen,
                inference_availability: 1.0,
                inference_capable: false,
                loaded_models: vec![],

                embed_model: None,            },
            addresses: vec![],
        }
    }

    fn mesh_with(members: Vec<MemberRecord>, id: MeshId, hash: [u8; 32]) -> Mesh {
        let mut map = HashMap::new();
        for m in members {
            map.insert(m.node_id, m);
        }
        Mesh {
            id,
            name: "test".into(),
            join_key_hash: hash,
            members: map,
            peers: vec![],
        }
    }

    #[test]
    fn merge_adds_missing_members() {
        let mesh_id = MeshId::from_u128(1);
        let hash = [7u8; 32];
        let a = NodeId::from_u128(100);
        let b = NodeId::from_u128(200);

        let mut local = mesh_with(vec![member(a, "A", 10)], mesh_id, hash);
        let remote = mesh_with(
            vec![member(a, "A", 10), member(b, "B", 20)],
            mesh_id,
            hash,
        );

        let report = local.merge_from(a, &remote);
        assert_eq!(report.added, 1);
        assert_eq!(report.updated, 0);
        assert!(!report.rejected);
        assert_eq!(local.members.len(), 2);
        assert!(local.members.contains_key(&b));
    }

    #[test]
    fn merge_updates_stale_records_via_last_seen() {
        let mesh_id = MeshId::from_u128(1);
        let hash = [7u8; 32];
        let a = NodeId::from_u128(100);
        let b = NodeId::from_u128(200);

        let mut local = mesh_with(
            vec![member(a, "A", 10), member(b, "B-stale", 5)],
            mesh_id,
            hash,
        );
        let remote = mesh_with(
            vec![member(b, "B-fresh", 50)],
            mesh_id,
            hash,
        );

        let report = local.merge_from(a, &remote);
        assert_eq!(report.added, 0);
        assert_eq!(report.updated, 1);
        assert_eq!(local.members.get(&b).unwrap().name, "B-fresh");
        assert_eq!(local.members.get(&b).unwrap().last_seen, 50);
    }

    #[test]
    fn merge_keeps_newer_local_over_older_incoming() {
        let mesh_id = MeshId::from_u128(1);
        let hash = [7u8; 32];
        let a = NodeId::from_u128(100);
        let b = NodeId::from_u128(200);

        let mut local = mesh_with(
            vec![member(a, "A", 10), member(b, "B-fresh", 100)],
            mesh_id,
            hash,
        );
        let remote = mesh_with(vec![member(b, "B-stale", 20)], mesh_id, hash);

        let report = local.merge_from(a, &remote);
        assert_eq!(report.added, 0);
        assert_eq!(report.updated, 0);
        assert_eq!(local.members.get(&b).unwrap().name, "B-fresh");
    }

    #[test]
    fn merge_never_overwrites_self_even_if_incoming_is_newer() {
        // A peer could ship us an old view of ourselves that has a
        // stale address list, wrong name, or Offline status. We're
        // authoritative for self — ignore it.
        let mesh_id = MeshId::from_u128(1);
        let hash = [7u8; 32];
        let me = NodeId::from_u128(100);

        let mut local = mesh_with(vec![member(me, "Me-Real", 10)], mesh_id, hash);
        let remote = mesh_with(
            vec![{
                let mut m = member(me, "Me-Imposter", 9999);
                m.status = NodeStatus::Offline;
                m
            }],
            mesh_id,
            hash,
        );

        let report = local.merge_from(me, &remote);
        assert_eq!(report.added, 0);
        assert_eq!(report.updated, 0);
        assert_eq!(local.members.get(&me).unwrap().name, "Me-Real");
        assert_eq!(local.members.get(&me).unwrap().last_seen, 10);
        assert_eq!(local.members.get(&me).unwrap().status, NodeStatus::Online);
    }

    #[test]
    fn merge_rejects_different_mesh_id() {
        let me = NodeId::from_u128(1);
        let hash = [7u8; 32];
        let mut local = mesh_with(vec![member(me, "M", 10)], MeshId::from_u128(1), hash);
        let remote = mesh_with(
            vec![member(NodeId::from_u128(2), "X", 100)],
            MeshId::from_u128(2), // different!
            hash,
        );

        let report = local.merge_from(me, &remote);
        assert!(report.rejected);
        assert_eq!(report.added, 0);
        assert_eq!(local.members.len(), 1, "no mutation on reject");
    }

    #[test]
    fn merge_rejects_mismatched_join_key_hash() {
        let me = NodeId::from_u128(1);
        let mesh_id = MeshId::from_u128(1);
        let mut local = mesh_with(vec![member(me, "M", 10)], mesh_id, [7u8; 32]);
        let remote = mesh_with(
            vec![member(NodeId::from_u128(2), "X", 100)],
            mesh_id,
            [9u8; 32], // different hash!
        );

        let report = local.merge_from(me, &remote);
        assert!(report.rejected);
        assert_eq!(local.members.len(), 1);
    }
}
