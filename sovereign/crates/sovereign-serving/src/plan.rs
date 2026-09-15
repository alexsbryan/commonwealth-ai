// SPDX-License-Identifier: AGPL-3.0-or-later
//! Mesh-level inference plan types.
//!
//! [`MeshPlan`] once carried the complete inference topology — the scheduling
//! strategy, node roles, request router and tier queue depths. That routing
//! vocabulary had no caller outside this crate's own definitions and tests and
//! was deleted 2026-09-14; the fields naming it went with it, leaving the
//! plan's identity and version.

use serde::{Deserialize, Serialize};

use commonwealth_core::ids::PlanId;

// ─── MeshPlan ────────────────────────────────────────────────

/// The identity and version of the current mesh plan.
///
/// Its topology fields (trigger, strategy, node roles, router) named the dead
/// routing vocabulary deleted 2026-09-14. What remains is what a reader uses:
/// who wrote it, when, and how fresh it is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshPlan {
    pub id: PlanId,
    pub computed_at: chrono::DateTime<chrono::Utc>,
    /// Version counter — monotonically increasing.
    /// Nodes reject plans with lower version than the one they hold.
    pub version: u64,
}
