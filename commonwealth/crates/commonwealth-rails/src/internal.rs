// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /internal/gossip` — the route other daemons dial every round — and
//! `POST /internal/join`, the one a joiner dials once. Together they are the
//! only inbound HTTP this process serves to the mesh.
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
//!
//! # Admission
//!
//! `/internal/join` admits through
//! `commonwealth_discovery::membership::accept_join_with_identity`, the same
//! decider the inference daemon's join route calls
//! (`routes_internal/mesh_admin.rs`). The two checks that route makes before
//! it — the pubkey's proof of possession and the mesh's invite expiry — are
//! repeated here from their owners (`verify_join_proof`,
//! `Mesh::invite_expired_at`) until pb-mesh-exit-mesh retires the daemon's
//! route and this is the only one.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::Json;
use commonwealth_core::clock::unix_now_secs;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::wire::{
    GossipRejection, GossipRequest, GossipResponse, JoinRejection, JoinRequest, JoinResponse,
};
use commonwealth_core::mesh::{GossipAuth, GossipAuthArm, Mesh, MeshWire, SecretDisclosure};
use host_kit::shell::RouteBundle;
use tokio::sync::{Mutex, Notify, RwLock};

use crate::{identity, note_contact, Refusal};

#[derive(Clone)]
pub struct Inbound {
    pub mesh: Arc<RwLock<Mesh>>,
    pub contacts: Arc<Mutex<HashMap<NodeId, u64>>>,
    pub self_id: NodeId,
    pub data_dir: PathBuf,
    /// Woken when an inbound round brings an Offline member back Online
    /// (`crate::gossip::merge_round`).
    pub ring_nudge: Arc<Notify>,
    /// Where an inbound round records its sender's credential generation
    /// (`RailsDaemon::split_generation`).
    pub split_generation: crate::gossip::SplitGenerations,
}

/// Bind the internal listener on an ephemeral loopback port and serve.
/// Returns the address the acceptor splices to.
pub async fn serve(
    mesh: Arc<RwLock<Mesh>>,
    contacts: Arc<Mutex<HashMap<NodeId, u64>>>,
    self_id: NodeId,
    data_dir: PathBuf,
    ring_nudge: Arc<Notify>,
    split_generation: crate::gossip::SplitGenerations,
    ring: RouteBundle,
) -> Result<(SocketAddr, tokio::task::JoinHandle<()>), Refusal> {
    let bind: SocketAddr = ([127, 0, 0, 1], 0).into();
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| Refusal::Listen(bind, e))?;
    let addr = listener
        .local_addr()
        .map_err(|e| Refusal::Listen(bind, e))?;
    let bundle = router(Inbound {
        mesh,
        contacts,
        self_id,
        data_dir,
        ring_nudge,
        split_generation,
    });
    let task = tokio::spawn(async move {
        let forever = std::future::pending::<()>();
        if let Err(e) = host_kit::shell::serve([listener], vec![bundle, ring], forever).await {
            tracing::error!(target: "rails", error = %e, "internal: listener stopped");
        }
    });
    tracing::info!(target: "rails", addr = %addr, "internal: /internal/gossip, /internal/join and /internal/ring/* listening");
    Ok((addr, task))
}

pub fn router(state: Inbound) -> RouteBundle {
    RouteBundle::new("internal")
        .route("/internal/gossip", post(gossip))
        .route("/internal/join", post(join))
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
    let report = crate::gossip::merge_round(
        &mut mesh,
        state.self_id,
        &incoming,
        &auth,
        req.from,
        &state.split_generation,
        &state.ring_nudge,
    );

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

fn rejected(reason: &str) -> (StatusCode, Json<JoinRejection>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(JoinRejection {
            reason: reason.to_string(),
        }),
    )
}

/// Verify a join key and, on a match, admit the caller and hand it the mesh.
pub async fn join(
    State(state): State<Inbound>,
    Json(req): Json<JoinRequest>,
) -> Result<Json<JoinResponse>, (StatusCode, Json<JoinRejection>)> {
    // A presented pubkey must be proven before it is recorded: admitting an
    // unproven key would bind a transport identity the joiner may not hold.
    if let Some(pubkey) = req.node_pubkey.as_ref() {
        let proven = match (req.proposed_node_id.as_ref(), req.pubkey_proof.as_deref()) {
            (Some(id), Some(proof)) => commonwealth_transport::identity::verify_join_proof(
                pubkey,
                id,
                &req.joining_node_name,
                proof,
            ),
            _ => false,
        };
        if !proven {
            tracing::warn!(target: "rails", joining = %req.joining_node_name,
                           "join: REFUSED — node_pubkey without a valid proof of possession");
            return Err(rejected(
                "node_pubkey proof of possession missing or invalid",
            ));
        }
    }
    let now = unix_now_secs();
    let mut mesh = state.mesh.write().await;
    if mesh.invite_expired_at(now) {
        tracing::warn!(target: "rails", joining = %req.joining_node_name,
                       expires_at = ?mesh.invite_expires_at, "join: REFUSED — the invite has expired");
        return Err(rejected("invite link has expired"));
    }
    let new_id = match commonwealth_discovery::membership::accept_join_with_identity(
        &mut mesh,
        &req.join_key,
        &req.joining_node_name,
        req.joining_node_addresses,
        state.self_id,
        req.proposed_node_id,
        req.node_pubkey,
    ) {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(target: "rails", joining = %req.joining_node_name, error = %e,
                           "join: REFUSED by the membership decider");
            return Err(rejected(&e.to_string()));
        }
    };
    note_contact(&state.contacts, new_id, now).await;
    // Persist before answering: a founder that restarts inside a gossip
    // interval must not forget the member it just admitted.
    if let Err(e) = identity::save_mesh(&state.data_dir, &mesh) {
        tracing::warn!(target: "rails", error = %e, "join: could not persist the mesh");
    }
    tracing::info!(target: "rails", new_node = %new_id, joining = %req.joining_node_name,
                   members = mesh.members.len(), "join: admitted a member");
    // Disclose: a joiner has no other channel to learn the gossip credential.
    Ok(Json(JoinResponse {
        assigned_node_id: new_id,
        mesh: MeshWire::for_peer(&mesh, SecretDisclosure::Disclose),
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
            ring_nudge: Arc::new(Notify::new()),
            split_generation: Default::default(),
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
    /// `mesh` with a second member `caller`, as a caller's view of it.
    fn with_caller(mesh: &Mesh, caller: NodeId) -> Mesh {
        let mut theirs = mesh.clone();
        let mut row = theirs.members.values().next().unwrap().clone();
        row.node_id = caller;
        row.name = "caller".into();
        row.last_seen += 100;
        theirs.members.insert(caller, row);
        theirs
    }

    /// The daemon's gossip_route `an_upgraded_caller_gets_no_raw_secret_back`:
    /// a caller that proves AND still ships its secret (every upgraded pair's
    /// first round) gets none back, and our live secret is untouched.
    #[tokio::test]
    async fn an_upgraded_caller_gets_no_raw_secret_back() {
        let (mesh, _key) =
            commonwealth_discovery::membership::init_mesh("Lab", "founder", Vec::new());
        let st = state(mesh.clone());
        let caller = NodeId::generate();
        let now = unix_now_secs();
        let req = GossipRequest {
            mesh: MeshWire::for_peer(&with_caller(&mesh, caller), SecretDisclosure::Disclose),
            from: Some(caller),
            mesh_proof: mesh.mesh_proof(caller, now),
        };
        let out = gossip(State(st.clone()), Json(req))
            .await
            .expect("authorized");
        assert_eq!(
            out.0.mesh.mesh_secret, [0u8; 32],
            "the reply leaked the secret"
        );
        assert_eq!(st.mesh.read().await.mesh_secret, mesh.mesh_secret);
    }

    /// The daemon's gossip_route
    /// `a_proving_caller_that_withholds_its_secret_is_recorded_post_split`: a
    /// proof is post-split by definition, whatever the payload carried; a
    /// pre-split record here blocks rotation between two upgraded nodes.
    #[tokio::test]
    async fn a_proving_caller_that_withholds_its_secret_is_recorded_post_split() {
        let (mesh, _key) =
            commonwealth_discovery::membership::init_mesh("Lab", "founder", Vec::new());
        let st = state(mesh.clone());
        let caller = NodeId::generate();
        let now = unix_now_secs();
        let req = GossipRequest {
            mesh: MeshWire::for_peer(&with_caller(&mesh, caller), SecretDisclosure::Redact),
            from: Some(caller),
            mesh_proof: mesh.mesh_proof(caller, now),
        };
        gossip(State(st.clone()), Json(req))
            .await
            .expect("authorized");
        assert_eq!(
            crate::gossip::split_generation_of(&st.split_generation, caller),
            Some(true)
        );
    }

    /// The daemon's gossip_route `a_pre_split_caller_is_still_recorded_pre_split`:
    /// a caller with neither proof nor secret is admitted on the compat arm
    /// and recorded pre-split, so rotate keeps refusing while it is online.
    #[tokio::test]
    async fn a_pre_split_caller_is_still_recorded_pre_split() {
        let (mesh, _key) =
            commonwealth_discovery::membership::init_mesh("Lab", "founder", Vec::new());
        let st = state(mesh.clone());
        let caller = NodeId::generate();
        let req = GossipRequest {
            mesh: MeshWire::for_peer(&with_caller(&mesh, caller), SecretDisclosure::Redact),
            from: Some(caller),
            mesh_proof: None,
        };
        let out = gossip(State(st.clone()), Json(req))
            .await
            .expect("the compat arm admits a pre-split caller");
        assert_eq!(
            out.0.mesh.mesh_secret, [0u8; 32],
            "and discloses nothing to it"
        );
        assert_eq!(
            crate::gossip::split_generation_of(&st.split_generation, caller),
            Some(false)
        );
    }

    fn join_req(key: &str, name: &str) -> JoinRequest {
        let signer = ed25519_dalek::SigningKey::from_bytes(&[5u8; 32]);
        let id = NodeId::generate();
        JoinRequest {
            join_key: key.to_string(),
            joining_node_name: name.to_string(),
            joining_node_addresses: Vec::new(),
            proposed_node_id: Some(id),
            node_pubkey: Some(commonwealth_transport::identity::node_pubkey(&signer)),
            pubkey_proof: Some(commonwealth_transport::identity::sign_join_proof(
                &signer, &id, name,
            )),
        }
    }

    /// The happy path: the founder's key admits the joiner under the id it
    /// proposed, and the reply carries the gossip credential.
    #[tokio::test]
    async fn the_founders_key_admits_a_joiner() {
        let (mesh, key) =
            commonwealth_discovery::membership::init_mesh("Lab", "founder", Vec::new());
        let st = state(mesh);
        let req = join_req(&key, "joiner");
        let proposed = req.proposed_node_id.unwrap();
        let out = join(State(st.clone()), Json(req)).await.expect("admitted");
        assert_eq!(out.0.assigned_node_id, proposed);
        assert_ne!(
            out.0.mesh.mesh_secret, [0u8; 32],
            "a joiner learns the credential here"
        );
        assert!(st.mesh.read().await.members.contains_key(&proposed));
    }

    /// **The failing input.** A wrong key is a 401 that adds nobody.
    #[tokio::test]
    async fn a_wrong_key_is_401_and_admits_nobody() {
        let (mesh, _key) =
            commonwealth_discovery::membership::init_mesh("Lab", "founder", Vec::new());
        let st = state(mesh);
        let wrong = commonwealth_discovery::membership::generate_join_key();
        let err = join(State(st.clone()), Json(join_req(&wrong, "joiner")))
            .await
            .expect_err("a wrong key must be refused");
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
        assert_eq!(st.mesh.read().await.members.len(), 1);
    }

    /// A pubkey whose proof does not verify is refused before the key is read.
    #[tokio::test]
    async fn an_unproven_pubkey_is_401() {
        let (mesh, key) =
            commonwealth_discovery::membership::init_mesh("Lab", "founder", Vec::new());
        let st = state(mesh);
        let mut req = join_req(&key, "joiner");
        req.joining_node_name = "someone-else".into();
        let err = join(State(st.clone()), Json(req))
            .await
            .expect_err("the proof binds the name");
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
        assert!(
            err.1.reason.contains("proof of possession"),
            "{}",
            err.1.reason
        );
    }

    /// An expired invite admits nobody, whoever minted it.
    #[tokio::test]
    async fn an_expired_invite_is_401() {
        let (mut mesh, key) =
            commonwealth_discovery::membership::init_mesh("Lab", "founder", Vec::new());
        mesh.invite_expires_at = Some(1);
        let st = state(mesh);
        let err = join(State(st), Json(join_req(&key, "joiner")))
            .await
            .expect_err("expired");
        assert!(err.1.reason.contains("expired"), "{}", err.1.reason);
    }
}
