// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ring rail's PORT — what the mesh round, the KV pump and the
//! measurements publisher consume instead of a journal on this disk.
//!
//! # Why the seam is here and not in the rail
//!
//! fp-54's flip moves the journals to the serving cluster (`cw-rails`): the
//! daemon's rail routes proxy it, the round and the pump read and write it
//! over the sync doors, and `sovereign-daemon` drops the journal crate
//! entirely. What stays on this side is the LOGIC — chunked digest exchange,
//! projection, sealing, snapshotting — and none of it needs to know where the
//! bytes live. So the round's view of a rail is this trait: one object per
//! process, namespaced methods, no journal handle to hold (§12 decision 2 —
//! the serving cluster owns the verbs; this crate owns the round).
//!
//! `sovereign-mesh` is a commonwealth-package member, so its LOCAL
//! implementation may name the journal crate; the DAEMON's dialing
//! implementation (over `cw-rails`' doors) may not — it speaks rail-core
//! vocabulary only, which is exactly what these signatures use.
//!
//! # The method set is what the mesh consumes, and no more
//!
//! Every method has a named caller in this crate (`ring_sync`, `rail_kv_pump`,
//! `measurements_rail`) or in the daemon's rail routes. The sync leaves the
//! local surface carries that the mesh never calls — `ops_missing_from`'s
//! unbudgeted form, the `SkippedLine` half of a read, a bare `on_behalf_of`
//! (the mesh passes `None` everywhere and the door drops it) — stay off the
//! port: a smaller trait is a smaller wire contract to keep honest. A guest's
//! name crosses only as a signed [`GuestAttestation`]
//! ([`RingRailPort::journal_append_attested`], decision five-programs-34).
//!
//! **The signer is the PORT'S identity, never a per-call argument.** The mesh
//! always passed `rail.signer()`; post-flip the signature happens where the
//! journal lives (the door signs as the node — the same key, loaded through
//! the ONE loader on both sides), so the port exposes only [`RingRailPort::actor`],
//! the read half of that identity.
//!
//! **Roster installation stays LOCAL-ONLY and off the trait.**
//! [`crate::ring_roster::MeshRosterSource::install`] works on the concrete
//! journal rail (who derives which roster is the OWNER's setup, not a
//! per-call fact); post-flip the serving process derives rosters itself and
//! the daemon's bootstrap install dies with its local rail. Tests reach the
//! local impl through this module's re-export.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use commonwealth_rail_core::{
    Admission, Compaction, Digest, GuestAttestation, Op, RailAct, RailError, Roster, RosterOrigin,
    SignedOp,
};

/// The local journal types, re-exported so a consumer that names ONLY the
/// port can still drive the local implementation in tests (the daemon's test
/// tree seeds a real journal and asserts on it directly; the journal crate
/// itself left its Cargo.toml at the flip).
pub use commonwealth_rail::{RingJournal, RingRail};

/// The rail as the mesh round sees it: namespaced, async, signer implied.
///
/// Every future is boxed (`Send`) rather than the trait being async-trait:
/// the same shape [`commonwealth_rail_core::RosterSource`] uses, so the local
/// impl reads like the code it wraps and the dialing impl can await its HTTP
/// calls without blocking a tokio worker.
pub trait RingRailPort: Send + Sync {
    /// Every ring this side holds journals for.
    fn namespaces(&self) -> RailFut<'_, Vec<String>>;
    /// The identity every line this port writes carries — the hex pubkey the
    /// roster names members by.
    fn actor(&self) -> RailFut<'_, String>;
    /// Whether the namespace's roster is a hand-written file or derived.
    fn roster_origin(&self, namespace: &str) -> RailFut<'_, RosterOrigin>;
    /// The namespace's roster, through the rail's ONE reader.
    fn roster(&self, namespace: &str) -> RailFut<'_, Roster>;
    /// Every op the journal holds. The `SkippedLine` half of the local read
    /// is not carried: no mesh consumer branches on it, and the door that
    /// answers this reports the count instead.
    fn journal_read(&self, namespace: &str) -> RailFut<'_, Vec<Op<SignedOp>>>;
    /// Admit the journal against THIS roster — the caller's, not a fresh
    /// server-side read, so the answer and the decision that used it cannot
    /// disagree.
    fn journal_admit(&self, namespace: &str, roster: &Roster) -> RailFut<'_, Admission>;
    /// Sign and append one act as this port's identity. Refused — typed —
    /// when the roster does not name the signer, so the pump's
    /// defer-on-not-in-roster survives the dial.
    fn journal_append(
        &self,
        namespace: &str,
        act: RailAct,
        roster: &Roster,
    ) -> RailFut<'_, Op<SignedOp>>;
    /// [`Self::journal_append`] on a guest's behalf: the journal's side
    /// verifies `attestation` against the namespace's roster and the clock,
    /// and only then signs the act with `on_behalf_of` = its name. A refusal
    /// is [`RailError::AttestRefused`] and nothing is written.
    fn journal_append_attested(
        &self,
        namespace: &str,
        act: RailAct,
        roster: &Roster,
        attestation: &GuestAttestation,
    ) -> RailFut<'_, Op<SignedOp>>;
    /// The seal act plus the prune it authorises, in one result: a refused
    /// prune is not a failed seal, and the pair stays one fact over the wire.
    fn journal_seal(
        &self,
        namespace: &str,
        roster: &Roster,
    ) -> RailFut<'_, (Op<SignedOp>, Result<Compaction, RailError>)>;
    /// Delete what a seal retired, decided by THIS roster.
    fn journal_compact(&self, namespace: &str, roster: &Roster) -> RailFut<'_, Compaction>;
    /// Per-actor contiguous high-water marks.
    fn journal_digest(&self, namespace: &str) -> RailFut<'_, Digest>;
    /// Append a batch of peer ops as-signed; answers how many were new.
    fn journal_ingest_all(&self, namespace: &str, ops: &[Op<SignedOp>]) -> RailFut<'_, usize>;
    /// One budget's worth of what this journal holds that `theirs` lacks,
    /// with `more` saying whether the selection was cut short.
    fn journal_ops_missing_from_within(
        &self,
        namespace: &str,
        theirs: &Digest,
        budget_bytes: usize,
    ) -> RailFut<'_, (Vec<Op<SignedOp>>, bool)>;
    /// The `work` namespace folded where its journal lives (fp-45): the
    /// donor receives the queue and never admits the journal itself.
    fn work_projection(&self) -> RailFut<'_, commonwealth_work::projection::WorkProjection>;

    /// The local journal surface, when this port IS the local
    /// implementation. Tests use it to place a roster file or read a
    /// journal directly; the dialing implementation has no journal and
    /// answers `None`. Production code must not branch on this — asking
    /// "is the rail local?" is the ownership question the port exists to
    /// settle.
    fn as_local(&self) -> Option<&commonwealth_rail::RingRail> {
        None
    }
}

/// `Pin<Box<…>>` future returning `Result<T, RailError>` — the boxed-future
/// shape [`commonwealth_rail_core::RosterSource`] established.
pub type RailFut<'a, T> = Pin<Box<dyn Future<Output = Result<T, RailError>> + Send + 'a>>;

/// The LOCAL implementation: the concrete journal rail on this disk.
///
/// This is what the mesh ran on before the flip, wrapped — every method
/// delegates to the call it always made, boxed. It stays the implementation
/// tests use (and the one `MeshRosterSource::install` is defined against),
/// so the round's behaviour is pinned against real journals, not only
/// against the wire.
#[derive(Clone)]
pub struct LocalRingRail(Arc<commonwealth_rail::RingRail>);

impl LocalRingRail {
    pub fn new(
        root: impl Into<std::path::PathBuf>,
        signer: Arc<dyn commonwealth_rail_core::RingSigner>,
    ) -> Self {
        Self(Arc::new(commonwealth_rail::RingRail::new(root, signer)))
    }

    /// The concrete rail — the roster-installation path (`MeshRosterSource`)
    /// and tests that assert on file-level state reach the journal surface
    /// here, deliberately off the port.
    pub fn inner(&self) -> &Arc<commonwealth_rail::RingRail> {
        &self.0
    }

    fn journal(&self, namespace: &str) -> Result<Arc<commonwealth_rail::RingJournal>, RailError> {
        self.0.journal(namespace)
    }
}

impl RingRailPort for LocalRingRail {
    fn namespaces(&self) -> RailFut<'_, Vec<String>> {
        Box::pin(async move { self.0.namespaces() })
    }

    fn actor(&self) -> RailFut<'_, String> {
        Box::pin(async move { Ok(self.0.signer().actor()) })
    }

    fn roster_origin(&self, namespace: &str) -> RailFut<'_, RosterOrigin> {
        let namespace = namespace.to_string();
        Box::pin(async move { Ok(self.0.roster_origin(&namespace)) })
    }

    fn roster(&self, namespace: &str) -> RailFut<'_, Roster> {
        let namespace = namespace.to_string();
        Box::pin(async move {
            let journal = self.journal(&namespace)?;
            self.0.roster(&journal).await
        })
    }

    fn journal_read(&self, namespace: &str) -> RailFut<'_, Vec<Op<SignedOp>>> {
        let namespace = namespace.to_string();
        Box::pin(async move {
            let (ops, _) = self.journal(&namespace)?.read()?;
            Ok(ops)
        })
    }

    fn journal_admit(&self, namespace: &str, roster: &Roster) -> RailFut<'_, Admission> {
        let namespace = namespace.to_string();
        let roster = roster.clone();
        Box::pin(async move {
            self.journal(&namespace)?
                .admit(&roster, &commonwealth_rail_core::Ed25519Verifier)
        })
    }

    fn journal_append(
        &self,
        namespace: &str,
        act: RailAct,
        roster: &Roster,
    ) -> RailFut<'_, Op<SignedOp>> {
        let namespace = namespace.to_string();
        let roster = roster.clone();
        Box::pin(async move {
            self.journal(&namespace)?
                .append(act, self.0.signer(), &roster, None)
        })
    }

    fn journal_append_attested(
        &self,
        namespace: &str,
        act: RailAct,
        roster: &Roster,
        attestation: &GuestAttestation,
    ) -> RailFut<'_, Op<SignedOp>> {
        let namespace = namespace.to_string();
        let roster = roster.clone();
        let attestation = attestation.clone();
        Box::pin(async move {
            let now = commonwealth_core::clock::unix_now_secs() as i64;
            attestation
                .verify(&roster, &namespace, now)
                .map_err(RailError::AttestRefused)?;
            self.journal(&namespace)?.append(
                act,
                self.0.signer(),
                &roster,
                Some(attestation.name.as_str()),
            )
        })
    }

    fn journal_seal(
        &self,
        namespace: &str,
        roster: &Roster,
    ) -> RailFut<'_, (Op<SignedOp>, Result<Compaction, RailError>)> {
        let namespace = namespace.to_string();
        let roster = roster.clone();
        Box::pin(async move {
            let sealed = self.journal(&namespace)?.seal(
                self.0.signer(),
                &roster,
                &commonwealth_rail_core::Ed25519Verifier,
            )?;
            Ok((sealed.op, sealed.retired))
        })
    }

    fn journal_compact(&self, namespace: &str, roster: &Roster) -> RailFut<'_, Compaction> {
        let namespace = namespace.to_string();
        let roster = roster.clone();
        Box::pin(async move {
            self.journal(&namespace)?
                .compact(&roster, &commonwealth_rail_core::Ed25519Verifier)
        })
    }

    fn journal_digest(&self, namespace: &str) -> RailFut<'_, Digest> {
        let namespace = namespace.to_string();
        Box::pin(async move { self.journal(&namespace)?.digest() })
    }

    fn journal_ingest_all(&self, namespace: &str, ops: &[Op<SignedOp>]) -> RailFut<'_, usize> {
        let namespace = namespace.to_string();
        let ops = ops.to_vec();
        Box::pin(async move { self.journal(&namespace)?.ingest_all(&ops) })
    }

    fn journal_ops_missing_from_within(
        &self,
        namespace: &str,
        theirs: &Digest,
        budget_bytes: usize,
    ) -> RailFut<'_, (Vec<Op<SignedOp>>, bool)> {
        let namespace = namespace.to_string();
        let theirs = theirs.clone();
        Box::pin(async move {
            self.journal(&namespace)?
                .ops_missing_from_within(&theirs, budget_bytes)
        })
    }

    fn work_projection(&self) -> RailFut<'_, commonwealth_work::projection::WorkProjection> {
        Box::pin(async move {
            let journal = self.journal(commonwealth_work::WORK_NAMESPACE)?;
            let roster = self.0.roster(&journal).await?;
            let admission = journal.admit(&roster, &commonwealth_rail_core::Ed25519Verifier)?;
            Ok(commonwealth_work::projection::WorkProjection::fold(
                &admission,
            ))
        })
    }

    fn as_local(&self) -> Option<&commonwealth_rail::RingRail> {
        Some(&self.0)
    }
}
