// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which endpoint ggml dials for a worker once its RPC port is known: the
//! direct raw-TCP probe of the member's addresses and the choice between it,
//! the iroh bridge and the last-resort probe host (lifted out of
//! `RpcWorkerDiscovery::discover` unchanged, pb-serve-distributes).

use std::net::SocketAddr;
use std::sync::Arc;

use mesh_reach::{PeerContact, PeerTransport};

use super::{bridge_rpc_endpoint, rpc_tunnel_mode, RpcTunnelMode};

/// Choose the endpoint ggml will dial for a worker on `rpc_port`. Direct raw
/// TCP to a member IP is the LAN fast path; the iroh bridge is the
/// cross-network path; the parsed probe host is the last resort.
/// `SOVEREIGN_RPC_TUNNEL` = `always` prefers the bridge; `never` opts out of
/// bridging. Returns `(endpoint, via)`.
pub(super) async fn select_rpc_endpoint(
    transport: &Arc<dyn PeerTransport>,
    dial: &PeerContact,
    rpc_port: u16,
    iroh_advertised: bool,
    probe_host: &str,
) -> Option<(String, String)> {
    let mode = rpc_tunnel_mode();
    let allow_bridge = iroh_advertised && mode != RpcTunnelMode::Never;
    let mut sel: Option<(String, String)> = None;
    if allow_bridge && mode == RpcTunnelMode::Always {
        sel = bridge_rpc_endpoint(transport, dial).await;
    }
    if sel.is_none() {
        sel = reachable_rpc_endpoint(direct_candidates(dial), rpc_port)
            .await
            .map(|d| (d, "direct-ip".to_string()));
    }
    if sel.is_none() && allow_bridge {
        sel = bridge_rpc_endpoint(transport, dial).await;
    }
    if sel.is_none() {
        sel = Some((format!("{probe_host}:{rpc_port}"), "probe-host".to_string()));
    }
    sel
}

/// The IPs the direct probe tries: the member's overlay addresses, or, when
/// the roster carries none, the iroh direct addresses it gossips — the same
/// LAN and Tailscale IPs, which cw-rails' roster fills and its overlay field
/// leaves empty (phase-b-36: the bigger-model flow keeps its direct path).
pub(super) fn direct_candidates(dial: &PeerContact) -> &[SocketAddr] {
    if dial.addresses.is_empty() {
        tracing::debug!(
            node = %dial.node_id,
            direct = dial.iroh_direct_addrs.len(),
            "no overlay address on the roster: probing the member's iroh direct addresses"
        );
        &dial.iroh_direct_addrs
    } else {
        &dial.addresses
    }
}

/// The raw-TCP rpc-server needs the peer's DIRECT IP. The `/status` probe
/// URL host is unreliable for this: when `status_probe` is routed over iroh,
/// the probe authority is a loopback proxy (`127.0.0.1:<ephemeral>`), which
/// is NOT where the peer's rpc-server listens. Derive the endpoint from the
/// member's advertised IPs instead — prefer private-LAN (lowest latency for
/// per-layer activation traffic), then CGNAT/Tailscale, then anything else —
/// and reachability-probe each so we only return an openable socket.
pub(super) async fn reachable_rpc_endpoint(
    addresses: &[SocketAddr],
    rpc_port: u16,
) -> Option<String> {
    fn rank(ip: &std::net::IpAddr) -> u8 {
        match ip {
            std::net::IpAddr::V4(v) if v.is_private() => 0,
            std::net::IpAddr::V4(v) if v.octets()[0] == 100 && (v.octets()[1] & 0xC0) == 0x40 => 1,
            std::net::IpAddr::V4(_) => 2,
            std::net::IpAddr::V6(_) => 3,
        }
    }
    let mut cands: Vec<std::net::IpAddr> = addresses.iter().map(|a| a.ip()).collect();
    cands.sort_by_key(rank);
    cands.dedup();
    for ip in cands {
        let ep = SocketAddr::new(ip, rpc_port);
        if tokio::time::timeout(
            std::time::Duration::from_millis(600),
            tokio::net::TcpStream::connect(ep),
        )
        .await
        .ok()
        .and_then(|r| r.ok())
        .is_some()
        {
            return Some(ep.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_types::NodeId;
    use mesh_reach::{PeerEndpoint, TrafficClass};

    /// A transport with no path to anyone: no bridge, so only the direct
    /// probe can choose `direct-ip`.
    #[derive(Debug)]
    struct NoBridge;

    #[async_trait::async_trait]
    impl PeerTransport for NoBridge {
        fn name(&self) -> &'static str {
            "none"
        }
        async fn endpoints(&self, _: &PeerContact, _: TrafficClass) -> Vec<PeerEndpoint> {
            Vec::new()
        }
    }

    fn contact(addresses: Vec<SocketAddr>, iroh_direct_addrs: Vec<SocketAddr>) -> PeerContact {
        PeerContact {
            node_id: NodeId::from_u128(7),
            addresses,
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs,
        }
    }

    /// phase-b-36: cw-rails' roster leaves the overlay addresses empty and
    /// carries the member's iroh direct addresses, and the bigger-model flow
    /// must still reach a worker on its LAN IP by raw TCP. A worker listening
    /// on loopback, named only by an iroh direct address (its UDP port is
    /// not the worker's), is chosen `direct-ip` at the advertised rpc port.
    /// Failing input: read only `dial.addresses`, and nothing answers, so the
    /// choice falls to the probe host.
    #[tokio::test]
    async fn a_member_with_no_overlay_address_is_dialled_at_its_iroh_direct_address() {
        let worker = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = worker.local_addr().expect("addr").port();
        let transport: Arc<dyn PeerTransport> = Arc::new(NoBridge);
        let roster_only_iroh = contact(Vec::new(), vec!["127.0.0.1:4433".parse().unwrap()]);
        assert_eq!(
            select_rpc_endpoint(&transport, &roster_only_iroh, port, false, "probe").await,
            Some((format!("127.0.0.1:{port}"), "direct-ip".to_string()))
        );
        // A member the overlay does name is probed there, as before.
        let overlay = contact(vec!["127.0.0.1:9742".parse().unwrap()], Vec::new());
        assert_eq!(
            select_rpc_endpoint(&transport, &overlay, port, false, "probe").await,
            Some((format!("127.0.0.1:{port}"), "direct-ip".to_string()))
        );
    }
}
