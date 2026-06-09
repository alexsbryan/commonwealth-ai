// SPDX-License-Identifier: AGPL-3.0-or-later
//! MCP tools for the work atlas.
//!
//! Phase 1 exposes three tools:
//! - [`DeclareScope`] — write a claim, broadcast immediately.
//! - [`ReleaseScope`] — drop a claim (no history).
//! - [`WorkInFlight`] — read overlapping live work for a scope.

pub mod broadcast;
pub mod declare_scope;
pub mod release_scope;
pub mod work_in_flight;

pub use broadcast::{ClaimBroadcaster, DeferredBroadcaster, NullBroadcaster};
pub use declare_scope::DeclareScopeTool;
pub use release_scope::ReleaseScopeTool;
pub use work_in_flight::WorkInFlightTool;
