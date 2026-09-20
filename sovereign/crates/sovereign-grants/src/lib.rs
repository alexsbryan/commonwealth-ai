// SPDX-License-Identifier: AGPL-3.0-or-later
pub mod auto_recover;
pub mod guest_grant;
pub mod guest_session;
pub mod ingest_grant;
pub mod knowledge_assignment;
pub mod shard_manager;
pub mod work_queue;

pub use auto_recover::{FoldRecovery, RecoveryOutcome};

pub use guest_grant::{GuestGrant, GuestGrantStore, Scope};
pub use guest_session::{GuestSession, GuestSessionStore, NameHeld};
pub use ingest_grant::{EphemeralGrantStore, EphemeralIngestGrant};
pub use shard_manager::{verify_merge_sample, MergePlan, ShardManager, VerifyReport};
pub use work_queue::{
    HandoffQueue, HeartbeatResult, LeasedUnit, QueueError, ReapStats, WorkQueueManager,
};
