// SPDX-License-Identifier: AGPL-3.0-or-later
//! Signed iroh dial-info — tamper-evidence for the mutable reachability
//! fields (`relay_url` + `iroh_direct_addrs`) on a [`MemberRecord`].
//!
//! Those fields ride last-writer-wins gossip, so without a signature a
//! peer past the `join_key_hash` auth boundary could publish a
//! forged-newer record that strips or substitutes another node's dial
//! info — forcing it unreachable (DoS) or, on a non-required class, a
//! downgrade. Signing binds the dial info to the OWNING node's key:
//! only the holder of the private key can change its own reachability.
//!
//! The canonical message lives here (the lowest crate) so the signer in
//! `commonwealth-transport` and the verifier in [`crate::mesh::Mesh::merge_from`]
//! agree byte-for-byte. A monotonic `dial_info_version` rides alongside
//! so a replayed OLDER signed record loses the version comparison on
//! merge — closing the rollback hole a bare signature would leave open.

use std::net::SocketAddr;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use crate::ids::NodePubkey;

/// Domain separator: binds a signature to the "dial info" context so it
/// can never be cross-protocol replayed (e.g. as a join proof).
const DIAL_INFO_DOMAIN: &[u8] = b"cwth-dial-info-binding:";

/// Canonical, length-prefixed byte layout that is signed/verified for a
/// node's dial info. Layout:
///
/// `DOMAIN || pubkey[32] || version[8 BE] || relay(flag[1] +
/// (len[4 BE] + utf8)?) || addrs(count[4 BE] + (len[4 BE] + utf8)*)`
///
/// The address list is SORTED + deduped on its string form, so the bytes
/// are independent of `Vec` order. Length prefixes prevent any field
/// boundary from bleeding into the next (a stronger version of the bare
/// concatenation used by the join proof of possession).
pub fn dial_info_message(
    node_pubkey: &NodePubkey,
    version: u64,
    relay_url: Option<&str>,
    direct_addrs: &[SocketAddr],
) -> Vec<u8> {
    let mut msg = Vec::with_capacity(DIAL_INFO_DOMAIN.len() + 64);
    msg.extend_from_slice(DIAL_INFO_DOMAIN);
    msg.extend_from_slice(node_pubkey.as_bytes());
    msg.extend_from_slice(&version.to_be_bytes());
    match relay_url {
        Some(r) => {
            msg.push(1);
            msg.extend_from_slice(&(r.len() as u32).to_be_bytes());
            msg.extend_from_slice(r.as_bytes());
        }
        None => msg.push(0),
    }
    let mut addrs: Vec<String> = direct_addrs.iter().map(|a| a.to_string()).collect();
    addrs.sort();
    addrs.dedup();
    msg.extend_from_slice(&(addrs.len() as u32).to_be_bytes());
    for a in addrs {
        msg.extend_from_slice(&(a.len() as u32).to_be_bytes());
        msg.extend_from_slice(a.as_bytes());
    }
    msg
}

/// Verify a dial-info signature under `node_pubkey`. Returns `false` on
/// ANY malformed input (bad key bytes) — callers treat that as
/// "unsigned / untrusted", never as verified. The caller is responsible
/// for the monotonic `version >= existing` rollback check.
pub fn verify_dial_info(
    node_pubkey: &NodePubkey,
    version: u64,
    relay_url: Option<&str>,
    direct_addrs: &[SocketAddr],
    sig: &[u8; 64],
) -> bool {
    let Ok(vk) = VerifyingKey::from_bytes(node_pubkey.as_bytes()) else {
        return false;
    };
    let signature = Signature::from_bytes(sig);
    vk.verify(
        &dial_info_message(node_pubkey, version, relay_url, direct_addrs),
        &signature,
    )
    .is_ok()
}

/// Hex convenience for [`verify_dial_info`] — the wire form stores the
/// 64-byte signature hex-encoded (serde has no impl for `[u8; 64]`, and
/// this matches the join proof's hex encoding). `false` on bad hex or
/// wrong length, never a panic.
pub fn verify_dial_info_hex(
    node_pubkey: &NodePubkey,
    version: u64,
    relay_url: Option<&str>,
    direct_addrs: &[SocketAddr],
    sig_hex: &str,
) -> bool {
    let Ok(bytes) = hex::decode(sig_hex) else {
        return false;
    };
    let Ok(arr) = <[u8; 64]>::try_from(bytes.as_slice()) else {
        return false;
    };
    verify_dial_info(node_pubkey, version, relay_url, direct_addrs, &arr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[5u8; 32])
    }

    fn sign(
        k: &SigningKey,
        version: u64,
        relay: Option<&str>,
        addrs: &[SocketAddr],
    ) -> [u8; 64] {
        let pk = NodePubkey(k.verifying_key().to_bytes());
        k.sign(&dial_info_message(&pk, version, relay, addrs))
            .to_bytes()
    }

    #[test]
    fn sign_then_verify_round_trips() {
        let k = key();
        let pk = NodePubkey(k.verifying_key().to_bytes());
        let addrs: Vec<SocketAddr> = vec!["10.0.0.5:9742".parse().unwrap()];
        let sig = sign(&k, 3, Some("https://relay.example./"), &addrs);
        assert!(verify_dial_info(
            &pk,
            3,
            Some("https://relay.example./"),
            &addrs,
            &sig
        ));
    }

    #[test]
    fn addr_order_does_not_affect_signature() {
        let k = key();
        let pk = NodePubkey(k.verifying_key().to_bytes());
        let a: Vec<SocketAddr> = vec!["10.0.0.5:9742".parse().unwrap(), "10.0.0.6:9742".parse().unwrap()];
        let b: Vec<SocketAddr> = vec!["10.0.0.6:9742".parse().unwrap(), "10.0.0.5:9742".parse().unwrap()];
        let sig = sign(&k, 1, None, &a);
        // Same set in a different order still verifies (canonical sort).
        assert!(verify_dial_info(&pk, 1, None, &b, &sig));
    }

    #[test]
    fn tampering_is_rejected() {
        let k = key();
        let pk = NodePubkey(k.verifying_key().to_bytes());
        let addrs: Vec<SocketAddr> = vec!["10.0.0.5:9742".parse().unwrap()];
        let sig = sign(&k, 3, Some("https://relay.example./"), &addrs);

        // Wrong version.
        assert!(!verify_dial_info(&pk, 4, Some("https://relay.example./"), &addrs, &sig));
        // Stripped relay.
        assert!(!verify_dial_info(&pk, 3, None, &addrs, &sig));
        // Substituted addr.
        let other: Vec<SocketAddr> = vec!["10.0.0.99:9742".parse().unwrap()];
        assert!(!verify_dial_info(&pk, 3, Some("https://relay.example./"), &other, &sig));
        // Different key.
        let attacker = SigningKey::from_bytes(&[9u8; 32]);
        let apk = NodePubkey(attacker.verifying_key().to_bytes());
        assert!(!verify_dial_info(&apk, 3, Some("https://relay.example./"), &addrs, &sig));
    }

    #[test]
    fn malformed_inputs_return_false_not_panic() {
        // An all-zero pubkey is not a valid Ed25519 point.
        let pk = NodePubkey([0u8; 32]);
        assert!(!verify_dial_info(&pk, 1, None, &[], &[0u8; 64]));
    }
}
