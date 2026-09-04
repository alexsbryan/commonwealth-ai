// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ring rail — a shared append-only log that converges without a server.
//!
//! # What this is for
//!
//! A house of twenty people already keeps its shared state somewhere:
//! Splitwise for the expenses, a spreadsheet for the chores, a running Signal
//! thread for who has the drill. Every one of those is a private database
//! somewhere else. This is the same book, kept as an append-only journal on
//! each housemate's own machine, gossiped between them, and handed back in an
//! order every node agrees on.
//!
//! # The rail carries an opaque payload, and that is the whole design
//!
//! This started as an expense ledger, and the ledger's types were the rail's
//! types: the journal line's body was an `ExpenseOp`, and the reader's
//! failure enum had thirteen variants of which five were about money. It
//! worked, and it was one layer pretending to be one thing.
//!
//! The cut is now explicit. **The rail knows about delivery, authenticity and
//! convergence. It does not know what an act means.**
//!
//! ```text
//!   Payload          the app's act, canonical bytes, opaque here
//!   RailAct          Record{payload} | Correct{corrects, replacement?}
//!   SignedOp         + seq + signature — the journal line's body
//!   Op<SignedOp>     corpus-engine's envelope: id, v, ts, actor
//!   RingJournal      the log on disk, one writer
//!   admit()          ops -> (acts in ONE order, gaps)
//!   <the app>        acts -> whatever the app is about
//! ```
//!
//! What the app gets back from [`admit`] is already deduplicated, signature-
//! checked, roster-admitted, sequence-audited, void-filtered and **totally
//! ordered**. Its reducer runs over that list and cannot reintroduce a
//! divergence, because there is no ordering decision left to make.
//!
//! # Two things live in the rail that look like they belong to an app
//!
//! **Correction.** "This earlier act was wrong, and it never comes back" is
//! not an expense rule — a tool-lending board needs it the first time someone
//! writes *I returned the drill* and then *no I didn't*. It is also the rule
//! most easily got wrong: the void set has to be built from every correction
//! at once, never by walking for liveness, or it stops being commutative.
//! One implementation, in [`admit`], and no app re-derives it.
//!
//! **The roster.** Who is in this ring and which keys they sign with is
//! membership, not meaning. An op signed by a key nobody claims is a gap
//! before any app sees it.
//!
//! # Three decisions worth knowing before reading the code
//!
//! **The node key signs, the roster binds.** An op's `actor` is the hex
//! public key that signed it, because that is the only field in the whole
//! line the writer cannot forge for someone else (ARCH §18.1). The [`Roster`]
//! then says which keys belong to which person. One person with two laptops
//! is two entries in one roster row — accepted, not solved.
//!
//! **A correction is a compensating entry, and it is visible.** Nothing is
//! ever rewritten or deleted. A [`RailAct::Correct`] voids an earlier op and
//! may re-state it; the void is permanent, so a correction never resurrects
//! what an earlier correction voided. To put something back, write it again.
//!
//! **The rail refuses to fake completeness.** [`admit`] returns
//! [`RailGap`]s alongside the acts. A log that cannot say "I may be missing
//! something" lets an app state a wrong total with complete confidence, which
//! is worse than refusing to answer (ARCH §18.3).
//!
//! # Where the journal is
//!
//! This crate is the FOLD and nothing else: vocabulary, signing, admission,
//! the sync digest. It performs no I/O and opens no file. The append-only
//! journal on disk — `RingRail`, `RingJournal`, and the door that refuses
//! what the rail can judge — is `commonwealth-rail`, which depends on this
//! crate. The split is what lets a second application (canon) reuse the fold
//! without inheriting a file layout, and it is why `admit` takes ops rather
//! than a path.

mod admit;
mod payload;
mod sig;
mod sync;

use std::collections::BTreeMap;

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

pub use admit::{admit, body_json, Admission, AdmittedOp, RailGap};
pub use payload::{Payload, PayloadError, MAX_PAYLOAD_BYTES};
pub use sig::{actor_of, ring_op_message, sign_ring_op, verify_ring_op};
pub use sync::{digest, ops_missing_from, Digest};

/// The journal envelope, re-exported so a consumer of the rail names ONE
/// crate. `Op<SignedOp>` is what crosses the ring-sync wire
/// (`commonwealth-api/src/routes_internal/ring_sync.rs`,
/// `sovereign-mesh/src/ring_sync.rs`), and a caller that had to name `oplog`
/// separately would be free to reach a different version of it.
pub use oplog::{Journaled, Op, OpId, SkippedLine};

// ── People ───────────────────────────────────────────────────

/// A member of the ring, by the name the house calls them.
///
/// A display name and not a key: the whole point of a ring app is that "Alex
/// paid for groceries" renders as *Alex*. The binding from name to signing
/// key lives in the [`Roster`], which is why the name can be changed without
/// rewriting history.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct Person(String);

impl Person {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Person {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for Person {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl std::fmt::Display for Person {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Who is in this ring, and which keys they sign with.
///
/// A parameter of [`admit`], never part of the admitted state. It decides
/// *membership* — an op signed by a key nobody claims is a gap, not an act —
/// and it is never handed to an app as anything but the `person` on an
/// already-admitted op. That separation is what lets an app's arithmetic stay
/// a function of the op set as people join and leave: an app that reads the
/// roster to decide who shares a cost re-divides every past expense the day a
/// housemate moves in, and the reference app has a test pinning that it does
/// not.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Roster {
    /// Person → the hex node public keys that person signs with. Two laptops
    /// is two keys in one row.
    pub members: BTreeMap<Person, Vec<String>>,
}

impl Roster {
    pub fn new(members: BTreeMap<Person, Vec<String>>) -> Self {
        Self { members }
    }

    /// Who signed with this key, if anyone in the ring did.
    pub fn person_for(&self, actor: &str) -> Option<&Person> {
        self.members
            .iter()
            .find(|(_, keys)| keys.iter().any(|k| k == actor))
            .map(|(p, _)| p)
    }

    /// Whether this name is one the ring knows.
    pub fn knows(&self, person: &Person) -> bool {
        self.members.contains_key(person)
    }
}

// ── What a line says ─────────────────────────────────────────

/// What one line of a ring journal asserts.
///
/// Two cases, and the rail understands neither payload. `Correct` is here
/// rather than in an app because voiding-without-resurrection is the rule
/// that makes the whole thing commutative, and it is generic — see the module
/// docs.
///
/// A correction's `replacement` is a [`Payload`] and never another
/// `RailAct`, so a correction of a correction is *unrepresentable* rather
/// than checked (ARCH §7.1). There is no nested-correction branch in
/// [`admit`] because there is no way to write one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum RailAct {
    /// The app said something.
    Record { payload: Payload },
    /// An earlier op was wrong. It is voided — permanently, and visibly — and
    /// optionally re-stated by `replacement`.
    ///
    /// A correction of a correction cancels that correction's replacement and
    /// leaves the original op voided. Nothing ever comes back to life; to
    /// restore an act, write it again.
    Correct {
        corrects: OpId,
        replacement: Option<Payload>,
    },
}

impl RailAct {
    /// Read an act from an app's request body, with refusals a person can
    /// act on.
    ///
    /// The rail's own refusals ([`PayloadError`]) are already sentences
    /// naming the fix, but they reach here wrapped by serde, which renders a
    /// `Deserialize` failure with its own prose and a trailing position that
    /// means nothing for a value that never came from a file. This unwraps
    /// that: the route returns what the type said, not what serde said about
    /// what the type said.
    ///
    /// One entry point for the same reason the gap renderer is one function
    /// (ARCH §10.6) — the 422 body an app shows a housemate must be the same
    /// words the terminal shows.
    pub fn from_json(value: serde_json::Value) -> Result<Self, RailError> {
        serde_json::from_value(value).map_err(|e| {
            let raw = e.to_string();
            // serde appends " at line 0 column 0" to a `from_value` failure,
            // which is a position in a document that does not exist.
            let cleaned = raw
                .split(" at line ")
                .next()
                .unwrap_or(&raw)
                .trim()
                .to_string();
            RailError::Rejected(if cleaned.is_empty() { raw } else { cleaned })
        })
    }

    /// Every payload this act carries — one, or none.
    ///
    /// Public since the split (2026-09-04): the append door in
    /// `commonwealth-rail` asserts on it, and a second walk of the two
    /// variants there would be a second answer to "what does this act carry"
    /// (ARCH §10.6).
    pub fn payloads(&self) -> Option<&Payload> {
        match self {
            Self::Record { payload } => Some(payload),
            Self::Correct { replacement, .. } => replacement.as_ref(),
        }
    }
}

/// The journal line's body: the act, plus the two fields that make it
/// attributable and countable.
///
/// `seq` is the keystone of the whole design. Per-actor sequence numbers are
/// the only thing here that can tell "nothing happened" apart from "it never
/// reached me", and they give a sync digest of `{actor → max_seq}` — about
/// 600 bytes regardless of how long the log is — instead of shipping the
/// whole namespace to every peer.
///
/// They also fix a collision the envelope documents as by-design:
/// [`Op::new`] hashes `(prefix, ts, actor, body)`, so two byte-identical acts
/// in the same second by the same actor share an id. Harmless for governance;
/// for a money app it silently merges two real coffees into one. `seq` differs,
/// so the body differs, so the ids differ.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedOp {
    /// This actor's own counter within this namespace, starting at 0 and
    /// contiguous. A hole means an op has not reached us.
    pub seq: u64,
    /// Hex Ed25519 signature over [`sig::ring_op_message`]. Not itself signed
    /// (that would be circular) and not part of what is signed.
    pub sig: String,
    #[serde(flatten)]
    pub act: RailAct,
}

impl Journaled for SignedOp {
    const FILE: &'static str = "ring_oplog.jsonl";
    const ID_PREFIX: &'static str = "ring";
    const LABEL: &'static str = "ring_rail";
}

// ── Errors ───────────────────────────────────────────────────

/// Named `RailError` and not `LedgerError` because `sovereign-pipeline`
/// already owns that name for a cost ledger — a different concept that
/// shared a word — and because this layer stopped being a ledger when the
/// payload became opaque.
#[derive(Debug, thiserror::Error)]
pub enum RailError {
    /// A namespace is a directory name. Anything that is not plainly one is
    /// refused here rather than sanitised downstream, so no caller can reach
    /// outside its own namespace by naming a path (ARCH §7.1).
    #[error("'{0}' is not a ring namespace — use 1..=64 characters from a-z, 0-9, '-' and '_'")]
    BadNamespace(String),
    #[error("ring rail io: {0}")]
    Io(String),
    /// The door refused to author this act. The string is a sentence, because
    /// it is the 422 body an app shows a person.
    #[error("refused: {0}")]
    Rejected(String),
}

// ── Signing without holding key material ─────────────────────

/// How the rail gets a signature, without ever seeing a private key.
///
/// The daemon owns the node's `SigningKey`; `AppState` installs one of these
/// and so never holds raw key material, and `commonwealth-api` needs no
/// crypto dependency. That is not a new seam — it is exactly the shape
/// `AppState::self_dial_signer` already uses for the gossip self-stamp, and
/// reusing it means there is one answer to "who may sign as this node"
/// (ARCH §10.6) rather than two.
pub trait RingSigner: Send + Sync {
    /// The hex public key this signer signs as. Becomes the op's `actor`, and
    /// is what the roster is looked up by.
    fn actor(&self) -> String;
    /// Hex Ed25519 signature over [`sig::ring_op_message`].
    fn sign(&self, namespace: &str, ts_unix: i64, seq: u64, body_json: &str) -> String;
}

/// The direct implementation, for a caller that legitimately holds the key —
/// the daemon at startup, and the tests.
impl RingSigner for SigningKey {
    fn actor(&self) -> String {
        sig::actor_of(self)
    }
    fn sign(&self, namespace: &str, ts_unix: i64, seq: u64, body_json: &str) -> String {
        sig::sign_ring_op(self, namespace, ts_unix, seq, body_json)
    }
}

#[cfg(test)]
mod tests;

/// The rail's ONE set of op fixtures.
///
/// Public behind `test-support` because the journal tests live in
/// `commonwealth-rail` and the fold tests live here, and both have to be
/// talking about the same signed op or neither proves anything. Two copies of
/// "what does a signed op look like" would be two answers to the question the
/// signature exists to settle (ARCH §10.6).
#[cfg(any(test, feature = "test-support"))]
// Fixture code, and it compiles as LIB code under the feature rather than
// under `cfg(test)` — so clippy's allow-unwrap-in-tests exemption does not
// reach it and the panic ratchet would read a builder as production risk.
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub mod tests_support;
