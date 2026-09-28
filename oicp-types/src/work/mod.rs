// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `work` plane's vocabulary — the acts, the folded queue's types, the
//! refusal set and the `process:v1` payload — beside [`crate::job`]'s
//! `JobKind`/`JobRequirements`/`JobUnit`.
//!
//! Moved here from `commonwealth-work` (and the two lease constants from
//! `commonwealth-core::knowledge`) by pb-work-doors (FIVE_PROGRAMS §12 3a
//! rung 2): a client that submits work and reads the queue speaks these on a
//! wire and links no rail. The decisions stay with the rail — the codec over
//! rail-core's `Payload`, the seal, the fold and `may_take` are
//! `commonwealth-work`'s, which re-exports every item here at its historical
//! path.

pub mod act;
pub mod process;
pub mod projection;
pub mod refusal;
mod wire;

pub use act::{
    Completion, Failure, Revocation, Submission, UnitRef, WorkAct, WorkActKind, DEFAULT_TTL_SECS,
    MAX_TTL_SECS, MIN_TTL_SECS,
};
pub use process::{ProcessPayload, ResultSource, PROCESS_KIND};
pub use projection::{
    LeaseState, LostLease, ProjectedUnit, ReapStats, WorkHandoff, WorkProjection, WorkUnitStatus,
};
pub use refusal::{GrantSide, UnmetRequirement, WorkRefusal};

/// Maximum re-lease attempts before a unit becomes terminal `Failed`.
/// Unit fails three peers in a row → the merge leader proceeds without it.
pub const MAX_UNIT_ATTEMPTS: u32 = 3;

/// Default lease duration in milliseconds (5 minutes). Heartbeats refresh
/// the lease every `LEASE_MS / 3` on the peer side.
pub const LEASE_MS: u64 = 300_000;
