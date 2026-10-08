// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`Sha256Hash`] — the published identity of some bytes.
//!
//! [`ContentHash`](crate::ContentHash) is BLAKE3 and internal: it keys chunk
//! rows and atoms, and nothing outside this workspace is promised its value.
//! What the platform PUBLISHES — a stored text's name, the bytes an extractor
//! read — is SHA-256, the value space the asset store and ATTESTED_RECORDS'
//! `DocId` already use. The two are different value spaces and nothing
//! converts between them: to get one from the other, rehash the bytes.
//!
//! One encoding: 64 lowercase hex characters. A string in any other shape —
//! uppercase, `sha256:`-prefixed, truncated, padded — is refused, so two
//! spellings can never name the same bytes. [`Sha256Hash::to_ni`] is the
//! RFC 6920 rendering, `ni:///sha-256;<base64url, no padding>`, for a reader
//! that wants a URI; it is a rendering of the same value, not a second name.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::fmt;

/// The SHA-256 digest of some bytes, published as 64 lowercase hex.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Sha256Hash([u8; 32]);

impl Sha256Hash {
    /// Hash bytes. The one place the platform computes a published identity.
    pub fn of(bytes: &[u8]) -> Self {
        Sha256Hash(Sha256::digest(bytes).into())
    }

    /// Hash text as its UTF-8 bytes; identical to `of(s.as_bytes())`.
    pub fn of_str(s: &str) -> Self {
        Sha256Hash::of(s.as_bytes())
    }

    /// Adopt a digest computed elsewhere — `from_bytes`, not `new`, so a
    /// reader sees that no hashing happened here.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Sha256Hash(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The one encoding: 64 lowercase hex characters.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parse the one encoding. `None` on anything that is not exactly 64
    /// lowercase hex characters (no trimming, no case folding, no prefix).
    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 64 || !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        let arr: [u8; 32] = hex::decode(s).ok()?.try_into().ok()?;
        Some(Sha256Hash(arr))
    }

    /// RFC 6920 named-information URI: `ni:///sha-256;<base64url-nopad>`.
    pub fn to_ni(&self) -> String {
        format!("ni:///sha-256;{}", URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl fmt::Display for Sha256Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Sha256Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sha256Hash({})", &self.to_hex()[..16])
    }
}

impl Serialize for Sha256Hash {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Sha256Hash {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Sha256Hash::from_hex(&s).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "expected 64 lowercase hex characters (SHA-256), got {s:?}"
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_published_sha256_vector() {
        // FIPS 180-2 appendix B.1.
        assert_eq!(
            Sha256Hash::of(b"abc").to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn ni_rendering_matches_rfc_6920() {
        // RFC 6920 section 8.1's example, "Hello World!".
        assert_eq!(
            Sha256Hash::of_str("Hello World!").to_ni(),
            "ni:///sha-256;f4OxZX_x_FO5LcGBSKHWXfwtSx-j1ncoSt3SABJtkGk"
        );
    }

    #[test]
    fn hex_round_trips() {
        let h = Sha256Hash::of_str("a text");
        assert_eq!(Sha256Hash::from_hex(&h.to_hex()), Some(h));
    }

    #[test]
    fn any_second_spelling_is_refused() {
        let hex = Sha256Hash::of_str("x").to_hex();
        assert_eq!(Sha256Hash::from_hex(&hex.to_uppercase()), None);
        assert_eq!(Sha256Hash::from_hex(&format!("sha256:{hex}")), None);
        assert_eq!(Sha256Hash::from_hex(&format!(" {hex}")), None);
        assert_eq!(Sha256Hash::from_hex(&hex[..63]), None);
    }

    #[test]
    fn is_not_the_blake3_value_space() {
        assert_ne!(
            Sha256Hash::of(b"hello").to_hex(),
            crate::ContentHash::of(b"hello").to_hex()
        );
    }

    #[test]
    fn serde_wire_form_is_the_hex_string() {
        let h = Sha256Hash::of_str("wire");
        let f = crate::wire::WireFixture::json(&h.to_hex(), &h).unwrap();
        assert!(f.is_transparent(), "{} != {}", f.before, f.after);
        assert!(serde_json::from_str::<Sha256Hash>("\"not-a-hash\"").is_err());
    }
}
