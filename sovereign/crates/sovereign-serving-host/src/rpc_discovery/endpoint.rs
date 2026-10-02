// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which endpoint ggml dials for a worker once its RPC port is known: the
//! direct raw-TCP probe of the member's addresses and the choice between it,
//! the iroh bridge and the last-resort probe host (lifted out of
//! `RpcWorkerDiscovery::discover` unchanged, pb-serve-distributes).

use std::net::SocketAddr;
use std::sync::Arc;

use mesh_reach::{PeerContact, PeerTransport};

use super::{bridge_rpc_endpoint, rpc_tunnel_mode, RpcTunnelMode};

/// What a worker's record says about binding where a host could dial it
/// directly. Raw ggml RPC carries no identity, so only the iroh bridge (bound
/// to the member's key) proves the member; a direct address is dialled only
/// where the worker said it listens (pc-rpc-probe-identity).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DirectBind {
    /// The operator let the worker bind past loopback (plaintext LAN): the
    /// direct probe and the probe host may run, unproven inside the boundary
    /// that operator declared.
    Declared,
    /// No direct bind declared: the worker listens on loopback, so anything
    /// answering at the member's address is not it. Bridge only.
    Undeclared,
    /// A `/status` record from a build that predates the declaration: main's
    /// path, every direct choice traced as unproven.
    Unstated,
}

impl DirectBind {
    /// The anchor record's `rpc_direct`. Every record that carries `rpc_port`
    /// is a cut build, so a missing field is a declaration of none.
    pub(super) fn from_anchor(rpc_direct: bool) -> Self {
        if rpc_direct {
            Self::Declared
        } else {
            Self::Undeclared
        }
    }

    /// The `/status` `rpc_worker.direct` field; absent from a pre-cut daemon.
    pub(super) fn from_status(direct: Option<bool>) -> Self {
        direct.map_or(Self::Unstated, Self::from_anchor)
    }
}

/// Choose the endpoint ggml will dial for a worker on `rpc_port`. Direct raw
/// TCP to a member IP is the LAN fast path, for a worker that declared it;
/// the iroh bridge is the identity-bound path; the parsed probe host, when
/// the port came from a `/status` probe, is the last resort.
/// `SOVEREIGN_RPC_TUNNEL` = `always` prefers the bridge; `never` opts out of
/// bridging. Returns `(endpoint, via)`, or `None` when nothing answered and
/// there is no probe host.
pub(super) async fn select_rpc_endpoint(
    transport: &Arc<dyn PeerTransport>,
    dial: &PeerContact,
    rpc_port: u16,
    iroh_advertised: bool,
    direct: DirectBind,
    probe_host: Option<&str>,
) -> Option<(String, String)> {
    select_in_mode(
        rpc_tunnel_mode(),
        transport,
        dial,
        rpc_port,
        iroh_advertised,
        direct,
        probe_host,
    )
    .await
}

async fn select_in_mode(
    mode: RpcTunnelMode,
    transport: &Arc<dyn PeerTransport>,
    dial: &PeerContact,
    rpc_port: u16,
    iroh_advertised: bool,
    direct: DirectBind,
    probe_host: Option<&str>,
) -> Option<(String, String)> {
    let allow_bridge = iroh_advertised && mode != RpcTunnelMode::Never;
    if direct == DirectBind::Undeclared {
        if !allow_bridge {
            tracing::warn!(
                node = %dial.node_id,
                rpc_port,
                tunnel = ?mode,
                iroh_advertised,
                "rpc-discovery: the worker declared no direct bind and the bridge is not allowed, \
                 so no path proves this member: none chosen (its address answers for anything but the worker)"
            );
            return None;
        }
        let sel = bridge_rpc_endpoint(transport, dial).await;
        tracing::debug!(
            node = %dial.node_id,
            rpc_port,
            bridged = sel.is_some(),
            "rpc-discovery: the worker declared no direct bind: bridge only, its address is never probed"
        );
        return sel;
    }
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
    if let (None, Some(host)) = (&sel, probe_host) {
        sel = Some((format!("{host}:{rpc_port}"), "probe-host".to_string()));
    }
    if let Some((endpoint, via)) = sel
        .as_ref()
        .filter(|(_, via)| !via.starts_with("iroh-bridge"))
    {
        tracing::debug!(
            node = %dial.node_id,
            %endpoint,
            %via,
            direct = ?direct,
            "rpc-discovery: chosen endpoint is not identity-bound (raw ggml RPC proves no member): \
             unproven inside the plaintext LAN its operator declared, or a pre-declaration /status"
        );
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

    /// A transport that bridges every member to one loopback authority, as
    /// the iroh transport does once it has dialled the member's key.
    #[derive(Debug)]
    struct Bridge;

    #[async_trait::async_trait]
    impl PeerTransport for Bridge {
        fn name(&self) -> &'static str {
            "bridge"
        }
        async fn endpoints(&self, _: &PeerContact, _: TrafficClass) -> Vec<PeerEndpoint> {
            vec![PeerEndpoint {
                base_url: "http://127.0.0.1:4242".into(),
                label: "x".into(),
            }]
        }
    }

    /// pc-rpc-probe-identity: a stranger listens at the member's direct
    /// address on the advertised rpc port while the worker itself binds
    /// loopback (declares no direct bind). The host must take the bridge,
    /// which proves the member, and never the stranger; a worker that did
    /// declare a direct bind is still dialled `direct-ip`. Failing input:
    /// drop the `DirectBind::Undeclared` gate, and the stranger is chosen.
    #[tokio::test]
    async fn a_stranger_on_the_members_address_is_never_chosen_for_an_undeclared_worker() {
        let stranger = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = stranger.local_addr().expect("addr").port();
        let transport: Arc<dyn PeerTransport> = Arc::new(Bridge);
        let member = contact(Vec::new(), vec!["127.0.0.1:4433".parse().unwrap()]);
        let bridged = Some(("127.0.0.1:4242".to_string(), "iroh-bridge:x".to_string()));
        let direct_ip = Some((format!("127.0.0.1:{port}"), "direct-ip".to_string()));
        let pick = |direct, probe_host| {
            select_in_mode(
                RpcTunnelMode::Auto,
                &transport,
                &member,
                port,
                true,
                direct,
                probe_host,
            )
        };
        assert_eq!(pick(DirectBind::Undeclared, None).await, bridged);
        assert_eq!(
            pick(DirectBind::Undeclared, Some("127.0.0.1")).await,
            bridged
        );
        assert_eq!(pick(DirectBind::Declared, None).await, direct_ip);
        // A pre-declaration `/status` keeps main's path (traced unproven).
        assert_eq!(pick(DirectBind::from_status(None), None).await, direct_ip);
        assert_eq!(DirectBind::from_status(Some(false)), DirectBind::Undeclared);
        assert_eq!(DirectBind::from_anchor(false), DirectBind::Undeclared);
    }

    /// `SOVEREIGN_RPC_TUNNEL=never` with a worker that declared no direct
    /// bind leaves no path that reaches it: a named absence, never a probe of
    /// its address or the probe host. Failing input: fall through to the
    /// direct probe, and the stranger is chosen.
    #[tokio::test]
    async fn never_bridging_an_undeclared_worker_chooses_nothing() {
        let stranger = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = stranger.local_addr().expect("addr").port();
        let transport: Arc<dyn PeerTransport> = Arc::new(Bridge);
        let member = contact(vec!["127.0.0.1:9742".parse().unwrap()], Vec::new());
        assert_eq!(
            select_in_mode(
                RpcTunnelMode::Never,
                &transport,
                &member,
                port,
                true,
                DirectBind::Undeclared,
                Some("127.0.0.1"),
            )
            .await,
            None
        );
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
            select_in_mode(
                RpcTunnelMode::Auto,
                &transport,
                &roster_only_iroh,
                port,
                false,
                DirectBind::Declared,
                Some("probe")
            )
            .await,
            Some((format!("127.0.0.1:{port}"), "direct-ip".to_string()))
        );
        // A member the overlay does name is probed there, as before.
        let overlay = contact(vec!["127.0.0.1:9742".parse().unwrap()], Vec::new());
        assert_eq!(
            select_in_mode(
                RpcTunnelMode::Auto,
                &transport,
                &overlay,
                port,
                false,
                DirectBind::Declared,
                Some("probe")
            )
            .await,
            Some((format!("127.0.0.1:{port}"), "direct-ip".to_string()))
        );
    }
}
