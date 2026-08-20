// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::{Deserialize, Serialize};
use std::fmt;

// The id shape moved to `kernel-types` (layer 0) on 2026-08-20,
// noun-convergence rung nc-1-kernel. Why: `NodeId` is the one id all three
// product domains must be able to name — `kernel_types::Origin::served_by`
// cannot say "a peer served this evidence" without it — and this crate sits
// three layers above the kernel with nine dependencies including
// `ed25519-dalek`, so the kernel cannot reach up to here.
//
// Pulling ONE id out of a six-member macro family leaves either a duplicated
// macro or an orphan. Moving the MACRO down and leaving the five
// mesh-specific ids here does neither: there is still exactly one
// implementation (ARCH §10.6), the five ids below are unchanged, and `NodeId`
// is re-exported so all 755 existing reference sites are untouched.
use kernel_types::define_id;
pub use kernel_types::NodeId;

define_id!(MeshId, "mesh");
define_id!(ModelId, "model");
define_id!(ProcessId, "proc");
define_id!(PlanId, "plan");
define_id!(HandoffId, "handoff");

/// A node's Ed25519 verifying key — the mesh-wide cryptographic
/// identity of a node, distinct from the opaque random [`NodeId`].
///
/// Why both exist: `NodeId` predates this key and is the join/gossip
/// primary key everywhere; changing it is a wire bump across every
/// surface. The pubkey is the *transport-grade* identity — it is,
/// byte for byte, a valid iroh node id, so a future dial-by-key
/// transport authenticates peers end-to-end with this exact value.
/// Until then it travels alongside the record so the trust ring is
/// transport-ready.
///
/// Serializes as a 32-byte array (same convention as
/// `Mesh::join_key_hash`). Display is full lowercase hex — this is
/// public key material, never secret.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodePubkey(pub [u8; 32]);

impl NodePubkey {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for NodePubkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode(self.0))
    }
}

impl fmt::Debug for NodePubkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodePubkey({})", self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_display_format() {
        let id = NodeId::from_u128(0x0123456789abcdef_0000000000000000);
        let s = id.to_string();
        assert!(s.starts_with("node-"));
        assert_eq!(s, "node-0123456789abcdef");
    }

    #[test]
    fn id_ordering_is_deterministic() {
        let a = NodeId::from_u128(1);
        let b = NodeId::from_u128(2);
        assert!(a < b);
    }

    #[test]
    fn id_serde_roundtrip() {
        let id = MeshId::generate();
        let json = serde_json::to_string(&id).unwrap();
        let back: MeshId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn hex_round_trips_full_16_bytes() {
        let id = NodeId::generate();
        let hex = id.to_hex();
        assert_eq!(
            hex.len(),
            32,
            "full 16-byte hex, not the short Display form"
        );
        assert_eq!(NodeId::from_hex(&hex), Some(id));
        // Tolerates surrounding whitespace (config values often have it).
        assert_eq!(NodeId::from_hex(&format!("  {hex}\n")), Some(id));
    }

    #[test]
    fn from_hex_rejects_malformed() {
        assert_eq!(NodeId::from_hex("not-hex"), None);
        assert_eq!(NodeId::from_hex("abcd"), None, "wrong length");
        assert_eq!(NodeId::from_hex(""), None);
    }

    #[test]
    fn distinct_id_types_are_not_interchangeable() {
        // This is a compile-time guarantee, but we verify they're distinct types
        let node = NodeId::from_u128(1);
        let mesh = MeshId::from_u128(1);
        // Same bytes, but different types — this would fail to compile:
        // let _: NodeId = mesh;
        assert_eq!(node.as_bytes(), mesh.as_bytes());
    }
}
