// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gossip exchange + scheduling intent/plan endpoints.
//!
//! These three handlers form the cluster-coordination layer of the
//! internal API: pairwise mesh-state convergence (`/internal/gossip`),
//! scheduling lock acquisition (`/internal/scheduling/intent`), and
//! shard-plan broadcast (`/internal/scheduling/plan`).

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use commonwealth_core::mesh::GossipAuthArm;
use commonwealth_inference::inference_plan::InferencePlan;

use crate::state::AppState;

use super::MeshWire;

/// POST /internal/gossip — pairwise mesh-state exchange.
///
/// Symmetric: caller sends us their `Mesh` view, we merge it into ours
/// via `Mesh::merge_from` (per-member `last_seen` last-writer-wins),
/// and reply with our now-updated snapshot so the caller can merge it
/// in turn. After one round both sides have converged on the pairwise
/// union.
///
/// Rejects with 401 when the incoming `Mesh` has a different `mesh_id`
/// or `invite_key_hash` — the auth boundary. Any member with the join
/// key can gossip freely; outsiders can't inject.
pub async fn gossip(
    State(state): State<AppState>,
    Json(req): Json<GossipRequest>,
) -> Result<Json<GossipResponse>, (StatusCode, Json<GossipRejection>)> {
    let incoming = req.mesh.into_mesh();
    let self_node_id = *state.inner.self_node_id_swap.load_full().as_ref();
    let now_secs = state.clock().now_unix_secs();
    let auth = commonwealth_core::mesh::GossipAuth {
        sender: req.from,
        proof: req.mesh_proof.clone(),
        now_secs,
    };
    let mut mesh = state.inner.mesh.write().await;
    let report = mesh.merge_from_authenticated(self_node_id, &incoming, &auth);

    if report.rejected() {
        tracing::warn!("gossip: rejected — mesh_id or invite_key_hash mismatch");
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(GossipRejection {
                reason: "mesh_id or invite_key_hash does not match".into(),
            }),
        ));
    }

    // Stamp local-observation time for every peer whose record advanced, so
    // offline-decay measures staleness against our own clock (not the peer's
    // gossiped `last_seen`). See `AppState::observe_peer_contact`.
    let now_local = state.clock().now_unix_secs();
    for observed_id in report.observed() {
        state.observe_peer_contact(*observed_id, now_local);
    }

    if report.added() > 0 || report.updated() > 0 {
        // Info only when a NEW member was added. `updated > 0`
        // alone is the routine last_seen refresh that fires every
        // 10s — noisy heartbeat, not an event. Debug-level so
        // operators can still see it with a tighter filter.
        if report.added() > 0 {
            tracing::info!(
                added = report.added(),
                updated = report.updated(),
                members = mesh.members.len(),
                "gossip: member added via incoming delta"
            );
        } else {
            tracing::debug!(
                updated = report.updated(),
                members = mesh.members.len(),
                "gossip: merged incoming delta (last_seen refresh)"
            );
        }
        // Persist immediately on any added/updated member. The
        // gossip loop re-persists on its own cadence too, but that
        // leaves a 10s window where the founder could restart and
        // forget a newly-admitted joiner. Only fire on actual
        // deltas — no point re-writing mesh.json for a last_seen
        // bump that changed nothing structural.
        if let Some(hook) = state.inner.on_mesh_mutation.as_ref() {
            hook(&mesh, self_node_id);
        }
    }

    // Never hand `mesh_secret` back to a caller that authorized on the LEGACY
    // arm.
    //
    // `gossip_authorized` falls through to comparing `invite_key_hash` whenever
    // EITHER side's secret is zeroed — including when ours is set and the
    // caller simply omits the field. So the caller, not us, chooses the weaker
    // predicate, and its entry ticket is an `invite_key_hash` that rides every
    // gossip payload and every join snapshot. Replying with the real secret
    // would let anyone holding a current-or-stale invite hash upgrade
    // themselves to permanent gossip auth: `mesh_secret` never rotates and
    // `rotate_invite_key` is structurally unable to change it, so there is no
    // revocation path afterwards. That is strictly worse than the pre-split
    // model, where rotating the key DID revoke a departed member.
    //
    // Redacting costs a genuine pre-split peer nothing: its build has no such
    // field and drops it on receive. It only denies the upgrade.
    //
    // The reply carries the raw secret on exactly ONE arm — the caller
    // compared raw secrets, so its build authorizes our reply the same way and
    // withholding would partition it. Every other arm is redacted:
    //
    // - `Proof`: the caller already proved it holds the secret. Sending it
    //   back is a pure leak, and leaving it here is what kept the credential
    //   on the wire every 10s even between two fully upgraded nodes — the
    //   request half of P4b stopped sending it and the reply half did not.
    // - `Legacy`: the security rule above. Never, at any point.
    let mut wire = MeshWire::from(&*mesh);
    if report.auth_arm() != GossipAuthArm::RawSecret {
        wire.mesh_secret = [0u8; 32];
        tracing::debug!(
            mesh = %mesh.name,
            arm = ?report.auth_arm(),
            "gossip: redacted mesh_secret from the reply"
        );
    }
    // Our own proof, so the caller can authorize this REPLY without needing our
    // raw secret either. Both directions or neither — a reply the caller must
    // fall back to raw-secret comparison for keeps the credential on the wire.
    let proof = mesh.mesh_proof(self_node_id, now_secs);
    // The inbound path can finally attribute what it merged: `req.from` names
    // the sender, so a peer that only ever pushes to us — never one we dial —
    // is now confirmable by the rotate guard. That asymmetry is why BeefyMac
    // blocked rotation while online despite nothing being wrong with it.
    if let Some(sender) = req.from {
        if !report.rejected() {
            state.observe_peer_split_generation(sender, !report.peer_pre_split());
        }
    }
    Ok(Json(GossipResponse {
        mesh: wire,
        from: Some(self_node_id),
        mesh_proof: proof,
    }))
}

#[derive(Debug, Deserialize)]
pub struct GossipRequest {
    pub mesh: MeshWire,
    /// Who is sending. Absent on a pre-proof peer.
    ///
    /// Closes two gaps at once. It binds [`GossipRequest::mesh_proof`] to a
    /// sender, so a captured proof cannot be presented by another node; and it
    /// is the attribution the inbound path never had, which is why the rotate
    /// guard could only ever confirm peers this node dialled outbound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<commonwealth_core::ids::NodeId>,
    /// Proof of `mesh_secret` possession — see `Mesh::mesh_proof`. Lets an
    /// upgraded pair authorize without the raw credential ever crossing the
    /// wire. Absent on a pre-proof peer, which falls through to comparing raw
    /// secrets exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh_proof: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GossipResponse {
    pub mesh: MeshWire,
    /// The responder's identity and proof. The CALLER merges this response, so
    /// it is an authorization boundary in its own direction and needs the same
    /// evidence — a reply is not trusted just because we initiated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<commonwealth_core::ids::NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh_proof: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GossipRejection {
    pub reason: String,
}

/// POST /internal/scheduling/intent — scheduling lock acquisition.
pub async fn scheduling_intent(
    State(_state): State<AppState>,
    Json(_payload): Json<SchedulingIntent>,
) -> (StatusCode, Json<SchedulingIntentResponse>) {
    (
        StatusCode::OK,
        Json(SchedulingIntentResponse {
            granted: true,
            leader: String::new(),
        }),
    )
}

/// POST /internal/scheduling/plan — shard plan broadcast.
///
/// Peer nodes call this when they compute a new inference plan.
/// The plan is stored in MeshStore and propagated via gossip.
pub async fn scheduling_plan(
    State(state): State<AppState>,
    Json(plan): Json<InferencePlan>,
) -> StatusCode {
    state.inner.inference_store.set_plan(&plan);
    StatusCode::OK
}

#[derive(Debug, Deserialize)]
pub struct SchedulingIntent {
    pub node_id: String,
    pub intent: String,
}

#[derive(Debug, Serialize)]
pub struct SchedulingIntentResponse {
    pub granted: bool,
    pub leader: String,
}
