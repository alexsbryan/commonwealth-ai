// SPDX-License-Identifier: AGPL-3.0-or-later
//! The donor's reads and writes of the `work` journal this process holds:
//! the fold, the one append, and the lease question over the fold. A sibling
//! of `donor.rs` so it stays out of ARCH §3.1's approach band.

use commonwealth_rail::RailAct;
use commonwealth_work::act::{UnitRef, WorkAct};
use commonwealth_work::actor::ActorKey;
use commonwealth_work::projection::{lease_state, LeaseState, WorkProjection};
use commonwealth_work::WORK_NAMESPACE;
use tracing::warn;

use super::TRACE_TARGET;
use crate::RailsDaemon;

/// The I/O half of "do I still hold this lease".
///
/// The DECIDER moved to `commonwealth_work::projection::lease_state` — it is
/// pure over the fold, and a lifted peer needs exactly the same three-state
/// answer (cw-lift 5f found this by re-deriving it as a bool and cancelling a
/// running unit on one unreadable heartbeat). What stays here is the only part
/// that is this crate's business: obtaining the fold from this process's
/// journal, and answering `Unknown` when that fails.
pub(super) async fn still_ours(
    daemon: &RailsDaemon,
    unit_ref: &UnitRef,
    self_key: &ActorKey,
) -> LeaseState {
    match fold_now(daemon).await {
        Some((proj, _, now_ms)) => lease_state(&proj, unit_ref, self_key, now_ms),
        // A journal that could not be read is NOT evidence that somebody else
        // holds the lease. Kept apart from `Lost` so the caller can hold
        // rather than kill (ARCH §18.2 — could-not-judge is its own verdict).
        None => LeaseState::Unknown,
    }
}

/// Append one act to the local `work` journal, then nudge the ring round.
///
/// THE door, and the same journal call the append door makes: the journal
/// signs and sequences it under the rail's one roster reader, the ring round
/// carries it. No HTTP, so no new sender of replicated state.
pub(super) async fn append(daemon: &RailsDaemon, act: &WorkAct) -> Result<(), String> {
    let payload = commonwealth_work::act::to_payload(act).map_err(|e| e.to_string())?;
    let journal = daemon
        .rail
        .journal(WORK_NAMESPACE)
        .map_err(|e| e.to_string())?;
    let roster = daemon
        .rail
        .roster(&journal)
        .await
        .map_err(|e| e.to_string())?;
    journal
        .append(
            RailAct::Record { payload },
            daemon.rail.signer(),
            &roster,
            None,
            &commonwealth_rail::Ed25519Verifier,
        )
        .map_err(|e| e.to_string())?;
    daemon.ring_nudge.notify_one();
    Ok(())
}

/// Now, in the milliseconds the fold speaks.
pub(super) fn now_ms() -> u64 {
    commonwealth_core::clock::unix_now_millis()
}

/// The `work` namespace, folded as it stands right now.
///
/// The fold runs where the journal lives, which is this process: the same
/// `crate::work::fold_work` the `/v1/work/projection` door answers from, and
/// this node's signing key off the rail's signer.
///
/// `None` when the journal could not be folded (unreadable roster, a journal
/// that will not admit) — traced, and a condition that heals, so the round is
/// skipped rather than the loop exiting.
pub(super) async fn fold_now(daemon: &RailsDaemon) -> Option<(WorkProjection, ActorKey, u64)> {
    let proj = match crate::work::fold_work(daemon).await {
        Ok(p) => p,
        Err(e) => {
            warn!(target: TRACE_TARGET, error = %e, "work donor: the `work` queue could not be folded, nothing to take");
            return None;
        }
    };
    let self_key = match ActorKey::parse(daemon.rail.signer().actor()) {
        Ok(k) => k,
        Err(e) => {
            warn!(target: TRACE_TARGET, error = %e, "work donor: this node's own signing key is not an actor key");
            return None;
        }
    };
    Some((proj, self_key, now_ms()))
}
