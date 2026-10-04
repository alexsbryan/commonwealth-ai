// SPDX-License-Identifier: AGPL-3.0-or-later
//! The wire-form fixture: does adopting a typed value at a `String` site
//! change the bytes on the wire?
//!
//! # Why this is one module and not three tests
//!
//! `ContentHash` carried a hand-written version of this question
//! (`serde_wire_form_is_a_plain_hex_string`), `CorpusId` carried another
//! (`corpus_id_is_transparent_on_the_wire`), and `NodeId` carried the
//! negative print (`node_id_serialises_as_a_16_byte_array_not_a_string`).
//! Three implementations of one decider is the §10.6 smell, and the refactor
//! factory's stage-6 encoding gate (`quality/REFACTOR_FACTORY.md`) needs the
//! same computation at runtime, not just under test cfg. So the decider
//! lives here, once; the three tests and the factory's wire differ
//! (`sovereign-cli-dev/src/refactor_wire.rs`) all call it.
//!
//! # What the compiler cannot see
//!
//! The reason this exists at all: **rustc is exhaustive over types and blind
//! to encoding.** `node_id: String -> NodeId` compiles clean while turning
//! `"node_id":"node-6c955b5f1361aaaa"` into `"node_id":[108,149,…]` on live
//! mesh endpoints — derived serde on a `[u8; 16]` tuple struct is an integer
//! array. This module is the instrument that makes that divergence a
//! first-class value instead of a production incident.
//!
//! Compiled under `cfg(any(test, feature = "wire-fixture"))` so the kernel's
//! default build keeps its four-dep budget; `serde_json` is an optional dep
//! that only the fixture gate pays for.

use serde::{de::DeserializeOwned, Serialize};

/// The two byte strings the adoption question is about. Public fields on
/// purpose: the bytes ARE the evidence, and a gate that hides them is a
/// verdict without a reason (principle 1 — glassbox).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireFixture {
    /// The exact JSON bytes a bare `String` site puts on the wire today.
    pub before: String,
    /// The exact JSON bytes the typed value puts on the wire.
    pub after: String,
}

/// Why a fixture could not be built. Distinct from divergence — a fixture
/// that could not be built proves nothing in either direction, and the
/// caller must report that as could-not-judge or failed, never as a pass
/// (ARCH §18.2, §18.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireFixtureError {
    /// Serialisation itself failed; there are no after-bytes to judge.
    Serialize(String),
    /// The typed value does not survive its own wire form. The after-bytes
    /// exist and are carried so the failure is inspectable.
    RoundTrip { after: String, detail: String },
}

impl std::fmt::Display for WireFixtureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireFixtureError::Serialize(e) => write!(f, "serialisation failed: {e}"),
            WireFixtureError::RoundTrip { after, detail } => {
                write!(
                    f,
                    "value does not round-trip through its wire form {after}: {detail}"
                )
            }
        }
    }
}

impl WireFixture {
    /// THE decider (§10.6 — one implementation): the JSON wire form of
    /// `string_form` as a bare `String` site serialises it today, next to the
    /// JSON wire form of `typed`, with `typed` proven to round-trip through
    /// its own bytes. Byte-identical means adoption is a type change, not a
    /// data migration; anything else means adopting the type REWRITES the
    /// wire.
    pub fn json<T>(string_form: &str, typed: &T) -> Result<WireFixture, WireFixtureError>
    where
        T: Serialize + DeserializeOwned + PartialEq,
    {
        let before = serde_json::to_string(string_form)
            .map_err(|e| WireFixtureError::Serialize(e.to_string()))?;
        let after =
            serde_json::to_string(typed).map_err(|e| WireFixtureError::Serialize(e.to_string()))?;
        match serde_json::from_str::<T>(&after) {
            Ok(back) if back == *typed => {}
            Ok(_) => {
                return Err(WireFixtureError::RoundTrip {
                    after,
                    detail: "deserialised cleanly but to a DIFFERENT value".to_string(),
                })
            }
            Err(e) => {
                return Err(WireFixtureError::RoundTrip {
                    after,
                    detail: e.to_string(),
                })
            }
        }
        Ok(WireFixture { before, after })
    }

    /// `true` when the typed value is byte-identical to the string it
    /// replaces — the adoption changes nothing on the wire.
    pub fn is_transparent(&self) -> bool {
        self.before == self.after
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Deserializer};

    /// A codec that refuses its own wire form: serialises fine, always
    /// errors on deserialise. The round-trip guard exists for this shape —
    /// an encoding that can write bytes nothing can read back.
    #[derive(Debug, PartialEq, Serialize)]
    struct WriteOnly(String);

    impl<'de> Deserialize<'de> for WriteOnly {
        fn deserialize<D: Deserializer<'de>>(_: D) -> Result<Self, D::Error> {
            Err(serde::de::Error::custom("write-only codec"))
        }
    }

    #[test]
    fn a_transparent_newtype_reads_transparent() {
        let f = WireFixture::json("wikipedia", &"wikipedia".to_string()).unwrap();
        assert!(f.is_transparent());
        assert_eq!(f.before, "\"wikipedia\"");
    }

    #[test]
    fn diverging_bytes_are_carried_not_collapsed() {
        // A String site holding "1" against a typed value serialising as 1.
        let f = WireFixture::json("1", &1_u32).unwrap();
        assert!(!f.is_transparent());
        assert_eq!(f.before, "\"1\"");
        assert_eq!(f.after, "1");
    }

    /// A codec that refuses to serialise at all. (`f64::NAN` is NOT this
    /// case — serde_json writes it as `null`, which the round-trip guard
    /// catches instead; observed 2026-08-23.)
    #[derive(Debug, PartialEq, Deserialize)]
    struct ReadOnly;

    impl Serialize for ReadOnly {
        fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("read-only codec"))
        }
    }

    #[test]
    fn a_value_that_cannot_serialise_is_an_error_not_a_fixture() {
        // No after-bytes exist, so nothing can be judged.
        let err = WireFixture::json("x", &ReadOnly).unwrap_err();
        assert!(matches!(err, WireFixtureError::Serialize(_)), "{err}");
    }

    #[test]
    fn nan_reaches_the_round_trip_guard_not_the_serialiser() {
        // serde_json serialises NaN as `null`; the after-bytes exist but
        // cannot be read back as f64. The guard that fires must say so.
        let err = WireFixture::json("x", &f64::NAN).unwrap_err();
        assert!(
            matches!(err, WireFixtureError::RoundTrip { ref after, .. } if after == "null"),
            "{err}"
        );
    }

    #[test]
    fn a_value_that_cannot_round_trip_is_an_error_not_a_fixture() {
        let err = WireFixture::json("x", &WriteOnly("x".into())).unwrap_err();
        match err {
            WireFixtureError::RoundTrip { after, detail } => {
                assert_eq!(after, "\"x\"");
                assert!(detail.contains("write-only codec"), "{detail}");
            }
            other => panic!("expected RoundTrip, got {other}"),
        }
    }
}
