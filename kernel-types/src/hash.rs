// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`ContentHash`] — identity from essence (ARCH §7.5).
//!
//! # Why this type exists
//!
//! Measured 2026-08-20 across the workspace: content hashes were carried as a
//! bare `String` in FOUR mutually incompatible encodings — full 64-hex BLAKE3,
//! 16-hex-prefix BLAKE3, 16-hex-prefix SHA-256, and `sha256:`-prefixed 64-hex
//! — produced by TWO algorithms. A field named `content_hash` in one crate
//! could not be safely compared against one from another, and nothing in the
//! type system said so. Alongside that: `short_hash` was implemented verbatim
//! three times (`enrichment/atlas/atoms.rs`, `enrichment/governance.rs`,
//! `enrichment/code_intel/mod.rs`), each with a comment naming the others.
//!
//! This is the one decider (ARCH §10.6). One algorithm, one encoding, one
//! implementation of the truncated form.
//!
//! # The algorithm is not a parameter
//!
//! `ContentHash` is **always BLAKE3-256**. There is deliberately no algorithm
//! field and no `Sha256` variant: a hash whose algorithm is data is a hash two
//! values of which may be equal-as-bytes and different-as-content, which is
//! precisely the defect above wearing a type. Content hashed under SHA-256
//! elsewhere in the tree is a different value space and must not be converted
//! into this type — rehash the content instead.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// The BLAKE3-256 digest of some content — its identity, derived from what it
/// IS rather than from where it sits or when it arrived (ARCH §7.5: identity
/// from essence, never a counter or an address).
///
/// # Wire form
///
/// Serializes as a 64-character lowercase hex **string**, not as a byte array.
/// That is deliberate and load-bearing: the tree already persists this value
/// as `content_hash TEXT` in SQLite and as `content_hash: String` in serde
/// rows, so a stored row round-trips into this type without a migration.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// Hash content. **The** implementation — every other content-hash site in
    /// the workspace should reach this one rather than calling `blake3`
    /// directly, so the encoding cannot diverge again.
    pub fn of(content: &[u8]) -> Self {
        ContentHash(*blake3::hash(content).as_bytes())
    }

    /// Hash text. Convenience for the overwhelmingly common case; identical to
    /// `ContentHash::of(s.as_bytes())`.
    pub fn of_str(s: &str) -> Self {
        ContentHash::of(s.as_bytes())
    }

    /// Adopt a digest computed elsewhere. Named `from_bytes` rather than
    /// `new` so a reader sees that no hashing happened here and asks where it
    /// did.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        ContentHash(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Full 64-character lowercase hex — the canonical wire and storage form.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parse the canonical form. `None` on anything that is not exactly 64 hex
    /// characters — a truncated `short()` value is REFUSED rather than
    /// zero-extended, because silently accepting it would re-create the
    /// compare-across-encodings defect this type exists to remove
    /// (principle 6: absence is reported, never defaulted).
    pub fn from_hex(s: &str) -> Option<Self> {
        let arr: [u8; 32] = hex::decode(s.trim()).ok()?.try_into().ok()?;
        Some(ContentHash(arr))
    }

    /// The 16-character prefix — 64-bit truncation, safe for <10M items per
    /// corpus by the birthday bound. This is the ONE implementation of a
    /// convention that existed verbatim in three modules.
    ///
    /// A `short()` value is a display and bucketing key, never an identity:
    /// it does not parse back via [`from_hex`](Self::from_hex), on purpose.
    pub fn short(&self) -> String {
        hex::encode(&self.0[..8])
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for ContentHash {
    /// Truncated for humans; [`Display`](fmt::Display) is the full value.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContentHash({})", self.short())
    }
}

impl Serialize for ContentHash {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        ContentHash::from_hex(&s).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "expected 64 hex characters (BLAKE3-256), got {:?}",
                s
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let h = ContentHash::of_str("the quick brown fox");
        assert_eq!(ContentHash::from_hex(&h.to_hex()), Some(h));
    }

    #[test]
    fn to_hex_is_64_lowercase_hex() {
        let h = ContentHash::of_str("x");
        assert_eq!(h.to_hex().len(), 64);
        assert!(h
            .to_hex()
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    #[test]
    fn matches_the_encoding_corpus_engine_already_writes() {
        // corpus-engine/src/engine/mod.rs:2732 is
        //   `blake3::hash(s.as_bytes()).to_hex().to_string()`
        // Pinning it here means adopting ContentHash on an existing
        // `content_hash: String` column is a type change, not a data
        // migration. If this test ever fails, stored rows have been orphaned.
        let expected = blake3::hash(b"hello").to_hex().to_string();
        assert_eq!(ContentHash::of(b"hello").to_hex(), expected);
    }

    #[test]
    fn short_is_the_16_char_prefix_three_modules_hand_rolled() {
        // enrichment/atlas/atoms.rs, enrichment/governance.rs and
        // enrichment/code_intel/mod.rs each computed `full[..16]`.
        let h = ContentHash::of_str("atom");
        assert_eq!(h.short().len(), 16);
        assert_eq!(h.short(), h.to_hex()[..16]);
    }

    #[test]
    fn a_truncated_hash_is_refused_not_zero_extended() {
        let h = ContentHash::of_str("anything");
        assert_eq!(ContentHash::from_hex(&h.short()), None);
    }

    #[test]
    fn a_sha256_hex_string_is_not_silently_adopted_as_blake3() {
        // Same width, different value space. from_hex CANNOT detect this —
        // which is exactly why `of()` is the only way to mint one from
        // content, and why the doc forbids converting a SHA-256 digest.
        // What this test pins is the weaker, checkable claim: BLAKE3 and
        // SHA-256 of the same input do not collide, so a mixed store is
        // detectable as garbage rather than as agreement.
        let blake = ContentHash::of(b"hello").to_hex();
        let sha256_of_hello = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert_ne!(blake, sha256_of_hello);
    }

    #[test]
    fn serde_wire_form_is_a_plain_hex_string() {
        // Asserted through the ONE wire decider (`crate::wire`, §10.6) —
        // this was the hand-written original the module generalises.
        let h = ContentHash::of_str("wire");
        let f = crate::wire::WireFixture::json(&h.to_hex(), &h).unwrap();
        assert!(f.is_transparent(), "{} != {}", f.before, f.after);
        assert_eq!(f.after, format!("\"{}\"", h.to_hex()));
    }

    #[test]
    fn deserializing_a_non_hash_string_errors_rather_than_defaulting() {
        assert!(serde_json::from_str::<ContentHash>("\"not-a-hash\"").is_err());
    }
}
