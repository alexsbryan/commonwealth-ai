// SPDX-License-Identifier: AGPL-3.0-or-later
//! The identity family — [`NodeId`], [`CorpusId`], and the [`define_id`] macro
//! that mints the opaque-random shape.
//!
//! The campaign's shape map counted 48 production types ending in `Id`, `Ref`
//! or `Hash` and placed the family here: they are what every domain must be
//! able to name in order to talk about the same thing, and they are the
//! cheapest members to move because an id has no behaviour to relocate.
//!
//! # Why `define_id!` lives at layer 0 and not in the mesh crate
//!
//! It was `commonwealth-core/src/ids.rs`, generating six ids. [`NodeId`] is
//! the one all three domains need — [`Origin::served_by`](crate::Origin)
//! cannot say "a peer served this" without naming a node — but
//! `commonwealth-core` sits three layers up and carries nine dependencies
//! including `ed25519-dalek`, so the kernel cannot reach it. Pulling one id
//! out of a six-member macro family leaves either a duplicated macro or an
//! orphan; moving the MACRO down and leaving the five mesh-specific ids where
//! they are does neither. `commonwealth-core` now invokes this macro for
//! `MeshId`, `ModelId`, `ProcessId`, `PlanId` and `HandoffId`, and re-exports
//! `NodeId` from here, so all 755 existing reference sites are untouched and
//! there is still exactly one implementation (ARCH §10.6).

use serde::{Deserialize, Serialize};
use std::fmt;

/// Implementation detail of [`define_id!`](crate::define_id). Re-exported so a
/// crate invoking the macro needs only `serde` in scope, not `getrandom` and
/// `hex` as well. Not a public API — do not name it.
#[doc(hidden)]
pub mod __private {
    pub use getrandom;
    pub use hex;
}

/// Mint an opaque 128-bit identity newtype with the workspace's wire
/// conventions: full 32-char hex via `to_hex`/`from_hex`, and a
/// `<prefix>-<16 hex>` truncated `Display` for humans.
///
/// The bytes are random, never derived from a row count, a sequence number or
/// a network address (ARCH §7.5). Where identity CAN come from essence, use
/// [`ContentHash`](crate::ContentHash) instead — this macro is for the cases
/// where a thing has no content to hash, such as a node.
///
/// # What the call site must provide
///
/// `serde` only — `getrandom` and `hex` are routed through this crate so a
/// consumer does not inherit them. `commonwealth-core` invokes this for its
/// five mesh-specific ids; `NodeId` itself is defined here because
/// [`Origin::served_by`](crate::Origin) needs it at layer 0.
#[macro_export]
macro_rules! define_id {
    ($name:ident, $prefix:expr) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, ::serde::Serialize, ::serde::Deserialize)]
        pub struct $name([u8; 16]);

        impl $name {
            pub fn generate() -> Self {
                let mut bytes = [0u8; 16];
                $crate::ids::__private::getrandom::fill(&mut bytes)
                    .expect("failed to generate random bytes");
                Self(bytes)
            }

            /// Create an ID from a u128 value. Useful for deterministic test IDs.
            pub fn from_u128(val: u128) -> Self {
                Self(val.to_be_bytes())
            }

            pub fn as_bytes(&self) -> &[u8; 16] {
                &self.0
            }

            /// Full 32-char lowercase hex of all 16 bytes — the wire form used
            /// for `X-Node-Id` and `[shared_model] host_node_id`. (NOT the
            /// truncated `Display`/`Debug` form, which is for humans.)
            pub fn to_hex(&self) -> String {
                $crate::ids::__private::hex::encode(self.0)
            }

            /// Inverse of [`to_hex`](Self::to_hex). `None` on malformed input
            /// (non-hex, or not exactly 16 bytes).
            pub fn from_hex(s: &str) -> Option<Self> {
                let arr: [u8; 16] = $crate::ids::__private::hex::decode(s.trim())
                    .ok()?
                    .try_into()
                    .ok()?;
                Some(Self(arr))
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                write!(
                    f,
                    "{}-{}",
                    $prefix,
                    $crate::ids::__private::hex::encode(&self.0[..8])
                )
            }
        }

        impl ::std::fmt::Debug for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                write!(f, "{}({})", stringify!($name), self)
            }
        }

        impl PartialOrd for $name {
            fn partial_cmp(&self, other: &Self) -> Option<::std::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }

        impl Ord for $name {
            fn cmp(&self, other: &Self) -> ::std::cmp::Ordering {
                self.0.cmp(&other.0)
            }
        }
    };
}

define_id!(NodeId, "node");

/// Which corpus. A slug, not a random id: corpora are named by the human who
/// installs them and the name is the join key across every surface — 6,710
/// occurrences of `corpus_id` across 449 production files at the time this
/// type was minted, every one of them a bare `String` or `&str`.
///
/// Non-empty by construction. That is the whole invariant, and it is a real
/// one: an empty corpus id currently reads as "all corpora" in some call
/// sites and as "no corpus" in others.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CorpusId(String);

impl CorpusId {
    /// `None` on an empty or whitespace-only id — refused rather than
    /// normalised to some default corpus (principle 6).
    pub fn new(id: impl Into<String>) -> Option<Self> {
        let id = id.into();
        if id.trim().is_empty() {
            return None;
        }
        Some(CorpusId(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

// ── [prepare]: the five impls the corpus-id spec declares ────────────────
//
// Error-class count is a property of the TARGET TYPE, not of the codebase.
// Each of these deletes a whole class of E0308/E0277 before a single call
// site is touched, which is the difference between a ~40-line edit here and
// an edit at every one of 6,710 `corpus_id` occurrences. Declared in
// quality/refactors/corpus-id.toml [prepare]; measured on the first order
// (rf-field-atom-kernel-types-corpusid) as 30 errors across two classes.
//
// None of them weakens the invariant: every one hands out the slug that a
// `CorpusId` already proved non-empty at construction. There is deliberately
// no `From<String> for CorpusId` — that direction is FALLIBLE (empty is
// refused), so it stays `CorpusId::new(..) -> Option`, and a caller must say
// what it means by an absent corpus rather than having one defaulted in.

impl AsRef<str> for CorpusId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for CorpusId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for CorpusId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for CorpusId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<CorpusId> for str {
    fn eq(&self, other: &CorpusId) -> bool {
        other.0 == *self
    }
}

impl From<CorpusId> for String {
    fn from(id: CorpusId) -> String {
        id.0
    }
}

impl std::str::FromStr for CorpusId {
    /// The empty id, named. `FromStr` cannot return `Option`, so the refusal
    /// travels as a typed error rather than as a silently-defaulted value.
    type Err = EmptyCorpusId;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        CorpusId::new(s).ok_or(EmptyCorpusId)
    }
}

/// A corpus id was empty or whitespace-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmptyCorpusId;

impl fmt::Display for EmptyCorpusId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("corpus id is empty")
    }
}

impl std::error::Error for EmptyCorpusId {}

impl fmt::Display for CorpusId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for CorpusId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CorpusId({:?})", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_wire_form_is_unchanged_by_the_move() {
        // Pinned against commonwealth-core's behaviour before the macro
        // moved down. 755 reference sites and the `X-Node-Id` header depend
        // on these three exact shapes; if this test fails, the move was a
        // wire break rather than a relocation.
        let id = NodeId::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
        assert_eq!(id.to_hex(), "0123456789abcdef0123456789abcdef");
        assert_eq!(id.to_string(), "node-0123456789abcdef");
        assert_eq!(format!("{id:?}"), "NodeId(node-0123456789abcdef)");
        assert_eq!(NodeId::from_hex(&id.to_hex()), Some(id));
    }

    #[test]
    fn node_id_serialises_as_a_16_byte_array_not_a_string() {
        // The derive puts a tuple struct of [u8;16] on the wire as an array.
        // Stored mesh records depend on it; a switch to hex here would be a
        // silent data break. Asserted through the ONE wire decider
        // (`crate::wire`, §10.6) — the same computation the refactor
        // factory's differ runs, so this test and that gate cannot drift.
        let id = NodeId::from_u128(1);
        let f = crate::wire::WireFixture::json(&id.to_string(), &id).unwrap();
        assert!(
            !f.is_transparent(),
            "adopting NodeId at a String site must NOT be wire-transparent"
        );
        assert!(
            f.after.starts_with("[0,0,0"),
            "unexpected wire form: {}",
            f.after
        );
    }

    #[test]
    fn generate_does_not_repeat() {
        assert_ne!(NodeId::generate(), NodeId::generate());
    }

    #[test]
    fn from_hex_refuses_malformed_input() {
        assert_eq!(NodeId::from_hex("nonsense"), None);
        assert_eq!(NodeId::from_hex("00"), None);
    }

    #[test]
    fn an_empty_corpus_id_is_refused_not_defaulted() {
        assert_eq!(CorpusId::new(""), None);
        assert_eq!(CorpusId::new("   "), None);
        assert!(CorpusId::new("wikipedia").is_some());
    }

    #[test]
    fn corpus_id_is_transparent_on_the_wire() {
        // Existing rows persist `corpus_id` as a plain string; adopting the
        // newtype must not be a data migration. Asserted through the ONE
        // wire decider (`crate::wire`, §10.6).
        let c = CorpusId::new("wikipedia").unwrap();
        let f = crate::wire::WireFixture::json(c.as_str(), &c).unwrap();
        assert!(f.is_transparent(), "{} != {}", f.before, f.after);
        assert_eq!(f.before, "\"wikipedia\"");
    }
}
