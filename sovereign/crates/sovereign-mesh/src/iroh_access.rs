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
    build_relayed_endpoint, format_dial_string, Endpoint, IrohAcceptor, IrohTransport, RelayConfig,
    ALPN, CLIENT_ALPN,
};
use commonwealth_transport::TrafficClass;

/// Map `[iroh.transport]` config to the traffic classes routed over
/// iroh. Since the iroh-first flip (2026-07): iroh enabled means EVERY
/// class routes iroh-first (with automatic per-dial IP fallback via
/// `RoutedTransport`'s empty required set), and the config is an
/// opt-OUT — `<class> = "ip"` pins that class to the IP path. A legacy
/// `"iroh"` entry is a no-op (it names the default) and is logged as
/// such; unknown values get the default (routed) with a warning. The
/// string→`TrafficClass` interpretation lives here (Track W3) because
/// `sovereign-mesh` owns both the `SetupConfig` schema and the
/// transport types — the config crate stays free of `TrafficClass`.
pub fn iroh_routed_classes(
    t: &sovereign_core::setup_config::TransportSection,
) -> Vec<TrafficClass> {
    let mut out = Vec::new();
    for (class, val) in class_entries(t) {
        match val.as_deref() {
            Some("ip") => {}
            None => out.push(class),
            Some("iroh") => {
                tracing::info!(
                    target: "transport",
                    class = class.as_str(),
                    "iroh(mesh): [iroh.transport] {} = \"iroh\" is now the default — \
                     the entry is a no-op and can be removed",
                    class.as_str()
                );
                out.push(class);
            }
            Some(other) => {
                tracing::warn!(
                    target: "transport",
                    class = class.as_str(),
                    value = %other,
                    "iroh(mesh): unknown transport for class — using the default (iroh-first)"
                );
                out.push(class);
            }
        }
    }
    out
}

/// True when the config explicitly names iroh for at least one class.
/// Only used to scope the "routes configured but [iroh] enabled=false"
/// startup warning: under opt-out semantics `iroh_routed_classes` is
/// non-empty for an empty section, so the warning must key off
/// explicit intent, not the derived class list.
pub fn has_explicit_iroh_routes(t: &sovereign_core::setup_config::TransportSection) -> bool {
    class_entries(t)
        .into_iter()
        .any(|(_, val)| val.as_deref() == Some("iroh"))
}

fn class_entries(
    t: &sovereign_core::setup_config::TransportSection,
) -> [(TrafficClass, &Option<String>); 6] {
    [
        (TrafficClass::Gossip, &t.gossip),
        (TrafficClass::ControlPlane, &t.control_plane),
        (TrafficClass::KnowledgeSearch, &t.knowledge_search),
        (TrafficClass::ModelTransfer, &t.model_transfer),
        (TrafficClass::Inference, &t.inference),
        (TrafficClass::StatusProbe, &t.status_probe),
    ]
}

/// Resolve whether this daemon's iroh endpoint turns on. Explicit
/// config wins; otherwise mesh participation decides (the
/// `client-exposed` marker every explicit create/join surface writes)
/// — consent-by-mesh-participation, so a meshless daemon never
/// contacts relay infrastructure. A mesh-wide `require_encryption`
/// overrides everything: an encrypted mesh cannot run without iroh
/// (the daemon hard-fails later if the endpoint won't bind, rather
/// than silently downgrading).
pub fn resolve_enabled(
    cfg_enabled: Option<bool>,
    mesh_participant: bool,
    require_encryption: bool,
) -> bool {
    cfg_enabled.unwrap_or(mesh_participant) || require_encryption
}

/// Ops/CI kill-switch: `SOVEREIGN_IROH=off|0|false` prevents the mesh
/// endpoint from binding regardless of config or the participation
/// marker. Checked in [`MeshIrohAccess::start`]. (On an encrypted mesh
/// this trips the daemon's require-encryption hard-fail — fail closed,
/// never plaintext.)
fn env_kill_switch() -> bool {
    matches!(
        std::env::var("SOVEREIGN_IROH").ok().as_deref(),
        Some("off") | Some("0") | Some("false")
    )
}

/// The live iroh connection path to one peer (H2 observability). A
/// point-in-time snapshot from the endpoint's `remote_info`; the
/// operator's answer to "is this peer on a direct path or the relay?"
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
}

/// Classify a peer's live path from what iroh reports ACTIVE: a direct
/// (hole-punched) IP path is preferred once established; the relay is
/// the always-works fallback. `any_addr` distinguishes "known peer,
/// nothing active right now" (`idle`) from "endpoint has no record"
/// (`unknown`).
fn classify_path(direct_active: bool, relay_active: bool, any_addr: bool) -> &'static str {
    match (direct_active, relay_active) {
        (true, true) => "mixed",
        (true, false) => "direct",
        (false, true) => "relayed",
        (false, false) if any_addr => "idle",
        (false, false) => "unknown",
    }
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
        relay_cfg: &RelayConfig,
    ) -> Option<MeshIrohAccess> {
        if !enabled {
            return None;
        }
        if env_kill_switch() {
            tracing::info!(
                "iroh(mesh): SOVEREIGN_IROH kill-switch set — dial-by-key access stays off \
                 (tailnet/LAN path unaffected)"
            );
            return None;
        }

        // The mesh identity, NOT a fresh key: this is exactly what
        // gossip stamps into `MemberRecord.node_pubkey`, so "dialable
        // by key" and "known member" are one fact (the W2 collapse
        // this server half is built to anticipate).
        let identity = commonwealth_transport::identity::load_or_generate_node_key(data_dir);
        let secret = commonwealth_transport::iroh::SecretKey::from_bytes(&identity.to_bytes());

        let endpoint = match build_relayed_endpoint(
            secret,
            vec![ALPN.to_vec(), CLIENT_ALPN.to_vec()],
            relay_cfg,
        )
        .await
        {
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

    /// Public form of [`Self::dial_for`] for callers holding a cloned
    /// [`Endpoint`] handle (see [`Self::endpoint_handle`]) instead of
    /// the access struct — e.g. `current_invite`'s live dial read.
    pub fn dial_for_endpoint(endpoint: &Endpoint) -> Option<String> {
        Self::dial_for(endpoint)
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

    /// This node's current dial-by-key string (`<hex-id>@<relay-or-addr>[,…]`)
    /// for embedding in a mesh invite, so a joiner can dial this
    /// founder by key over iroh. `None` before any reachable address
    /// is known (no relay connected and no direct addr yet).
    pub fn dial_string(&self) -> Option<String> {
        Self::dial_for(&self.endpoint)
    }

    /// A cheap clone of the endpoint handle (iroh `Endpoint` is
    /// Arc-backed), for callers that must not hold the daemon-state
    /// lock across an await — e.g. [`Self::wait_for_relay`].
    pub fn endpoint_handle(&self) -> Endpoint {
        self.endpoint.clone()
    }

    /// The live connection path to `peer_pubkey` over iroh, from the
    /// endpoint's `remote_info` snapshot (H2 observability — the
    /// "is anyone actually on the relay?" question). Returns the
    /// `path` classification, the active relay if any, and the count
    /// of active direct addresses. `None` when the endpoint has no
    /// record of this peer (never dialed, or not iroh-reachable).
    /// Takes an [`Endpoint`] handle (see [`Self::endpoint_handle`]) so
    /// the caller needn't hold the daemon-state lock across the await.
    pub async fn peer_path_on(
        endpoint: &Endpoint,
        peer_pubkey: &[u8; 32],
    ) -> Option<PeerTransportPath> {
        let id = commonwealth_transport::iroh::PublicKey::from_bytes(peer_pubkey).ok()?;
        let info = endpoint.remote_info(id).await?;
        let mut active_relay: Option<String> = None;
        let mut active_direct = 0usize;
        let mut any_addr = false;
        for a in info.addrs() {
            any_addr = true;
            let active = matches!(
                a.usage(),
                commonwealth_transport::iroh::TransportAddrUsage::Active
            );
            match a.addr() {
                commonwealth_transport::iroh::TransportAddr::Relay(url) if active => {
                    active_relay.get_or_insert_with(|| url.to_string());
                }
                commonwealth_transport::iroh::TransportAddr::Ip(_) if active => {
                    active_direct += 1;
                }
                _ => {}
            }
        }
        let path = classify_path(active_direct > 0, active_relay.is_some(), any_addr);
        Some(PeerTransportPath {
            path: path.to_string(),
            relay: active_relay,
            active_direct_addrs: active_direct,
        })
    }

    /// Bounded wait for a RELAY-bearing dial string, polling every
    /// 250 ms. Direct addrs appear near-instantly at bind, but a
    /// relay-bearing dial is what makes an invite work OFF-LAN — so
    /// invite generation waits briefly for the relay connection. On
    /// timeout it falls back to whatever dial exists: a
    /// direct-addrs-only dial is the CORRECT invite for a
    /// LAN-without-internet mesh, not a failure.
    pub async fn wait_for_relay(
        endpoint: &Endpoint,
        timeout: std::time::Duration,
    ) -> Option<String> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if endpoint.addr().relay_urls().next().is_some()
                || tokio::time::Instant::now() >= deadline
            {
                return Self::dial_for(endpoint);
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::setup_config::TransportSection;

    fn section(f: impl FnOnce(&mut TransportSection)) -> TransportSection {
        let mut t = TransportSection::default();
        f(&mut t);
        t
    }

    #[test]
    fn empty_section_routes_every_class() {
        let out = iroh_routed_classes(&TransportSection::default());
        assert_eq!(out, TrafficClass::ALL.to_vec());
    }

    #[test]
    fn ip_entry_opts_a_class_out() {
        let out = iroh_routed_classes(&section(|t| t.gossip = Some("ip".into())));
        assert!(!out.contains(&TrafficClass::Gossip));
        assert_eq!(out.len(), TrafficClass::ALL.len() - 1);
    }

    #[test]
    fn all_classes_opted_out_yields_empty() {
        let out = iroh_routed_classes(&section(|t| {
            t.gossip = Some("ip".into());
            t.control_plane = Some("ip".into());
            t.knowledge_search = Some("ip".into());
            t.model_transfer = Some("ip".into());
            t.inference = Some("ip".into());
            t.status_probe = Some("ip".into());
        }));
        assert!(out.is_empty());
    }

    #[test]
    fn legacy_iroh_entry_is_a_routed_noop() {
        // Pre-flip configs said `gossip = "iroh"` to opt IN; the entry
        // now names the default and must not change the outcome.
        let out = iroh_routed_classes(&section(|t| t.gossip = Some("iroh".into())));
        assert_eq!(out, TrafficClass::ALL.to_vec());
    }

    #[test]
    fn unknown_value_gets_the_default_routed() {
        let out = iroh_routed_classes(&section(|t| t.inference = Some("carrier-pigeon".into())));
        assert!(out.contains(&TrafficClass::Inference));
        assert_eq!(out, TrafficClass::ALL.to_vec());
    }

    #[test]
    fn classify_path_covers_all_states() {
        assert_eq!(classify_path(true, true, true), "mixed");
        assert_eq!(classify_path(true, false, true), "direct");
        assert_eq!(classify_path(false, true, true), "relayed");
        // Known peer (has addrs) but nothing active this instant.
        assert_eq!(classify_path(false, false, true), "idle");
        // Endpoint has no record of the peer at all.
        assert_eq!(classify_path(false, false, false), "unknown");
    }

    #[test]
    fn resolve_enabled_matrix() {
        // Explicit config wins over the participation marker…
        assert!(resolve_enabled(Some(true), false, false));
        assert!(!resolve_enabled(Some(false), true, false));
        // …absent config defers to mesh participation…
        assert!(resolve_enabled(None, true, false));
        assert!(!resolve_enabled(None, false, false));
        // …and require_encryption overrides everything, including an
        // explicit opt-out (an encrypted mesh cannot run without iroh).
        assert!(resolve_enabled(Some(false), false, true));
        assert!(resolve_enabled(None, false, true));
    }

    #[test]
    fn explicit_routes_detection_keys_off_iroh_entries_only() {
        assert!(!has_explicit_iroh_routes(&TransportSection::default()));
        assert!(!has_explicit_iroh_routes(&section(
            |t| t.gossip = Some("ip".into())
        )));
        assert!(!has_explicit_iroh_routes(&section(
            |t| t.gossip = Some("carrier-pigeon".into())
        )));
        assert!(has_explicit_iroh_routes(&section(
            |t| t.model_transfer = Some("iroh".into())
        )));
    }
}
