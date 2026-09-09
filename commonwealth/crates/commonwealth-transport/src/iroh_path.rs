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
}

/// One peer's live path, plus the detail the operator surface prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerPathSnapshot {
    pub path: PeerPath,
    /// The relay URL in active use, when the path rides one.
    pub relay: Option<String>,
    /// Count of ACTIVE direct (IP) addresses to this peer.
    pub active_direct_addrs: usize,
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
    let mut active_direct = 0usize;
    let mut any_addr = false;
    for a in info.addrs() {
        any_addr = true;
        let active = matches!(a.usage(), TransportAddrUsage::Active);
        match a.addr() {
            TransportAddr::Relay(url) if active => {
                active_relay.get_or_insert_with(|| url.to_string());
            }
            TransportAddr::Ip(_) if active => {
                active_direct += 1;
            }
            _ => {}
        }
    }
    Some(PeerPathSnapshot {
        path: classify_peer_path(active_direct > 0, active_relay.is_some(), any_addr),
        relay: active_relay,
        active_direct_addrs: active_direct,
    })
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
