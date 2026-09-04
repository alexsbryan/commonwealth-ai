// SPDX-License-Identifier: AGPL-3.0-or-later
pub mod guest_grant;
pub mod ingest_grant;
/// The ring rail — MOVED OUT on 2026-09-04 (campaign cw-lift, order 1b) into
/// `commonwealth-rail-core` (the fold) and `commonwealth-rail` (the journal).
///
/// This is a re-export and nothing else, so that the move and the repoint of
/// its eight consumers are two commits rather than one 800-line diff. Order
/// 1c deletes it; import `commonwealth_rail` directly in new code.
pub mod rail {
    pub use commonwealth_rail::*;
}
pub mod shard_manager;
pub mod store_adapter;
pub mod work_queue;

pub use guest_grant::{GuestGrant, GuestGrantStore, Scope};
pub use ingest_grant::{EphemeralGrantStore, EphemeralIngestGrant};
pub use rail::{
    admit, Admission, AdmittedOp, Payload, PayloadError, Person, RailAct, RailError, RailGap,
    RingJournal, RingRail, RingSigner, Roster,
};
pub use shard_manager::{verify_merge_sample, ShardManager, VerifyReport};
pub use store_adapter::KnowledgeStateStore;
pub use work_queue::{
    HandoffQueue, HeartbeatResult, LeasedUnit, QueueError, ReapStats, WorkQueueManager,
};
