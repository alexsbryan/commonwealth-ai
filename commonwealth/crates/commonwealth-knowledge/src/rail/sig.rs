// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a ring op is signed over, and the two functions that agree on it.
//!
//! # Why an op is signed at all
//!
//! `Op::actor` is a string the writer supplies, and ring ops arrive over
//! gossip from every node in the ring. A fold that trusted `actor` would be
//! asserting on a field the subject authored — the same defect as a wrong-slot
//! guard reading the `model` an SSE client echoed back (ARCH §18.1). The
//! signature is the part the subject *cannot* author for someone else, so it
//! is what admission actually checks; `actor` is then not a claim but the
//! public key the check ran under.
//!
//! # The layout, and why it is length-prefixed
//!
//! Copied in shape — not in bytes — from [`commonwealth_core::dial_sig`],
//! which is the tree's existing answer to "sign a struct that rides gossip."
//! Two properties carry over:
//!
//! - **A domain separator**, so a ring-op signature can never be replayed as a
//!   dial-info signature or a join proof, and vice versa.
//! - **Length prefixes on every variable field**, so no field boundary can
//!   bleed into the next. Without them `("ab", "c")` and `("a", "bc")` sign
//!   identical bytes, and a payer could be renamed inside a description.
//!
//! One field is ours rather than inherited: **the namespace**. A signed op
//! lifted out of the tool-lending board and replayed into the expense ledger
//! would otherwise verify perfectly. Binding the namespace into the message
//! makes that cross-namespace replay fail the signature rather than fail a
//! check somewhere downstream that someone might forget to write.
//!
//! # What is deliberately NOT signed
//!
//! The [`OpId`](corpus_engine::oplog::OpId) is not in the message, because it
//! is derived from the signature (the id hashes the whole line body, `sig`
//! included). That is not a hole: admission re-derives the id from content
//! and ignores the one on the line, so a rewritten `id` changes nothing and
//! is reported as a gap. Signing it would be circular.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

/// Domain separator. Distinct from `cwth-dial-info-binding:` and from the
/// join proof-of-possession, so no signature is valid in two contexts.
const RING_OP_DOMAIN: &[u8] = b"cwth-ring-op-binding:";

/// The hex form of a node's public key, which is how a ring op names its
/// signer. 64 lowercase hex characters.
///
/// A hex string rather than a `NodePubkey` because it is what lands on the
/// journal line — `Op::actor` is a `String`, and re-parsing it once per
/// verification is cheaper than a second wire representation to keep in sync.
pub fn actor_of(key: &SigningKey) -> String {
    hex::encode(key.verifying_key().to_bytes())
}

fn field(msg: &mut Vec<u8>, bytes: &[u8]) {
    msg.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    msg.extend_from_slice(bytes);
}

/// The canonical, length-prefixed bytes signed and verified for one ring op.
///
/// `DOMAIN || ns || ts[8 BE] || actor || seq[8 BE] || body`, every variable
/// field preceded by its `u32` big-endian length.
///
/// `body` is the JSON of the [`RailAct`](super::RailAct) alone — not the
/// envelope, not `seq`, not `sig`. serde_json emits struct fields in
/// declaration order, and a [`Payload`](super::Payload) is canonical by
/// construction, so the same act produces the same bytes on every node and
/// build. That is the property the whole scheme rests on, and the payload
/// half of it is why `Payload` is a type rather than a `serde_json::Value`.
pub fn ring_op_message(
    namespace: &str,
    ts_unix: i64,
    actor: &str,
    seq: u64,
    body_json: &str,
) -> Vec<u8> {
    let mut msg = Vec::with_capacity(RING_OP_DOMAIN.len() + body_json.len() + 96);
    msg.extend_from_slice(RING_OP_DOMAIN);
    field(&mut msg, namespace.as_bytes());
    msg.extend_from_slice(&ts_unix.to_be_bytes());
    field(&mut msg, actor.as_bytes());
    msg.extend_from_slice(&seq.to_be_bytes());
    field(&mut msg, body_json.as_bytes());
    msg
}

/// Sign one ring op. Hex-encoded 64-byte Ed25519 signature, matching how the
/// join proof and the dial-info signature are already carried (serde has no
/// `[u8; 64]` impl).
pub fn sign_ring_op(
    key: &SigningKey,
    namespace: &str,
    ts_unix: i64,
    seq: u64,
    body_json: &str,
) -> String {
    let actor = actor_of(key);
    let msg = ring_op_message(namespace, ts_unix, &actor, seq, body_json);
    hex::encode(key.sign(&msg).to_bytes())
}

/// Verify one ring op under the public key its `actor` names.
///
/// `false` on ANY malformed input — bad hex, wrong length, a public key that
/// is not on the curve. The caller turns that into a gap, never into a silent
/// admit (ARCH §4.3: unknown-id handling is explicit and loud).
///
/// Verifying under `actor`'s own key is self-certifying and therefore proves
/// nothing on its own — anyone can mint a keypair. It is the
/// [`Roster`](super::Roster) that makes it mean something, by saying which
/// keys belong to people in this ring. Both checks are required and admission
/// runs both.
pub fn verify_ring_op(
    actor: &str,
    namespace: &str,
    ts_unix: i64,
    seq: u64,
    body_json: &str,
    sig_hex: &str,
) -> bool {
    let Ok(key_bytes) = hex::decode(actor) else {
        return false;
    };
    let Ok(key_arr) = <[u8; 32]>::try_from(key_bytes.as_slice()) else {
        return false;
    };
    let Ok(verifying) = VerifyingKey::from_bytes(&key_arr) else {
        return false;
    };
    let Ok(sig_bytes) = hex::decode(sig_hex) else {
        return false;
    };
    let Ok(sig_arr) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else {
        return false;
    };
    let msg = ring_op_message(namespace, ts_unix, actor, seq, body_json);
    verifying
        .verify(&msg, &Signature::from_bytes(&sig_arr))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    #[test]
    fn a_signature_verifies_under_the_key_that_made_it() {
        let k = key(1);
        let sig = sign_ring_op(&k, "house", 100, 1, r#"{"op":"record"}"#);
        assert!(verify_ring_op(
            &actor_of(&k),
            "house",
            100,
            1,
            r#"{"op":"record"}"#,
            &sig
        ));
    }

    /// Every field is bound. Flip any one and the signature must fail —
    /// otherwise that field is forgeable after the fact.
    #[test]
    fn every_signed_field_is_actually_bound() {
        let k = key(1);
        let a = actor_of(&k);
        let body = r#"{"op":"record"}"#;
        let sig = sign_ring_op(&k, "house", 100, 1, body);
        assert!(!verify_ring_op(&a, "lending", 100, 1, body, &sig), "namespace");
        assert!(!verify_ring_op(&a, "house", 101, 1, body, &sig), "ts_unix");
        assert!(!verify_ring_op(&a, "house", 100, 2, body, &sig), "seq");
        assert!(
            !verify_ring_op(&a, "house", 100, 1, r#"{"op":"correct"}"#, &sig),
            "body"
        );
        assert!(
            !verify_ring_op(&actor_of(&key(2)), "house", 100, 1, body, &sig),
            "actor"
        );
    }

    /// The reason for length prefixes, stated as a test: two different
    /// (namespace, actor) splits of the same characters must not sign the
    /// same bytes.
    #[test]
    fn field_boundaries_cannot_bleed() {
        assert_ne!(
            ring_op_message("ab", 1, "cd", 1, "x"),
            ring_op_message("a", 1, "bcd", 1, "x"),
        );
    }

    #[test]
    fn malformed_input_is_false_never_a_panic_and_never_an_admit() {
        let k = key(1);
        let sig = sign_ring_op(&k, "house", 100, 1, "{}");
        let a = actor_of(&k);
        assert!(!verify_ring_op("not-hex", "house", 100, 1, "{}", &sig));
        assert!(!verify_ring_op("ab", "house", 100, 1, "{}", &sig), "short key");
        assert!(!verify_ring_op(&a, "house", 100, 1, "{}", "not-hex"));
        assert!(!verify_ring_op(&a, "house", 100, 1, "{}", "abcd"), "short sig");
        assert!(!verify_ring_op(&a, "house", 100, 1, "{}", ""));
    }

    /// A ring-op signature must not be reusable as a dial-info signature.
    /// The domain separator is the whole defence, so pin that it is present
    /// and different.
    #[test]
    fn the_domain_separator_prefixes_the_message() {
        let msg = ring_op_message("house", 1, "aa", 1, "{}");
        assert!(msg.starts_with(RING_OP_DOMAIN));
        assert_ne!(RING_OP_DOMAIN, b"cwth-dial-info-binding:");
    }
}
