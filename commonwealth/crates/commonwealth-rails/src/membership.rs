// SPDX-License-Identifier: AGPL-3.0-or-later
//! The membership doors (phase-b pb-rails-membership): what makes this node a
//! member of a mesh, and of which one, asked of the process that holds the
//! key and the endpoint (FIVE_PROGRAMS §4 rule 8).
//!
//! `POST /v1/mesh/create` · `POST /v1/mesh/join` · `POST /v1/mesh/join/preview`
//! · `POST /v1/mesh/rotate` · `POST /v1/mesh/leave` · `POST /v1/mesh/switch` ·
//! `POST /v1/mesh/forget`, and the known-mesh list as `meshes` on
//! `/v1/mesh/status`. Same paths, bodies and field names as the inference
//! daemon's routes (sovereign-daemon mesh_http.rs), so its clients dial these
//! unchanged at the flip. Every verb acts on a RUNNING node: the endpoint that
//! joins is the one that serves, and nothing restarts.
//!
//! What each verb does to the node, all under the `verbs` lock:
//! - create and join PARK the active mesh ([`crate::known`]) and make the new
//!   one active; switch parks the active and resumes a parked one. Parking
//!   announces this node offline, so the mesh it left reads a pause.
//! - leave gives the active membership up (a tombstone gossiped to the online
//!   members) and runs solo. It parks nothing.
//! - forget deletes a parked membership and refuses the active one.
//! - rotate mints a new invite key; an encrypted mesh's invite then expires
//!   after `INVITE_TTL_SECS`, and the old key opens nothing.
//!
//! The inference daemon's pre-rotation partition check has no half here: it
//! guards peers on builds from before the mesh-secret split, and this process
//! only joins meshes that minted one (gossip.rs, "post-split by construction").

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Json;
use commonwealth_core::mesh::Mesh;
use commonwealth_discovery::deep_link::{parse_join_argument, DeepLink};
use commonwealth_discovery::membership::{generate_join_key, hash_join_key, INVITE_TTL_SECS};
use host_kit::shell::RouteBundle;
use serde::Deserialize;

use crate::identity::{self, StoreRefusal};
use crate::join::JoinRefusal;
use crate::{found, gossip, known, RailsDaemon, Refusal};

/// The seven membership routes.
pub fn bundle(daemon: Arc<RailsDaemon>) -> RouteBundle {
    RouteBundle::new("membership")
        .route("/v1/mesh/create", post(create))
        .route("/v1/mesh/join", post(join))
        .route("/v1/mesh/join/preview", post(preview))
        .route("/v1/mesh/rotate", post(rotate))
        .route("/v1/mesh/leave", post(leave))
        .route("/v1/mesh/switch", post(switch))
        .route("/v1/mesh/forget", post(forget))
        .with_state(daemon)
}

/// `POST /v1/mesh/create` body. `encrypt` and `node_name` are the inference
/// daemon's fields; each is accepted only where it asks for what this node
/// does, and refused by name otherwise.
#[derive(Debug, Default, Deserialize)]
pub struct CreateRequest {
    /// The mesh's name. Absent: `<node name>'s Mesh`, as the daemon names it.
    #[serde(default)]
    pub name: Option<String>,
    /// Must be this node's `rails.toml` name when present.
    #[serde(default)]
    pub node_name: Option<String>,
    /// Must not be `false`: cw-rails founds encrypted meshes only.
    #[serde(default)]
    pub encrypt: Option<bool>,
}

/// `POST /v1/mesh/join` body.
#[derive(Debug, Deserialize)]
pub struct JoinInvite {
    /// A `sovereign://join/…` link, an `https://…/join/…` link or a bare key.
    pub key_or_url: String,
    /// Must be this node's `rails.toml` name when present.
    #[serde(default)]
    pub node_name: Option<String>,
}

/// `POST /v1/mesh/join/preview` body.
#[derive(Debug, Deserialize)]
pub struct PreviewRequest {
    /// The invite as the user pasted it.
    pub link: String,
}

/// `POST /v1/mesh/switch` and `/forget` body: one reference, one resolver.
#[derive(Debug, Deserialize)]
pub struct MeshRef {
    /// Mesh name, full hex id, or an id prefix of at least 8 characters.
    pub mesh: String,
}

fn answer(code: StatusCode, body: serde_json::Value) -> Response {
    (code, Json(body)).into_response()
}

/// A refusal whose body carries more than its sentence.
fn answer_refusal(code: StatusCode, verb: &str, body: serde_json::Value) -> Response {
    tracing::info!(target: "rails", verb, status = code.as_u16(), body = %body, "membership: refused");
    answer(code, body)
}

fn refuse(code: StatusCode, verb: &str, why: impl std::fmt::Display) -> Response {
    tracing::info!(target: "rails", verb, status = code.as_u16(), why = %why, "membership: refused");
    answer(code, serde_json::json!({ "error": why.to_string() }))
}

/// A disk write the verb needed did not land; the live state is unchanged.
fn store_failed(verb: &str, e: StoreRefusal) -> Response {
    tracing::error!(target: "rails", verb, error = %e, "membership: a store write failed");
    answer(
        StatusCode::INTERNAL_SERVER_ERROR,
        serde_json::json!({ "error": e.to_string() }),
    )
}

/// This node has one member name, `rails.toml`'s; a request for another is
/// refused rather than answered under a name it did not ask for.
fn check_node_name(daemon: &RailsDaemon, asked: Option<&str>) -> Result<(), String> {
    match asked.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) if n != daemon.node.config.name => Err(format!(
            "this node's member name is `{}` (rails.toml `name`); cw-rails has one name per node, so it will not act as `{n}`",
            daemon.node.config.name
        )),
        _ => Ok(()),
    }
}

/// The invite link for `mesh`, from this endpoint's dial, or why none.
fn invite(daemon: &RailsDaemon, key: &str, mesh: &Mesh) -> Result<String, found::InviteAbsent> {
    let dial = commonwealth_transport::iroh::format_dial_string(&daemon.endpoint().addr());
    found::invite_link(Some(key), mesh, dial.as_deref())
}

/// Park the active mesh, announcing the pause first. Solo parks nothing.
async fn park_active(daemon: &RailsDaemon) -> Result<(), StoreRefusal> {
    if daemon.is_solo() {
        return Ok(());
    }
    gossip::announce_departure(daemon, false).await;
    let mesh = daemon.mesh.read().await.clone();
    known::park(&daemon.node.data_dir, &mesh, daemon.join_key().as_deref())
}

/// Write `mesh` and its key at the root and make it the live one.
async fn activate(
    daemon: &RailsDaemon,
    mesh: Mesh,
    join_key: Option<String>,
) -> Result<(), StoreRefusal> {
    let root = &daemon.node.data_dir;
    match &join_key {
        Some(key) => identity::save_join_key(root, key)?,
        None => identity::clear_join_key(root)?,
    }
    identity::save_mesh(root, &mesh)?;
    daemon.swap_membership(mesh, join_key, false).await;
    Ok(())
}

/// `POST /v1/mesh/create` — found a mesh with this node as its first member.
pub async fn create(
    State(daemon): State<Arc<RailsDaemon>>,
    body: Option<Json<CreateRequest>>,
) -> Response {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    if let Err(why) = check_node_name(&daemon, req.node_name.as_deref()) {
        return refuse(StatusCode::BAD_REQUEST, "create", why);
    }
    if req.encrypt == Some(false) {
        return refuse(
            StatusCode::BAD_REQUEST,
            "create",
            "cw-rails founds encrypted meshes only: it reaches peers by key or not at all",
        );
    }
    let node_name = daemon.node.config.name.clone();
    let mesh_name = req
        .name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("{node_name}'s Mesh"));

    let _verbs = daemon.verbs.lock().await;
    let root = daemon.node.data_dir.clone();
    if let Err(e) = park_active(&daemon).await {
        return store_failed("create", e);
    }
    // `found` refuses over a mesh.json, and the active one is parked now.
    if let Err(e) = identity::clear_mesh(&root) {
        return store_failed("create", e);
    }
    let founded = match found::found(&root, &mesh_name, &node_name) {
        Ok(f) => f,
        Err(e) => {
            // Put the parked mesh back where the live state still says it is.
            if !daemon.is_solo() {
                let mesh = daemon.mesh.read().await.clone();
                if let Err(restore) = activate(&daemon, mesh, daemon.join_key()).await {
                    tracing::error!(target: "rails", error = %restore, "create: the active mesh could not be restored after a failed founding");
                }
            }
            return refuse(StatusCode::CONFLICT, "create", e);
        }
    };
    let link = invite(&daemon, &founded.join_key, &founded.mesh);
    let body = serde_json::json!({
        "mesh_name": founded.mesh.name,
        "mesh_id": founded.mesh.id.to_hex(),
        "join_key": founded.join_key,
        "join_link": link.as_ref().ok(),
        "join_link_absent": link.as_ref().err().map(|a| a.reason()),
    });
    daemon
        .swap_membership(founded.mesh, Some(founded.join_key), false)
        .await;
    answer(StatusCode::OK, body)
}

/// `POST /v1/mesh/join` — join a mesh by invite, over this node's own
/// endpoint, and make it the active one.
pub async fn join(State(daemon): State<Arc<RailsDaemon>>, Json(req): Json<JoinInvite>) -> Response {
    if let Err(why) = check_node_name(&daemon, req.node_name.as_deref()) {
        return refuse(StatusCode::BAD_REQUEST, "join", why);
    }
    let _verbs = daemon.verbs.lock().await;
    let joined = match crate::join::join(&daemon.node, &req.key_or_url).await {
        Ok(j) => j,
        Err(e) => {
            let code = match e {
                JoinRefusal::NotAJoinLink(_) | JoinRefusal::BadDial(_) => StatusCode::BAD_REQUEST,
                JoinRefusal::NoIrohDial | JoinRefusal::NoLan(_) => StatusCode::BAD_REQUEST,
                JoinRefusal::Rejected(_) => StatusCode::FORBIDDEN,
                JoinRefusal::NoTunnel(_)
                | JoinRefusal::NoAnswer(_, _)
                | JoinRefusal::BadResponse(_) => StatusCode::BAD_GATEWAY,
            };
            return refuse(code, "join", e);
        }
    };
    // Every mesh this node holds names it by the one node_id on disk. A
    // founder assigns another only when a differently named member already
    // holds ours; adopting it here would change who this running node is.
    if joined.self_id != daemon.node.self_id {
        return refuse(
            StatusCode::CONFLICT,
            "join",
            format!(
                "the founder admitted this node as {} because another member of that mesh holds this node's id {} under a different name; forget that member there, or join from a fresh --data-dir",
                joined.self_id, daemon.node.self_id
            ),
        );
    }
    let already_active = !daemon.is_solo() && daemon.mesh.read().await.id == joined.mesh.id;
    if !already_active {
        if let Err(e) = park_active(&daemon).await {
            return store_failed("join", e);
        }
    }
    let (name, id) = (joined.mesh.name.clone(), joined.mesh.id);
    if let Err(e) = activate(&daemon, joined.mesh, None).await {
        return store_failed("join", e);
    }
    // A mesh joined again is active now, not parked as well.
    if let Err(e) = known::remove(&daemon.node.data_dir, &id) {
        tracing::warn!(target: "rails", error = %e, "join: a parked copy of the joined mesh could not be removed");
    }
    answer(
        StatusCode::OK,
        serde_json::json!({
            "mesh_name": name,
            "mesh_id": id.to_hex(),
            "node_id": daemon.node.self_id.to_string(),
        }),
    )
}

/// `POST /v1/mesh/join/preview` — what `join` WOULD join, without joining,
/// in the inference daemon's `JoinConfirmation` shape. A guest link is not a
/// join and previews as 400.
pub async fn preview(Json(req): Json<PreviewRequest>) -> Response {
    match parse_join_argument(&req.link) {
        Some(DeepLink::Join {
            join_key,
            relay_hint,
            mesh_name,
            iroh_dial,
            encrypted,
            expires_at,
        }) => answer(
            StatusCode::OK,
            serde_json::json!({
                "mesh_name": mesh_name.unwrap_or_else(|| "Unknown Mesh".to_string()),
                "invited_by": serde_json::Value::Null,
                "join_key": join_key,
                "relay_hint": relay_hint,
                "iroh_dial": iroh_dial,
                "encrypted": encrypted,
                "expires_at": expires_at,
            }),
        ),
        Some(_) => refuse(StatusCode::BAD_REQUEST, "preview", "not a join invite"),
        None => refuse(
            StatusCode::BAD_REQUEST,
            "preview",
            "link must be a bare cwth-… key, an https://sovereign.dev/join/… URL, or a sovereign://join/… deep link",
        ),
    }
}

/// `POST /v1/mesh/rotate` query.
#[derive(Debug, Default, Deserialize)]
pub struct RotateQuery {
    /// Rotate even though an Online peer is on a pre-split build, or has not
    /// been confirmed since start, and may be partitioned by it.
    #[serde(default)]
    pub force: bool,
}

/// The Online peers a rotation could partition, in two populations with two
/// remedies: `pre_split` (merged, and it offered neither a proof nor a
/// secret: upgrade it) and `unconfirmed` (not merged since start: wait one
/// round). A pre-split peer authorizes gossip on `invite_key_hash`, which a
/// rotation changes.
pub fn rotate_blockers(
    mesh: &Mesh,
    self_id: commonwealth_core::ids::NodeId,
    split: &gossip::SplitGenerations,
) -> (Vec<String>, Vec<String>) {
    let (mut pre_split, mut unconfirmed) = (Vec::new(), Vec::new());
    for m in mesh.members.values() {
        if m.node_id == self_id
            || !m.is_active()
            || m.status != commonwealth_core::mesh::NodeStatus::Online
        {
            continue;
        }
        let generation = gossip::split_generation_of(split, m.node_id);
        tracing::debug!(target: "rails", peer = %m.node_id, name = %m.name,
                        generation = ?generation, "rotate: pre-split check");
        match generation {
            Some(true) => {}
            Some(false) => pre_split.push(m.name.clone()),
            None => unconfirmed.push(m.name.clone()),
        }
    }
    (pre_split, unconfirmed)
}

/// The refusal's sentence: which population blocked, with its own remedy.
fn describe_rotate_refusal(pre_split: &[String], unconfirmed: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !pre_split.is_empty() {
        parts.push(format!(
            "{} peer(s) are still on a pre-split build ({}) — upgrade them first",
            pre_split.len(),
            pre_split.join(", ")
        ));
    }
    if !unconfirmed.is_empty() {
        parts.push(format!(
            "{} peer(s) have not been confirmed since this daemon started ({}) — \
             retry after the next gossip round",
            unconfirmed.len(),
            unconfirmed.join(", ")
        ));
    }
    format!(
        "Rotating now could partition the mesh: {}. Or re-run with --force to rotate anyway.",
        parts.join("; ")
    )
}

/// `POST /v1/mesh/rotate` — mint a new invite key for the active mesh. The
/// new hash rides the next gossip round (`invite_version`), so every member
/// refuses the old key once it has converged.
///
/// Refused (409) while an Online peer is pre-split or unconfirmed, unless
/// `?force=true` (the daemon's guard, here since pb-mesh-exit-transport;
/// FE-15, FE-17). The generation map is in memory, so a rotate soon after
/// start would refuse on an instrument that has not run: one gossip round
/// first, when any Online peer is unconfirmed, before the verbs lock (the
/// round takes it).
pub async fn rotate(
    State(daemon): State<Arc<RailsDaemon>>,
    axum::extract::Query(q): axum::extract::Query<RotateQuery>,
) -> Response {
    if !q.force && !daemon.is_solo() {
        let unconfirmed = {
            let mesh = daemon.mesh.read().await;
            rotate_blockers(&mesh, daemon.node.self_id, &daemon.split_generation).1
        };
        if !unconfirmed.is_empty() {
            tracing::info!(target: "rails", unconfirmed = ?unconfirmed,
                "rotate: peers unconfirmed since start — one gossip round before deciding");
            gossip::run_one_round(&daemon, 0).await;
        }
    }
    let _verbs = daemon.verbs.lock().await;
    if daemon.is_solo() {
        return refuse(StatusCode::NOT_FOUND, "rotate", "no mesh to rotate");
    }
    let key = generate_join_key();
    let now = commonwealth_core::clock::unix_now_secs();
    let mesh = {
        let mut mesh = daemon.mesh.write().await;
        if !q.force {
            let (pre_split, unconfirmed) =
                rotate_blockers(&mesh, daemon.node.self_id, &daemon.split_generation);
            if !pre_split.is_empty() || !unconfirmed.is_empty() {
                return answer_refusal(
                    StatusCode::CONFLICT,
                    "rotate",
                    serde_json::json!({
                        "error": describe_rotate_refusal(&pre_split, &unconfirmed),
                        "pre_split": pre_split,
                        "unconfirmed": unconfirmed,
                    }),
                );
            }
        }
        let expires_at = mesh.require_encryption.then_some(now + INVITE_TTL_SECS);
        mesh.rotate_invite_key(hash_join_key(&key), expires_at);
        mesh.clone()
    };
    let root = &daemon.node.data_dir;
    if let Err(e) =
        identity::save_mesh(root, &mesh).and_then(|()| identity::save_join_key(root, &key))
    {
        return store_failed("rotate", e);
    }
    daemon.set_join_key(Some(key.clone()));
    tracing::info!(target: "rails", mesh = %mesh.name, expires_at = ?mesh.invite_expires_at,
                   version = mesh.invite_version, "rotate: invite key rotated; mesh_secret untouched");
    let link = invite(&daemon, &key, &mesh);
    answer(
        StatusCode::OK,
        serde_json::json!({
            "mesh_name": mesh.name,
            "join_key": key,
            "join_link": link.as_ref().ok(),
            "join_link_absent": link.as_ref().err().map(|a| a.reason()),
            "expires_at": mesh.invite_expires_at,
        }),
    )
}

/// `POST /v1/mesh/leave` — give the active membership up and run solo.
pub async fn leave(State(daemon): State<Arc<RailsDaemon>>) -> Response {
    let _verbs = daemon.verbs.lock().await;
    if daemon.is_solo() {
        return refuse(StatusCode::CONFLICT, "leave", "this node is on no mesh");
    }
    let name = daemon.mesh.read().await.name.clone();
    gossip::announce_departure(&daemon, true).await;
    if let Err(e) = identity::clear_mesh(&daemon.node.data_dir) {
        return store_failed("leave", e);
    }
    daemon
        .swap_membership(daemon.node.solo_mesh(), None, true)
        .await;
    answer(StatusCode::OK, serde_json::json!({ "left": name }))
}

/// `POST /v1/mesh/switch` — park the active mesh and resume a parked one.
/// Answered after the switch: unlike the inference daemon, the listener that
/// serves this is not the one a switch rebuilds.
pub async fn switch(State(daemon): State<Arc<RailsDaemon>>, Json(req): Json<MeshRef>) -> Response {
    let _verbs = daemon.verbs.lock().await;
    let root = daemon.node.data_dir.clone();
    let parked = match known::parked(&root) {
        Ok(p) => p,
        Err(e) => return store_failed("switch", e),
    };
    let Some(target) = known::resolve(&parked, &req.mesh) else {
        return unknown_or_active(&daemon, "switch", &req.mesh).await;
    };
    if let Err(e) = park_active(&daemon).await {
        return store_failed("switch", e);
    }
    let (name, id) = (target.mesh.name.clone(), target.mesh.id);
    if let Err(e) = activate(&daemon, target.mesh.clone(), target.join_key.clone()).await {
        return store_failed("switch", e);
    }
    if let Err(e) = known::remove(&root, &id) {
        tracing::warn!(target: "rails", error = %e, "switch: the resumed mesh's parked copy could not be removed");
    }
    answer(StatusCode::OK, serde_json::json!({ "switched_to": name }))
}

/// `POST /v1/mesh/forget` — delete a parked membership.
pub async fn forget(State(daemon): State<Arc<RailsDaemon>>, Json(req): Json<MeshRef>) -> Response {
    let _verbs = daemon.verbs.lock().await;
    let root = daemon.node.data_dir.clone();
    let parked = match known::parked(&root) {
        Ok(p) => p,
        Err(e) => return store_failed("forget", e),
    };
    let Some(target) = known::resolve(&parked, &req.mesh) else {
        return unknown_or_active(&daemon, "forget", &req.mesh).await;
    };
    if let Err(e) = known::remove(&root, &target.mesh.id) {
        return store_failed("forget", e);
    }
    answer(
        StatusCode::OK,
        serde_json::json!({ "forgot": target.mesh.name }),
    )
}

/// A reference no parked mesh answers to: the active mesh (409, a verb it
/// cannot take), or nothing this node belongs to (404).
async fn unknown_or_active(daemon: &RailsDaemon, verb: &str, reference: &str) -> Response {
    let active = daemon.mesh.read().await;
    if !daemon.is_solo()
        && commonwealth_discovery::membership::names_mesh(
            &active.name,
            &active.id.to_hex(),
            reference,
        )
    {
        return refuse(
            StatusCode::CONFLICT,
            verb,
            format!(
                "'{}' is the active mesh — switch or leave first",
                active.name
            ),
        );
    }
    refuse(
        StatusCode::NOT_FOUND,
        verb,
        format!("not a member of any mesh matching '{reference}'"),
    )
}

/// The known-mesh list for `/v1/mesh/status`, in the inference daemon's
/// `KnownMeshDto` shape: the active mesh first (unless solo), then the
/// parked ones. A store that does not read is an `Err`, never an empty list.
pub fn listing(daemon: &RailsDaemon, active: &Mesh) -> Result<Vec<serde_json::Value>, Refusal> {
    let row = |mesh: &Mesh, is_active: bool| {
        serde_json::json!({
            "name": mesh.name,
            "mesh_id": mesh.id.to_hex(),
            "members_total": mesh.members.len(),
            "last_seen_unix": mesh.members.values().map(|m| m.last_seen).max().unwrap_or(0),
            "is_active": is_active,
        })
    };
    let mut rows = Vec::new();
    if !daemon.is_solo() {
        rows.push(row(active, true));
    }
    for p in known::parked(&daemon.node.data_dir)? {
        rows.push(row(&p.mesh, false));
    }
    Ok(rows)
}
