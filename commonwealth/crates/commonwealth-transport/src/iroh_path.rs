// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-peer live path classification for the iroh endpoint.
//!
//! Split out of `iroh.rs` (already past ARCH §3.2's 1200-line ceiling) rather
//! than added to it. Everything here is re-exported from [`crate::iroh`], so
//! the public spelling is unchanged: `commonwealth_transport::iroh::PeerPath`.
//!
//! This is the ONE implementation of "how is this peer reachable right now"
//! (ARCH §10.6). Two consumers read it and they must not disagree: the
//! operator surface (`/v1/mesh/status.iroh_transport`, `svrn mesh transport`)
//! and the reachability watchdog's peer-path health term.

use crate::iroh::{Endpoint, PublicKey, TransportAddr, TransportAddrUsage};
use std::net::SocketAddr;

/// How a peer is reachable on this endpoint RIGHT NOW, as the endpoint's
/// `remote_info` sees it. A closed set (ARCH §2) with one spelling
/// ([`PeerPath::as_str`]), so the operator surface and the watchdog's
/// peer-path health term cannot drift apart on the vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerPath {
    /// An active direct (hole-punched) IP path — no relay in the loop.
    Direct,
    /// Active only through a relay.
    Relayed,
    /// A direct path and a relay are both active.
    Mixed,
    /// The endpoint holds addresses for this peer but NONE is active.
    Idle,
    /// A record exists carrying no addresses at all.
    Unknown,
}

impl PeerPath {
    /// The one wire/display spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Relayed => "relayed",
            Self::Mixed => "mixed",
            Self::Idle => "idle",
            Self::Unknown => "unknown",
        }
    }

    /// Whether traffic can flow to this peer this instant.
    ///
    /// `Idle`/`Unknown` are records WITHOUT a live path, which is exactly
    /// what a connection that degraded and died looks like from here — so
    /// a reachability judgement must read this, not "does a record exist".
    pub fn is_active(self) -> bool {
        matches!(self, Self::Direct | Self::Relayed | Self::Mixed)
    }

    /// Whether a measurement taken over this path RIGHT NOW is a RELAYED
    /// reading — the one the federated-media bar is stated on
    /// (WORK_PLANE.md: ≥25 Mbit/s for 10 min, no stall over 2 s, on the
    /// relayed path).
    ///
    /// Only `Relayed` qualifies. `Mixed` means a direct leg and a relay are
    /// both live, and iroh sends on the direct leg whenever it can — so a
    /// play over `mixed` is a direct number wearing a relay's name. That is
    /// the failing input this method exists to refuse: on one LAN every
    /// path reads `mixed`, and a bar cleared there would be a claim about a
    /// relay nothing went through. The way to take a relayed reading on
    /// purpose is `SOVEREIGN_IROH_RELAY_ONLY=1` on BOTH ends, after which
    /// this reads `relayed`.
    pub fn is_relayed_reading(self) -> bool {
        matches!(self, Self::Relayed)
    }
}

/// One peer's live path, plus the detail the operator surface prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerPathSnapshot {
    pub path: PeerPath,
    /// The relay URL in active use, when the path rides one.
    pub relay: Option<String>,
    /// Count of ACTIVE direct (IP) addresses to this peer.
    pub active_direct_addrs: usize,
    /// The ACTIVE direct addresses themselves, in the order the endpoint
    /// lists them. A count cannot name a wire: on 2026-09-20 the ring-room
    /// cut leg recorded this surface hoping to learn WHICH address carried
    /// bytes through a cut uplink, and the only reading available was "2".
    /// `active_direct_addrs` is this vector's length, so the two cannot
    /// disagree.
    pub active_direct_socket_addrs: Vec<SocketAddr>,
}

/// Pure half of [`peer_path_snapshot`] — the classification, so it has
/// failing inputs a test can name without binding an endpoint (ARCH §18.1).
fn classify_peer_path(direct_active: bool, relay_active: bool, any_addr: bool) -> PeerPath {
    match (direct_active, relay_active) {
        (true, true) => PeerPath::Mixed,
        (true, false) => PeerPath::Direct,
        (false, true) => PeerPath::Relayed,
        (false, false) if any_addr => PeerPath::Idle,
        (false, false) => PeerPath::Unknown,
    }
}

/// THE per-peer path classifier (ARCH §10.6 — one implementation, read by
/// both `/v1/mesh/status.iroh_transport` and the reachability watchdog).
///
/// `None` means the endpoint holds no `remote_info` record for this peer AT
/// ALL — never dialed, or the record has been dropped. That is a different
/// fact from [`PeerPath::Idle`] (a record with nothing active), and the
/// difference is load-bearing: a live capture on 2026-09-09 showed every
/// peer decaying from an active path to NO RECORD while the endpoint's own
/// relay-home and self-discovery signals stayed green.
pub async fn peer_path_snapshot(endpoint: &Endpoint, peer: PublicKey) -> Option<PeerPathSnapshot> {
    let info = endpoint.remote_info(peer).await?;
    let mut active_relay: Option<String> = None;
    let mut active_direct: Vec<SocketAddr> = Vec::new();
    let mut any_addr = false;
    for a in info.addrs() {
        any_addr = true;
        let active = matches!(a.usage(), TransportAddrUsage::Active);
        match a.addr() {
            TransportAddr::Relay(url) if active => {
                active_relay.get_or_insert_with(|| url.to_string());
            }
            TransportAddr::Ip(ip) if active => {
                active_direct.push(*ip);
            }
            _ => {}
        }
    }
    Some(snapshot_of(active_direct, active_relay, any_addr))
}

/// Pure half of the ASSEMBLY, for the same reason [`classify_peer_path`] is
/// pure: the count and the address list are one fact told twice, and this is
/// the one place that can make them disagree.
fn snapshot_of(
    active_direct: Vec<SocketAddr>,
    active_relay: Option<String>,
    any_addr: bool,
) -> PeerPathSnapshot {
    PeerPathSnapshot {
        path: classify_peer_path(!active_direct.is_empty(), active_relay.is_some(), any_addr),
        relay: active_relay,
        active_direct_addrs: active_direct.len(),
        active_direct_socket_addrs: active_direct,
    }
}

#[cfg(test)]
mod tests {
    /// Every state of the path classifier, including the one the wire cares
    /// about most: a record with addresses but nothing active is `idle`, NOT
    /// a missing record. `peer_path_snapshot` returns `None` for the latter.
    #[test]
    fn classify_peer_path_covers_all_states() {
        use super::{classify_peer_path, PeerPath};
        assert_eq!(classify_peer_path(true, true, true), PeerPath::Mixed);
        assert_eq!(classify_peer_path(true, false, true), PeerPath::Direct);
        assert_eq!(classify_peer_path(false, true, true), PeerPath::Relayed);
        assert_eq!(classify_peer_path(false, false, true), PeerPath::Idle);
        assert_eq!(classify_peer_path(false, false, false), PeerPath::Unknown);
    }

    /// The bar's reading, watched on the input that matters: `mixed` is a
    /// direct number under a relay's name, and must not count. `relayed`
    /// alone does.
    #[test]
    fn only_a_relayed_path_is_a_relayed_reading() {
        use super::PeerPath;
        assert!(PeerPath::Relayed.is_relayed_reading());
        for p in [
            PeerPath::Mixed,
            PeerPath::Direct,
            PeerPath::Idle,
            PeerPath::Unknown,
        ] {
            assert!(
                !p.is_relayed_reading(),
                "{} must not pass as a relayed reading",
                p.as_str()
            );
        }
    }

    /// The count NAMES the list. A snapshot that says `direct=2` while
    /// carrying no address is the reading the ring-room cut leg got on
    /// 2026-09-20 when it needed the wire, and it is unreachable here by
    /// construction — the length is taken from the vector itself.
    #[test]
    fn the_direct_count_is_the_address_list() {
        use super::{snapshot_of, PeerPath};
        let addrs = vec![
            "10.89.60.11:19942".parse().unwrap(),
            "10.89.61.11:19942".parse().unwrap(),
        ];
        let s = snapshot_of(addrs.clone(), None, true);
        assert_eq!(s.path, PeerPath::Direct);
        assert_eq!(s.active_direct_addrs, 2);
        assert_eq!(s.active_direct_socket_addrs, addrs);
        let empty = snapshot_of(Vec::new(), Some("relay".into()), true);
        assert_eq!(empty.path, PeerPath::Relayed);
        assert_eq!(empty.active_direct_addrs, 0);
        assert!(empty.active_direct_socket_addrs.is_empty());
    }

    /// `is_active` is what a reachability judgement reads. A path that
    /// degraded to `idle` must NOT count as reachable — that conflation is
    /// how a dead connection reads healthy.
    #[test]
    fn only_paths_with_a_live_leg_are_active() {
        use super::PeerPath;
        assert!(PeerPath::Direct.is_active());
        assert!(PeerPath::Relayed.is_active());
        assert!(PeerPath::Mixed.is_active());
        assert!(!PeerPath::Idle.is_active());
        assert!(!PeerPath::Unknown.is_active());
    }
}
