// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`RingJournal`] — the single-writer append log behind a ring namespace.
//!
//! Its own file because it and `RingRail` together put `lib.rs` into the
//! 800-1200 approach band (ARCH §3.1). Re-exported at
//! `commonwealth_rail::RingJournal`, so no caller moves.

use super::*;
// The rail stamps its own lines: it may not link `commonwealth-core` for a
// clock (Cargo.toml, and the [[forbid]] in quality/ARCH_LAYERS.toml).
use std::time::{SystemTime, UNIX_EPOCH};

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
    ///
    /// `on_behalf_of` states whose words the act was when the key signing it
    /// is not theirs — see [`SignedOp::on_behalf_of`]. A parameter rather
    /// than a second `append_*` door: the name is inside the signature, so a
    /// caller that could sign without deciding about it would be a second
    /// answer to what these bytes are (ARCH §10.6). `None` is every caller
    /// that writes its own acts.
    pub fn append(
        &self,
        act: RailAct,
        signer: &dyn RingSigner,
        roster: &Roster,
        on_behalf_of: Option<&str>,
    ) -> Result<Op<SignedOp>, RailError> {
        let actor = signer.actor();
        // Authoring under a key the ring does not carry produces an op that
        // every node — including this one — reports as `UnknownSigner`
        // forever. Refusing at the door turns a permanent silent gap into one
        // sentence naming the command that fixes it. Checked against OUR
        // roster and OUR key, never against a field the caller supplied
        // (ARCH §18.1).
        // TYPED, not a sentence: the command that fixes this depends on where
        // this namespace's roster comes from, and the door does not know —
        // `RingRail::roster_origin` does. A renderer picking the right words
        // by matching on the prose would be the string `match` §2.1 forbids.
        if roster.person_for(&actor).is_none() {
            return Err(RailError::NotInRoster {
                actor: actor.clone(),
                namespace: self.namespace.clone(),
            });
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
        let body = body_json(&act, on_behalf_of);
        let signature = signer.sign(&self.namespace, ts_unix, seq, &body);
        let op = Op::new(
            SignedOp {
                seq,
                sig: signature,
                act,
                on_behalf_of: on_behalf_of.map(str::to_string),
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
            on_behalf_of = ?on_behalf_of,
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
    /// (`sovereign_daemon::routes_internal::RING_SYNC_OPS_BUDGET_BYTES`), so
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

    /// Sign a [`Seal`](RailAct::Seal) and delete what it retires — the pair.
    ///
    /// **THE one seal-and-prune (ARCH §10.6).** Rung S taught the read path to
    /// skip a retired prefix; nothing removed one from disk, so a seal on its
    /// own changed no bytes anywhere, and a prune with no seal behind it
    /// reports the deleted range as missing, forever, on every node. Neither
    /// half is worth anything alone. The two callers that seal — the rail
    /// route an app reaches (`sovereign_daemon::routes_rail::append`) and the
    /// daemon's own KV pump (`sovereign_mesh::rail_kv_pump`) — reach this
    /// rather than each writing the sequence themselves, because a second
    /// spelling of "seal, then compact, and a refused compaction is not a
    /// failed seal" is a decider with two answers.
    ///
    /// The seal's own failure IS an `Err`: nothing was written, so there is no
    /// half-done state to describe.
    pub fn seal(
        &self,
        signer: &dyn RingSigner,
        roster: &Roster,
        verifier: &dyn RingVerifier,
    ) -> Result<Sealed, RailError> {
        let op = self.append(RailAct::Seal, signer, roster, None)?;
        let retired = self.compact(roster, verifier);
        if let Err(e) = &retired {
            tracing::warn!(
                namespace = %self.namespace,
                error = %e,
                "ring rail: the seal is written, the prune it authorises was refused"
            );
        }
        Ok(Sealed { op, retired })
    }

    /// Delete every line a seal has retired, and report what went.
    ///
    /// **This is the half that makes the rail's storage bounded.** A journal
    /// that can only be appended to grows for as long as the ring is used, and
    /// every peer holds a full copy of everyone's history forever; the seal
    /// (rung S) taught the READ path to stop asking for a retired prefix, but
    /// nothing until now removed one from disk. Both halves are needed and
    /// neither is provable alone: a seal nobody prunes on saves no bytes, and a
    /// prune with no seal behind it produced ten thousand
    /// [`SequenceHole`](RailGap::SequenceHole)s in the ceiling measurement,
    /// because every node then reported the retired range as missing forever.
    ///
    /// # The floor is admission's, never re-derived here
    ///
    /// An op is dropped when its `seq` is strictly below its own actor's floor
    /// on [`Admission::floors`] — the map [`admit`] built from seals that
    /// passed the signature and roster checks. A prune that read the seals
    /// itself would be a second answer to "what is retired" (ARCH §10.6), and
    /// the two would part company at the worst possible place: a forged seal is
    /// refused by admission and would be believed by a naive re-read, so one
    /// pushed line would erase a member's history from disk on every node that
    /// received it. Deleting by the ADMITTED floor makes that unwritable rather
    /// than checked.
    ///
    /// The seal itself is kept — the floor IS its `seq`, and the comparison is
    /// strict — which is what keeps [`digest`]'s contiguous run non-empty for a
    /// compacted actor. Dropping it would make this node claim it holds nothing
    /// of that actor, and every peer would re-send the whole history back.
    ///
    /// It prunes **every** actor's retired prefix, not just this node's. A seal
    /// is a statement by its author that binds whoever admits it, so a peer
    /// stops storing what the author retired as soon as the seal reaches it.
    /// Author-only pruning would bound the writer's disk and nobody else's.
    ///
    /// # Two things it will not delete
    ///
    /// **A journal with unreadable lines is not rewritten at all.**
    /// [`SkippedLine`] carries a line number and a parse error, never the
    /// bytes, so a line this build cannot parse cannot survive a rewrite — and
    /// that includes a line from a NEWER format version, which is exactly the
    /// content an old build must not be the one to destroy. Refusing is the
    /// only honest option; silently dropping them would be a compaction that
    /// deletes the future (ARCH §18.3).
    ///
    /// **An op a surviving correction names is kept regardless of the floor.**
    /// Retired means "nobody will ask for this again", and a
    /// [`Correct`](RailAct::Correct) still pointing at it says otherwise.
    /// Without this clause, sealing over a corrected op turns its correction
    /// into a permanent [`DanglingCorrection`](RailGap::DanglingCorrection) —
    /// and the guard below would then refuse every future compaction of that
    /// ring, so the growth this method exists to stop would quietly come back.
    ///
    /// # It verifies its own result before committing it (ARCH §7)
    ///
    /// The kept set is re-admitted and compared against the admission we
    /// started from; if compaction would raise a gap the journal did not
    /// already have, nothing is written and the refusal names the gap. This is
    /// the invariant "a node may delete only what a floor covers" encoded so it
    /// cannot be forgotten, rather than left as a rule a future caller has to
    /// remember — and it costs one fold over an in-memory `Vec` that is by
    /// construction shorter than the one already read.
    ///
    /// Gaps may DISAPPEAR, and that is not refused: a line that failed the
    /// signature check below a floor its claimed author authenticated is one
    /// nobody will ever want, and keeping it would let anyone grow a peer's
    /// journal without bound by pushing junk under an old seal. The count is
    /// reported on [`Compaction::gaps_cleared`] rather than left silent.
    ///
    /// The writer lock is held across read, decide and replace, because
    /// `O_APPEND` orders appends against each other and does nothing for an
    /// append racing the rename underneath it.
    pub fn compact(
        &self,
        roster: &Roster,
        verifier: &dyn RingVerifier,
    ) -> Result<Compaction, RailError> {
        let log = self.log();
        let (ops, skipped) = log
            .read_all_with_skips()
            .map_err(|e| RailError::Io(e.to_string()))?;

        if !skipped.is_empty() {
            return Err(RailError::Rejected(format!(
                "`{}` holds {} line(s) this build cannot read, and a rewrite would \
                 destroy them — their bytes are not recoverable from the skip \
                 report. Nothing was deleted. Run `svrn ring log {}` to see what \
                 they are; a line from a newer format version means this node is \
                 the old one and must not be the one to compact.",
                self.namespace,
                skipped.len(),
                self.namespace,
            )));
        }

        let before = admit(&ops, &skipped, roster, &self.namespace, verifier);
        let unchanged = |floors: Floors, kept: usize| Compaction {
            removed: 0,
            kept,
            gaps_cleared: 0,
            floors,
        };
        if before.floors.is_empty() {
            tracing::debug!(
                namespace = %self.namespace,
                held = ops.len(),
                "ring rail: nothing sealed, nothing to compact"
            );
            return Ok(unchanged(before.floors, ops.len()));
        }

        // Derived ids, because that is what a correction resolves against —
        // see `admit`. An op whose on-disk id was rewritten therefore does not
        // match here; the guard below is what stops that becoming a deletion
        // the correction would dangle over.
        let referenced: std::collections::BTreeSet<OpId> = before
            .ops
            .iter()
            .filter_map(|o| o.corrects.clone())
            .collect();

        let kept: Vec<Op<SignedOp>> = ops
            .iter()
            .filter(|op| {
                let floor = before.floors.get(&op.actor).copied().unwrap_or(0);
                op.kind.seq >= floor || referenced.contains(&op.id)
            })
            .cloned()
            .collect();
        let removed = ops.len() - kept.len();
        if removed == 0 {
            tracing::debug!(
                namespace = %self.namespace,
                held = ops.len(),
                sealed = ?before.floors,
                "ring rail: every line held is at or above its floor"
            );
            return Ok(unchanged(before.floors, kept.len()));
        }

        let after = admit(&kept, &[], roster, &self.namespace, verifier);
        let raised: Vec<&RailGap> = after
            .gaps
            .iter()
            .filter(|g| !before.gaps.contains(g))
            .collect();
        if !raised.is_empty() {
            return Err(RailError::Rejected(format!(
                "compacting `{}` would raise {} gap(s) the journal does not have, \
                 so nothing was deleted. First: {}",
                self.namespace,
                raised.len(),
                raised[0],
            )));
        }
        let gaps_cleared = before
            .gaps
            .iter()
            .filter(|g| !after.gaps.contains(g))
            .count();

        log.replace_all(&kept)
            .map_err(|e| RailError::Io(e.to_string()))?;
        // INFO, not debug: this is the one call in the rail that destroys
        // something, and an operator reading logs after a shrinking journal
        // needs the floors it happened under without turning debug on.
        tracing::info!(
            namespace = %self.namespace,
            held = ops.len(),
            removed,
            kept = kept.len(),
            gaps_cleared,
            sealed = ?before.floors,
            "ring rail: compacted"
        );
        Ok(Compaction {
            removed,
            kept: kept.len(),
            gaps_cleared,
            floors: before.floors,
        })
    }

    /// The roster FILE. A missing file is an empty ring, not an error — and
    /// an empty ring admits nothing but gaps, which is the honest answer
    /// before anyone has been added.
    ///
    /// This is the storage half, and its name says so. A caller deciding
    /// what a journal admits goes through [`RingRail::roster`], which knows
    /// whether this namespace's roster is the file at all; the two callers
    /// left on this are that door and the writer (`svrn ring roster add`),
    /// which has to read the file it is about to rewrite.
    pub fn roster_file(&self) -> Result<Roster, RailError> {
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
