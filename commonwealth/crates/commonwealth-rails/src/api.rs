// SPDX-License-Identifier: AGPL-3.0-or-later
//! The loopback routes a shim author touches, and nothing else.
//!
//! `GET /v1/mesh/status` · `GET /v1/mesh/media[?peer=]` ·
//! `GET /v1/mesh/app[?peer=]` · `POST /v1/mesh/fanout` (and its media
//! spelling) · the four `/v1/mesh/publish` routes · the two roster verbs ·
//! the ring rail's doors, `/v1/rail/{append,log,live}`, in [`crate::rail`] ·
//! the mesh store's `/v1/mesh/kv/*`, in [`crate::kv`]. Every answer is
//! `commonwealth_media`'s — the same functions the inference daemon's
//! `/v1/mesh/*` routes call, so a shim written against one daemon behaves the
//! same against the other (ARCH §10.6), and `svrn run` publishes into either
//! one without knowing which it is talking to. This module is the HTTP shape
//! and the status mapping, and that is all it is.
//!
//! **Loopback IS the auth.** There is no bearer here, and the bind refuses
//! any address that is not loopback rather than serving an unauthenticated
//! API to a network. A URL this hands back is a bridge on this machine's own
//! `127.0.0.1` and is useless anywhere else in any case.

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use commonwealth_core::capabilities::OriginKind;
use commonwealth_media::apps::PublishRefusal;
use commonwealth_media::fanout::FanoutRequest;
use commonwealth_media::MediaReachRefusal;
use serde::Deserialize;

use crate::{RailsDaemon, Refusal};

/// The one bind decision. A rails daemon has no non-loopback posture, so this
/// is a total function of the port rather than a configurable address.
pub fn bind_addr(port: u16) -> SocketAddr {
    ([127, 0, 0, 1], port).into()
}

pub fn router(daemon: Arc<RailsDaemon>) -> Router {
    Router::new()
        .route("/v1/mesh/status", get(status))
        .route("/v1/mesh/media", get(media))
        .route("/v1/mesh/app", get(app))
        // The offer catalogue (five-programs fp-46): what the house has going
        // spare, behind its own origin kind and its own allow list — the same
        // kind-generic answer the media and app spellings serve.
        .route("/v1/mesh/offers", get(offers))
        // The presence poll's last reading (five-programs fp-46):
        // `media_available` as this node's own poll read it, `null` when
        // nobody answered — "could not ask", never "free".
        .route("/v1/mesh/media/presence", get(media_presence))
        // One handler, two paths: the media spelling is the generic body with
        // `kind` absent, not a second implementation.
        .route("/v1/mesh/fanout", post(origin_fanout))
        .route("/v1/mesh/media/fanout", post(origin_fanout))
        // The publishing half, byte-identical to the inference daemon's, so
        // `svrn run` does not have to know which daemon it reached.
        .route("/v1/mesh/publish", get(publishing).post(publish_app))
        .route(
            "/v1/mesh/publish/{claim_id}",
            axum::routing::delete(unpublish_app),
        )
        .route("/v1/mesh/publish/{claim_id}/renew", post(renew_app))
        // The roster verbs (FIVE_PROGRAMS fp-6 / §12 decision 2): retiring a
        // member row and the ring-roster membership test are the MESH's to
        // answer — the inference daemon dials these instead of mutating its
        // own copy. Same paths and bodies as the daemon's routes, so a client
        // works against either.
        .route("/v1/mesh/forget-member", post(forget_member))
        .route("/v1/mesh/roster-names/{pubkey}", get(roster_names))
        // The ring rail's doors (FIVE_PROGRAMS fp-44): durable append and
        // log over this node's own journals, and the live lane's local
        // buffer half. The bodies mirror the daemon's `routes_rail` doors;
        // the guest half does not exist here — the namespace is always the
        // caller's explicit one. The sync doors (fp-54) are the round's
        // read/write surface over the same journals — rail-core JSON, no
        // sovereign-* wire types.
        .route("/v1/rail/append", post(crate::rail::append))
        .route("/v1/rail/log", get(crate::rail::log))
        .route(
            "/v1/rail/live",
            post(crate::rail::live_push).get(crate::rail::live_drain),
        )
        .route("/v1/rail/namespaces", get(crate::rail::namespaces))
        .route("/v1/rail/actor", get(crate::rail::actor))
        .route("/v1/rail/digest", get(crate::rail::journal_digest))
        .route("/v1/rail/roster", get(crate::rail::journal_roster))
        .route("/v1/rail/read", get(crate::rail::journal_read))
        .route("/v1/rail/missing", post(crate::rail::journal_missing))
        .route("/v1/rail/ingest", post(crate::rail::journal_ingest))
        .route("/v1/rail/admit", post(crate::rail::journal_admit))
        .route("/v1/rail/compact", post(crate::rail::journal_compact))
        // The `work` queue, folded where its journal lives (fp-45).
        .route("/v1/work/projection", get(crate::work::projection))
        .with_state(daemon.clone())
        // The mesh store's doors (fp-77), over its own state.
        .merge(crate::kv::router(daemon.kv.clone()))
        // The typed ledger doors (fp-78), over the same store.
        .merge(crate::ledger::router(crate::ledger::LedgerDoors::new(
            daemon.kv.store.clone(),
        )))
}

/// The bind guard, as a function so it has a failing input that can be
/// named without standing an iroh endpoint up. This API has no auth because
/// loopback IS the auth; anywhere else is publishing it.
pub fn check_loopback(listen: SocketAddr) -> Result<(), Refusal> {
    if listen.ip().is_loopback() {
        Ok(())
    } else {
        Err(Refusal::NotLoopback(listen))
    }
}

/// Bind and serve. Refuses a non-loopback address by name.
pub async fn serve(
    daemon: Arc<RailsDaemon>,
    listen: SocketAddr,
) -> Result<tokio::task::JoinHandle<()>, Refusal> {
    check_loopback(listen)?;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|e| Refusal::Listen(listen, e))?;
    let app = router(daemon);
    Ok(tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(target: "rails", error = %e, "api: listener stopped");
        }
    }))
}

/// `GET /v1/mesh/status` — who I am, who is on the roster, who offers media.
pub async fn status(State(daemon): State<Arc<RailsDaemon>>) -> impl IntoResponse {
    let mesh = daemon.mesh.read().await;
    let members: Vec<serde_json::Value> = {
        let mut rows: Vec<_> = mesh
            .members
            .values()
            .filter(|m| m.removed_at.is_none())
            .collect();
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        rows.into_iter()
            .map(|m| {
                serde_json::json!({
                    "name": m.name,
                    "node_id": m.node_id.to_string(),
                    "status": m.status,
                    // The catalogue's fact, read from the gossiped record
                    // rather than derived a second way here. `offers_media`
                    // keeps its name and meaning for the shims already
                    // parsing it; `offers` carries the whole set now that a
                    // node can publish more than one kind.
                    "offers_media": commonwealth_media::candidate_of(m).offers(OriginKind::Media),
                    "offers": commonwealth_media::candidate_of(m).origins,
                    "last_seen": m.last_seen,
                    "is_self": m.node_id == daemon.node.self_id,
                })
            })
            .collect()
    };
    let addr = daemon.node.endpoint.addr();
    // The posture the endpoint was bound with (`RailsNode::bind` reads the
    // same `relay_config`), so a client can refuse an n0-homed cw-rails.
    let relay = daemon.node.config.relay_config();
    Json(serde_json::json!({
        "self": {
            "node_id": daemon.node.self_id.to_string(),
            "name": daemon.node.config.name,
            "pubkey": hex::encode(daemon.node.pubkey().0),
            "dial": commonwealth_transport::iroh::format_dial_string(&addr),
            "media_origin": daemon.node.config.media.origin,
        },
        "mesh": { "id": mesh.id.to_string(), "name": mesh.name },
        "members": members,
        "fanout_inflight": daemon.gauge.load(Ordering::Relaxed),
        "internal_listener": daemon.internal_addr.to_string(),
        "relay": {
            "n0_services": relay.n0_services,
            "relay_urls": relay.relay_urls,
        },
    }))
}

#[derive(Debug, Deserialize)]
pub struct MediaQuery {
    /// Member name or node-id prefix (≥4 chars). Absent: list who offers.
    #[serde(default)]
    pub peer: Option<String>,
}

/// `GET /v1/mesh/media` — the catalogue, or one member's loopback URL.
pub async fn media(state: State<Arc<RailsDaemon>>, q: Query<MediaQuery>) -> impl IntoResponse {
    origin(state, q, OriginKind::Media).await
}

/// `GET /v1/mesh/app` — the same two questions for PUBLISHED APPS.
///
/// A rails node can VIEW the house's apps here whether or not it publishes
/// any; the catalogue is the roster's gossip and the reach is a bridge over
/// `cwth/app/0`.
///
/// It can publish its own too, as of the claim tier — through
/// `POST /v1/mesh/publish` below, never through `rails.toml`. The config
/// route was the blocked one and stays blocked on purpose: an `[apps]` table
/// would make an un-upgraded rails daemon REFUSE TO BOOT on a config a newer
/// one wrote, because `Config` and `MediaSection` are
/// `#[serde(deny_unknown_fields)]`. A claim needs no config key, so that
/// hazard is absent rather than handled — and the thing a rails node loses by
/// it, a durable entry that survives restart, is the thing the closure-loop
/// rule says nobody should have wanted for an app anyway.
pub async fn app(state: State<Arc<RailsDaemon>>, q: Query<MediaQuery>) -> impl IntoResponse {
    origin(state, q, OriginKind::App).await
}

/// `GET /v1/mesh/offers` — the same two questions for OFFERED ORIGINS, the
/// third kind: what this node has going spare, behind `cwth/offer/0` and its
/// own `offer_allow` grant. One handler with the other two spellings because
/// the catalogue, the reach and the refusals ARE one implementation
/// (`origin` is kind-generic); only the kind differs.
pub async fn offers(state: State<Arc<RailsDaemon>>, q: Query<MediaQuery>) -> impl IntoResponse {
    origin(state, q, OriginKind::Offer).await
}

/// `GET /v1/mesh/media/presence` — the presence poll's last reading.
///
/// `{"media_available": <number|null>}`: `1.0` free, `0.0` the holder is
/// watching, `null` nobody answered. `null` is served as a VALUE, never as a
/// refusal, so a client reads the field the same way it reads the gossiped
/// capability — and a poll that cannot ask is reported, not defaulted
/// (principle 6).
pub async fn media_presence(State(daemon): State<Arc<RailsDaemon>>) -> impl IntoResponse {
    let reading = *daemon
        .media_presence
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Json(serde_json::json!({ "media_available": reading }))
}

async fn origin(
    State(daemon): State<Arc<RailsDaemon>>,
    Query(q): Query<MediaQuery>,
    kind: OriginKind,
) -> axum::response::Response {
    let roster = daemon.roster().await;
    let paths = daemon.paths().await;
    let self_id = daemon.node.self_id;
    let Some(peer) = q.peer.as_deref().map(str::trim).filter(|p| !p.is_empty()) else {
        let offering = commonwealth_media::offers(self_id, &roster, &paths, kind);
        return (
            StatusCode::OK,
            Json(serde_json::json!({ "offering": offering })),
        )
            .into_response();
    };
    match commonwealth_media::reach(self_id, &roster, peer, &daemon.transport, &paths, kind).await {
        Ok(reach) => (StatusCode::OK, Json(serde_json::json!(reach))).into_response(),
        // A name nobody has is the one refusal that is about the REQUEST.
        Err(e @ MediaReachRefusal::UnknownMember(_)) => refusal(StatusCode::NOT_FOUND, e),
        // Everything else is coherent and the mesh's state is what says no.
        Err(e) => refusal(StatusCode::CONFLICT, e),
    }
}

/// `POST /v1/mesh/fanout` — the same request to every member publishing the
/// requested kind of origin. `kind` absent means media, which is what the
/// `/v1/mesh/media/fanout` spelling relies on.
pub async fn origin_fanout(
    State(daemon): State<Arc<RailsDaemon>>,
    Json(req): Json<FanoutRequest>,
) -> impl IntoResponse {
    let roster = daemon.roster().await;
    match commonwealth_media::fanout::fanout(
        daemon.node.self_id,
        &roster,
        req,
        daemon.transport.clone(),
        daemon.gauge.clone(),
    )
    .await
    {
        Ok(doc) => (StatusCode::OK, Json(serde_json::json!(doc))).into_response(),
        Err(e @ MediaReachRefusal::BadRequest(_)) => refusal(StatusCode::BAD_REQUEST, e),
        Err(e) => refusal(StatusCode::CONFLICT, e),
    }
}

/// `GET /v1/mesh/publish` — what this node offers the house.
pub async fn publishing(State(daemon): State<Arc<RailsDaemon>>) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "apps": daemon.published_apps.listing(),
            "media_origin": daemon.node.config.media.origin,
        })),
    )
}

/// `POST /v1/mesh/publish` — take a claim on a name for a loopback port.
pub async fn publish_app(
    State(daemon): State<Arc<RailsDaemon>>,
    Json(req): Json<ClaimRequest>,
) -> impl IntoResponse {
    let addr: SocketAddr = ([127, 0, 0, 1], req.port).into();
    match daemon
        .published_apps
        .claim(&req.name, addr, ttl_of(req.ttl_secs))
    {
        Ok(claim) => (StatusCode::OK, Json(serde_json::json!(claim))).into_response(),
        Err(e) => publish_refusal(e),
    }
}

/// `POST /v1/mesh/publish/{claim_id}/renew`
pub async fn renew_app(
    State(daemon): State<Arc<RailsDaemon>>,
    axum::extract::Path(claim_id): axum::extract::Path<String>,
    body: Option<Json<RenewRequest>>,
) -> impl IntoResponse {
    let ttl = ttl_of(body.and_then(|Json(b)| b.ttl_secs));
    match daemon.published_apps.renew(&claim_id, ttl) {
        Ok(claim) => (StatusCode::OK, Json(serde_json::json!(claim))).into_response(),
        Err(e) => publish_refusal(e),
    }
}

/// `DELETE /v1/mesh/publish/{claim_id}`
pub async fn unpublish_app(
    State(daemon): State<Arc<RailsDaemon>>,
    axum::extract::Path(claim_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    match daemon.published_apps.release(&claim_id) {
        Ok(name) => (
            StatusCode::OK,
            Json(serde_json::json!({ "released": name })),
        )
            .into_response(),
        Err(e) => publish_refusal(e),
    }
}

/// `POST /v1/mesh/forget-member` body — the same shape the inference daemon's
/// route takes, so its client posts one body to either.
#[derive(Debug, Deserialize)]
pub struct ForgetMemberRequest {
    /// Member name, or a node_id prefix of at least 4 hex characters.
    pub member: String,
    /// Retire the row even though the member is online and not aliased.
    #[serde(default)]
    pub force: bool,
}

/// `POST /v1/mesh/forget-member` — retire one member row: tombstone it in
/// THIS process's mesh (the one the roster readers converge on), persist the
/// mesh file, and let gossip carry the removal. The mutation is
/// [`commonwealth_core::mesh_identity::Mesh::forget_member`] — the same
/// implementation the daemon's fabric delegate calls — so the refusal arms
/// cannot drift between the two processes (ARCH §10.6).
///
/// Status codes match the daemon's route: 404 for an unknown member, 409 for
/// the two "coherent but refused" arms. The body carries a `kind` beside the
/// sentence so a client can tell those two arms apart without parsing prose.
pub async fn forget_member(
    State(daemon): State<Arc<RailsDaemon>>,
    Json(req): Json<ForgetMemberRequest>,
) -> axum::response::Response {
    let now = commonwealth_core::clock::unix_now_secs();
    let outcome = {
        let mut mesh = daemon.mesh.write().await;
        mesh.forget_member(daemon.node.self_id, &req.member, req.force, now)
    };
    match outcome {
        Ok(outcome) => {
            let mesh = daemon.mesh.read().await;
            if let Err(e) = crate::identity::save_mesh(&daemon.node.data_dir, &mesh) {
                tracing::warn!(
                    target: "rails",
                    error = %e,
                    "forget-member: mesh.json could not be written"
                );
            }
            tracing::info!(
                target: "rails",
                member = %outcome.name,
                node_id = %outcome.node_id,
                was_aliased = outcome.was_aliased,
                already_retired = outcome.already_retired,
                "forget-member: member row retired; gossip carries the tombstone"
            );
            (StatusCode::OK, Json(serde_json::json!(outcome))).into_response()
        }
        Err(e @ commonwealth_core::mesh_identity::ForgetMemberError::UnknownMember(_)) => {
            forget_refusal(StatusCode::NOT_FOUND, "unknown-member", e)
        }
        Err(e @ commonwealth_core::mesh_identity::ForgetMemberError::CannotForgetSelf) => {
            forget_refusal(StatusCode::CONFLICT, "cannot-forget-self", e)
        }
        Err(e @ commonwealth_core::mesh_identity::ForgetMemberError::MemberStillLive(_)) => {
            forget_refusal(StatusCode::CONFLICT, "member-still-live", e)
        }
    }
}

/// The refusal shape for `forget-member`: the sentence under `error` (the
/// same sentence the daemon's own route would have carried — one
/// implementation), plus `kind` naming the arm.
fn forget_refusal(
    code: StatusCode,
    kind: &'static str,
    e: commonwealth_core::mesh_identity::ForgetMemberError,
) -> axum::response::Response {
    tracing::info!(target: "rails", status = code.as_u16(), error = %e, "api: forget-member refused");
    (
        code,
        Json(serde_json::json!({ "error": e.to_string(), "kind": kind })),
    )
        .into_response()
}

/// `GET /v1/mesh/roster-names/{pubkey}` — does the mesh's membership name
/// this key? This is the ring-roster membership test (fp-6): a ring roster
/// derived from membership keeps TOMBSTONED rows — a departed member's
/// journal lines still count, and dropping them would turn its signing
/// history into gaps — so unlike `/status` this answer does not filter
/// `removed_at`. The key is `NodePubkey`'s own lowercase-hex `Display`.
pub async fn roster_names(
    State(daemon): State<Arc<RailsDaemon>>,
    axum::extract::Path(pubkey): axum::extract::Path<String>,
) -> axum::response::Response {
    let key = match hex::decode(pubkey.trim())
        .map_err(|_| "the key is not hex")
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).map_err(|_| "the key is not 32 bytes"))
    {
        Ok(bytes) => commonwealth_core::ids::NodePubkey(bytes),
        Err(why) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": why })),
            )
                .into_response()
        }
    };
    let mesh = daemon.mesh.read().await;
    let named = mesh.members.values().any(|m| m.node_pubkey == Some(key));
    (StatusCode::OK, Json(serde_json::json!({ "named": named }))).into_response()
}

/// `POST /v1/mesh/publish` body — the same shape the inference daemon takes,
/// because `svrn run` posts one body to whichever daemon answered.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaimRequest {
    pub name: String,
    pub port: u16,
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}

/// `POST /v1/mesh/publish/{claim_id}/renew` body. An empty body is valid.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RenewRequest {
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}

fn ttl_of(secs: Option<u64>) -> std::time::Duration {
    secs.map(std::time::Duration::from_secs)
        .unwrap_or(commonwealth_media::apps::DEFAULT_CLAIM_TTL)
}

/// A name collision is a 409, an unusable name a 400, an unknown claim a 404.
/// Same mapping as the inference daemon's, so a runner reads one contract.
fn publish_refusal(e: PublishRefusal) -> axum::response::Response {
    let code = match e {
        PublishRefusal::BadName(_) => StatusCode::BAD_REQUEST,
        PublishRefusal::NameTaken { .. } => StatusCode::CONFLICT,
        PublishRefusal::NoSuchClaim(_) => StatusCode::NOT_FOUND,
    };
    tracing::info!(target: "rails", status = code.as_u16(), error = %e, "api: publish refused");
    (code, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
}

/// One refusal shape for all three routes, so a shim parses `error` once.
fn refusal(code: StatusCode, e: MediaReachRefusal) -> axum::response::Response {
    tracing::info!(target: "rails", status = code.as_u16(), error = %e, "api: refused");
    (code, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bind_is_loopback_whatever_the_port() {
        assert!(bind_addr(9747).ip().is_loopback());
        assert_eq!(bind_addr(9747).port(), 9747);
    }

    /// **The failing input.** This API has no auth because loopback IS the
    /// auth; serving it on `0.0.0.0` would publish an unauthenticated route
    /// that mints bridges into other people's media servers. The refusal
    /// happens before the bind, so there is no window in which it is up.
    #[test]
    fn a_non_loopback_listen_address_is_refused_before_it_binds() {
        for public in ["0.0.0.0:9747", "192.168.1.8:9747", "[::]:9747"] {
            let err = check_loopback(public.parse().unwrap())
                .expect_err("a non-loopback address must be refused");
            assert!(matches!(err, Refusal::NotLoopback(_)), "{err}");
            assert!(err.to_string().contains("loopback IS the auth"), "{err}");
        }
        for local in ["127.0.0.1:9747", "[::1]:9747", "127.1.2.3:9747"] {
            check_loopback(local.parse().unwrap()).expect(local);
        }
    }
}
