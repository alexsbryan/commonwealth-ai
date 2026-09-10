// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`ActorKey`] — who signed an act, in the one spelling consent is checked in.
//!
//! On the rail the actor IS the signing key: `AdmittedOp::actor` is
//! `hex::encode(verifying_key.to_bytes())`, 64 lowercase hex characters, and
//! it is the only field on a journal line a writer cannot forge for somebody
//! else (ARCH §18.1). The roster binds that key to a [`Person`]; nothing else
//! about the writer is trustworthy.
//!
//! # Why this is a type and not a `String`
//!
//! Consent on this plane is a set-membership test:
//! `Submit.allowed` ∩ `Offer.accept_from` is the grant, and both sides are
//! compared against `AdmittedOp::actor`. An uppercase spelling, a `0x` prefix
//! or a trailing newline in a hand-written roster or config file would make
//! that test silently never match — a donor that quietly takes no work, with
//! nothing red anywhere. Two spellings of one identity is exactly what §7.5
//! forbids, so the strictness lives in the constructor rather than in a
//! reviewer's head.
//!
//! `WorkOffer::accept_from` is `Vec<String>` in `oicp-types` because that leaf
//! cannot name a rail; this is the type it binds to on the way in.

use commonwealth_rail_core::AdmittedOp;
use serde::{Deserialize, Serialize};

/// The number of hex characters in an Ed25519 verifying key.
const KEY_HEX_LEN: usize = 64;

/// A rail actor: the lowercase-hex Ed25519 public key that signed an act.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ActorKey(String);

impl ActorKey {
    /// Parse the canonical spelling, or say which rule it broke.
    ///
    /// Surrounding whitespace is trimmed — a key pasted into a roster file
    /// carries a newline and that is not a different key. Case is NOT
    /// normalised: `hex::encode` emits lowercase and accepting uppercase
    /// would mean two `ActorKey`s that are the same actor and compare
    /// unequal, which is the defect this type removes.
    pub fn parse(raw: impl AsRef<str>) -> Result<ActorKey, InvalidActorKey> {
        let s = raw.as_ref().trim();
        if s.is_empty() {
            return Err(InvalidActorKey::Empty);
        }
        if s.len() != KEY_HEX_LEN {
            return Err(InvalidActorKey::WrongLength(s.len()));
        }
        if !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(InvalidActorKey::NotLowercaseHex);
        }
        Ok(ActorKey(s.to_string()))
    }

    /// The key of whoever signed this admitted op.
    ///
    /// Admission has already verified the signature and placed the key in the
    /// roster, so this only fails on a build that admitted something this one
    /// cannot spell — reported by the caller as an unreadable line, never
    /// panicked on.
    pub fn of_op(op: &AdmittedOp) -> Result<ActorKey, InvalidActorKey> {
        ActorKey::parse(&op.actor)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ActorKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<ActorKey> for String {
    fn from(key: ActorKey) -> String {
        key.0
    }
}

impl TryFrom<String> for ActorKey {
    type Error = InvalidActorKey;
    fn try_from(raw: String) -> Result<ActorKey, InvalidActorKey> {
        ActorKey::parse(raw)
    }
}

/// Why a string is not an actor key. One variant per rule, so a refusal names
/// the fix rather than saying "invalid".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidActorKey {
    #[error("an actor key is empty — it is the 64-hex-character Ed25519 public key that signs this ring's acts, as `svrn ring roster` prints it")]
    Empty,
    #[error("an actor key is 64 hex characters and this one is {0} — a truncated or display-shortened key is refused rather than extended, because a prefix is not an identity")]
    WrongLength(usize),
    #[error("an actor key is LOWERCASE hex — an uppercase or `0x`-prefixed spelling is a second name for one actor, and consent here is a set-membership test that would silently never match")]
    NotLowercaseHex,
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29";

    #[test]
    fn a_roster_pasted_key_keeps_its_identity_across_a_trailing_newline() {
        assert_eq!(ActorKey::parse(format!("{KEY}\n")).unwrap().as_str(), KEY);
    }

    /// The failing input: the SAME key, uppercased. Accepting it would put two
    /// `ActorKey`s that are one actor on either side of an `allowed` check.
    #[test]
    fn an_uppercase_key_is_refused_rather_than_folded_to_lowercase() {
        assert_eq!(
            ActorKey::parse(KEY.to_uppercase()),
            Err(InvalidActorKey::NotLowercaseHex)
        );
    }

    /// The failing input: a `short()`-style 16-char prefix, which is what a
    /// human copies out of a log line.
    #[test]
    fn a_display_shortened_key_is_refused_rather_than_extended() {
        assert_eq!(
            ActorKey::parse(&KEY[..16]),
            Err(InvalidActorKey::WrongLength(16))
        );
    }

    #[test]
    fn the_wire_form_is_the_bare_hex_string() {
        let key = ActorKey::parse(KEY).unwrap();
        assert_eq!(serde_json::to_value(&key).unwrap(), serde_json::json!(KEY));
        let back: ActorKey = serde_json::from_value(serde_json::json!(KEY)).unwrap();
        assert_eq!(back, key);
    }

    #[test]
    fn deserialization_runs_the_same_rules_as_parse() {
        let err = serde_json::from_value::<ActorKey>(serde_json::json!("nope")).unwrap_err();
        assert!(err.to_string().contains("64 hex characters"), "{err}");
    }
}
