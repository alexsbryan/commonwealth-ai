// SPDX-License-Identifier: AGPL-3.0-or-later
//! Node identity keys — the Ed25519 keypair behind
//! [`NodePubkey`](commonwealth_core::ids::NodePubkey).
//!
//! The 32-byte seed persisted at `<data_dir>/node_key` is, byte for
//! byte, a valid iroh `SecretKey`: when the dial-by-key transport
//! lands, THIS file is the node's transport identity — verifying a
//! peer's key and being able to dial it become the same fact. Until
//! then the pubkey rides along in `MemberRecord` so the trust ring
//! is transport-ready.
//!
//! Persistence mirrors `sovereign-mesh::persist::
//! load_or_generate_self_node_id` exactly: tmp-then-rename, 0600 on
//! Unix, graceful fallback to an ephemeral key on I/O errors (the
//! daemon stays usable; identity stability degrades until the file
//! can be written).

use std::fs;
use std::io::Write;
use std::path::Path;

use commonwealth_core::ids::{NodeId, NodePubkey};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};

/// File name under the daemon data dir, sibling of `node_id`.
pub const NODE_KEY_FILE: &str = "node_key";

/// Domain separator for the join-time proof of possession. Signing
/// `domain || node_id || node_name` (rather than a bare challenge)
/// binds the pubkey to the specific identity being admitted, so a
/// captured proof can't be replayed to bind the same key to a
/// different node_id/name.
const JOIN_POP_DOMAIN: &[u8] = b"cwth-join-pubkey-binding:";

/// The pubkey for a signing key, in mesh wire form.
pub fn node_pubkey(key: &SigningKey) -> NodePubkey {
    NodePubkey(key.verifying_key().to_bytes())
}

fn key_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join(NODE_KEY_FILE)
}

fn load_key(data_dir: &Path) -> std::io::Result<Option<SigningKey>> {
    let path = key_path(data_dir);
    match fs::read(&path) {
        Ok(bytes) => {
            let seed: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "node_key at {} is {} bytes, expected 32",
                        path.display(),
                        bytes.len()
                    ),
                )
            })?;
            Ok(Some(SigningKey::from_bytes(&seed)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn save_key(data_dir: &Path, key: &SigningKey) -> std::io::Result<()> {
    fs::create_dir_all(data_dir)?;
    let target = key_path(data_dir);
    let tmp = target.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&key.to_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &target)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Best-effort: failure doesn't invalidate the write, the
        // file is just possibly group-readable.
        let _ = fs::set_permissions(&target, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Load-or-generate wrapper with graceful fallback. First boot
/// writes the seed; subsequent boots return the persisted key. On
/// I/O error writing the generated key, returns the fresh key
/// anyway and logs — the daemon is still usable, the node just
/// presents a different pubkey next session.
pub fn load_or_generate_node_key(data_dir: &Path) -> SigningKey {
    match load_key(data_dir) {
        Ok(Some(key)) => key,
        Ok(None) => {
            let mut seed = [0u8; 32];
            if let Err(e) = getrandom::fill(&mut seed) {
                // getrandom failing is catastrophic enough that an
                // expect matches NodeId::generate's posture.
                panic!("failed to generate node key entropy: {e}");
            }
            let fresh = SigningKey::from_bytes(&seed);
            if let Err(e) = save_key(data_dir, &fresh) {
                tracing::warn!(
                    error = %e,
                    data_dir = %data_dir.display(),
                    "node_key persistence failed — daemon will present a \
                     fresh identity key this session"
                );
            } else {
                tracing::info!(
                    node_pubkey = %node_pubkey(&fresh),
                    "node_key: generated + persisted identity key (first boot)"
                );
            }
            fresh
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "node_key: failed to load persisted key — using fresh this session"
            );
            let mut seed = [0u8; 32];
            getrandom::fill(&mut seed).expect("failed to generate node key entropy");
            SigningKey::from_bytes(&seed)
        }
    }
}

fn join_pop_message(node_id: &NodeId, node_name: &str) -> Vec<u8> {
    let mut msg =
        Vec::with_capacity(JOIN_POP_DOMAIN.len() + node_id.as_bytes().len() + node_name.len());
    msg.extend_from_slice(JOIN_POP_DOMAIN);
    msg.extend_from_slice(node_id.as_bytes());
    msg.extend_from_slice(node_name.as_bytes());
    msg
}

/// Joiner side: sign the proof of possession sent in `JoinRequest`.
/// Hex-encoded 64-byte Ed25519 signature.
pub fn sign_join_proof(key: &SigningKey, node_id: &NodeId, node_name: &str) -> String {
    hex::encode(key.sign(&join_pop_message(node_id, node_name)).to_bytes())
}

/// Sign this node's iroh dial info, hex-encoded (matches the join-proof
/// encoding — serde has no `[u8; 64]` impl). Reuses the canonical message
/// in [`commonwealth_core::dial_sig`] so this signer and the verifier in
/// `Mesh::merge_from` agree byte-for-byte. The gossip self-stamp calls
/// this each time it (re)stamps our reachability.
pub fn sign_dial_info(
    key: &SigningKey,
    version: u64,
    relay_url: Option<&str>,
    direct_addrs: &[std::net::SocketAddr],
) -> String {
    let pubkey = node_pubkey(key);
    let msg =
        commonwealth_core::dial_sig::dial_info_message(&pubkey, version, relay_url, direct_addrs);
    hex::encode(key.sign(&msg).to_bytes())
}

/// Founder side: verify a joiner's proof of possession. `false` on
/// any malformed input (bad hex, wrong lengths, invalid key) — the
/// caller turns that into a loud 401, never a silent admit.
pub fn verify_join_proof(
    pubkey: &NodePubkey,
    node_id: &NodeId,
    node_name: &str,
    proof_hex: &str,
) -> bool {
    let Ok(sig_bytes) = hex::decode(proof_hex) else {
        return false;
    };
    let Ok(sig_arr) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else {
        return false;
    };
    let Ok(verifying) = VerifyingKey::from_bytes(pubkey.as_bytes()) else {
        return false;
    };
    verifying
        .verify(
            &join_pop_message(node_id, node_name),
            &ed25519_dalek::Signature::from_bytes(&sig_arr),
        )
        .is_ok()
}

/// Filename of the client-API bearer token, a sibling of `node_key`
/// under `<data_dir>`.
pub const CLIENT_TOKEN_FILE: &str = "client-token";

fn client_token_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join(CLIENT_TOKEN_FILE)
}

/// Load the persisted client-API bearer token, or generate + persist a
/// fresh one (256-bit, hex-encoded) on first call. Used to authenticate
/// non-loopback callers of `:9741` when the daemon binds a routable
/// address — see `commonwealth_api::client_auth`.
///
/// Mirrors [`load_or_generate_node_key`]'s persistence shape: atomic
/// write via a `.tmp` rename, `0600` on unix. Unlike the node key, the
/// token is a shared secret distributed to mesh peers / remote clients
/// (the symmetric-token tier — node-identity auth is a later milestone),
/// so it is stored in cleartext by design: the daemon must present it
/// verbatim to compare against an incoming `Authorization: Bearer`.
pub fn load_or_create_client_token(data_dir: &Path) -> std::io::Result<String> {
    let path = client_token_path(data_dir);
    match fs::read_to_string(&path) {
        Ok(s) => {
            let token = s.trim().to_string();
            if token.is_empty() {
                // An empty/corrupt file is worse than none — a blank
                // secret would match a blank bearer. Regenerate.
                save_client_token(data_dir)
            } else {
                Ok(token)
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => save_client_token(data_dir),
        Err(e) => Err(e),
    }
}

/// Mint a fresh bearer token: 256 bits of OS entropy, hex-encoded.
///
/// THE definition of what a bearer this daemon accepts looks like, shared by
/// the persisted client token below and by ephemeral guest grants
/// (`commonwealth_knowledge::guest_grant`). Both land in the same
/// `Authorization: Bearer` header and are compared by the same
/// `client_auth_layer`, so two generators would be two answers to one
/// question (ARCH §10.6) — and the weaker one would set the real strength.
pub fn generate_bearer_token() -> std::io::Result<String> {
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw)
        .map_err(|e| std::io::Error::other(format!("bearer-token entropy failed: {e}")))?;
    Ok(hex::encode(raw))
}

fn save_client_token(data_dir: &Path) -> std::io::Result<String> {
    let token = generate_bearer_token()?;

    fs::create_dir_all(data_dir)?;
    let target = client_token_path(data_dir);
    let tmp = target.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(token.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &target)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&target, fs::Permissions::from_mode(0o600));
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    /// covers: FE-3
    #[test]
    fn load_or_generate_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_generate_node_key(dir.path());
        let second = load_or_generate_node_key(dir.path());
        assert_eq!(first.to_bytes(), second.to_bytes(), "same key across boots");
        assert!(dir.path().join(NODE_KEY_FILE).exists());
    }

    #[cfg(unix)]
    #[test]
    fn key_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let _ = load_or_generate_node_key(dir.path());
        let mode = std::fs::metadata(dir.path().join(NODE_KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn corrupt_key_file_falls_back_to_fresh() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(NODE_KEY_FILE), b"way too short").unwrap();
        // Must not panic; returns a usable (ephemeral) key.
        let _ = load_or_generate_node_key(dir.path());
    }

    #[test]
    fn client_token_persists_and_is_stable_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create_client_token(dir.path()).unwrap();
        let second = load_or_create_client_token(dir.path()).unwrap();
        assert_eq!(first, second, "token must be stable across boots");
        assert_eq!(first.len(), 64, "256-bit hex token");
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(dir.path().join(CLIENT_TOKEN_FILE).exists());
    }

    #[test]
    fn empty_token_file_is_regenerated_not_returned_blank() {
        // A blank secret would match a blank bearer — must never be
        // returned as-is.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CLIENT_TOKEN_FILE), b"   \n").unwrap();
        let token = load_or_create_client_token(dir.path()).unwrap();
        assert_eq!(token.len(), 64, "blank file regenerated into a real token");
    }

    #[cfg(unix)]
    #[test]
    fn client_token_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let _ = load_or_create_client_token(dir.path()).unwrap();
        let mode = std::fs::metadata(dir.path().join(CLIENT_TOKEN_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn join_proof_round_trips() {
        let key = test_key();
        let id = NodeId::from_u128(42);
        let proof = sign_join_proof(&key, &id, "Bob's Build");
        assert!(verify_join_proof(
            &node_pubkey(&key),
            &id,
            "Bob's Build",
            &proof
        ));
    }

    /// covers: FE-5
    #[test]
    fn join_proof_binds_node_id_and_name() {
        let key = test_key();
        let id = NodeId::from_u128(42);
        let proof = sign_join_proof(&key, &id, "Bob's Build");
        // Same proof must not bind a different id or name.
        assert!(!verify_join_proof(
            &node_pubkey(&key),
            &NodeId::from_u128(43),
            "Bob's Build",
            &proof
        ));
        assert!(!verify_join_proof(
            &node_pubkey(&key),
            &id,
            "Eve's Build",
            &proof
        ));
        // Or a different key.
        let other = SigningKey::from_bytes(&[9u8; 32]);
        assert!(!verify_join_proof(
            &node_pubkey(&other),
            &id,
            "Bob's Build",
            &proof
        ));
    }

    #[test]
    fn malformed_proofs_are_rejected_not_panicked() {
        let key = test_key();
        let id = NodeId::from_u128(1);
        for bad in ["", "zz", "deadbeef", &"00".repeat(64)] {
            assert!(!verify_join_proof(&node_pubkey(&key), &id, "n", bad));
        }
    }
}
