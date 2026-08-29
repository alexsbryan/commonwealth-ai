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

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension, Json};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use tracing::{info, warn};

use crate::daemon::{EmbeddedDaemon, MeshError};
use crate::loopback_guard::enforce_localhost;
use crate::persist;

/// What [`EmbeddedDaemon::forget_member`] retired.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ForgottenMember {
    pub name: String,
    pub node_id: commonwealth_core::ids::NodeId,
    /// The row was one of a colliding pair — this call was a repair rather
    /// than a removal. Reported so the CLI can say which it did.
    pub was_aliased: bool,
    /// Already a tombstone when we got here; nothing was written. Distinct
    /// from a fresh retirement so a caller never reports work it did not do.
    pub already_retired: bool,
}

/// Resolve an operator's `<node>` argument against a member row: exact
/// name, or a node_id prefix of at least 4 hex characters.
///
/// A prefix shorter than 4 is refused rather than matched loosely — a
/// one-character prefix against a 16-character id is very nearly "retire an
/// arbitrary member", and this command writes a tombstone.
fn member_matches(node_id: commonwealth_core::ids::NodeId, name: &str, query: &str) -> bool {
    if name == query {
        return true;
    }
    let id = node_id.to_string();
    let q = query.trim_start_matches("node-");
    q.len() >= 4 && id.trim_start_matches("node-").starts_with(q)
}

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

        let outcome = {
            let mut mesh = app_state.inner.mesh.write().await;

            let aliased: std::collections::HashSet<commonwealth_core::ids::NodeId> = mesh
                .aliased_endpoint_keys()
                .into_iter()
                .flat_map(|a| a.members.into_iter().map(|(id, _)| id))
                .collect();

            let target = mesh
                .members
                .values()
                .find(|m| member_matches(m.node_id, &m.name, query))
                .map(|m| m.node_id)
                .ok_or_else(|| MeshError::UnknownMember(query.to_string()))?;

            if target == self_id {
                return Err(MeshError::CannotForgetSelf);
            }

            let record = mesh.members.get(&target).expect("just resolved");
            let was_aliased = aliased.contains(&target);
            let name = record.name.clone();

            if !record.is_active() {
                // Idempotent: already retired, nothing to do and nothing to
                // report as if it had happened.
                ForgottenMember {
                    name,
                    node_id: target,
                    was_aliased,
                    already_retired: true,
                }
            } else {
                let live = record.status == commonwealth_core::mesh::NodeStatus::Online;
                if live && !was_aliased && !force {
                    return Err(MeshError::MemberStillLive(name));
                }
                let record = mesh.members.get_mut(&target).expect("just resolved");
                record.removed_at = Some(now);
                record.last_seen = record.last_seen.max(now);
                record.status = commonwealth_core::mesh::NodeStatus::Offline;
                ForgottenMember {
                    name,
                    node_id: target,
                    was_aliased,
                    already_retired: false,
                }
            }
        };

        if !outcome.already_retired && self.persistence_enabled() {
            let mesh = app_state.inner.mesh.read().await;
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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(req): Json<ForgetMemberRequest>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The resolver's safety rule, watched failing. `forget-member` WRITES a
    /// tombstone, so a loose match is not a usability nicety — a
    /// one-character prefix against a 16-character id is very nearly "retire
    /// an arbitrary member". Four is the floor.
    #[test]
    fn a_short_node_id_prefix_never_resolves_a_member() {
        let id = commonwealth_core::ids::NodeId::from_u128(0xb88252e400000000_0000000000000000);
        let m = |q: &str| member_matches(id, "BeefyMac", q);
        assert!(!m("b"), "1 char must not match");
        assert!(!m("b88"), "3 chars must not match");
        assert!(m("b882"), "4 chars is the floor");
        assert!(m("node-b882"), "the node- prefix is optional");
        assert!(m("BeefyMac"), "exact name matches");
        assert!(!m("Beefy"), "a partial NAME must not match");
        assert!(!m("b883"), "a wrong prefix must not match");
    }

    /// An empty query must never match. It reaches here as `--force` with no
    /// member, and matching everything would retire whichever row the
    /// iteration happened to reach first.
    #[test]
    fn an_empty_query_matches_nothing() {
        let id = commonwealth_core::ids::NodeId::from_u128(0xb88252e400000000_0000000000000000);
        assert!(!member_matches(id, "BeefyMac", ""));
    }
}
