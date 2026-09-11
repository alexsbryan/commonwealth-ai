// SPDX-License-Identifier: AGPL-3.0-or-later
//! Federated media, the viewer half: a member asks this daemon for a local
//! port that reaches a peer's `[iroh] media_origin`, and points a player at
//! it.
//!
//! The holder half (`iroh_access::AcceptorRoutes::media`, `MEDIA_ALPN`)
//! forwards a MEMBER's dial to the origin bound on its loopback and refuses
//! a stranger's. What was missing is the ask: nothing on this side minted a
//! bridge over that ALPN, so the only way to reach a peer's library was the
//! bench binary. `GET /v1/mesh/media?peer=<name-or-id>` is that ask, and
//! `svrn mesh media <peer>` prints what it returns.
//!
//! Nothing here parses HTTP or knows what a title is. The transport already
//! caches one bridge per `(peer, ALPN)` and retargets it in place when the
//! peer's dial info moves, so the URL this hands out stays valid for the life
//! of the daemon — a player can hold it. What it refuses, it refuses by name:
//! a missing member, an ambiguous prefix, an offline peer, a peer with no
//! iroh identity, a mesh with no iroh path to it. None of those is a URL that
//! will not answer (ARCH §18.3).
//!
//! Split out of `daemon.rs` and `mesh_http.rs` rather than added to them —
//! both are past ARCH §3.1's ceiling, and this is one concern with a seam of
//! its own, the same shape as `roster_repair`.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension, Query};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::NodeStatus;
use commonwealth_transport::{peer_contact, PeerEndpoint, TrafficClass};
use serde::{Deserialize, Serialize};

use crate::daemon::EmbeddedDaemon;
use crate::iroh_access::PeerTransportPath;
use crate::loopback_guard::enforce_localhost;
use crate::roster_repair::member_matches;

/// What a viewer gets back: the loopback URL to hand a player, and enough
/// about the path for the person watching to know what they are watching.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaReach {
    /// The peer's member name, as gossiped.
    pub peer: String,
    /// The peer's node id as `svrn mesh status` prints it (`node-…`), a
    /// string on the wire like `MemberDto::node_id` rather than a byte array.
    pub node_id: String,
    /// `http://127.0.0.1:<port>` — the bridge-local listener. Every TCP
    /// connection a player opens here becomes one QUIC stream to the peer,
    /// dialed by its Ed25519 key. Stable for the life of the daemon.
    pub url: String,
    /// The transport's own label for the endpoint (`iroh:127.0.0.1:NNNNN→<key8>`).
    pub via: String,
    /// The live QUIC path to this peer — `direct`, `relayed`, `mixed`,
    /// `idle` — read from the endpoint at the moment of the ask. `None` when
    /// the endpoint holds no record yet (nothing has been dialed). The demo's
    /// bar is stated on the RELAYED path, so this is what says which reading
    /// a play is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PeerTransportPath>,
}

/// Why no URL was handed out. Each is a fact about the roster or the
/// transport, named so the CLI can say it; none is a guess.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MediaReachRefusal {
    /// The daemon is not running, or runs solo with no mesh to look in.
    #[error("no mesh is running here — `svrn mesh status`")]
    NoMesh,
    /// No active member's name or id prefix matched.
    #[error("no member matching '{0}' — `svrn mesh status` lists the roster")]
    UnknownMember(String),
    /// More than one member matched a prefix; naming one is the caller's job.
    #[error("'{0}' matches more than one member ({1}) — give a longer id prefix or the name")]
    Ambiguous(String, String),
    /// The member is this node. Its own origin is on its own loopback.
    #[error("'{0}' is this node — its media origin is already local")]
    IsSelf(String),
    /// Gossip has not seen the peer recently. A bridge accepts instantly
    /// regardless, so the URL would look alive and the player would stall.
    #[error("'{0}' is {1} — a bridge to it would accept and then never answer")]
    Offline(String, &'static str),
    /// The peer runs a pre-identity build; there is no key to dial.
    #[error("'{0}' has no iroh identity (pre-identity daemon) — nothing to dial by key")]
    NoIdentity(String),
    /// The peer advertises no media origin. A bridge to it would accept and
    /// then be closed at the far end (`MEDIA_ALPN` is not served), which a
    /// player reports as a stall. The roster already knows; say it — and say
    /// the one case where the roster can be behind.
    #[error(
        "'{0}' advertises no media origin — it declares no [iroh] media_origin, or its daemon \
         predates the advertisement and needs a restart; `svrn mesh media` lists who offers one"
    )]
    NoOrigin(String),
    /// The transport composed here has no iroh path for this class —
    /// `[iroh] enabled = false` locally, or the peer gossips no relay and no
    /// direct address. Says which side, because the fixes differ.
    #[error("no iroh path to '{0}': {1}")]
    NoPath(String, String),
    /// The transport produced a non-loopback authority, which only a
    /// misroute can do. Refused rather than handed to a player.
    #[error("transport handed back a non-loopback endpoint for '{0}' ({1}) — refusing")]
    NotLoopback(String, String),
}

/// The roster facts the resolver reads — projected so the decision can be
/// tested without a `MemberRecord` (fourteen fields, most irrelevant here).
#[derive(Debug, Clone)]
pub struct MediaCandidate {
    pub node_id: NodeId,
    pub name: String,
    pub status: NodeStatus,
    pub has_identity: bool,
    pub active: bool,
    /// `NodeCapabilities::origins` contains `Media` — the member's live
    /// acceptor routes the media ALPN to a local origin.
    pub offers_media: bool,
}

/// One row of `svrn mesh media` with no peer: a member that serves a media
/// origin, as the roster knows it right now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaOffer {
    pub peer: String,
    pub node_id: String,
    /// `online` | `busy` | `away` | `offline`, the roster's word.
    pub status: NodeStatus,
    /// The live QUIC path to this peer, when the endpoint holds one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PeerTransportPath>,
}

/// The members that offer a media origin — every active member other than
/// this node whose gossiped capabilities carry `Media`. Offline members are
/// listed with their status rather than dropped: a person deciding what to
/// play wants to know a library exists and is away, not that it does not
/// exist. Self is excluded for the reason `pick_member` refuses it.
pub fn offering_members(candidates: &[MediaCandidate], self_id: NodeId) -> Vec<MediaCandidate> {
    let mut rows: Vec<MediaCandidate> = candidates
        .iter()
        .filter(|c| c.active && c.offers_media && c.node_id != self_id)
        .cloned()
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

/// Pick the one member `query` names, or say why there is not one.
///
/// Tombstoned rows are invisible (a departed node is not a place to play
/// from). Matching is `roster_repair::member_matches` — exact name, or an id
/// prefix of at least four characters — so `svrn mesh media` and
/// `svrn mesh forget-member` resolve the same argument the same way.
pub fn pick_member(
    candidates: &[MediaCandidate],
    self_id: NodeId,
    query: &str,
) -> Result<MediaCandidate, MediaReachRefusal> {
    let matched: Vec<&MediaCandidate> = candidates
        .iter()
        .filter(|c| c.active && member_matches(c.node_id, &c.name, query))
        .collect();
    let picked = match matched.as_slice() {
        [] => return Err(MediaReachRefusal::UnknownMember(query.to_string())),
        [one] => (*one).clone(),
        many => {
            let names = many
                .iter()
                .map(|c| format!("{} {}", c.name, c.node_id))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(MediaReachRefusal::Ambiguous(query.to_string(), names));
        }
    };
    if picked.node_id == self_id {
        return Err(MediaReachRefusal::IsSelf(picked.name));
    }
    match picked.status {
        NodeStatus::Online | NodeStatus::Busy => {}
        NodeStatus::Away => return Err(MediaReachRefusal::Offline(picked.name, "away")),
        NodeStatus::Offline => return Err(MediaReachRefusal::Offline(picked.name, "offline")),
    }
    if !picked.has_identity {
        return Err(MediaReachRefusal::NoIdentity(picked.name));
    }
    if !picked.offers_media {
        return Err(MediaReachRefusal::NoOrigin(picked.name));
    }
    Ok(picked)
}

/// The URL a player gets, from the first candidate the transport resolved.
/// Loopback is the whole contract: the iroh transport hands back a bridge
/// on `127.0.0.1`, and anything else means the class was routed somewhere
/// it must never go.
pub fn player_url(peer: &str, ep: &PeerEndpoint) -> Result<String, MediaReachRefusal> {
    let authority = ep.base_url.strip_prefix("http://").unwrap_or(&ep.base_url);
    let addr: SocketAddr = authority
        .parse()
        .map_err(|_| MediaReachRefusal::NotLoopback(peer.to_string(), ep.base_url.clone()))?;
    if !addr.ip().is_loopback() {
        return Err(MediaReachRefusal::NotLoopback(
            peer.to_string(),
            ep.base_url.clone(),
        ));
    }
    Ok(format!("http://{addr}"))
}

impl EmbeddedDaemon {
    /// Mint (or reuse) the loopback bridge to `query`'s media origin and
    /// return the URL a player is pointed at.
    ///
    /// Not TCP-probed, for the same reason `bridge_rpc_endpoint` is not: the
    /// loopback listener accepts instantly whether or not the peer is
    /// dialable, so a connect probe is a false positive by construction. The
    /// peer's gossip status is the liveness evidence, and the CLI does one
    /// real `GET /` through the bridge so the person sees an HTTP status
    /// rather than a port.
    /// The members offering a media origin, with the live path to each. The
    /// read behind `svrn mesh media` with no peer; nothing is dialed.
    pub async fn media_offers(&self) -> Result<Vec<MediaOffer>, MediaReachRefusal> {
        let app_state = self.app_state().await.ok_or(MediaReachRefusal::NoMesh)?;
        let self_id = app_state.self_node_id();
        let candidates: Vec<MediaCandidate> = {
            let mesh = app_state.inner.mesh.read().await;
            mesh.members
                .values()
                .map(|m| MediaCandidate {
                    node_id: m.node_id,
                    name: m.name.clone(),
                    status: m.status,
                    has_identity: m.node_pubkey.is_some(),
                    active: m.is_active(),
                    offers_media: m
                        .capabilities
                        .origins
                        .contains(&commonwealth_core::capabilities::OriginKind::Media),
                })
                .collect()
        };
        let paths = self.iroh_transport_snapshot().await;
        let offers: Vec<MediaOffer> = offering_members(&candidates, self_id)
            .into_iter()
            .map(|c| MediaOffer {
                path: paths
                    .iter()
                    .find(|p| p.node_id == c.node_id)
                    .and_then(|p| p.path.clone()),
                peer: c.name,
                node_id: c.node_id.to_string(),
                status: c.status,
            })
            .collect();
        tracing::info!(
            target: "transport",
            offering = offers.len(),
            roster = candidates.len(),
            "media offers: listed from gossiped origins, nothing dialed"
        );
        Ok(offers)
    }

    pub async fn media_reach(&self, query: &str) -> Result<MediaReach, MediaReachRefusal> {
        let app_state = self.app_state().await.ok_or(MediaReachRefusal::NoMesh)?;
        let self_id = app_state.self_node_id();
        let (candidates, record_of) = {
            let mesh = app_state.inner.mesh.read().await;
            let candidates: Vec<MediaCandidate> = mesh
                .members
                .values()
                .map(|m| MediaCandidate {
                    node_id: m.node_id,
                    name: m.name.clone(),
                    status: m.status,
                    has_identity: m.node_pubkey.is_some(),
                    active: m.is_active(),
                    offers_media: m
                        .capabilities
                        .origins
                        .contains(&commonwealth_core::capabilities::OriginKind::Media),
                })
                .collect();
            let picked = pick_member(&candidates, self_id, query)?;
            let record = mesh
                .members
                .get(&picked.node_id)
                .cloned()
                .expect("picked from this roster");
            (picked, record)
        };
        let transport = app_state.peer_transport();
        let endpoints = transport
            .endpoints(&peer_contact(&record_of), TrafficClass::Media)
            .await;
        let Some(ep) = endpoints.into_iter().next() else {
            // The transport says which half is missing at debug; here the
            // operator gets the two causes that reading the config decides.
            let why = if record_of.relay_url.is_none() && record_of.iroh_direct_addrs.is_empty() {
                "the peer gossips no relay and no direct address (its [iroh] endpoint is off or not yet homed)"
            } else {
                "this node routes no class over iroh ([iroh] enabled = false here)"
            };
            tracing::info!(
                target: "transport",
                peer = %record_of.name,
                node_id = %record_of.node_id,
                why,
                "media reach: refused — no iroh path"
            );
            return Err(MediaReachRefusal::NoPath(
                candidates.name.clone(),
                why.to_string(),
            ));
        };
        let url = player_url(&candidates.name, &ep)?;
        let path = self
            .iroh_transport_snapshot()
            .await
            .into_iter()
            .find(|p| p.node_id == record_of.node_id)
            .and_then(|p| p.path);
        tracing::info!(
            target: "transport",
            peer = %record_of.name,
            node_id = %record_of.node_id,
            url = %url,
            via = %ep.label,
            path = ?path.as_ref().map(|p| p.path.as_str()),
            "media reach: bridge minted — a player pointed at this URL reaches the peer's media origin by key"
        );
        Ok(MediaReach {
            peer: candidates.name,
            node_id: record_of.node_id.to_string(),
            url,
            via: ep.label,
            path,
        })
    }
}

/// Query for `GET /v1/mesh/media`.
#[derive(Debug, Deserialize)]
pub struct MediaQuery {
    /// Member name or node-id prefix (≥4 chars), as `svrn mesh status` shows.
    /// Absent: list the members that offer a media origin instead.
    #[serde(default)]
    pub peer: Option<String>,
}

/// `GET /v1/mesh/media?peer=<name-or-id>` — the loopback URL that reaches
/// that member's `[iroh] media_origin`. Loopback-only like every `/v1/mesh/*`
/// route: the URL it returns is only usable from this machine anyway.
pub async fn mesh_media(
    ConnectInfo(caller): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Query(q): Query<MediaQuery>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&caller) {
        return r;
    }
    let Some(peer) = q.peer.as_deref().map(str::trim).filter(|p| !p.is_empty()) else {
        return match daemon.media_offers().await {
            Ok(offers) => (
                StatusCode::OK,
                Json(serde_json::json!({ "offering": offers })),
            )
                .into_response(),
            Err(e) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response(),
        };
    };
    match daemon.media_reach(peer).await {
        Ok(reach) => (StatusCode::OK, Json(serde_json::json!(reach))).into_response(),
        Err(e @ MediaReachRefusal::UnknownMember(_)) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
        // Every other refusal is "the request is coherent and the mesh's
        // state is what says no" — 409, as `forget-member` reports it.
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

    fn cand(id: u128, name: &str, status: NodeStatus) -> MediaCandidate {
        MediaCandidate {
            node_id: NodeId::from_u128(id),
            name: name.into(),
            status,
            has_identity: true,
            active: true,
            offers_media: true,
        }
    }

    const ME: u128 = 0xA11CE;

    fn roster() -> Vec<MediaCandidate> {
        vec![
            cand(ME, "RuggedFox", NodeStatus::Online),
            cand(0xB0B, "LittleMac", NodeStatus::Online),
            cand(0xB0B0, "BeefyMac", NodeStatus::Offline),
        ]
    }

    /// **The refusal the catalogue is built on.** A member whose gossiped
    /// capabilities carry no `media` origin would still accept a bridge —
    /// the far end closes the dial and the player stalls. The failing input
    /// is `pick_member` returning `Ok` for it.
    #[test]
    fn a_member_that_advertises_no_media_origin_is_refused_by_name() {
        let mut roster = roster();
        roster[1].offers_media = false;
        let err = pick_member(&roster, NodeId::from_u128(ME), "LittleMac").unwrap_err();
        assert_eq!(err, MediaReachRefusal::NoOrigin("LittleMac".into()));
        assert!(
            err.to_string().contains("advertises no media origin"),
            "{err}"
        );
    }

    /// The list is the roster's `media` origins minus this node; a member
    /// offering nothing is not a row, and an offline member that offers IS a
    /// row, carrying its status, so a person learns the library exists.
    #[test]
    fn the_offer_list_is_offering_members_other_than_self_with_their_status() {
        let mut roster = roster();
        roster.push(cand(0xC0DE, "Quiet", NodeStatus::Online));
        roster[3].offers_media = false;
        let rows = offering_members(&roster, NodeId::from_u128(ME));
        let names: Vec<(&str, NodeStatus)> =
            rows.iter().map(|c| (c.name.as_str(), c.status)).collect();
        assert_eq!(
            names,
            vec![
                ("BeefyMac", NodeStatus::Offline),
                ("LittleMac", NodeStatus::Online)
            ]
        );
    }

    /// The happy path, and the one that makes the refusals below mean
    /// something: an online member with a key resolves by name.
    #[test]
    fn an_online_member_resolves_by_name() {
        let picked = pick_member(&roster(), NodeId::from_u128(ME), "LittleMac").unwrap();
        assert_eq!(picked.node_id, NodeId::from_u128(0xB0B));
    }

    /// **The refusal that matters for a demo.** A bridge to an offline peer
    /// binds and accepts like any other — the failing input is this
    /// returning `Ok` for `BeefyMac`, which hands the player a port that
    /// looks alive and never sends a byte. Offline is a fact the roster
    /// already holds; say it.
    #[test]
    fn an_offline_member_is_refused_by_name_not_handed_a_dead_port() {
        let err = pick_member(&roster(), NodeId::from_u128(ME), "BeefyMac").unwrap_err();
        assert_eq!(
            err,
            MediaReachRefusal::Offline("BeefyMac".into(), "offline")
        );
    }

    /// A tombstoned row is not a place to play from, even when its status
    /// field still says online (removal is a separate fact from liveness).
    #[test]
    fn a_retired_member_is_unknown() {
        let mut r = roster();
        r[1].active = false;
        let err = pick_member(&r, NodeId::from_u128(ME), "LittleMac").unwrap_err();
        assert_eq!(err, MediaReachRefusal::UnknownMember("LittleMac".into()));
    }

    /// Two members share the prefix `node-0000` here; the resolver must not
    /// pick one, and it names both so the caller can.
    #[test]
    fn an_ambiguous_prefix_names_the_candidates_rather_than_picking_one() {
        let a = NodeId::from_u128(0xB0B).to_string();
        let b = NodeId::from_u128(0xB0B0).to_string();
        let common = a
            .chars()
            .zip(b.chars())
            .take_while(|(x, y)| x == y)
            .map(|(x, _)| x)
            .collect::<String>();
        assert!(
            common.len() >= 4,
            "fixture ids must share ≥4 chars: {common}"
        );
        let err = pick_member(&roster(), NodeId::from_u128(ME), &common).unwrap_err();
        match err {
            MediaReachRefusal::Ambiguous(q, names) => {
                assert_eq!(q, common);
                assert!(
                    names.contains("LittleMac") && names.contains("BeefyMac"),
                    "{names}"
                );
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    /// This node's own origin is on its own loopback; a bridge to itself is
    /// a loop, not a feature.
    #[test]
    fn this_node_is_refused_as_self() {
        let err = pick_member(&roster(), NodeId::from_u128(ME), "RuggedFox").unwrap_err();
        assert_eq!(err, MediaReachRefusal::IsSelf("RuggedFox".into()));
    }

    /// A pre-identity peer has nothing to dial by key. Refused here rather
    /// than letting the transport say "no pubkey" at debug and the CLI print
    /// nothing.
    #[test]
    fn a_member_without_an_identity_key_is_refused() {
        let mut r = roster();
        r[1].has_identity = false;
        let err = pick_member(&r, NodeId::from_u128(ME), "LittleMac").unwrap_err();
        assert_eq!(err, MediaReachRefusal::NoIdentity("LittleMac".into()));
    }

    /// The URL contract, watched failing: an endpoint on anything but
    /// loopback is refused, because the only transport that may answer
    /// this class hands back bridges on `127.0.0.1` and a LAN address here
    /// means the class was routed to the plaintext overlay.
    #[test]
    fn a_non_loopback_endpoint_is_refused_not_handed_to_a_player() {
        let ok = PeerEndpoint {
            base_url: "http://127.0.0.1:41231".into(),
            label: "iroh:127.0.0.1:41231→46d0c1fb".into(),
        };
        assert_eq!(
            player_url("LittleMac", &ok).unwrap(),
            "http://127.0.0.1:41231"
        );
        let lan = PeerEndpoint {
            base_url: "http://192.168.1.8:8096".into(),
            label: "ip:192.168.1.8:8096".into(),
        };
        assert!(matches!(
            player_url("LittleMac", &lan).unwrap_err(),
            MediaReachRefusal::NotLoopback(_, _)
        ));
    }
}
