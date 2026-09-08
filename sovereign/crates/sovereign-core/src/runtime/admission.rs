// SPDX-License-Identifier: AGPL-3.0-or-later
//! The turn's ADMISSION — "this host already accepted this turn".
//!
//! # The measurement this exists for
//!
//! `sovereign-inference`'s model-slot queue sheds a caller whose predicted
//! wait exceeds the bound, and the decision was priority-blind: it could not
//! tell a fresh request from the fourth judge call of a turn the daemon had
//! already admitted, routed, retrieved for, drafted, and was now verifying.
//! Measured on this host 2026-09-04/05 (note `d6e13797`), 32 chat turns under
//! one concurrent `chat ask`: **every `judge_failed_open` exit (5 of 5) was
//! `queue_shed` with zero judging calls answered**, and five further turns
//! died whole at the DRAFT with `host busy: ~30000 ms predicted wait at queue
//! position 1`.
//!
//! Refusing the tail of already-accepted work sheds no load — the retrieval,
//! the prefill and the draft are already paid for. It converts a slow turn
//! into a failed one. Retrying at the gate is not a fix either: the
//! `grounding-footguns` worker measured an immediate retry shed again, and a
//! hint-honouring one adds 30 s per call to a 40-70 s turn.
//!
//! # The token is the turn's foreground lease
//!
//! `1426177dd` already established the noun: a turn is FOREGROUND for its
//! whole life, and holds a `corpus_engine::ForegroundLease` so background
//! work parks for the entire turn. That lease IS the admission fact — the
//! daemon saying "a person is waiting on this, I accepted it". No second
//! priority concept is minted here; in particular this is NOT
//! `sovereign_inference::selector::BackendEntry::priority`, which ranks which
//! BACKEND to prefer and says nothing about whether a request is new load
//! (ARCH §10.6, checked before writing this).
//!
//! [`AdmittedTurn`] is that lease plus an id, in one object with one
//! lifetime: the token cannot outlive the turn that earned it, because
//! dropping it drops the lease.
//!
//! # Why ambient rather than threaded
//!
//! The same argument `stage_ledger.rs` makes in this directory, and for the
//! same call path: an admission parameter threaded through the router, the
//! handlers, `gate_answer` → `gate_answer_inner` → the ladder is a signature
//! change on dozens of functions for a value none of them read. It is
//! installed once per turn by the code that owns the turn, and read at the
//! two funnels that BUILD the calls the measurement showed shedding.
//!
//! **The honest limit on it, stated rather than discovered.** A task-local
//! does not cross `tokio::spawn`, so `streaming.rs` re-installs it inside the
//! spawned turn body exactly as it re-installs the stage ledger. And the
//! ambient value is only ever read at:
//!
//!  * [`stamp`] in `grounding::call_census::gate_call` — the gate's own
//!    documented single funnel, so every judge, extraction, scan, citation,
//!    retry, rewrite and surgery call the ladder makes carries it; and
//!  * the draft in `streaming.rs`, which is the other class that died.
//!
//! A model call built anywhere else is UNSTAMPED and keeps today's shed
//! behaviour. That is a named coverage boundary, not an oversight: those
//! calls were not the ones the measurement showed dying, and a stamp added
//! without a measurement behind it is a policy change nobody priced.
//!
//! # What it is allowed to do
//!
//! Exactly one thing: let the queue park a continuation instead of refusing
//! it. It changes no verdict, no threshold, no route and no prompt.

use std::future::Future;
use std::sync::Arc;

use crate::types::{CompletionRequest, TurnAdmission};

tokio::task_local! {
    /// The turn's admission token, installed by [`scope`].
    static ADMITTED_TURN: TurnAdmission;
}

/// A turn this host has admitted: the foreground lease it holds, and the id
/// its model calls carry.
///
/// Dropped when the turn's stream drops — which ends the lease and, with it,
/// any claim its in-flight calls had on being parked.
pub(crate) struct AdmittedTurn {
    token: TurnAdmission,
    /// The turn's foreground lease. Held, never read: its whole job is to be
    /// alive for as long as this value is.
    _lease: corpus_engine::ForegroundLease,
}

impl AdmittedTurn {
    /// Admit a turn, if this process has a foreground signal installed.
    ///
    /// `None` when it does not — a CLI with no daemon, a test `Runtime`, a
    /// `Runtime` built without a corpus engine. Absence means "nothing here
    /// tracks foreground work", and the honest consequence is that the
    /// turn's calls are treated as fresh load, exactly as they were before
    /// this module existed. Absence is reported at `info` — the level the
    /// daemon actually runs at, on the DEFAULT module target so the daemon's
    /// filter allowlist cannot drop it — never defaulted into a claim of
    /// admission (ARCH §18.3).
    pub(crate) fn open(engine: &Arc<corpus_engine::CorpusEngine>) -> Option<Self> {
        let Some(lease) = engine.foreground_lease() else {
            // `info`, and NO custom target. The daemon's `EnvFilter` is an
            // allowlist of literal target strings plus module paths
            // (`sovereign_cli_daemon::DAEMON_TRACING_FILTER`), so a
            // `target: "inference.admission"` event is DROPPED unless someone
            // remembers to add it — the trap that has silently darkened this
            // codebase's observability four times, per that constant's own
            // doc. WATCHED: the first arm of the 2026-09-07 measurement put
            // ZERO of these into `daemon.err`, while the queue's own lines
            // (module-targeted, caught by `sovereign_inference=info`) landed.
            // The subsystem name lives in the MESSAGE, exactly as
            // `inference.queue:` does one crate over.
            tracing::info!(
                "inference.admission: turn opened with no foreground signal installed — its model \
                 calls are admitted as fresh load"
            );
            return None;
        };
        // Identity for an occurrence that has no other key. The same choice
        // `GroundingDecisionLine::episode_id` makes one directory over, and
        // for the same reason: two concurrent turns differ in nothing a
        // reader could name (ARCH §7.5 — never a counter, never an address).
        let token = TurnAdmission::new(uuid::Uuid::new_v4().to_string());
        tracing::info!(
            admitted_turn = %token,
            "inference.admission: turn admitted — its model calls may park at the slot queue"
        );
        Some(Self {
            token,
            _lease: lease,
        })
    }

    /// The token this turn's model calls carry.
    pub(crate) fn token(&self) -> TurnAdmission {
        self.token.clone()
    }
}

/// Run `fut` with `token` installed as the ambient admission.
///
/// `None` is a plain passthrough, so an unadmitted turn costs nothing and
/// behaves exactly as it did before.
pub(crate) async fn scope<F: Future>(token: Option<TurnAdmission>, fut: F) -> F::Output {
    match token {
        Some(t) => ADMITTED_TURN.scope(t, fut).await,
        None => fut.await,
    }
}

/// The ambient admission, if this task is running inside an admitted turn.
pub(crate) fn current() -> Option<TurnAdmission> {
    ADMITTED_TURN.try_with(Clone::clone).ok()
}

/// Stamp a request with the ambient admission, in place.
///
/// A request that already carries one is left alone: the caller knows
/// something this function does not.
pub(crate) fn stamp(req: &mut CompletionRequest) {
    if req.admission.is_none() {
        req.admission = current();
    }
}

/// Stamp a BORROWED request, returning an owned copy only when there is
/// something to stamp.
///
/// The clone is why this returns an `Option` rather than a `Cow`-shaped
/// convenience: on an unadmitted call — every background call, every test —
/// the funnel must not copy a 30 KB prompt to add nothing.
pub(crate) fn stamped(req: &CompletionRequest) -> Option<CompletionRequest> {
    if req.admission.is_some() {
        return None;
    }
    let token = current()?;
    let mut owned = req.clone();
    owned.admission = Some(token);
    Some(owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn outside_a_turn_there_is_no_admission() {
        assert!(current().is_none());
        let mut r = CompletionRequest::new("x");
        stamp(&mut r);
        assert!(r.admission.is_none(), "a bare request is fresh load");
        assert!(
            stamped(&CompletionRequest::new("x")).is_none(),
            "nothing to stamp means no clone"
        );
    }

    #[tokio::test]
    async fn inside_a_turn_every_call_carries_the_same_token() {
        let token = TurnAdmission::new("turn-1");
        scope(Some(token.clone()), async {
            let a = stamped(&CompletionRequest::new("judge")).expect("stamped");
            let mut b = CompletionRequest::new("draft");
            stamp(&mut b);
            assert_eq!(a.admission.as_ref(), Some(&token));
            assert_eq!(b.admission.as_ref(), Some(&token));
        })
        .await;
        assert!(current().is_none(), "the scope closes with the turn");
    }

    /// A token the caller set explicitly is never overwritten — the ambient
    /// is a default, not an authority.
    #[tokio::test]
    async fn an_explicit_token_survives_the_ambient_one() {
        let mine = TurnAdmission::new("mine");
        scope(Some(TurnAdmission::new("ambient")), async {
            let mut r = CompletionRequest::new("x");
            r.admission = Some(mine.clone());
            stamp(&mut r);
            assert_eq!(r.admission.as_ref(), Some(&mine));
            assert!(stamped(&r).is_none());
        })
        .await;
    }
}
