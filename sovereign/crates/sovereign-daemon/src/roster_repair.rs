// SPDX-License-Identifier: AGPL-3.0-or-later
//! Roster repair: retiring one member row, and the route and resolver that
//! reach it.
//!
//! The third of the endpoint-key loop. The rule
//! ([`commonwealth_core::mesh::aliased_endpoint_keys`]) could be CHECKED by
//! the DST pack and ENFORCED at gossip admission, but a roster that already
//! held a collision had no repair — `svrn mesh` could forget a whole parked
//! mesh or leave the active one, and nothing in between. So a confirmed
//! collision stayed broken and every read through it — liveness, rotate's
//! online-peer guard, guest routing — stayed wrong.
//!
//! Split out of `daemon.rs` and `mesh_http.rs` rather than added to them:
//! both are long past ARCH §3.1's ceiling, and this is one concern with a
//! seam of its own. It is the sovereign-side counterpart to
//! `commonwealth-core/src/mesh_identity.rs`, which owns the rule itself.

use std::sync::Arc;

use axum::extract::{Extension, Json};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use tracing::{info, warn};

use crate::daemon::{EmbeddedDaemon, MeshError};
use crate::loopback_guard::LocalOnly;
use sovereign_mesh::persist;

/// What [`EmbeddedDaemon::forget_member`] retired. The type moved to Fabric
/// with its tombstone core at domains `REVIEW-build-daemon-membership-lifecycle`
/// (DC §4.1: the roster mutations are Fabric's); re-exported here so the CLI's
/// `roster_repair::ForgottenMember` path keeps resolving.
pub use sovereign_mesh::fabric::ForgottenMember;

impl EmbeddedDaemon {
    /// Retire one member row: tombstone it locally and let the ordinary
    /// gossip round carry the removal mesh-wide.
    ///
    /// # The closure loop for an endpoint-key collision
    ///
    /// `merge_from_authenticated` now REFUSES to create an alias, but a
    /// roster that already holds one had no repair: `svrn mesh` could forget a
    /// whole parked mesh or leave the active one, and nothing in between. So
    /// the confirmed `BeefyMac`/`Alexs-MacBook-Pro-2` collision on mesh
    /// `27ba8166…` was diagnosable and not fixable, and every read through
    /// that roster — liveness, rotate's online-peer guard, guest routing —
    /// stayed wrong. A rule that can be checked and enforced but not repaired
    /// is two thirds of a loop.
    ///
    /// # Why a tombstone rather than a delete
    ///
    /// Deleting the row locally would work until the next gossip round, when
    /// a peer still holding it hands it straight back. `removed_at` is the
    /// mesh's removal primitive and it converges: it wins the
    /// [`commonwealth_core::mesh::MemberRecord::effective_at`] LWW against
    /// any older `last_seen`, and it is what `leave` already uses.
    ///
    /// It is also self-limiting in the right way. A GHOST — a stale row for a
    /// machine that re-registered under a new node_id — has nothing left to
    /// defend it, so the tombstone sticks. A row belonging to a daemon that
    /// is genuinely alive gets re-announced on that node's next round, since
    /// a node is authoritative for itself. The repair therefore cannot evict
    /// a live member even by mistake, which is why `force` is a guard against
    /// operator surprise rather than against damage.
    ///
    /// The roster mutation itself is Fabric's
    /// ([`sovereign_mesh::fabric::FabricPart::forget_member`]); the daemon
    /// maps Fabric's refusal onto its own [`MeshError`] and persists the
    /// tombstone so a restart does not resurrect the row before gossip carries
    /// it (DC §4.1 "`MeshError` maps at the daemon boundary").
    pub async fn forget_member(
        &self,
        query: &str,
        force: bool,
    ) -> Result<ForgottenMember, MeshError> {
        let app_state = self.app_state().await.ok_or(MeshError::NotRunning)?;
        let self_id = app_state.self_node_id();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let outcome = app_state
            .inner
            .fabric
            .forget_member(query, force, now)
            .await
            .map_err(|e| match e {
                sovereign_mesh::fabric::ForgetMemberError::UnknownMember(q) => {
                    MeshError::UnknownMember(q)
                }
                sovereign_mesh::fabric::ForgetMemberError::CannotForgetSelf => {
                    MeshError::CannotForgetSelf
                }
                sovereign_mesh::fabric::ForgetMemberError::MemberStillLive(n) => {
                    MeshError::MemberStillLive(n)
                }
            })?;

        if !outcome.already_retired && self.persistence_enabled() {
            let mesh = app_state.inner.fabric.mesh.read().await;
            if let Err(e) = persist::save(self.data_dir(), &mesh, self_id) {
                warn!(error = %e, "forget-member: mesh.json could not be written");
            }
        }

        info!(
            member = %outcome.name,
            node_id = %outcome.node_id,
            was_aliased = outcome.was_aliased,
            already_retired = outcome.already_retired,
            "forget-member: member row retired; gossip carries the tombstone"
        );
        Ok(outcome)
    }
}

/// Request body for `POST /v1/mesh/forget-member`.
#[derive(Debug, Deserialize)]
pub struct ForgetMemberRequest {
    /// Member name, or a node_id prefix of at least 4 hex characters.
    pub member: String,
    /// Retire the row even though the member is online and not aliased.
    #[serde(default)]
    pub force: bool,
}

/// `POST /v1/mesh/forget-member` — retire one member row.
///
/// The repair half of the endpoint-key rule: `merge_from_authenticated`
/// refuses to CREATE a collision, this retires one that already exists. See
/// [`EmbeddedDaemon::forget_member`] for why it tombstones rather than
/// deletes, and why it cannot evict a live member.
pub async fn mesh_forget_member(
    _: LocalOnly,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(req): Json<ForgetMemberRequest>,
) -> impl IntoResponse {
    match daemon.forget_member(&req.member, req.force).await {
        Ok(outcome) => (StatusCode::OK, Json(serde_json::json!(outcome))).into_response(),
        Err(e @ MeshError::UnknownMember(_)) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
        Err(e @ MeshError::NotRunning) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
        // CannotForgetSelf and MemberStillLive are both "the request is
        // coherent but we will not do it" — 409, not 400: nothing about the
        // syntax is wrong, the roster's state is what refuses.
        Err(e) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
