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
}
