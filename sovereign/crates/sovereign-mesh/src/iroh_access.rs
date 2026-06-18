// SPDX-License-Identifier: AGPL-3.0-or-later
//! Dial-by-key MESH access over iroh (Track W, W1 — see
//! `sovereign/docs/specs/TRANSPORT_MIGRATION.md`).
//!
//! When `[iroh] enabled = true`, the daemon binds ONE iroh endpoint
//! from its `<data_dir>/node_key` identity — the same Ed25519 key it
//! already gossips as `MemberRecord.node_pubkey` — and forwards
//! accepted bi-streams to the daemon's two existing local listeners,
//! chosen by the connection's negotiated ALPN:
//!
//! - `cwth/http/0`  → internal router (gossip / control / knowledge)
//! - `cwth/client/0` → client router (`/v1` inference, `/status`)
//!
//! This is the **server half**: it makes this daemon DIALABLE by key,
//! so a peer (or phone) can reach it with no VPN. The class chooses
//! the ALPN, not a port — which is why the IP transport's port-rewrite
//! has no iroh analogue. Dialing *peers* by key (the client half —
//! `IrohTransport` composed via a `RoutedTransport`) is W3 and is not
//! wired here.
//!
//! Strictly **additive and fail-soft**: a disabled config or any bind
//! failure logs and yields `None`; the tailnet/LAN (`IpTransport`)
//! path is never taken down by this. The HTTP stack (routers, auth,
//! WS upgrade) is untouched — auth stays bearer-token / loopback-guard,
//! which are transport-independent by construction.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;

use commonwealth_transport::iroh::{
    build_relayed_endpoint, format_dial_string, Endpoint, IrohAcceptor, IrohTransport, ALPN,
    CLIENT_ALPN,
};
use commonwealth_transport::TrafficClass;

/// Map `[iroh.transport]` config to the traffic classes routed over
/// iroh (value `"iroh"`). Unknown non-`ip`/`iroh` values are treated
/// as `"ip"` with a warning. The string→`TrafficClass` interpretation
/// lives here (Track W3) because `sovereign-mesh` owns both the
/// `SetupConfig` schema and the transport types — the config crate
/// stays free of `TrafficClass`.
pub fn iroh_routed_classes(
    t: &sovereign_core::setup_config::TransportSection,
) -> Vec<TrafficClass> {
    let pairs: [(TrafficClass, &Option<String>); 6] = [
        (TrafficClass::Gossip, &t.gossip),
        (TrafficClass::ControlPlane, &t.control_plane),
        (TrafficClass::KnowledgeSearch, &t.knowledge_search),
        (TrafficClass::ModelTransfer, &t.model_transfer),
        (TrafficClass::Inference, &t.inference),
        (TrafficClass::StatusProbe, &t.status_probe),
    ];
    let mut out = Vec::new();
    for (class, val) in pairs {
        match val.as_deref() {
            Some("iroh") => out.push(class),
            None | Some("ip") => {}
            Some(other) => tracing::warn!(
                target: "transport",
                class = class.as_str(),
                value = %other,
                "iroh(mesh): unknown transport for class — using ip"
            ),
        }
    }
    out
}

/// Handle to the running mesh iroh access path. Holds the endpoint
/// (for live status / pairing reads) and the acceptor task, which is
/// aborted on drop — so dropping this stops accepting dial-by-key
/// traffic (the lifetime is tied to `DaemonState::Running`).
pub struct MeshIrohAccess {
    endpoint: Endpoint,
    _acceptor: IrohAcceptor,
}

impl MeshIrohAccess {
    /// Bind the endpoint from `<data_dir>/node_key` and route by ALPN
    /// to the daemon's two loopback listeners. Returns `None` when
    /// `enabled` is false or on any bind failure (always logged).
    ///
    /// `internal_port` / `client_port` are this daemon's resolved
    /// ports; the acceptor forwards to `127.0.0.1:<port>` (the
    /// listeners bind `0.0.0.0`, which includes loopback). Forwarding
    /// is lazy per stream, so binding this before the listeners are up
    /// is safe — an early dial just fails and the client retries.
    pub async fn start(
        data_dir: &Path,
        internal_port: u16,
        client_port: u16,
        enabled: bool,
    ) -> Option<MeshIrohAccess> {
        if !enabled {
            return None;
        }

        // The mesh identity, NOT a fresh key: this is exactly what
        // gossip stamps into `MemberRecord.node_pubkey`, so "dialable
        // by key" and "known member" are one fact (the W2 collapse
        // this server half is built to anticipate).
        let identity = commonwealth_transport::identity::load_or_generate_node_key(data_dir);
        let secret = commonwealth_transport::iroh::SecretKey::from_bytes(&identity.to_bytes());

        let endpoint =
            match build_relayed_endpoint(secret, vec![ALPN.to_vec(), CLIENT_ALPN.to_vec()]).await {
                Ok(ep) => ep,
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "iroh(mesh): endpoint bind failed — dial-by-key mesh access \
                         disabled (tailnet path unaffected)"
                    );
                    return None;
                }
            };

        let internal_addr: SocketAddr = ([127, 0, 0, 1], internal_port).into();
        let client_addr: SocketAddr = ([127, 0, 0, 1], client_port).into();
        let mut routes: HashMap<Vec<u8>, SocketAddr> = HashMap::new();
        routes.insert(ALPN.to_vec(), internal_addr);
        routes.insert(CLIENT_ALPN.to_vec(), client_addr);
        let acceptor = IrohAcceptor::spawn_routed(endpoint.clone(), routes);

        tracing::info!(
            endpoint_id = %endpoint.id(),
            internal_forward = %internal_addr,
            client_forward = %client_addr,
            dial = %Self::dial_for(&endpoint).unwrap_or_else(|| "<no relay yet>".to_string()),
            "iroh(mesh): dial-by-key access enabled \
             (ALPN cwth/http/0 -> internal, cwth/client/0 -> client)"
        );
        Some(MeshIrohAccess {
            endpoint,
            _acceptor: acceptor,
        })
    }

    /// An `IrohTransport` that dials peers FROM this node's endpoint —
    /// the SAME endpoint the acceptor serves on (iroh endpoints both
    /// accept and connect). W3's `RoutedTransport` uses this for the
    /// traffic classes flipped to iroh; ALPN is chosen per class by
    /// `IrohTransport` itself.
    pub fn client_transport(&self) -> IrohTransport {
        IrohTransport::new(self.endpoint.clone())
    }

    /// A pull-provider yielding this node's LIVE iroh dial info for
    /// the gossip self-stamp (W2). Captures a clone of the endpoint
    /// (cheap — it's `Arc` inside) and re-reads `endpoint.addr()` on
    /// each call, so the relay + hole-punched addrs that iroh
    /// discovers after bind are always current. Installed on
    /// `AppState` so gossip can stamp `relay_url` + `iroh_direct_addrs`
    /// into our own `MemberRecord` without `commonwealth-api` needing
    /// an iroh dependency.
    pub fn dial_info_provider(
        &self,
    ) -> std::sync::Arc<dyn Fn() -> commonwealth_core::mesh::IrohDialInfo + Send + Sync> {
        let endpoint = self.endpoint.clone();
        std::sync::Arc::new(move || {
            let addr = endpoint.addr();
            // Bind to locals (statements) so the transient `relay_urls`
            // iterator borrow of `addr` drops at the `;`, not at the
            // end of the closure where `addr` itself is dropped.
            let relay_url = addr.relay_urls().next().map(|r| r.to_string());
            let direct_addrs = addr.ip_addrs().copied().collect();
            commonwealth_core::mesh::IrohDialInfo {
                relay_url,
                direct_addrs,
            }
        })
    }

    /// The pairing string a peer/phone would dial: `id@relay` once a
    /// relay is connected, else the full `id@addr,...` (relay-less
    /// LANs / hermetic setups). `None` before any address is known.
    fn dial_for(endpoint: &Endpoint) -> Option<String> {
        let addr = endpoint.addr();
        let id_hex = hex::encode(addr.id.as_bytes());
        // Bind to a local (not a tail expression) so the transient
        // `relay_urls()` iterator borrow of `addr` is dropped at the
        // `;`, while `addr` itself lives to the end of the fn.
        let dial = addr
            .relay_urls()
            .next()
            .map(|relay| format!("{id_hex}@{relay}"))
            .or_else(|| format_dial_string(&addr));
        dial
    }

    /// Live glassbox status — endpoint id + the current dial string.
    /// The daemon surfaces this so "is this node dialable by key?" is
    /// a one-read question.
    pub fn status_json(&self) -> serde_json::Value {
        let addr = self.endpoint.addr();
        serde_json::json!({
            "endpoint_id": self.endpoint.id().to_string(),
            "dial": Self::dial_for(&self.endpoint),
            "dial_full": format_dial_string(&addr),
            "relay_urls": addr.relay_urls().map(|r| r.to_string()).collect::<Vec<_>>(),
        })
    }
}
