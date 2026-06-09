// SPDX-License-Identifier: AGPL-3.0-or-later
//! `MeshBroadcaster` — the production implementation of
//! [`sovereign_work_atlas::tools::ClaimBroadcaster`].
//!
//! Lives in `sovereign-mesh` (not in `sovereign-work-atlas`) so the
//! trait surface stays free of `AppState`. The dep direction is
//! work-atlas → (nothing extra); mesh → work-atlas. That keeps
//! work-atlas re-usable from contexts where AppState isn't in scope
//! (the CLI tools registry uses `NullBroadcaster` for the same
//! reason).
//!
//! Privacy: `broadcast_now` itself rejects calls with
//! `is_gossip_excluded(app_id) == true`, so a private app_id slipping
//! through here would still not leave the box. The work-atlas tools
//! also gate broadcast on `Privacy::Public` before calling. Two
//! independent locks on the invariant.

use async_trait::async_trait;
use commonwealth_api::state::AppState;
use sovereign_work_atlas::tools::ClaimBroadcaster;

use crate::gossip::broadcast_now;

/// Wraps `AppState` + the gossip layer so the work-atlas tools can
/// kick off an immediate fan-out without taking a direct dep on
/// `AppState`. `AppState` is `Clone` over an internal `Arc`, so the
/// stored value is cheap to copy and survives daemon state transitions.
pub struct MeshBroadcaster {
    app_state: AppState,
}

impl MeshBroadcaster {
    pub fn new(app_state: AppState) -> Self {
        Self { app_state }
    }
}

impl std::fmt::Debug for MeshBroadcaster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeshBroadcaster").finish_non_exhaustive()
    }
}

#[async_trait]
impl ClaimBroadcaster for MeshBroadcaster {
    async fn broadcast(&self, app_id: &str, key: &str) {
        // Fire-and-forget. `broadcast_now` already spawns a task per
        // peer and logs failures at warn — nothing useful to return.
        broadcast_now(&self.app_state, app_id, key).await;
    }
}
