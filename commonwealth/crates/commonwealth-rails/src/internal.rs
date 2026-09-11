// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /internal/gossip` — the one route other daemons dial, and the only
//! inbound HTTP this process serves to the mesh.
//!
//! **Without it this member vanishes.** A full daemon dials every member each
//! round; one that never answers stops refreshing that daemon's local contact
//! clock, and within its offline threshold the member is marked `Offline` —
//! at which point `pick_member` refuses it by name and it drops out of
//! `svrn mesh media` entirely. Answering is not an optimisation, it is what
//! being on the roster means.
//!
//! It is a mirror of `commonwealth-api`'s handler with the `AppState` taken
//! out, and every decision in it belongs to `commonwealth_core::mesh`: the
//! merge authorizes, the merge reports, and the disclosure arm is read off
//! the report. Nothing here re-derives who may gossip.
//!
//! The listener binds `127.0.0.1:0` and is reachable only through the
//! acceptor's splice, so a caller on this port has already completed a QUIC
//! handshake with a verified key. It still refuses a payload that is not this
//! mesh's, because the acceptor deliberately admits any dialer on the gossip
//! ALPN (a joiner is not yet a member).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use commonwealth_core::clock::unix_now_secs;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::wire::{GossipRejection, GossipRequest, GossipResponse};
use commonwealth_core::mesh::{GossipAuth, GossipAuthArm, Mesh, MeshWire, SecretDisclosure};
use tokio::sync::{Mutex, RwLock};

use crate::{identity, note_contact, Refusal};

#[derive(Clone)]
pub struct Inbound {
    pub mesh: Arc<RwLock<Mesh>>,
    pub contacts: Arc<Mutex<HashMap<NodeId, u64>>>,
    pub self_id: NodeId,
    pub data_dir: PathBuf,
}

/// Bind the internal listener on an ephemeral loopback port and serve.
/// Returns the address the acceptor splices to.
pub async fn serve(
    mesh: Arc<RwLock<Mesh>>,
    contacts: Arc<Mutex<HashMap<NodeId, u64>>>,
    self_id: NodeId,
    data_dir: PathBuf,
) -> Result<(SocketAddr, tokio::task::JoinHandle<()>), Refusal> {
    let bind: SocketAddr = ([127, 0, 0, 1], 0).into();
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| Refusal::Listen(bind, e))?;
    let addr = listener
        .local_addr()
        .map_err(|e| Refusal::Listen(bind, e))?;
    let app = router(Inbound {
        mesh,
        contacts,
        self_id,
        data_dir,
    });
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(target: "rails", error = %e, "internal: listener stopped");
        }
    });
    tracing::info!(target: "rails", addr = %addr, "internal: /internal/gossip listening");
    Ok((addr, task))
}

pub fn router(state: Inbound) -> Router {
    Router::new()
        .route("/internal/gossip", post(gossip))
        .with_state(state)
}

/// Merge the caller's view, reply with ours. Symmetric: after one round both
/// sides hold the pairwise union.
pub async fn gossip(
    State(state): State<Inbound>,
    Json(req): Json<GossipRequest>,
) -> Result<Json<GossipResponse>, (StatusCode, Json<GossipRejection>)> {
    let now = unix_now_secs();
    let incoming = req.mesh.into_mesh();
    let auth = GossipAuth {
        sender: req.from,
        proof: req.mesh_proof.clone(),
        now_secs: now,
    };
    let mut mesh = state.mesh.write().await;
    let report = mesh.merge_from_authenticated(state.self_id, &incoming, &auth);

    if report.rejected() {
        tracing::warn!(
            target: "gossip",
            from = ?req.from,
            "gossip: REFUSED an inbound round — mesh_id or invite_key_hash does not match ours"
        );
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(GossipRejection {
                reason: "mesh_id or invite_key_hash does not match".into(),
            }),
        ));
    }

    // The sender and everyone whose record advanced are peers we have heard
    // from on OUR clock. That is what the decay pass measures.
    for observed in report.observed() {
        note_contact(&state.contacts, *observed, now).await;
    }
    if let Some(sender) = req.from {
        note_contact(&state.contacts, sender, now).await;
    }

    if report.added() > 0 || report.updated() > 0 {
        if report.added() > 0 {
            tracing::info!(
                target: "gossip",
                added = report.added(),
                updated = report.updated(),
                members = mesh.members.len(),
                "gossip: member added from an inbound round"
            );
        } else {
            tracing::debug!(
                target: "gossip",
                updated = report.updated(),
                members = mesh.members.len(),
                "gossip: merged an inbound delta (last_seen refresh)"
            );
        }
        // Persist on a real delta, not on the 10-second heartbeat. Without
        // this a restart inside the round interval forgets a member we have
        // just learned of, and nothing tells us it did.
        if let Err(e) = identity::save_mesh(&state.data_dir, &mesh) {
            tracing::warn!(target: "rails", error = %e, "gossip: could not persist the mesh");
        }
    }

    // The raw credential rides the reply on exactly ONE arm — the caller
    // compared raw secrets, so its build authorizes our reply the same way
    // and withholding would partition it. `Proof` already holds the secret
    // (sending it back is a pure leak) and `Legacy` must never receive it:
    // an `invite_key_hash` rides every payload, so a holder of a stale invite
    // would otherwise upgrade itself to permanent gossip auth that no
    // rotation can revoke.
    let disclosure = if report.auth_arm() == GossipAuthArm::RawSecret {
        SecretDisclosure::Disclose
    } else {
        SecretDisclosure::Redact
    };
    let wire = MeshWire::for_peer(&mesh, disclosure);
    let proof = mesh.mesh_proof(state.self_id, now);
    tracing::debug!(
        target: "gossip",
        from = ?req.from,
        arm = ?report.auth_arm(),
        members = mesh.members.len(),
        "gossip: answered an inbound round"
    );
    Ok(Json(GossipResponse {
        mesh: wire,
        from: Some(state.self_id),
        mesh_proof: proof,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_core::ids::MeshId;

    fn state(mesh: Mesh) -> Inbound {
        Inbound {
            self_id: *mesh.members.keys().next().expect("a member"),
            mesh: Arc::new(RwLock::new(mesh)),
            contacts: Arc::new(Mutex::new(HashMap::new())),
            data_dir: std::env::temp_dir().join(format!("cw-rails-test-{}", std::process::id())),
        }
    }

    fn request(mesh: &Mesh, from: NodeId, now: u64) -> GossipRequest {
        GossipRequest {
            mesh: MeshWire::for_peer(mesh, SecretDisclosure::Redact),
            from: Some(from),
            mesh_proof: mesh.mesh_proof(from, now),
        }
    }

    /// The happy path: a peer on the same mesh is merged and answered, and
    /// its contact is noted on OUR clock.
    #[tokio::test]
    async fn a_round_from_the_same_mesh_is_merged_and_answered() {
        let (mesh, _key) =
            commonwealth_discovery::membership::init_mesh("Lab", "founder", Vec::new());
        let st = state(mesh.clone());
        let sender = *mesh.members.keys().next().unwrap();
        let out = gossip(
            State(st.clone()),
            Json(request(&mesh, sender, unix_now_secs())),
        )
        .await
        .expect("same mesh, same invite hash");
        assert_eq!(out.0.from, Some(st.self_id));
        assert!(
            st.contacts.lock().await.contains_key(&sender),
            "the sender's contact is noted on our clock"
        );
    }

    /// **The failing input.** A payload whose invite hash is not ours is a
    /// stranger past the QUIC handshake — the acceptor admits any dialer on
    /// the gossip ALPN on purpose, so this is the boundary. It must be a 401
    /// that merges NOTHING; a version that merged first and refused after
    /// would let a stranger inject members.
    #[tokio::test]
    async fn a_round_from_another_mesh_is_401_and_merges_nothing() {
        let (ours, _k1) =
            commonwealth_discovery::membership::init_mesh("Lab", "founder", Vec::new());
        let (mut theirs, _k2) =
            commonwealth_discovery::membership::init_mesh("Elsewhere", "stranger", Vec::new());
        // Same mesh id, different credentials — the hostile case, not the
        // merely-mistyped one: an id match alone must not authorize.
        theirs.id = ours.id;
        let st = state(ours);
        let before = st.mesh.read().await.members.len();
        let stranger = *theirs.members.keys().next().unwrap();
        let err = gossip(
            State(st.clone()),
            Json(request(&theirs, stranger, unix_now_secs())),
        )
        .await
        .expect_err("a different invite hash must be refused");
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
        assert_eq!(
            st.mesh.read().await.members.len(),
            before,
            "a refused round adds nobody"
        );
        assert!(
            !st.contacts.lock().await.contains_key(&stranger),
            "a refused sender is not a contact"
        );
    }

    /// A mesh id that does not match is refused for the same reason, and the
    /// test exists separately because the two predicates are separate.
    #[tokio::test]
    async fn a_round_for_a_different_mesh_id_is_401() {
        let (ours, _k) =
            commonwealth_discovery::membership::init_mesh("Lab", "founder", Vec::new());
        let mut theirs = ours.clone();
        theirs.id = MeshId::generate();
        let st = state(ours);
        let sender = *theirs.members.keys().next().unwrap();
        let err = gossip(State(st), Json(request(&theirs, sender, unix_now_secs())))
            .await
            .expect_err("a different mesh id must be refused");
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
    }

    /// The reply never carries the raw credential to a caller that did not
    /// send one. `MeshWire::Redact` zeroes it; a reply that disclosed would
    /// let an invite-hash holder upgrade itself to gossip auth no rotation
    /// can revoke.
    #[tokio::test]
    async fn the_reply_redacts_the_mesh_secret_for_a_proof_caller() {
        let (mesh, _key) =
            commonwealth_discovery::membership::init_mesh("Lab", "founder", Vec::new());
        let st = state(mesh.clone());
        let sender = *mesh.members.keys().next().unwrap();
        let out = gossip(State(st), Json(request(&mesh, sender, unix_now_secs())))
            .await
            .expect("authorized");
        assert_eq!(
            out.0.mesh.mesh_secret, [0u8; 32],
            "the caller proved possession; sending it back is a pure leak"
        );
        assert!(
            out.0.mesh_proof.is_some(),
            "both directions prove, or neither"
        );
    }
}
