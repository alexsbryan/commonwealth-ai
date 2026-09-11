// SPDX-License-Identifier: AGPL-3.0-or-later
//! The viewer half: who offers a library, which member a name resolves to,
//! and the loopback URL a player is pointed at.
//!
//! Nothing here parses HTTP or knows what a title is. The transport caches
//! one bridge per `(peer, ALPN)` and retargets it in place when the peer's
//! dial info moves, so the URL this hands out stays valid for the life of
//! the process — a player can hold it. What it refuses, it refuses by name:
//! a missing member, an ambiguous prefix, an offline peer, a peer with no
//! iroh identity, no iroh path to it. None of those is a URL that will not
//! answer (ARCH §18.3).

use std::net::SocketAddr;
use std::sync::Arc;

use commonwealth_core::capabilities::OriginKind;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::{member_matches, MemberRecord, Mesh, NodeStatus};
use commonwealth_transport::iroh::{peer_path_snapshot, Endpoint, PublicKey};
use commonwealth_transport::{
    peer_contact, PeerContact, PeerEndpoint, PeerTransport, TrafficClass,
};
use serde::{Deserialize, Serialize};

/// The live iroh path to a peer, as `svrn mesh status` and the media routes
/// report it: a point-in-time snapshot from the endpoint's `remote_info`;
/// the operator's answer to "is this peer on a direct path or the relay?"
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerTransportPath {
    /// `direct` (active IP path, hole-punched), `relayed` (active only
    /// via a relay), `mixed` (both active), `idle` (known peer, no
    /// active path this moment), or `unknown` (endpoint has no record).
    pub path: String,
    /// The relay URL in active use, if the path rides one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
    /// Count of active direct (IP) addresses to this peer.
    pub active_direct_addrs: usize,
    /// Whether the path classification counts as "relayed" for the
    /// health term — the one reading both surfaces share.
    #[serde(default)]
    pub relayed_reading: bool,
}

/// The live connection path to `peer_pubkey` over `endpoint`. `None` when
/// the endpoint has no record of this peer (never dialed, or not
/// iroh-reachable). One conversion from the transport's snapshot, so the
/// operator surface and the health term cannot disagree about "reachable".
pub async fn path_to(endpoint: &Endpoint, peer_pubkey: &[u8; 32]) -> Option<PeerTransportPath> {
    let id = PublicKey::from_bytes(peer_pubkey).ok()?;
    let snap = peer_path_snapshot(endpoint, id).await?;
    Some(PeerTransportPath {
        path: snap.path.as_str().to_string(),
        relay: snap.relay,
        active_direct_addrs: snap.active_direct_addrs,
        relayed_reading: snap.path.is_relayed_reading(),
    })
}

/// What `GET /v1/mesh/media?peer=` returns: a URL on this host's loopback
/// that reaches the peer's media origin by key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaReach {
    pub peer: String,
    pub node_id: String,
    /// `http://127.0.0.1:<port>` — the bridge minted (or reused) for this
    /// peer's `cwth/media/0`.
    pub url: String,
    /// The endpoint label the transport resolved, e.g. `iroh:127.0.0.1:41231→46d0c1fb`.
    pub via: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PeerTransportPath>,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum MediaReachRefusal {
    #[error("no mesh is running here — `svrn mesh status`")]
    NoMesh,
    #[error("no member matching '{0}' — `svrn mesh status` lists the roster")]
    UnknownMember(String),
    #[error("'{0}' matches more than one member ({1}) — give a longer id prefix or the name")]
    Ambiguous(String, String),
    #[error("'{0}' is this node — its media origin is already local")]
    IsSelf(String),
    #[error("'{0}' is {1} — a bridge to it would accept and then never answer")]
    Offline(String, &'static str),
    #[error("'{0}' has no iroh identity (pre-identity daemon) — nothing to dial by key")]
    NoIdentity(String),
    #[error(
        "'{0}' advertises no media origin — it declares no media_origin, or its daemon \
         predates the advertisement and needs a restart; `svrn mesh media` lists who offers one"
    )]
    NoOrigin(String),
    #[error("no iroh path to '{0}': {1}")]
    NoPath(String, String),
    #[error("transport handed back a non-loopback endpoint for '{0}' ({1}) — refusing")]
    NotLoopback(String, String),
    #[error("bad request: {0}")]
    BadRequest(String),
}

/// One roster row as the media questions see it — derived from a
/// [`MemberRecord`] by [`candidate_of`], never assembled by hand outside
/// tests, so "offers media" and "active" have one derivation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaCandidate {
    pub node_id: NodeId,
    pub name: String,
    pub status: NodeStatus,
    pub has_identity: bool,
    pub active: bool,
    /// Whether the member's gossiped capabilities carry a `media` origin.
    pub offers_media: bool,
}

pub fn candidate_of(m: &MemberRecord) -> MediaCandidate {
    MediaCandidate {
        node_id: m.node_id,
        name: m.name.clone(),
        status: m.status,
        has_identity: m.node_pubkey.is_some(),
        active: m.is_active(),
        offers_media: m.capabilities.origins.contains(&OriginKind::Media),
    }
}

/// The roster snapshot every question below takes: each member as a
/// candidate, with the contact the transport dials. Cloned out of the mesh
/// so no caller holds a lock across a dial.
pub fn roster_of(mesh: &Mesh) -> Vec<(MediaCandidate, PeerContact)> {
    mesh.members
        .values()
        .map(|m| (candidate_of(m), peer_contact(m)))
        .collect()
}

/// One row of `GET /v1/mesh/media` with no peer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaOffer {
    pub peer: String,
    pub node_id: String,
    pub status: NodeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PeerTransportPath>,
}

/// The members offering a media origin, other than this node, by name.
/// Offline members that offer ARE rows (with their status) so a person
/// learns the library exists; members offering nothing are not.
pub fn offering_members(candidates: &[MediaCandidate], self_id: NodeId) -> Vec<MediaCandidate> {
    let mut rows: Vec<MediaCandidate> = candidates
        .iter()
        .filter(|c| c.active && c.offers_media && c.node_id != self_id)
        .cloned()
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

/// The read behind `svrn mesh media` with no peer. Nothing is dialed; the
/// live path per member comes from `paths` (see [`path_to`]).
pub fn offers(
    self_id: NodeId,
    roster: &[(MediaCandidate, PeerContact)],
    paths: &[(NodeId, PeerTransportPath)],
) -> Vec<MediaOffer> {
    let candidates: Vec<MediaCandidate> = roster.iter().map(|(c, _)| c.clone()).collect();
    let offers: Vec<MediaOffer> = offering_members(&candidates, self_id)
        .into_iter()
        .map(|c| MediaOffer {
            path: paths
                .iter()
                .find(|(id, _)| *id == c.node_id)
                .map(|(_, p)| p.clone()),
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
    offers
}

/// Resolve `query` (a member name, or a ≥4-char node-id prefix) to exactly
/// one member a bridge can be minted to, or say why not.
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

/// The URL contract: the only transport that may answer the media class
/// hands back bridges on `127.0.0.1`; anything else means the class was
/// routed to the plaintext overlay, and is refused rather than handed to a
/// player.
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

/// Mint (or reuse) the loopback bridge to `query`'s media origin and return
/// the URL a player is pointed at.
///
/// Not TCP-probed: the loopback listener accepts instantly whether or not
/// the peer is dialable, so a connect probe is a false positive by
/// construction. The peer's gossip status is the liveness evidence, and a
/// caller does one real `GET /` through the bridge if it wants an HTTP
/// status rather than a port.
pub async fn reach(
    self_id: NodeId,
    roster: &[(MediaCandidate, PeerContact)],
    query: &str,
    transport: &Arc<dyn PeerTransport>,
    paths: &[(NodeId, PeerTransportPath)],
) -> Result<MediaReach, MediaReachRefusal> {
    let candidates: Vec<MediaCandidate> = roster.iter().map(|(c, _)| c.clone()).collect();
    let picked = pick_member(&candidates, self_id, query)?;
    let contact = roster
        .iter()
        .find(|(c, _)| c.node_id == picked.node_id)
        .map(|(_, contact)| contact.clone())
        .expect("picked from this roster");
    let endpoints = transport.endpoints(&contact, TrafficClass::Media).await;
    let Some(ep) = endpoints.into_iter().next() else {
        let why = if contact.relay_url.is_none() && contact.iroh_direct_addrs.is_empty() {
            "the peer gossips no relay and no direct address (its iroh endpoint is off or not yet homed)"
        } else {
            "this node routes no class over iroh"
        };
        tracing::info!(
            target: "transport",
            peer = %picked.name,
            node_id = %picked.node_id,
            why,
            "media reach: refused — no iroh path"
        );
        return Err(MediaReachRefusal::NoPath(picked.name, why.to_string()));
    };
    let url = player_url(&picked.name, &ep)?;
    let path = paths
        .iter()
        .find(|(id, _)| *id == picked.node_id)
        .map(|(_, p)| p.clone());
    tracing::info!(
        target: "transport",
        peer = %picked.name,
        node_id = %picked.node_id,
        url = %url,
        via = %ep.label,
        path = ?path.as_ref().map(|p| p.path.as_str()),
        "media reach: bridge minted — a player pointed at this URL reaches the peer's media origin by key"
    );
    Ok(MediaReach {
        peer: picked.name,
        node_id: picked.node_id.to_string(),
        url,
        via: ep.label,
        path,
    })
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
