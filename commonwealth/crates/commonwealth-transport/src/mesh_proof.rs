// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ONE outbound stamp that lets a member of a plaintext mesh prove it is
//! one on an internal-port call that is not gossip.
//!
//! ## What it proves, and what it deliberately does not
//!
//! A mesh proof proves the GROUP and nothing finer: it is a keyed BLAKE3 over
//! (mesh id, sender, time window) under `Mesh::mesh_secret`, so ANY holder of
//! that secret can mint one naming ANY sender
//! ([`commonwealth_core::mesh::Mesh::proof_for`]). The sender travels in the
//! value because [`commonwealth_core::mesh::Mesh::verify_mesh_proof`] is bound
//! to it — a proof captured from one member cannot be presented by another —
//! not because it identifies the caller to the receiver. The receiver
//! (`sovereign_daemon::internal_principal`) treats a valid proof as "a holder
//! of the mesh secret is calling" and leaves the principal untouched. On a
//! plaintext hop nothing proves WHICH member is calling; only the encrypted
//! posture, where the QUIC handshake proves a key, can say that.
//!
//! ## Why a function that RETURNS a header rather than a client that sends one
//!
//! This crate has no HTTP client and never will: the seam resolves an address
//! and stops (see the lib docs). `PeerTransport` hands out base URLs, not
//! requests, so there is no request here to stamp. One function mints the
//! pair, every caller applies it, and the literal header name is spelled in
//! this file alone — a grep test in `sovereign-daemon` fails the normal test
//! run if a second production file spells it.

use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::Mesh;

/// The header a mesh proof rides in on a non-gossip internal call.
///
/// It sits under `iroh_identity_forward::MESH_HEADER_PREFIX` on purpose: the
/// acceptor strips every client-supplied `x-mesh-*` on an iroh hop, so a proof
/// typed by a caller cannot survive into a request the acceptor vouched for.
/// Over iroh the verified key decides and this header never arrives.
pub const MESH_PROOF_HEADER: &str = "x-mesh-proof";

/// One `(name, value)` header pair, minted by [`mesh_proof_stamp`].
///
/// Opaque so a caller cannot build one without a `Mesh` that holds a secret:
/// the only constructor is the minting function, and the only reader hands
/// back the pair to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshProofStamp(String);

impl MeshProofStamp {
    /// The header name and value to apply to an outbound request.
    pub fn pair(&self) -> (&'static str, &str) {
        (MESH_PROOF_HEADER, &self.0)
    }
}

/// Mint this node's proof of mesh membership for right now, or `None` when
/// this mesh holds no gossip credential.
///
/// `None` is a reported absence, never a default: a node that has not
/// migrated MUST NOT offer a proof, because an offered-and-failed proof is a
/// refusal at the receiver while an absent one is simply unproved
/// ([`Mesh::mesh_proof`] carries the same rule for gossip).
///
/// The value is `<sender-hex>.<proof>` — the full 32-char
/// [`NodeId::to_hex`], not the truncated `Display` form, because the receiver
/// must reconstruct the exact `NodeId` the proof was keyed to in order to
/// verify it. The sender half is NOT an identity claim; see the module docs.
pub fn mesh_proof_stamp(mesh: &Mesh, sender: NodeId, now_secs: u64) -> Option<MeshProofStamp> {
    let proof = mesh.mesh_proof(sender, now_secs)?;
    Some(MeshProofStamp(format!("{}.{proof}", sender.to_hex())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_core::ids::MeshId;
    use std::collections::HashMap;

    fn mesh_with(secret: [u8; 32]) -> Mesh {
        Mesh {
            mesh_secret: secret,
            invite_expires_at: None,
            id: MeshId::from_u128(7),
            name: "Stamp Test".into(),
            invite_key_hash: [0u8; 32],
            invite_version: 0,
            require_encryption: false,
            members: HashMap::new(),
            peers: vec![],
        }
    }

    /// The round trip the receiver depends on: the value carries the sender
    /// in a form that reconstructs the exact `NodeId` the proof was keyed to.
    #[test]
    fn a_stamp_verifies_against_the_mesh_that_minted_it() {
        let mesh = mesh_with([3u8; 32]);
        let me = NodeId::from_u128(0xABCD);
        let (name, value) = {
            let stamp = mesh_proof_stamp(&mesh, me, 1_000).expect("secret is set");
            let (n, v) = stamp.pair();
            (n, v.to_string())
        };
        assert_eq!(name, "x-mesh-proof");
        let (sender_hex, proof) = value.split_once('.').expect("<sender>.<proof>");
        let sender = NodeId::from_hex(sender_hex).expect("full hex");
        assert_eq!(sender, me);
        assert!(mesh.verify_mesh_proof(proof, sender, 1_000));
    }

    /// A different mesh's secret does not accept it — the stamp proves the
    /// group, so the group has to be the same one.
    #[test]
    fn a_stamp_does_not_verify_against_another_meshs_secret() {
        let mine = mesh_with([3u8; 32]);
        let theirs = mesh_with([4u8; 32]);
        let me = NodeId::from_u128(0xABCD);
        let stamp = mesh_proof_stamp(&mine, me, 1_000).unwrap();
        let (_, value) = stamp.pair();
        let (sender_hex, proof) = value.split_once('.').unwrap();
        let sender = NodeId::from_hex(sender_hex).unwrap();
        assert!(!theirs.verify_mesh_proof(proof, sender, 1_000));
    }

    /// No credential, no stamp — and the caller sends nothing rather than
    /// something the receiver would have to refuse.
    #[test]
    fn a_mesh_with_no_secret_mints_nothing() {
        let mesh = mesh_with(commonwealth_core::mesh::MESH_SECRET_UNSET);
        assert_eq!(mesh_proof_stamp(&mesh, NodeId::from_u128(1), 1_000), None);
    }
}
