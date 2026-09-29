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
        sel = reachable_rpc_endpoint(&dial.addresses, rpc_port)
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
