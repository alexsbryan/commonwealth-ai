// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ring journal on disk — `<root>/rings/<namespace>/ring_oplog.jsonl`.
//!
//! [`commonwealth_rail_core`] is the FOLD: vocabulary, signing, admission,
//! the sync digest, and not one line of I/O. This crate is the half that
//! touches a filesystem — one writer per namespace, the door that signs, and
//! the peer-ingest path. Everything the core exports is re-exported here, so
//! an application names one crate and a peer that only needs the fold (canon)
//! names the other.
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

pub use commonwealth_rail_core::*;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

// The one type the core does NOT re-export: it is the writer, and nothing
// above this crate opens a journal directly.
use oplog::Oplog;

/// A namespace names a directory, so it may only be a plain name.
fn valid_namespace(ns: &str) -> bool {
    !ns.is_empty()
        && ns.len() <= 64
        && ns
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// Every ring this node holds, as a directory. The ONE place the literal
/// `rings` is spelled — [`ring_dir`] and [`RingRail::namespaces`] are its two
/// callers and there is no third spelling in the workspace (ARCH §10.6).
fn rings_root(root: &Path) -> PathBuf {
    root.join("rings")
}

/// Where one ring namespace keeps its journal and its roster:
/// `<root>/rings/<namespace>/`.
///
/// A caller that holds a [`RingJournal`] reads [`RingJournal::dir`] or
/// [`RingJournal::roster_path`] instead — those are the same join and they
/// also carry the namespace check. This is for the caller that has a root and
/// a name and nothing else.
fn ring_dir(root: &Path, namespace: &str) -> PathBuf {
    rings_root(root).join(namespace)
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
        let dir = rings_root(&self.root);
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
        let dir = ring_dir(root, namespace);
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

    /// This namespace's roster file. One spelling of `roster.json`, so the
    /// reader, the writer and the error message that names it cannot drift
    /// onto three files (ARCH §10.6).
    pub fn roster_path(&self) -> PathBuf {
        self.dir.join("roster.json")
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
    ///
    /// `verifier` is passed rather than held, so the journal on disk has no
    /// opinion about which scheme judged the signatures on it — the caller
    /// that reads an answer is the one that says what it trusted.
    /// [`Ed25519Verifier`] is the shipped one.
    pub fn admit(
        &self,
        roster: &Roster,
        verifier: &dyn RingVerifier,
    ) -> Result<Admission, RailError> {
        let (ops, skipped) = self.read()?;
        Ok(admit(&ops, &skipped, roster, &self.namespace, verifier))
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
        let body = body_json(&act);
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
    /// See [`digest`] for why the mark is contiguous.
    pub fn digest(&self) -> Result<Digest, RailError> {
        Ok(digest(&self.read()?.0))
    }

    /// Every op this node holds that `theirs` says the peer is missing.
    /// Author-blind: a node republishes what it HOLDS, so a housemate who
    /// leaves the ring does not take their half of the journal with them.
    ///
    /// The honest TOTAL, and therefore the wrong thing to put on a wire: use
    /// [`RingJournal::ops_missing_from_within`] for that.
    pub fn ops_missing_from(&self, theirs: &Digest) -> Result<Vec<Op<SignedOp>>, RailError> {
        Ok(ops_missing_from(&self.read()?.0, theirs))
    }

    /// [`RingJournal::ops_missing_from`], stopped at `budget_bytes` of
    /// serialised ops. Returns `(ops, more)` — see
    /// [`ops_missing_from_within`] for why the truncation is in the return
    /// type and why repeating this terminates.
    ///
    /// This is what every caller that sends ops over the wire uses. The
    /// budget itself is NOT decided here: it is derived from the receiver's
    /// body limit by the crate that owns that limit
    /// (`commonwealth_api::routes_internal::RING_SYNC_OPS_BUDGET_BYTES`), so
    /// the rail stays a crate a ring app can lift without an HTTP server.
    pub fn ops_missing_from_within(
        &self,
        theirs: &Digest,
        budget_bytes: usize,
    ) -> Result<(Vec<Op<SignedOp>>, bool), RailError> {
        Ok(ops_missing_from_within(
            &self.read()?.0,
            theirs,
            budget_bytes,
        ))
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
        let path = self.roster_path();
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
        std::fs::write(self.roster_path(), raw).map_err(|e| RailError::Io(e.to_string()))
    }
}

#[cfg(test)]
mod tests;
