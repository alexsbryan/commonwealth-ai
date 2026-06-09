// SPDX-License-Identifier: AGPL-3.0-or-later
use std::net::SocketAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::info;

use commonwealth_core::ids::MeshId;
use commonwealth_core::mesh::{MeshPeering, PeerTrustLevel};
use commonwealth_core::{Error, Result};

use crate::membership;

/// A request to establish peering between two meshes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeeringRequest {
    pub requesting_mesh_id: MeshId,
    pub requesting_mesh_name: String,
    pub trust_level: PeerTrustLevel,
    pub contact_nodes: Vec<SocketAddr>,
    /// BLAKE3 hash of the peering key (shared out-of-band, like join keys).
    pub peering_key_hash: [u8; 32],
}

/// Response to a peering request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeeringResponse {
    pub accepted: bool,
    pub responding_mesh_id: MeshId,
    pub responding_mesh_name: String,
    pub contact_nodes: Vec<SocketAddr>,
    pub reason: Option<String>,
}

/// Generate a peering key (same format as join keys, shared out-of-band).
pub fn generate_peering_key() -> String {
    membership::generate_join_key()
}

/// Hash a peering key.
pub fn hash_peering_key(key: &str) -> [u8; 32] {
    membership::hash_join_key(key)
}

/// Verify a peering key against an expected hash.
pub fn verify_peering_key(key: &str, expected_hash: &[u8; 32]) -> bool {
    membership::verify_join_key(key, expected_hash)
}

/// Create a peering request to send to another mesh.
pub fn create_peering_request(
    mesh_id: MeshId,
    mesh_name: &str,
    trust_level: PeerTrustLevel,
    contact_nodes: Vec<SocketAddr>,
    peering_key: &str,
) -> PeeringRequest {
    PeeringRequest {
        requesting_mesh_id: mesh_id,
        requesting_mesh_name: mesh_name.into(),
        trust_level,
        contact_nodes,
        peering_key_hash: hash_peering_key(peering_key),
    }
}

/// Accept a peering request and produce the MeshPeering record.
pub fn accept_peering(request: &PeeringRequest, peering_key: &str) -> Result<MeshPeering> {
    if !verify_peering_key(peering_key, &request.peering_key_hash) {
        return Err(Error::Membership("peering key mismatch".into()));
    }

    let peering = MeshPeering {
        peer_mesh_id: request.requesting_mesh_id,
        peer_mesh_name: request.requesting_mesh_name.clone(),
        trust_level: request.trust_level,
        established_at: now_secs(),
        contact_nodes: request.contact_nodes.clone(),
    };

    info!(
        peer_mesh = %request.requesting_mesh_name,
        trust_level = ?request.trust_level,
        "peering established"
    );

    Ok(peering)
}

/// Evaluate whether a request should overflow to a peered mesh.
/// Only `Full` trust peers allow inference overflow.
pub fn can_overflow_inference(peering: &MeshPeering) -> bool {
    peering.trust_level == PeerTrustLevel::Full
}

/// Evaluate whether model/index sharing is allowed with a peer.
pub fn can_share_resources(peering: &MeshPeering) -> bool {
    // Both trust levels allow model and knowledge sharing.
    matches!(
        peering.trust_level,
        PeerTrustLevel::ModelAndKnowledgeSharing | PeerTrustLevel::Full
    )
}

/// A transfer request for model files or corpus indexes between peered meshes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerTransferRequest {
    pub source_mesh_id: MeshId,
    pub target_mesh_id: MeshId,
    pub transfer_type: PeerTransferType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum PeerTransferType {
    /// Transfer a model file.
    Model {
        repo: String,
        file: String,
        size_bytes: u64,
    },
    /// Transfer a corpus index shard.
    CorpusIndex {
        corpus_id: String,
        shard_size_bytes: u64,
    },
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peering_key_generation_and_verification() {
        let key = generate_peering_key();
        let hash = hash_peering_key(&key);
        assert!(verify_peering_key(&key, &hash));
        assert!(!verify_peering_key("cwth-0000-0000-0000", &hash));
    }

    #[test]
    fn create_and_accept_peering() {
        let key = generate_peering_key();
        let mesh_id = MeshId::from_u128(1);

        let request = create_peering_request(
            mesh_id,
            "Sunset District Co-op",
            PeerTrustLevel::ModelAndKnowledgeSharing,
            vec!["10.0.1.50:9742".parse().unwrap()],
            &key,
        );

        let peering = accept_peering(&request, &key).unwrap();
        assert_eq!(peering.peer_mesh_name, "Sunset District Co-op");
        assert_eq!(
            peering.trust_level,
            PeerTrustLevel::ModelAndKnowledgeSharing
        );
    }

    #[test]
    fn accept_peering_rejects_bad_key() {
        let key = generate_peering_key();
        let request = create_peering_request(
            MeshId::from_u128(1),
            "Bad Mesh",
            PeerTrustLevel::Full,
            vec![],
            &key,
        );

        let result = accept_peering(&request, "cwth-0000-0000-0000");
        assert!(result.is_err());
    }

    #[test]
    fn overflow_only_with_full_trust() {
        let full_peer = MeshPeering {
            peer_mesh_id: MeshId::from_u128(2),
            peer_mesh_name: "Full Peer".into(),
            trust_level: PeerTrustLevel::Full,
            established_at: 0,
            contact_nodes: vec![],
        };
        assert!(can_overflow_inference(&full_peer));

        let sharing_peer = MeshPeering {
            peer_mesh_id: MeshId::from_u128(3),
            peer_mesh_name: "Sharing Peer".into(),
            trust_level: PeerTrustLevel::ModelAndKnowledgeSharing,
            established_at: 0,
            contact_nodes: vec![],
        };
        assert!(!can_overflow_inference(&sharing_peer));
    }

    #[test]
    fn both_trust_levels_allow_resource_sharing() {
        let full = MeshPeering {
            peer_mesh_id: MeshId::from_u128(2),
            peer_mesh_name: "Full".into(),
            trust_level: PeerTrustLevel::Full,
            established_at: 0,
            contact_nodes: vec![],
        };
        let sharing = MeshPeering {
            peer_mesh_id: MeshId::from_u128(3),
            peer_mesh_name: "Sharing".into(),
            trust_level: PeerTrustLevel::ModelAndKnowledgeSharing,
            established_at: 0,
            contact_nodes: vec![],
        };
        assert!(can_share_resources(&full));
        assert!(can_share_resources(&sharing));
    }

    #[test]
    fn peering_request_serde_roundtrip() {
        let key = generate_peering_key();
        let request = create_peering_request(
            MeshId::from_u128(1),
            "Test Mesh",
            PeerTrustLevel::Full,
            vec!["10.0.0.1:9742".parse().unwrap()],
            &key,
        );
        let json = serde_json::to_string(&request).unwrap();
        let back: PeeringRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.requesting_mesh_name, "Test Mesh");
    }

    #[test]
    fn transfer_request_serde_roundtrip() {
        let req = PeerTransferRequest {
            source_mesh_id: MeshId::from_u128(1),
            target_mesh_id: MeshId::from_u128(2),
            transfer_type: PeerTransferType::Model {
                repo: "Qwen/Qwen3-30B-GGUF".into(),
                file: "qwen3-30b-q4km.gguf".into(),
                size_bytes: 17_000_000_000,
            },
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: PeerTransferRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.source_mesh_id, MeshId::from_u128(1));
    }
}
