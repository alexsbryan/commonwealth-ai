// SPDX-License-Identifier: AGPL-3.0-or-later
//! The heartbeat decision — `Renewed | Miss | Abort(Gone|NotFound|Silence)`.
//!
//! A pure decider with its own closed enums and its own tests, extracted from
//! `auto_ingest.rs` so the 404-vs-410 call is decidable without a live
//! coordinator. The spawner in the parent is a thin loop over
//! [`heartbeat_verdict`].

/// How many consecutive heartbeats may fail to land before the donor
/// gives up on the coordinator. Same shape as `MAX_NEXT_UNIT_FAILURES`
/// above and for the same reason: a coordinator that is simply gone
/// never sends a 410, so silence is the only signal we get. Three at
/// `HEARTBEAT_INTERVAL` (one third of the lease) is exactly `LEASE_MS`
/// since the last beat that landed — the moment our lease is expired
/// and the reaper is free to give the unit to someone else. Ingesting
/// past that point writes into a lease we no longer hold.
const MAX_HEARTBEAT_MISSES: u32 = 3;

/// What one heartbeat POST came back as, owned and free of
/// `reqwest::Response` so [`heartbeat_verdict`] is decidable in a unit
/// test (ARCH §18.1 — a check with no failing input you can name is
/// not a check).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HeartbeatOutcome {
    /// The coordinator answered with this status.
    Answered(reqwest::StatusCode),
    /// No answer at all — connection refused, timeout, DNS. From the
    /// donor's side this is indistinguishable from a coordinator that
    /// has gone silent, which is why it counts rather than aborts.
    NoAnswer,
}

/// What the donor should do after one heartbeat. Mirrors
/// `commonwealth_knowledge::work_queue::HeartbeatResult` in vocabulary
/// (`Renewed`, and reclaimed-means-abort) WITHOUT depending on it:
/// `commonwealth-knowledge` was deliberately dropped from this crate in
/// cw-lift 1c (see Cargo.toml) and re-adding it to borrow two names
/// would relink the knowledge layer into the mesh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HeartbeatVerdict {
    /// Lease renewed; the miss counter resets.
    Renewed,
    /// Nothing conclusive. Keep ingesting and try the next tick.
    Miss,
    /// Stop ingesting — the lease is not ours any more.
    Abort(AbortCause),
}

impl HeartbeatVerdict {
    /// What the miss counter becomes after this verdict. Lives here so
    /// the spawner and its tests cannot drift apart on the reset —
    /// one decider for one rule (ARCH §10.6). Aborting ends the task,
    /// so the counter simply keeps its value for the warn line.
    pub(super) fn next_misses(self, consecutive_misses: u32) -> u32 {
        match self {
            HeartbeatVerdict::Renewed => 0,
            HeartbeatVerdict::Miss => consecutive_misses + 1,
            HeartbeatVerdict::Abort(_) => consecutive_misses,
        }
    }
}

/// Why the donor is aborting. Named in the warn line so a log reader
/// can tell a reclaimed lease from a vanished coordinator (ARCH §9.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AbortCause {
    /// 410 — the reaper handed our lease to someone else, or the
    /// handoff finished.
    Gone,
    /// 404 — the handoff itself is gone: a coordinator that restarted
    /// with empty state, or a reaped handoff. `commonwealth-api`
    /// answers `Reclaimed { reason: "handoff not found" }` at this
    /// status (`routes_internal/corpus_queue.rs`), which is correct —
    /// it was the donor that used to read it as "nothing happened".
    NotFound,
    /// `MAX_HEARTBEAT_MISSES` beats in a row failed to land.
    Silence,
}

/// The heartbeat decision, pure so it can be exercised without a live
/// coordinator. `consecutive_misses` is the count BEFORE this outcome,
/// so the caller keeps one counter and this function owns the limit.
pub(super) fn heartbeat_verdict(
    outcome: HeartbeatOutcome,
    consecutive_misses: u32,
) -> HeartbeatVerdict {
    match outcome {
        HeartbeatOutcome::Answered(s) if s == reqwest::StatusCode::GONE => {
            HeartbeatVerdict::Abort(AbortCause::Gone)
        }
        HeartbeatOutcome::Answered(s) if s == reqwest::StatusCode::NOT_FOUND => {
            HeartbeatVerdict::Abort(AbortCause::NotFound)
        }
        HeartbeatOutcome::Answered(s) if s.is_success() => HeartbeatVerdict::Renewed,
        // Every other status, and no answer at all, are the same thing
        // to the donor: this beat did not land. The lease may still be
        // ours, so one of them is not enough to throw the unit away —
        // but a run of them means nobody is listening.
        _ => {
            if consecutive_misses + 1 >= MAX_HEARTBEAT_MISSES {
                HeartbeatVerdict::Abort(AbortCause::Silence)
            } else {
                HeartbeatVerdict::Miss
            }
        }
    }
}

#[cfg(test)]
mod tests;
