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
//! # The door is strict about what the rail knows, and silent about the rest
//!
//! [`RingJournal::append`] refuses what the rail can judge: a payload with no
//! canonical form (see [`Payload`]), and an attempt to author under a key the
//! ring's own roster does not carry. It says nothing about whether an amount
//! is positive or a borrower exists, because it cannot — those are the app's,
//! and the app owns one validator that its own door and its own reducer both
//! call, exactly as this module used to.
//!
//! The journal is **truth**; the mesh store is only a transport buffer. In
//! production `MeshStore` is `in_memory()`, so anything treating it as
//! durable loses the log on restart.

mod admit;
mod payload;
mod sig;
mod sync;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use corpus_engine::oplog::{Journaled, Op, OpId, Oplog, SkippedLine};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

pub use admit::{admit, Admission, AdmittedOp, RailGap};
pub use payload::{Payload, PayloadError, MAX_PAYLOAD_BYTES};
pub use sig::{actor_of, ring_op_message, sign_ring_op, verify_ring_op};
pub use sync::{digest, ops_missing_from, Digest};

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
    fn payloads(&self) -> Option<&Payload> {
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

/// A namespace names a directory, so it may only be a plain name.
fn valid_namespace(ns: &str) -> bool {
    !ns.is_empty()
        && ns.len() <= 64
        && ns
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
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

// ── The rail's storage ───────────────────────────────────────

/// Every ring namespace this node holds, and the one key it signs with.
///
/// Installed once by the daemon. A namespace's journal is opened on first
/// touch and kept, so the write lock that serialises appends is per-namespace
/// and outlives a request — two ring apps do not contend, and one app's two
/// requests do.
pub struct RingRail {
    root: PathBuf,
    signer: std::sync::Arc<dyn RingSigner>,
    open: Mutex<BTreeMap<String, std::sync::Arc<RingJournal>>>,
}

impl RingRail {
    pub fn new(root: impl Into<PathBuf>, signer: std::sync::Arc<dyn RingSigner>) -> Self {
        Self {
            root: root.into(),
            signer,
            open: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn signer(&self) -> &dyn RingSigner {
        self.signer.as_ref()
    }

    /// Every namespace this node holds a journal for, read from disk.
    ///
    /// From DISK and not from the open map, because on boot nothing has been
    /// touched yet — and boot is exactly when replication most needs the
    /// list. A node that came back from a week off has to offer its whole
    /// journal to peers before anyone asks it anything.
    ///
    /// A missing `rings/` directory is an empty list, not an error: a daemon
    /// that has never hosted a ring is a normal daemon.
    pub fn namespaces(&self) -> Result<Vec<String>, RailError> {
        let dir = self.root.join("rings");
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(RailError::Io(format!("{}: {e}", dir.display()))),
        };
        let mut out: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .filter_map(|e| e.file_name().into_string().ok())
            // A directory whose name this build would refuse to open is not a
            // namespace — skip it rather than surfacing a path we cannot use.
            .filter(|name| valid_namespace(name))
            .collect();
        out.sort();
        Ok(out)
    }

    /// The journal for one namespace, opening it if this is the first touch.
    pub fn journal(&self, namespace: &str) -> Result<std::sync::Arc<RingJournal>, RailError> {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = open.get(namespace) {
            return Ok(existing.clone());
        }
        let journal = std::sync::Arc::new(RingJournal::open(&self.root, namespace)?);
        open.insert(namespace.to_string(), journal.clone());
        Ok(journal)
    }
}

// ── The journal on disk ──────────────────────────────────────

/// One namespace's append-only journal, plus its roster.
///
/// The `Mutex` is the single-writer rule the journal format needs: `O_APPEND`
/// makes one `write(2)` atomic, and this makes sure there is one. It is also
/// what makes the next sequence number safe to compute — read the log, take
/// the highest, add one — with no window for two appends to pick the same one.
pub struct RingJournal {
    namespace: String,
    dir: PathBuf,
    log: Mutex<Oplog<SignedOp>>,
}

impl RingJournal {
    /// Open (lazily — nothing is created until the first append) the journal
    /// for `namespace` under `root`.
    pub fn open(root: &Path, namespace: &str) -> Result<Self, RailError> {
        if !valid_namespace(namespace) {
            return Err(RailError::BadNamespace(namespace.to_string()));
        }
        let dir = root.join("rings").join(namespace);
        Ok(Self {
            namespace: namespace.to_string(),
            log: Mutex::new(Oplog::new(dir.clone())),
            dir,
        })
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn log(&self) -> std::sync::MutexGuard<'_, Oplog<SignedOp>> {
        // A panic in another appender must not take the journal offline; it
        // is on disk and re-read every time, so there is no in-memory state a
        // poisoned lock could have left half-written.
        self.log.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Every op on disk, plus the lines that could not be read.
    pub fn read(&self) -> Result<(Vec<Op<SignedOp>>, Vec<SkippedLine>), RailError> {
        self.log()
            .read_all_with_skips()
            .map_err(|e| RailError::Io(e.to_string()))
    }

    /// The acts in the order every node applies them, and what could not be
    /// accounted for.
    pub fn admit(&self, roster: &Roster) -> Result<Admission, RailError> {
        let (ops, skipped) = self.read()?;
        Ok(admit::admit(&ops, &skipped, roster, &self.namespace))
    }

    /// Sign and append one act under this node's key.
    ///
    /// Refuses only what the rail can judge — see the module docs on the
    /// door. The whole operation holds the writer lock, so `seq` cannot be
    /// handed out twice.
    pub fn append(
        &self,
        act: RailAct,
        signer: &dyn RingSigner,
        roster: &Roster,
    ) -> Result<Op<SignedOp>, RailError> {
        let actor = signer.actor();
        // Authoring under a key the ring does not carry produces an op that
        // every node — including this one — reports as `UnknownSigner`
        // forever. Refusing at the door turns a permanent silent gap into one
        // sentence naming the command that fixes it. Checked against OUR
        // roster and OUR key, never against a field the caller supplied
        // (ARCH §18.1).
        if roster.person_for(&actor).is_none() {
            return Err(RailError::Rejected(format!(
                "this node signs as {}… and nobody in the `{}` roster claims that \
                 key, so every op it writes would be unreadable to the ring — add \
                 yourself first with `svrn ring roster add <you> --self --ring {}`",
                &actor[..actor.len().min(12)],
                self.namespace,
                self.namespace,
            )));
        }
        // A payload is canonical by construction, so by the time one is a
        // `Payload` there is nothing left for the door to check. This assert
        // is the reader's reminder that the check happened at the type
        // boundary, not that it was skipped.
        debug_assert!(
            act.payloads()
                .map(|p| p.as_value().is_object())
                .unwrap_or(true),
            "Payload::new admits only objects"
        );

        let log = self.log();
        let (existing, _) = log
            .read_all_with_skips()
            .map_err(|e| RailError::Io(e.to_string()))?;
        let seq = existing
            .iter()
            .filter(|o| o.actor == actor)
            .map(|o| o.kind.seq)
            .max()
            .map_or(0, |m| m + 1);

        let ts_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let body = admit::body_json(&act);
        let signature = signer.sign(&self.namespace, ts_unix, seq, &body);
        let op = Op::new(
            SignedOp {
                seq,
                sig: signature,
                act,
            },
            ts_unix,
            actor.clone(),
        );
        log.append(&op).map_err(|e| RailError::Io(e.to_string()))?;
        tracing::debug!(
            namespace = %self.namespace,
            id = %op.id,
            actor = %actor,
            seq,
            "ring rail: appended"
        );
        Ok(op)
    }

    /// Append an op that arrived from a peer, exactly as it was signed.
    ///
    /// No validation and no re-signing: the signature covers the op, and
    /// anything wrong with it becomes a gap when [`admit`] reads it back.
    /// Doing otherwise would mean this node deciding what a peer said.
    pub fn ingest(&self, op: &Op<SignedOp>) -> Result<bool, RailError> {
        let log = self.log();
        let (existing, _) = log
            .read_all_with_skips()
            .map_err(|e| RailError::Io(e.to_string()))?;
        if existing.iter().any(|o| o.id == op.id) {
            tracing::debug!(
                namespace = %self.namespace,
                id = %op.id,
                "ring rail: peer op already held, not re-appended"
            );
            return Ok(false);
        }
        log.append(op).map_err(|e| RailError::Io(e.to_string()))?;
        Ok(true)
    }

    /// What this node can honestly claim to hold, per actor — the ~600-byte
    /// payload a peer needs in order to work out what to send back.
    /// See [`sync::digest`] for why the mark is contiguous.
    pub fn digest(&self) -> Result<sync::Digest, RailError> {
        Ok(sync::digest(&self.read()?.0))
    }

    /// Every op this node holds that `theirs` says the peer is missing.
    /// Author-blind: a node republishes what it HOLDS, so a housemate who
    /// leaves the ring does not take their half of the journal with them.
    pub fn ops_missing_from(&self, theirs: &sync::Digest) -> Result<Vec<Op<SignedOp>>, RailError> {
        Ok(sync::ops_missing_from(&self.read()?.0, theirs))
    }

    /// Append a batch of peer ops, skipping the ones already held. Returns
    /// how many were new.
    ///
    /// One read and one append for the whole batch, not one of each per op:
    /// a boot republish hands over the entire journal, and re-reading it per
    /// op would make catching up quadratic in a file that only ever grows.
    pub fn ingest_all(&self, ops: &[Op<SignedOp>]) -> Result<usize, RailError> {
        if ops.is_empty() {
            return Ok(0);
        }
        let log = self.log();
        let (existing, _) = log
            .read_all_with_skips()
            .map_err(|e| RailError::Io(e.to_string()))?;
        let mut held: std::collections::BTreeSet<&OpId> = existing.iter().map(|o| &o.id).collect();
        let mut fresh: Vec<Op<SignedOp>> = Vec::new();
        for op in ops {
            // Also dedupes WITHIN the batch: a peer that sends the same op
            // twice in one body must not get two lines out of it.
            if held.insert(&op.id) {
                fresh.push(op.clone());
            }
        }
        if fresh.is_empty() {
            tracing::debug!(
                namespace = %self.namespace,
                offered = ops.len(),
                "ring rail: peer batch held nothing new"
            );
            return Ok(0);
        }
        log.append_all(&fresh)
            .map_err(|e| RailError::Io(e.to_string()))?;
        tracing::debug!(
            namespace = %self.namespace,
            offered = ops.len(),
            ingested = fresh.len(),
            "ring rail: ingested a peer batch"
        );
        Ok(fresh.len())
    }

    /// The roster on disk. A missing file is an empty ring, not an error —
    /// and an empty ring admits nothing but gaps, which is the honest answer
    /// before anyone has been added.
    pub fn roster(&self) -> Result<Roster, RailError> {
        let path = self.dir.join("roster.json");
        match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw)
                .map_err(|e| RailError::Io(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Roster::default()),
            Err(e) => Err(RailError::Io(format!("{}: {e}", path.display()))),
        }
    }

    pub fn set_roster(&self, roster: &Roster) -> Result<(), RailError> {
        std::fs::create_dir_all(&self.dir).map_err(|e| RailError::Io(e.to_string()))?;
        let raw = serde_json::to_string_pretty(roster).map_err(|e| RailError::Io(e.to_string()))?;
        std::fs::write(self.dir.join("roster.json"), raw).map_err(|e| RailError::Io(e.to_string()))
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_support;
