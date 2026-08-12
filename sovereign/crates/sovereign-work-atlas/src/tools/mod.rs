// SPDX-License-Identifier: AGPL-3.0-or-later
//! MCP tools for the work atlas.
//!
//! Phase 1 exposes three tools:
//! - [`DeclareScope`] — write a claim, broadcast immediately.
//! - [`ReleaseScope`] — drop a claim (no history).
//! - [`WorkInFlight`] — read overlapping live work for a scope.
//!
//! Plus [`ResourceMayI`] (order `seat-resource-commons`): the
//! one-question read surface for shared-resource claims — including
//! EXPIRED ones, which `work_in_flight` filters away at read time.

pub mod broadcast;
pub mod declare_scope;
pub mod release_scope;
pub mod resource_may_i;
pub mod work_in_flight;

pub use broadcast::{ClaimBroadcaster, DeferredBroadcaster, NullBroadcaster};
pub use declare_scope::DeclareScopeTool;
pub use release_scope::ReleaseScopeTool;
pub use resource_may_i::{
    resource_verdict, ResourceMayITool, ResourceVerdict, DEFAULT_RESOURCE_TTL_SECS,
};
pub use work_in_flight::{collect_in_flight, InFlight, WorkInFlightTool};
