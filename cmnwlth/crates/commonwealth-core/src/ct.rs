// SPDX-License-Identifier: AGPL-3.0-or-later
//! Constant-time comparison, in one place.
//!
//! There were three implementations of this in the workspace and they were
//! byte-identical in behaviour: `mesh::ct_eq` for the 32-byte gossip
//! credential, an inline XOR fold inside `Mesh::verify_mesh_proof` for the
//! hex proof, and `client_auth::constant_time_eq` for the bearer token.
//! Three copies of a security primitive is three places to fix a mistake in
//! and two places to forget (ARCH §10.6). `commonwealth-api` already depends
//! on `commonwealth-core`, so there was never a layering reason for the
//! duplicate.

/// Compare two byte slices in time independent of WHERE they differ.
///
/// Unequal lengths short-circuit: length is not the secret here — the token
/// width and the digest width are both fixed and public — and folding over
/// mismatched lengths would need a padding rule that leaks its own signal.
/// Equal lengths fold an XOR accumulator over every byte, so the running time
/// does not depend on the position of the first differing byte.
///
/// Deliberately not `==`, which short-circuits at the first mismatch and hands
/// an attacker a byte-at-a-time oracle on the secret.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_only_identical_bytes() {
        assert!(constant_time_eq(b"abc123", b"abc123"));
        assert!(!constant_time_eq(b"abc123", b"abc124"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"x"));
        assert!(constant_time_eq(b"", b""));
    }

    /// The fold must not stop early: two inputs differing only in the LAST
    /// byte are as unequal as two differing in the first.
    #[test]
    fn a_difference_in_the_final_byte_still_fails() {
        let a = [7u8; 32];
        let mut b = a;
        b[31] = 8;
        assert!(!constant_time_eq(&a, &b));
    }
}
