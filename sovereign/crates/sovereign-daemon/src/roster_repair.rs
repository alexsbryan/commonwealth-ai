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
use tracing::info;

use crate::daemon::{EmbeddedDaemon, MeshError};
use crate::loopback_guard::LocalOnly;
use crate::rails_client::{self, RailsDial};

/// What retiring one member row reports. The type lives with the verb in
/// `commonwealth_core::mesh_identity` and is re-exported through
/// `sovereign_mesh::fabric`; this path is the CLI's historical spelling.
pub use sovereign_mesh::fabric::ForgottenMember;

impl EmbeddedDaemon {
    /// Retire one member row — by DIALING the mesh's serving process
    /// (FIVE_PROGRAMS fp-6 / §12 decision 2: the mesh owns the roster, and a
    /// daemon mutating its own copy is a component holding another's
    /// lifecycle). The tombstone is written where the roster's readers
    /// converge, persisted there, and carried back to this daemon by the
    /// ordinary gossip round.
    ///
    /// Until fp-6 this called `FabricPart::forget_member` in-process; the
    /// why of the verb (the closure loop, the tombstone rather than a
    /// delete) moved with the implementation to
    /// `commonwealth_core::mesh_identity::Mesh::forget_member`, which the
    /// serving route calls. What stays here is the boundary: absence is
    /// REPORTED (`ServingUnreachable` — the serving process is a required
    /// service per the TSV's behaviour delta), and the named refusal arms
    /// map back onto [`MeshError`] so the CLI's answers do not change.
    pub async fn forget_member(
        &self,
        query: &str,
        force: bool,
    ) -> Result<ForgottenMember, MeshError> {
        let app_state = self.app_state().await.ok_or(MeshError::NotRunning)?;
        let base = app_state.inner.node.rails_base.clone();

        let outcome = rails_client::forget_member(&base, query, force)
            .await
            .map_err(|e| match e {
                RailsDial::Absent { .. } => MeshError::ServingUnreachable(base.clone()),
                RailsDial::Unreadable { base, detail } => {
                    MeshError::Network(format!("unreadable answer from {base}: {detail}"))
                }
                RailsDial::Refused { kind, message, .. } => match kind.as_deref() {
                    // The two arms this daemon can reconstruct exactly: the
                    // sentences are one implementation's (`Mesh::forget_member`
                    // owns them), so the mapped error reads as before.
                    Some("unknown-member") => MeshError::UnknownMember(query.to_string()),
                    Some("cannot-forget-self") => MeshError::CannotForgetSelf,
                    _ => MeshError::RefusedByServing(message),
                },
            })?;

        info!(
            member = %outcome.name,
            node_id = %outcome.node_id,
            was_aliased = outcome.was_aliased,
            already_retired = outcome.already_retired,
            base = %base,
            "forget-member: retired on the mesh's serving process; gossip carries the tombstone"
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
        // The mesh's serving process did not answer. 503, and the sentence
        // names it: the roster is served by cw-rails, and this route holds no
        // answer of its own (principle 6 — absence reported, never defaulted).
        Err(e @ MeshError::ServingUnreachable(_)) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
        Err(e @ MeshError::NotRunning) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
        // CannotForgetSelf, MemberStillLive and RefusedByServing are all
        // "the request is coherent but we will not do it" — 409, not 400:
        // nothing about the syntax is wrong, the roster's state is what
        // refuses.
        Err(e) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
