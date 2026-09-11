// SPDX-License-Identifier: AGPL-3.0-or-later
//! The bodies of `POST /internal/join` and `POST /internal/gossip` — the two
//! requests a process must speak to BE a member of a mesh.
//!
//! They live here, in the crate every member links, rather than beside the
//! handlers in `commonwealth-api`, because the handler side is one speaker
//! of these shapes and not the only one. A package-only rails daemon
//! (`commonwealth-rails`) joins and gossips with nothing of the api crate —
//! that crate's closure carries the corpus engine — and the alternative was
//! a mirror struct, which is how this shape came to have four declarations
//! by 2026-08 (see the notes on `MeshWire`). One definition, two speakers.
//!
//! The rejection bodies derive `Deserialize` as well as `Serialize`: a
//! client reads them, and one that could only write its own refusal would
//! be the same asymmetry that minted the mirrors.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use crate::ids::{NodeId, NodePubkey};
use crate::mesh::MeshWire;

/// A node asking to join.
///
/// The three optionals carry `skip_serializing_if` as well as `default`, so
/// the bytes written are omitted rather than nulled — what keeps a
/// pre-identity founder's serde seeing exactly the input it saw before.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRequest {
    pub join_key: String,
    pub joining_node_name: String,
    pub joining_node_addresses: Vec<SocketAddr>,
    /// Stable `NodeId` the joiner persists at `<data_dir>/node_id`. When
    /// present and not already claimed under a different name, the founder
    /// admits the joiner under this exact ID so rejoins don't leave zombies.
    ///
    /// Backward-compatible: older joiners don't send this field;
    /// `#[serde(default)]` makes the founder accept those requests unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_node_id: Option<NodeId>,
    /// The joiner's Ed25519 identity pubkey (the dial-by-key transport
    /// identity). Optional and serde-defaulted: pre-identity joiners omit it
    /// and are admitted exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_pubkey: Option<NodePubkey>,
    /// Hex Ed25519 proof of possession over
    /// `"cwth-join-pubkey-binding:" || proposed_node_id || name`. Required
    /// whenever `node_pubkey` is present; a bad or missing proof is a loud
    /// 401, never a silent admit-without-key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pubkey_proof: Option<String>,
}

/// The founder's reply to a join.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinResponse {
    /// Freshly-assigned id for the joining node.
    pub assigned_node_id: NodeId,
    /// Full authoritative mesh snapshot. The joiner replaces its local
    /// placeholder with this so member lists, peers, and the canonical
    /// mesh_id all match the founder's view.
    pub mesh: MeshWire,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRejection {
    pub reason: String,
}

/// One side of a pairwise gossip exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipRequest {
    pub mesh: MeshWire,
    /// Who is sending. Absent on a pre-proof peer.
    ///
    /// Binds [`GossipRequest::mesh_proof`] to a sender, so a captured proof
    /// cannot be presented by another node; and it is the attribution the
    /// inbound path never had before it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<NodeId>,
    /// Proof of `mesh_secret` possession — see `Mesh::mesh_proof`. Lets an
    /// upgraded pair authorize without the raw credential ever crossing the
    /// wire. Absent on a pre-proof peer, which falls through to comparing raw
    /// secrets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh_proof: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipResponse {
    pub mesh: MeshWire,
    /// The responder's identity and proof. The CALLER merges this response,
    /// so it is an authorization boundary in its own direction and needs the
    /// same evidence — a reply is not trusted just because we initiated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh_proof: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipRejection {
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::MeshId;
    use crate::mesh::tests::mesh_with;
    use crate::mesh::{MeshWire, SecretDisclosure};

    fn wire() -> MeshWire {
        let mesh = mesh_with(Vec::new(), MeshId::from_u128(1), [9u8; 32]);
        MeshWire::for_peer(&mesh, SecretDisclosure::Redact)
    }

    /// The bytes on the wire are what the api-side handlers and the
    /// sovereign-mesh mirrors wrote before the move: an absent optional is
    /// OMITTED, never `null`. Failing input: drop `skip_serializing_if` on
    /// any optional and the `"from":null` below appears.
    #[test]
    fn absent_optionals_are_omitted_not_nulled() {
        let req = GossipRequest {
            mesh: wire(),
            from: None,
            mesh_proof: None,
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(!s.contains("\"from\""), "{s}");
        assert!(!s.contains("\"mesh_proof\""), "{s}");
        let join = JoinRequest {
            join_key: "k".into(),
            joining_node_name: "n".into(),
            joining_node_addresses: Vec::new(),
            proposed_node_id: None,
            node_pubkey: None,
            pubkey_proof: None,
        };
        let s = serde_json::to_string(&join).unwrap();
        assert!(!s.contains("proposed_node_id"), "{s}");
        assert!(!s.contains("null"), "{s}");
    }

    /// A pre-identity peer's request still parses: every optional is
    /// `default`, so an older joiner that never sends them is admitted
    /// exactly as before.
    #[test]
    fn a_request_without_the_optionals_parses() {
        let s = format!("{{\"mesh\":{}}}", serde_json::to_string(&wire()).unwrap());
        let req: GossipRequest = serde_json::from_str(&s).unwrap();
        assert!(req.from.is_none() && req.mesh_proof.is_none());
        let s = "{\"join_key\":\"k\",\"joining_node_name\":\"n\",\"joining_node_addresses\":[]}";
        let req: JoinRequest = serde_json::from_str(s).unwrap();
        assert!(req.node_pubkey.is_none());
        let rej: GossipRejection = serde_json::from_str("{\"reason\":\"no\"}").unwrap();
        assert_eq!(rej.reason, "no");
    }
}
