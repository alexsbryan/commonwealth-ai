// SPDX-License-Identifier: AGPL-3.0-or-later
//! A guest attestation — a roster member's signed word that, for a while,
//! somebody the ring holds no key for may write under a name.
//!
//! [`SignedOp::on_behalf_of`](crate::SignedOp::on_behalf_of) already carries a
//! name "this door says so". This is what makes the door's saying so
//! checkable by a DIFFERENT process: the door (the daemon's guest pages)
//! signs one of these per guest session, and the journal's writer verifies it
//! before it honours the name (decision five-programs-34).
//!
//! **The roster is the one trust rule.** An attestation is honoured when its
//! signer is a key some member of the namespace's [`Roster`] signs with —
//! the meaning `on_behalf_of` already documents, one decider reused. There is
//! no allow-list and no config key to disagree with it.
//!
//! Pure like the rest of the fold: the clock is the `now` parameter.

use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};

use crate::sig::{actor_of, field, verify_hex};
use crate::Roster;

/// Domain separator, distinct from `cwth-ring-op-binding:`, so an
/// attestation signature can never be replayed as a ring op or vice versa.
const GUEST_ATTEST_DOMAIN: &[u8] = b"cwth-guest-attestation:";

/// One signed guest session: `name` may write in `namespace` until
/// `expires_at` (unix seconds, exclusive), on `signer`'s word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestAttestation {
    /// The name the guest's acts are attributed to.
    pub name: String,
    pub namespace: String,
    pub expires_at: i64,
    /// Hex public key of the attesting node.
    pub signer: String,
    /// Hex Ed25519 signature over [`GuestAttestation::message`].
    pub sig: String,
}

/// Why an attestation was not honoured. Closed set; [`AttestRefusal::name`]
/// is the stable spelling a door returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AttestRefusal {
    #[error("the attestation's signature does not verify")]
    Forged,
    #[error("the attestation has expired")]
    Expired,
    #[error("the attestation is for another namespace")]
    WrongNamespace,
    #[error("nobody in the roster signs with the attesting key")]
    SignerNotInRoster,
}

impl AttestRefusal {
    pub fn name(self) -> &'static str {
        match self {
            Self::Forged => "forged",
            Self::Expired => "expired",
            Self::WrongNamespace => "wrong_namespace",
            Self::SignerNotInRoster => "signer_not_in_roster",
        }
    }

    /// The refusal a door named, read back from its [`Self::name`] — so a
    /// dialing client re-types the refusal instead of matching prose.
    pub fn from_name(name: &str) -> Option<Self> {
        [
            Self::Forged,
            Self::Expired,
            Self::WrongNamespace,
            Self::SignerNotInRoster,
        ]
        .into_iter()
        .find(|r| r.name() == name)
    }
}

impl GuestAttestation {
    /// `DOMAIN || ns || name || expires_at[8 BE] || signer`, every variable
    /// field length-prefixed as in [`crate::ring_op_message`].
    fn message(namespace: &str, name: &str, expires_at: i64, signer: &str) -> Vec<u8> {
        let mut msg = Vec::with_capacity(GUEST_ATTEST_DOMAIN.len() + 128);
        msg.extend_from_slice(GUEST_ATTEST_DOMAIN);
        field(&mut msg, namespace.as_bytes());
        field(&mut msg, name.as_bytes());
        msg.extend_from_slice(&expires_at.to_be_bytes());
        field(&mut msg, signer.as_bytes());
        msg
    }

    pub fn sign(key: &SigningKey, name: &str, namespace: &str, expires_at: i64) -> Self {
        let signer = actor_of(key);
        let msg = Self::message(namespace, name, expires_at, &signer);
        Self {
            name: name.to_string(),
            namespace: namespace.to_string(),
            expires_at,
            signer,
            sig: hex::encode(key.sign(&msg).to_bytes()),
        }
    }

    /// Whether this attestation lets its `name` write in `namespace` at
    /// `now`. The signature is checked first, so no other field of a forged
    /// attestation is ever believed enough to name a different refusal.
    pub fn verify(&self, roster: &Roster, namespace: &str, now: i64) -> Result<(), AttestRefusal> {
        let verdict = self.judge(roster, namespace, now);
        tracing::debug!(
            namespace,
            name = %self.name,
            signer = %self.signer,
            expires_at = self.expires_at,
            now,
            verdict = verdict.map_or_else(AttestRefusal::name, |()| "honoured"),
            "guest attestation judged"
        );
        verdict
    }

    fn judge(&self, roster: &Roster, namespace: &str, now: i64) -> Result<(), AttestRefusal> {
        let msg = Self::message(&self.namespace, &self.name, self.expires_at, &self.signer);
        if !verify_hex(&self.signer, &msg, &self.sig) {
            return Err(AttestRefusal::Forged);
        }
        if self.namespace != namespace {
            return Err(AttestRefusal::WrongNamespace);
        }
        if now >= self.expires_at {
            return Err(AttestRefusal::Expired);
        }
        if roster.person_for(&self.signer).is_none() {
            return Err(AttestRefusal::SignerNotInRoster);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests_support::{key, ring};

    // `ring()` binds keys 1..; key 99 is claimed by nobody.
    fn att(seed: u8) -> GuestAttestation {
        GuestAttestation::sign(&key(seed), "guest-ana", "house", 1_000)
    }

    #[test]
    fn a_member_signed_attestation_round_trips_and_verifies() {
        let a = att(1);
        let wire: GuestAttestation =
            serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(wire.verify(&ring(), "house", 999), Ok(()));
    }

    #[test]
    fn a_flipped_byte_is_forged() {
        let mut a = att(1);
        a.name = "guest-anb".into();
        assert_eq!(a.verify(&ring(), "house", 999), Err(AttestRefusal::Forged));
        let mut b = att(1);
        let flipped = if b.sig.starts_with('0') { "1" } else { "0" };
        b.sig.replace_range(0..1, flipped);
        assert_eq!(b.verify(&ring(), "house", 999), Err(AttestRefusal::Forged));
    }

    #[test]
    fn past_expiry_is_expired() {
        assert_eq!(
            att(1).verify(&ring(), "house", 1_000),
            Err(AttestRefusal::Expired)
        );
    }

    #[test]
    fn another_namespace_is_wrong_namespace() {
        assert_eq!(
            att(1).verify(&ring(), "lending", 999),
            Err(AttestRefusal::WrongNamespace)
        );
    }

    #[test]
    fn a_key_absent_from_the_roster_is_signer_not_in_roster() {
        assert_eq!(
            att(99).verify(&ring(), "house", 999),
            Err(AttestRefusal::SignerNotInRoster)
        );
    }
}
