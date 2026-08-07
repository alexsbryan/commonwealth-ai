// SPDX-License-Identifier: AGPL-3.0-or-later
//! EXPERIMENTAL dial-by-key transport over iroh (QUIC by Ed25519
//! key, hole-punching, optional relays). Feature-gated behind
//! `iroh` and excluded from default workspace gates; the spike e2e
//! lives in `sovereign-mesh/tests/iroh_transport_e2e.rs` (run with
//! `cargo test -p sovereign-mesh --features iroh-experimental`).
//!
//! ## Shape: localhost byte-tunnels, not a new HTTP stack
//!
//! The [`PeerTransport`] contract resolves base URLs, and every call
//! site keeps its existing `reqwest` client — so this transport
//! terminates iroh QUIC locally and hands HTTP a plain TCP socket:
//!
//! - Client side ([`IrohTransport`]): per-peer localhost
//!   `TcpListener`; each accepted TCP connection opens one iroh
//!   bi-stream to the peer (dialed by its Ed25519 key — the
//!   `MemberRecord.node_pubkey`) and copies bytes both ways.
//! - Server side ([`IrohAcceptor`]): accepts iroh bi-streams and
//!   copies each into a fresh TCP connection to the daemon's
//!   existing localhost listener — unmodified axum router,
//!   unmodified middleware.
//!
//! Known upgrade path (deliberately NOT in the spike): serve hyper
//! directly on iroh streams (drop the double-copy), carry the
//! traffic class in the ALPN or a stream header so one acceptor can
//! route to both daemon ports, and a tunnel-proxy sidecar for the
//! raw-TCP `rpc-server` tensor traffic that this transport
//! intentionally does not cover.
//!
//! ## Spike limitations (documented, intentional)
//!
//! - The acceptor forwards to ONE local address — fine for the
//!   internal-port classes the spike exercises; the class-in-ALPN
//!   upgrade lifts this.
//! - Peer iroh socket addresses come from an explicitly seeded map
//!   ([`IrohTransport::add_known_peer`]); production would use
//!   relays/address-lookup. `MemberRecord` already carries the key,
//!   which is the part that must travel in the trust ring.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use commonwealth_core::ids::{NodeId, NodePubkey};

use crate::{PeerContact, PeerEndpoint, PeerTransport, TrafficClass};

/// ALPN for mesh-internal HTTP-over-iroh tunnels. Version-suffixed so
/// a future class-aware protocol can coexist during migration.
pub const ALPN: &[u8] = b"cwth/http/0";

/// Whether this process runs in the bench-only relay-pinned posture
/// (`SOVEREIGN_IROH_RELAY_ONLY=1` + the `iroh-relay-only` feature). Read
/// per dial — cheap, and keeps the posture decision in one place for both
/// the endpoint builder (path selector) and dial-time addr seeding.
/// Always false without the feature, so production builds compile this
/// to a constant.
pub fn relay_pin_active() -> bool {
    #[cfg(feature = "iroh-relay-only")]
    {
        std::env::var("SOVEREIGN_IROH_RELAY_ONLY")
            .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
            .unwrap_or(false)
    }
    #[cfg(not(feature = "iroh-relay-only"))]
    {
        false
    }
}

/// ALPN for the ggml tensor-split RPC byte stream (task 6): a worker's
/// acceptor forwards this to its local rpc-server (`127.0.0.1:50052`);
/// the host reaches it through a bridge-local endpoint minted for
/// [`TrafficClass::RpcTensor`]. Raw bytes, not HTTP — the pump is
/// byte-generic. Version-suffixed like its siblings.
pub const RPC_ALPN: &[u8] = b"cwth/rpc/0";

/// ALPN for client-API traffic (Track M: phone → `sovereign-server`).
/// Distinct from [`ALPN`] so one daemon can later accept both and
/// route by protocol instead of by port.
pub const CLIENT_ALPN: &[u8] = b"cwth/client/0";

// Re-exported so feature consumers (sovereign-server, the mobile
// core, the sovereign-mesh spike test) build endpoints without
// declaring their own iroh dependency — keeps the version pin in
// exactly one place.
pub use iroh::endpoint::presets;
pub use iroh::endpoint::Builder as EndpointBuilder;
// Per-peer connection observability (H2): `remote_info` returns these.
pub use iroh::endpoint::TransportAddrUsage;
// Founder-reachability watchdog: `Endpoint::home_relay_status()` returns a
// `Watcher<Vec<RelayStatus>>`. Re-exported here so the mesh crate consumes them
// without declaring its own `iroh` dependency.
pub use iroh::endpoint::RelayStatus;
pub use iroh::{Endpoint, EndpointAddr, PublicKey, RelayUrl, SecretKey, TransportAddr, Watcher};

/// The rustls crypto provider for `EndpointBuilder::crypto_provider`.
/// iroh's `Builder::empty()` deliberately sets no provider (only
/// presets choose one), and `bind()` errors without it — pass this.
/// Ring, matching the rest of the workspace's rustls usage.
pub fn ring_crypto_provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// How an endpoint reaches the wider network — the sovereignty knob
/// (H1 of the enterprise-hardening plan). Bundled into one struct so
/// adding a relay/discovery option later is not another signature
/// change across every constructor call site.
#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// Custom relay URLs. Empty = the default for the chosen discovery
    /// posture (n0's public relays under `n0_services`, or NO relay —
    /// direct-addr only — without it).
    pub relay_urls: Vec<String>,
    /// Use n0's public infrastructure — BOTH the public relays AND the
    /// n0 DNS/pkarr address-lookup (`iroh.link`). `true` is the
    /// bootstrap default. `false` severs ALL n0 contact: the endpoint
    /// builds from `presets::Minimal` (crypto only — no n0 relay, no n0
    /// DNS), so peers are reached ONLY via gossiped `iroh_direct_addrs`
    /// (a flat LAN/VPC) and/or the `relay_urls` above (a self-hosted
    /// relay for cross-subnet NAT traversal). This is the knob a
    /// sovereignty- or air-gap-focused netops team requires — setting
    /// `relay_urls` alone does NOT stop the n0 DNS lookup.
    pub n0_services: bool,
}

impl Default for RelayConfig {
    fn default() -> Self {
        // Bootstrap posture: full n0 (relays + DNS). Callers that want
        // sovereignty opt out via `from_parts(discovery = "none")`.
        Self {
            relay_urls: Vec::new(),
            n0_services: true,
        }
    }
}

impl RelayConfig {
    /// Build from operator config: the `relay_urls` list and a
    /// `discovery` string (`"n0"` / absent = n0 services; `"none"` /
    /// `"self"` / `"local"` = sever n0). An unknown value warns and
    /// keeps the safe n0 default. Central so both `sovereign-mesh` and
    /// `sovereign-server` map their configs identically.
    pub fn from_parts(relay_urls: Vec<String>, discovery: Option<&str>) -> Self {
        let n0_services = match discovery.map(str::trim) {
            None | Some("") | Some("n0") => true,
            Some("none") | Some("self") | Some("local") => false,
            Some(other) => {
                tracing::warn!(
                    target: "transport",
                    value = %other,
                    "iroh: unknown [iroh] discovery — using n0 services (the safe default)"
                );
                true
            }
        };
        Self {
            relay_urls,
            n0_services,
        }
    }
}

/// Build an iroh endpoint from a node identity, serving `alpns`, per a
/// [`RelayConfig`]. With `n0_services` it starts from `presets::N0`
/// (n0 relays + n0 DNS/pkarr lookup); without it, from `presets::Minimal`
/// (crypto only — no n0 anything), so a sovereign/air-gapped node reaches
/// peers by gossiped direct addrs and/or self-hosted `relay_urls` alone.
/// Non-empty `relay_urls` overrides the relay set in either mode; an
/// empty list under Minimal means relays disabled (direct-addr only).
/// Always calls [`EndpointBuilder::proxy_from_env`], so a corporate
/// `HTTP_PROXY`/`HTTPS_PROXY` (incl. Basic auth) is honored for the
/// relay's WebSocket-over-TLS/443 connection — the path that carries the
/// mesh when UDP is blocked.
///
/// One constructor so every production caller — the mobile host
/// ([`crate`] consumers like `sovereign-server::iroh_access`) and the
/// mesh daemon — binds identically and any iroh-API churn stays here.
/// Hermetic tests still use `EndpointBuilder::empty()` directly.
pub async fn build_relayed_endpoint(
    secret_key: SecretKey,
    alpns: Vec<Vec<u8>>,
    cfg: &RelayConfig,
) -> Result<Endpoint, String> {
    relayed_endpoint_builder(secret_key, alpns, cfg)
        .bind()
        .await
        .map_err(|e| format!("iroh endpoint bind failed: {e}"))
}

/// The shared builder behind [`build_relayed_endpoint`] (and the bench-only
/// relay-pinned variant) — preset/relay/proxy policy lives exactly once.
fn relayed_endpoint_builder(
    secret_key: SecretKey,
    alpns: Vec<Vec<u8>>,
    cfg: &RelayConfig,
) -> EndpointBuilder {
    let preset_builder = if cfg.n0_services {
        EndpointBuilder::new(presets::N0)
    } else {
        // Minimal = crypto only: no n0 relay, no n0 DNS. Nothing this
        // endpoint does will contact n0 infrastructure.
        EndpointBuilder::new(presets::Minimal)
    };
    let mut builder = preset_builder
        .crypto_provider(ring_crypto_provider())
        .secret_key(secret_key)
        .alpns(alpns)
        // Honor corporate HTTP(S) proxies on the relay dial. No-op when
        // the env vars are unset, so it's safe on every deployment.
        .proxy_from_env();
    // Glassbox the egress posture so netops can confirm — from the log,
    // not by inference — which relays this node uses and whether a proxy
    // is engaged. Credentials are redacted.
    log_egress_posture(cfg);
    match parse_relay_mode(&cfg.relay_urls) {
        Some(mode) => builder = builder.relay_mode(mode),
        None if !cfg.n0_services => {
            // Sovereign mode, no custom relay: disable relays entirely.
            // Peers are reached by gossiped direct addrs only (flat
            // LAN/VPC). Minimal carries no relay by default, but be
            // explicit so intent is unmistakable.
            builder = builder.relay_mode(iroh::RelayMode::Disabled);
        }
        None => {} // n0_services + no custom relays → n0's default relays.
    }
    builder
}

/// Bench/measurement-only (feature `iroh-relay-only`): like
/// [`build_relayed_endpoint`] but with a [`PathSelector`] that ONLY ever
/// selects relay paths — application data stays on the relay even after
/// hole-punching discovers a direct path. This is the deterministic "relay
/// floor" for path characterization, replacing root-only UDP firewalling.
/// Both peers must use it: path selection is per-side, so a normal peer
/// would answer over the direct path and halve the measured relay tax.
///
/// [`PathSelector`]: iroh::endpoint::transports::PathSelector
#[cfg(feature = "iroh-relay-only")]
pub async fn build_relay_only_endpoint(
    secret_key: SecretKey,
    alpns: Vec<Vec<u8>>,
    cfg: &RelayConfig,
) -> Result<Endpoint, String> {
    use iroh::endpoint::transports::{PathSelection, PathSelectionContext, PathSelector};

    /// Selects the relay path when one is open; otherwise leaves the
    /// selection unchanged (an empty selection keeps the current path).
    #[derive(Debug)]
    struct RelayOnlySelector;
    impl PathSelector for RelayOnlySelector {
        fn select(&self, ctx: &PathSelectionContext<'_>) -> PathSelection {
            let mut selection = PathSelection::none();
            for psd in ctx.paths() {
                if psd.network_path().is_relay() {
                    selection.set(&psd);
                    break;
                }
            }
            selection
        }
    }

    tracing::warn!(
        target: "transport",
        "iroh: RELAY-ONLY path selection active — bench posture, never production"
    );
    relayed_endpoint_builder(secret_key, alpns, cfg)
        .path_selector(Arc::new(RelayOnlySelector))
        .bind()
        .await
        .map_err(|e| format!("iroh endpoint bind failed: {e}"))
}

/// Read the HTTP(S) proxy from the environment iroh's `proxy_from_env`
/// consults, with any `user:pass@` userinfo redacted. `None` when no
/// proxy is set. Public so the daemon's `doctor` can report the same
/// value it logs.
pub fn configured_proxy_redacted() -> Option<String> {
    // iroh checks HTTPS_PROXY then HTTP_PROXY (and lowercase). Mirror
    // that precedence so what we log is what it will actually use.
    for var in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
        if let Ok(v) = std::env::var(var) {
            if !v.trim().is_empty() {
                return Some(redact_userinfo(&v));
            }
        }
    }
    None
}

/// Replace a URL's `user:pass@` with `***@` for safe logging. Falls
/// back to the raw string if there's no userinfo to redact.
fn redact_userinfo(url: &str) -> String {
    match (url.find("://"), url.find('@')) {
        (Some(scheme_end), Some(at)) if at > scheme_end + 3 => {
            format!("{}***@{}", &url[..scheme_end + 3], &url[at + 1..])
        }
        _ => url.to_string(),
    }
}

/// One-line, info-level egress summary at endpoint construction:
/// discovery posture, relay set, and proxy (redacted). This is the
/// audit surface a netops team greps to confirm what the node touches.
fn log_egress_posture(cfg: &RelayConfig) {
    let relays = if cfg.relay_urls.is_empty() {
        if cfg.n0_services {
            "n0-default".to_string()
        } else {
            "none (direct-addr only)".to_string()
        }
    } else {
        cfg.relay_urls.join(",")
    };
    tracing::info!(
        target: "transport",
        n0_services = cfg.n0_services,
        relays = %relays,
        proxy = configured_proxy_redacted().as_deref().unwrap_or("none"),
        "iroh egress posture (n0_services=false severs all n0 contact; \
         proxy honored for relay TCP:443, Basic auth only)"
    );
}

/// Turn operator-configured relay URL strings into a custom
/// [`RelayMode`]. `None` (empty input, or every entry unparseable)
/// leaves the caller on the preset default. Unparseable entries are
/// logged and skipped rather than aborting the bind — a fat-fingered
/// relay URL must not take a node offline when the default relays
/// would still work.
fn parse_relay_mode(relay_urls: &[String]) -> Option<iroh::RelayMode> {
    if relay_urls.is_empty() {
        return None;
    }
    let parsed: Vec<RelayUrl> = relay_urls
        .iter()
        .filter_map(|u| match u.parse::<RelayUrl>() {
            Ok(url) => Some(url),
            Err(e) => {
                tracing::warn!(
                    target: "transport",
                    url = %u,
                    error = %e,
                    "iroh: ignoring unparseable relay_url — falling back to remaining/default relays"
                );
                None
            }
        })
        .collect();
    if parsed.is_empty() {
        tracing::warn!(
            target: "transport",
            "iroh: all configured relay_urls were unparseable — using default relays"
        );
        return None;
    }
    Some(iroh::RelayMode::custom(parsed))
}

/// One client-side tunnel: a localhost `TcpListener` whose accepted
/// connections each become an iroh bi-stream to a fixed peer. Point
/// any plain-TCP client (reqwest, tungstenite) at
/// `http://{local_addr()}` / `ws://{local_addr()}` and it transparently
/// rides QUIC dialed by the peer's Ed25519 key. Dropping the bridge
/// aborts the accept loop.
#[derive(Debug)]
pub struct HttpBridge {
    local_addr: SocketAddr,
    /// Where accepted connections dial, read fresh per connection. The
    /// loopback listener is the bridge's IDENTITY; the peer address it
    /// tunnels to is a mutable attribute — see [`retarget`](Self::retarget).
    target: Arc<std::sync::Mutex<EndpointAddr>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for HttpBridge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl HttpBridge {
    /// Bind the localhost listener and start tunneling to `target`
    /// over `alpn`. Dialing is lazy (per accepted TCP connection) and
    /// IS key verification — the QUIC handshake fails unless the
    /// responder holds the private key for `target.id`.
    pub async fn spawn(
        endpoint: Endpoint,
        target: EndpointAddr,
        alpn: &'static [u8],
    ) -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let local_addr = listener.local_addr()?;
        let peer_label = target.id.to_string();
        let target = Arc::new(std::sync::Mutex::new(target));
        let target_slot = Arc::clone(&target);
        let task = tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    break;
                };
                // Nagle on this loopback hop stacks with the peer's delayed
                // ACK into ~40 ms stalls per direction on request/response
                // traffic (measured: 82 ms added to a 16 KB round-trip).
                // The tunnel must be latency-transparent — disable it.
                tcp.set_nodelay(true).ok();
                let endpoint = endpoint.clone();
                // Read the CURRENT target per connection: a retarget between
                // accepts must take effect on the very next dial.
                let target = target_slot
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let peer_label = peer_label.clone();
                tokio::spawn(async move {
                    let conn = match endpoint.connect(target, alpn).await {
                        Ok(c) => c,
                        Err(e) => {
                            // The ALPN is the whole diagnosis for an "error 120:
                            // peer doesn't support any known protocol" reject —
                            // without it you cannot tell a peer that lacks a
                            // ggml rpc-server (no cwth/rpc/0) from one that is
                            // unreachable, and both surface as "dial failed".
                            tracing::warn!(
                                target: "transport",
                                peer = %peer_label,
                                alpn = %String::from_utf8_lossy(alpn),
                                error = %e,
                                "iroh bridge: dial failed"
                            );
                            return;
                        }
                    };
                    let (send, recv) = match conn.open_bi().await {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(
                                target: "transport",
                                peer = %peer_label,
                                error = %e,
                                "iroh bridge: open_bi failed"
                            );
                            return;
                        }
                    };
                    pump(tcp, send, recv).await;
                });
            }
        });
        Ok(Self {
            local_addr,
            target,
            task,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Point subsequent dials at `target` WITHOUT rebinding the loopback
    /// listener, so the local port survives a peer's dial-info change.
    ///
    /// That port is not an implementation detail: it is the address handed
    /// to plain-TCP clients that cannot be told to re-resolve — most
    /// sharply ggml's rpc-server list, which the mesh's RPC discovery
    /// advertises as the worker's endpoint STRING. Rebuilding the bridge
    /// minted a new ephemeral port on every gossiped address change, so a
    /// stable peer read downstream as a stream of different workers
    /// (observed 2026-07-25: 34207 → 40043 → 39419 → 34133 → 40021 for one
    /// unmoved Mac). Identity is the bridge; the peer address is an
    /// attribute of it.
    pub fn retarget(&self, target: EndpointAddr) {
        *self.target.lock().unwrap_or_else(|e| e.into_inner()) = target;
    }
}

/// Parse a pairing dial string: `<64-hex-endpoint-id>@<target>[,<target>...]`
/// where each target is either a UDP `SocketAddr` (LAN-direct / tests)
/// or a relay URL (`https://…`). This is the string a host's pairing
/// surface displays and a client stores as its opaque transport
/// address.
pub fn parse_dial_string(s: &str) -> Result<EndpointAddr, String> {
    let (id_hex, targets) = s.split_once('@').ok_or_else(|| {
        format!("dial string '{s}' missing '@' — expected <endpoint-id>@<relay-or-addr>[,...]")
    })?;
    let id_bytes =
        hex::decode(id_hex.trim()).map_err(|e| format!("endpoint id is not hex: {e}"))?;
    let id_arr: [u8; 32] = id_bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("endpoint id is {} bytes, expected 32", id_bytes.len()))?;
    let id = PublicKey::from_bytes(&id_arr).map_err(|e| format!("invalid endpoint id: {e}"))?;

    let mut ea = EndpointAddr::new(id);
    let mut any_target = false;
    for raw in targets.split(',') {
        let t = raw.trim();
        if t.is_empty() {
            continue;
        }
        any_target = true;
        if let Ok(sock) = t.parse::<SocketAddr>() {
            ea = ea.with_ip_addr(sock);
        } else {
            let relay: RelayUrl = t.parse().map_err(|e| {
                format!("target '{t}' is neither a socket address nor a relay URL: {e}")
            })?;
            ea = ea.with_relay_url(relay);
        }
    }
    if !any_target {
        return Err(format!("dial string '{s}' has no targets after '@'"));
    }
    Ok(ea)
}

/// Render an endpoint's current dial info as a pairing string
/// ([`parse_dial_string`]'s inverse). Relay URLs first (stable),
/// then direct addresses. `None` while the endpoint has no
/// reachable address yet.
pub fn format_dial_string(addr: &EndpointAddr) -> Option<String> {
    let mut targets: Vec<String> = addr.relay_urls().map(|r| r.to_string()).collect();
    targets.extend(addr.ip_addrs().map(|a| a.to_string()));
    if targets.is_empty() {
        return None;
    }
    Some(format!(
        "{}@{}",
        hex::encode(addr.id.as_bytes()),
        targets.join(",")
    ))
}

/// Client half: resolves a peer's `node_pubkey` to a localhost base
/// URL bridged over iroh.
#[derive(Debug)]
pub struct IrohTransport {
    endpoint: iroh::Endpoint,
    /// Out-of-band dial hints: pubkey → iroh UDP socket addresses.
    /// **Fallback only.** Production (W2) resolves dial info from the
    /// `PeerContact` the mesh gossiped — relay URL + direct addrs.
    /// Retained so hermetic tests can seed addresses directly; when a
    /// contact carries its own addrs, both are merged.
    known_addrs: std::sync::Mutex<HashMap<[u8; 32], Vec<SocketAddr>>>,
    /// One localhost bridge per (peer, ALPN): a peer is reached on the
    /// internal ALPN for most classes and the client ALPN for
    /// inference/status, so the two ride separate tunnels. Each entry
    /// remembers the dial info it was built with (`dial_key`) so a
    /// gossiped dial-info change REBUILDS the bridge — a frozen target
    /// kept dialing a restarted peer's dead ephemeral port forever
    /// (the 2026-07-19 dual-restart heal deadlock's transport half).
    bridges: tokio::sync::Mutex<HashMap<([u8; 32], &'static [u8]), CachedBridge>>,
}

/// A cached bridge plus the normalized contact dial-info it targets.
#[derive(Debug)]
struct CachedBridge {
    bridge: Arc<HttpBridge>,
    dial_key: DialKey,
}

/// Normalized (relay_url, sorted direct addrs) — the parts of a
/// `PeerContact` that determine where a bridge's iroh dials go.
type DialKey = (Option<String>, Vec<SocketAddr>);

fn dial_key_for(peer: &PeerContact) -> DialKey {
    let mut addrs = peer.iroh_direct_addrs.clone();
    addrs.sort();
    addrs.dedup();
    (peer.relay_url.clone(), addrs)
}

impl IrohTransport {
    pub fn new(endpoint: iroh::Endpoint) -> Self {
        Self {
            endpoint,
            known_addrs: std::sync::Mutex::new(HashMap::new()),
            bridges: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Seed dial hints for a peer. Test/fallback surface — production
    /// reads them from the gossiped `PeerContact` instead.
    pub fn add_known_peer(&self, pubkey: NodePubkey, addrs: Vec<SocketAddr>) {
        if let Ok(mut map) = self.known_addrs.lock() {
            map.insert(*pubkey.as_bytes(), addrs);
        }
    }

    /// The ALPN a traffic class rides: client-port classes
    /// (Inference/StatusProbe) reach the peer's client router, every
    /// other class reaches its internal router. The *class chooses the
    /// ALPN* — the iroh analogue of `IpTransport`'s per-class port
    /// policy, and why there's no port rewrite here.
    fn alpn_for_class(class: TrafficClass) -> &'static [u8] {
        match class {
            TrafficClass::Inference | TrafficClass::StatusProbe => CLIENT_ALPN,
            TrafficClass::RpcTensor => RPC_ALPN,
            _ => ALPN,
        }
    }

    /// Build the dial target from the peer's gossiped iroh info (relay
    /// URL + direct addrs), merging any test-seeded `known_addrs`.
    /// `None` when there's no usable path (no relay AND no address) — a
    /// bare key isn't dialable without one, so such a peer falls
    /// through to the IP transport in a routed composition.
    fn endpoint_addr_for(
        &self,
        pubkey: &NodePubkey,
        peer: &PeerContact,
    ) -> Option<iroh::EndpointAddr> {
        let id = iroh::PublicKey::from_bytes(pubkey.as_bytes()).ok()?;
        let mut ea = iroh::EndpointAddr::new(id);
        let mut has_path = false;
        if let Some(relay) = peer.relay_url.as_deref() {
            match relay.parse::<RelayUrl>() {
                Ok(url) => {
                    ea = ea.with_relay_url(url);
                    has_path = true;
                }
                Err(e) => tracing::warn!(
                    target: "transport",
                    transport = "iroh",
                    relay = %relay,
                    error = %e,
                    "iroh: peer relay_url did not parse — ignoring"
                ),
            }
        }
        // Relay-pin (bench posture): when this process is relay-pinned and
        // the peer has a relay, seed ONLY the relay. Seeding direct addrs
        // makes the pin a RACE the selector cannot win — a direct path that
        // validates first (same box ~1ms, warmed LAN) becomes the current
        // path, and the selector's no-relay-open fallback keeps it: the run
        // silently measures the direct path at full speed (observed
        // 2026-07-19: hairpin "relay" run at 0.9ms/16KB). With only the
        // relay seeded, relay is current from the first packet and
        // later-discovered directs stay unselected.
        if relay_pin_active() && has_path {
            return Some(ea);
        }
        let seeded = self
            .known_addrs
            .lock()
            .ok()
            .and_then(|m| m.get(pubkey.as_bytes()).cloned())
            .unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        for a in peer.iroh_direct_addrs.iter().copied().chain(seeded) {
            if seen.insert(a) {
                ea = ea.with_ip_addr(a);
                has_path = true;
            }
        }
        has_path.then_some(ea)
    }

    /// Get or create the localhost TCP bridge for `(pubkey, alpn)`,
    /// dialing the target resolved from `peer`.
    async fn bridge_for(
        &self,
        pubkey: &NodePubkey,
        peer: &PeerContact,
        alpn: &'static [u8],
    ) -> Option<SocketAddr> {
        let key = (*pubkey.as_bytes(), alpn);
        let dial_key = dial_key_for(peer);
        let mut bridges = self.bridges.lock().await;
        if let Some(cached) = bridges.get(&key) {
            if cached.dial_key == dial_key {
                return Some(cached.bridge.local_addr());
            }
        }
        let Some(target) = self.endpoint_addr_for(pubkey, peer) else {
            // The fresh contact has no dialable path at all (no relay, no
            // direct addrs). Nothing to retarget TO — drop any stale bridge
            // rather than keep tunneling at an address the peer has left.
            bridges.remove(&key);
            return None;
        };
        if let Some(cached) = bridges.get_mut(&key) {
            // The peer's gossiped dial info changed (typical: it
            // restarted and its ephemeral iroh port moved). A frozen
            // bridge would keep dialing the dead target forever — point
            // this one at the fresh contact instead. Retargeting rather
            // than rebuilding keeps the loopback port stable, which is
            // what plain-TCP clients holding that address depend on (see
            // `HttpBridge::retarget`); in-flight tunnels to the stale
            // target were doomed either way.
            cached.bridge.retarget(target);
            cached.dial_key = dial_key;
            let local_addr = cached.bridge.local_addr();
            tracing::info!(
                target: "transport",
                peer = %hex::encode(&key.0[..4]),
                bridge = %local_addr,
                "iroh bridge: peer dial info changed — retargeted in place (port held)"
            );
            return Some(local_addr);
        }
        let bridge = HttpBridge::spawn(self.endpoint.clone(), target, alpn)
            .await
            .ok()?;
        let local_addr = bridge.local_addr();
        bridges.insert(
            key,
            CachedBridge {
                bridge: Arc::new(bridge),
                dial_key,
            },
        );
        Some(local_addr)
    }
}

/// Copy bytes both ways between a TCP socket and an iroh bi-stream
/// until both directions close.
async fn pump(
    tcp: tokio::net::TcpStream,
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
) {
    let (mut tcp_r, mut tcp_w) = tcp.into_split();
    let up = async {
        let _ = tokio::io::copy(&mut tcp_r, &mut send).await;
        let _ = send.finish();
    };
    let down = async {
        let _ = tokio::io::copy(&mut recv, &mut tcp_w).await;
        let _ = tokio::io::AsyncWriteExt::shutdown(&mut tcp_w).await;
    };
    tokio::join!(up, down);
}

#[async_trait::async_trait]
impl PeerTransport for IrohTransport {
    fn name(&self) -> &'static str {
        "iroh"
    }

    async fn endpoints(&self, peer: &PeerContact, class: TrafficClass) -> Vec<PeerEndpoint> {
        // No identity key → not dialable on this transport. (A
        // routed/fallback composition would send such peers to the
        // IP transport.)
        let Some(pubkey) = peer.node_pubkey else {
            tracing::debug!(
                target: "transport",
                transport = "iroh",
                class = class.as_str(),
                peer = %peer.node_id,
                "iroh: peer has no node_pubkey — not dialable"
            );
            return Vec::new();
        };
        let alpn = Self::alpn_for_class(class);
        let Some(local) = self.bridge_for(&pubkey, peer, alpn).await else {
            tracing::debug!(
                target: "transport",
                transport = "iroh",
                class = class.as_str(),
                peer = %peer.node_id,
                "iroh: no dialable path in contact (no relay_url / iroh_direct_addrs) \
                 — not dialable (routed composition falls back to IP)"
            );
            return Vec::new();
        };
        let ep = PeerEndpoint {
            base_url: format!("http://{local}"),
            label: format!("iroh:{local}→{}", &pubkey.to_string()[..8]),
        };
        tracing::debug!(
            target: "transport",
            transport = "iroh",
            class = class.as_str(),
            peer = %peer.node_id,
            candidates = 1usize,
            first = %ep.label,
            "transport: resolved"
        );
        vec![ep]
    }

    fn note_success(&self, _peer: NodeId, _class: TrafficClass, _endpoint: &PeerEndpoint) {
        // iroh maintains and migrates paths itself; nothing to do.
    }
}

/// Server half: accept iroh bi-streams and forward each to the
/// daemon's existing localhost HTTP listener.
pub struct IrohAcceptor {
    task: tokio::task::JoinHandle<()>,
}

impl Drop for IrohAcceptor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl IrohAcceptor {
    /// Spawn the accept loop forwarding EVERY accepted bi-stream to a
    /// single local listener, regardless of negotiated ALPN. Right for
    /// a single-ALPN endpoint (Track M: `sovereign-server` binds only
    /// `cwth/client/0` → its HTTP listener).
    pub fn spawn(endpoint: iroh::Endpoint, forward_to: SocketAddr) -> Self {
        Self::run(endpoint, move |_alpn| Some(forward_to))
    }

    /// Spawn the accept loop routing each connection to a local
    /// listener chosen by its **negotiated ALPN** — the W1 capability
    /// that lets one daemon endpoint serve both the internal router
    /// (`cwth/http/0`) and the client router (`cwth/client/0`) without
    /// a port (the class chose the ALPN). A connection whose ALPN is
    /// not in `routes` is closed with a loud log, never misrouted.
    pub fn spawn_routed(endpoint: iroh::Endpoint, routes: HashMap<Vec<u8>, SocketAddr>) -> Self {
        Self::run(endpoint, move |alpn| routes.get(alpn).copied())
    }

    /// Shared accept loop. `resolve` maps a connection's negotiated
    /// ALPN to the local TCP target its bi-streams forward to; `None`
    /// closes the connection. Each accepted connection's ALPN is read
    /// once, then every bi-stream on it is pumped to that target.
    fn run<F>(endpoint: iroh::Endpoint, resolve: F) -> Self
    where
        F: Fn(&[u8]) -> Option<SocketAddr> + Send + Sync + 'static,
    {
        let resolve = Arc::new(resolve);
        let task = tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let resolve = resolve.clone();
                tokio::spawn(async move {
                    let conn = match incoming.await {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::debug!(
                                target: "transport",
                                error = %e,
                                "iroh acceptor: handshake failed"
                            );
                            return;
                        }
                    };
                    // A fully-accepted connection has a negotiated
                    // ALPN; route this connection's streams by it.
                    let alpn = conn.alpn().to_vec();
                    let Some(forward_to) = resolve(&alpn) else {
                        tracing::warn!(
                            target: "transport",
                            alpn = %String::from_utf8_lossy(&alpn),
                            "iroh acceptor: no local forward for negotiated ALPN — closing connection"
                        );
                        return;
                    };
                    loop {
                        match conn.accept_bi().await {
                            Ok((send, recv)) => {
                                tokio::spawn(async move {
                                    match tokio::net::TcpStream::connect(forward_to).await {
                                        Ok(tcp) => {
                                            // Same Nagle × delayed-ACK stall as the
                                            // bridge side — see HttpBridge::spawn.
                                            tcp.set_nodelay(true).ok();
                                            pump(tcp, send, recv).await
                                        }
                                        Err(e) => tracing::warn!(
                                            target: "transport",
                                            error = %e,
                                            forward_to = %forward_to,
                                            "iroh acceptor: local forward connect failed"
                                        ),
                                    }
                                });
                            }
                            Err(_) => break, // connection closed
                        }
                    }
                });
            }
        });
        Self { task }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn parse_relay_mode_empty_is_default() {
        // Empty config = leave the caller on the preset (n0) relays.
        assert!(parse_relay_mode(&[]).is_none());
    }

    #[test]
    fn parse_relay_mode_valid_urls_build_custom() {
        let mode = parse_relay_mode(&[
            "https://relay.corp.example:443".to_string(),
            "https://relay2.corp.example".to_string(),
        ]);
        assert!(
            matches!(mode, Some(iroh::RelayMode::Custom(_))),
            "valid relay URLs must build a custom relay map"
        );
    }

    #[test]
    fn parse_relay_mode_all_invalid_falls_back_to_default() {
        // A fat-fingered relay URL must not abort — fall back to the
        // default relays rather than take the node offline.
        assert!(parse_relay_mode(&["not a url".to_string()]).is_none());
    }

    #[test]
    fn parse_relay_mode_skips_bad_keeps_good() {
        let mode = parse_relay_mode(&[
            "://broken".to_string(),
            "https://relay.corp.example:443".to_string(),
        ]);
        assert!(
            matches!(mode, Some(iroh::RelayMode::Custom(_))),
            "one valid URL among bad ones still yields a custom map"
        );
    }

    /// Bind an empty (no-relay, deterministic) endpoint serving `alpns`.
    async fn hermetic_endpoint(seed: u8, alpns: Vec<Vec<u8>>) -> Endpoint {
        EndpointBuilder::empty()
            .crypto_provider(ring_crypto_provider())
            .secret_key(SecretKey::from_bytes(&[seed; 32]))
            .alpns(alpns)
            .bind()
            .await
            .expect("hermetic endpoint bind")
    }

    /// iroh binds the wildcard; rewrite to loopback so the address is
    /// dialable in-process (mirrors the spike e2e's `dialable_sockets`).
    fn loopback_sockets(endpoint: &Endpoint) -> Vec<SocketAddr> {
        endpoint
            .bound_sockets()
            .into_iter()
            .map(|mut a| {
                if a.ip().is_unspecified() {
                    let ip = if a.is_ipv4() { "127.0.0.1" } else { "::1" };
                    a.set_ip(ip.parse().unwrap());
                }
                a
            })
            .collect()
    }

    /// A trivial TCP "service": every accepted connection is answered
    /// with a fixed marker, then closed. Stands in for one of the
    /// daemon's two local HTTP listeners — the marker is the witness
    /// that a stream reached THIS listener and not the other. It also
    /// drains the client's bytes so the close is a clean FIN, not a
    /// RST that could truncate the marker in flight on loopback.
    async fn spawn_marker_listener(marker: &'static [u8]) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let _ = sock.write_all(marker).await;
                    let _ = sock.shutdown().await;
                    let mut drain = Vec::new();
                    let _ = sock.read_to_end(&mut drain).await;
                });
            }
        });
        addr
    }

    /// Dial the routed acceptor on `alpn` via an `HttpBridge` and read
    /// back whatever local listener the acceptor forwarded us to. Each
    /// call uses a FRESH client endpoint (seeded distinctly) so the two
    /// ALPN dials are unambiguously separate QUIC connections — no risk
    /// of a coalesced connection carrying the prior dial's ALPN.
    async fn read_marker_over(seed: u8, server: &EndpointAddr, alpn: &'static [u8]) -> Vec<u8> {
        let client_ep = hermetic_endpoint(seed, vec![]).await;
        let bridge = HttpBridge::spawn(client_ep, server.clone(), alpn)
            .await
            .expect("bridge spawns");
        let mut tcp = tokio::net::TcpStream::connect(bridge.local_addr())
            .await
            .expect("connect to bridge");
        // Send a probe and half-close our write side. A QUIC bi-stream
        // a client merely opens isn't surfaced to the server's
        // `accept_bi()` until the client sends on it — so without this,
        // the acceptor never sees the stream and never forwards. This
        // mirrors real traffic (reqwest sends a request first); the FIN
        // also lets the marker listener drain to a clean close.
        let _ = tcp.write_all(b"ping").await;
        let _ = tcp.shutdown().await;
        let mut buf = Vec::new();
        // Generous timeout: a routing miss closes the connection, which
        // surfaces here as an empty read rather than a hang.
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tcp.read_to_end(&mut buf),
        )
        .await;
        read.expect("read did not time out").expect("read ok");
        buf
    }

    /// W1 keystone: ONE iroh endpoint, bound with both the internal and
    /// client ALPNs, dispatches each accepted connection to a different
    /// local listener purely by its negotiated ALPN — the "class chose
    /// the ALPN, not a port" property that lets the mesh daemon serve
    /// its internal (`cwth/http/0`) and client (`cwth/client/0`)
    /// routers over a single dial-by-key endpoint.
    #[tokio::test]
    async fn routed_acceptor_dispatches_by_alpn() {
        let internal_addr = spawn_marker_listener(b"INTERNAL").await;
        let client_addr = spawn_marker_listener(b"CLIENT").await;

        let server_ep = hermetic_endpoint(11, vec![ALPN.to_vec(), CLIENT_ALPN.to_vec()]).await;
        let mut routes = HashMap::new();
        routes.insert(ALPN.to_vec(), internal_addr);
        routes.insert(CLIENT_ALPN.to_vec(), client_addr);
        let _acceptor = IrohAcceptor::spawn_routed(server_ep.clone(), routes);

        // The dialable target: the server's key + its loopback sockets.
        let mut target = EndpointAddr::new(server_ep.id());
        for s in loopback_sockets(&server_ep) {
            target = target.with_ip_addr(s);
        }

        // Internal ALPN must land on the internal listener…
        assert_eq!(
            read_marker_over(21, &target, ALPN).await,
            b"INTERNAL",
            "cwth/http/0 must route to the internal listener"
        );
        // …and the client ALPN on the client listener — same endpoint,
        // same key, routed solely by ALPN.
        assert_eq!(
            read_marker_over(22, &target, CLIENT_ALPN).await,
            b"CLIENT",
            "cwth/client/0 must route to the client listener"
        );
    }

    /// A connection negotiating an ALPN with no route is closed, not
    /// misrouted to whatever happens to be in the map.
    #[tokio::test]
    async fn routed_acceptor_drops_unknown_alpn() {
        let internal_addr = spawn_marker_listener(b"INTERNAL").await;
        // Server offers BOTH ALPNs (so the handshake succeeds) but the
        // route table only knows the internal one.
        let server_ep = hermetic_endpoint(13, vec![ALPN.to_vec(), CLIENT_ALPN.to_vec()]).await;
        let mut routes = HashMap::new();
        routes.insert(ALPN.to_vec(), internal_addr);
        let _acceptor = IrohAcceptor::spawn_routed(server_ep.clone(), routes);

        let mut target = EndpointAddr::new(server_ep.id());
        for s in loopback_sockets(&server_ep) {
            target = target.with_ip_addr(s);
        }
        // Dialing the unrouted (but offered) client ALPN: the acceptor
        // closes the connection, so we read zero bytes — never INTERNAL.
        let got = read_marker_over(24, &target, CLIENT_ALPN).await;
        assert!(got.is_empty(), "unrouted ALPN must be dropped, got {got:?}");
    }

    #[test]
    fn redact_userinfo_hides_credentials() {
        assert_eq!(
            redact_userinfo("https://user:secret@proxy.corp:443"),
            "https://***@proxy.corp:443"
        );
        // No userinfo → unchanged.
        assert_eq!(
            redact_userinfo("https://proxy.corp:443"),
            "https://proxy.corp:443"
        );
        // Non-URL → unchanged (don't mangle).
        assert_eq!(redact_userinfo("proxy.corp:443"), "proxy.corp:443");
    }

    #[test]
    fn relay_config_from_parts_maps_discovery() {
        // Default / "n0" / absent → n0 services on.
        assert!(RelayConfig::default().n0_services);
        assert!(RelayConfig::from_parts(vec![], None).n0_services);
        assert!(RelayConfig::from_parts(vec![], Some("n0")).n0_services);
        // Sovereignty spellings → n0 severed.
        for d in ["none", "self", "local"] {
            assert!(
                !RelayConfig::from_parts(vec![], Some(d)).n0_services,
                "discovery={d} must sever n0"
            );
        }
        // Unknown → safe default (n0 on).
        assert!(RelayConfig::from_parts(vec![], Some("carrier-pigeon")).n0_services);
        // relay_urls passes through.
        let c = RelayConfig::from_parts(vec!["https://r.example:443".into()], Some("none"));
        assert_eq!(c.relay_urls, vec!["https://r.example:443".to_string()]);
        assert!(!c.n0_services);
    }

    #[tokio::test]
    async fn build_relayed_endpoint_with_custom_relay_binds() {
        // A configured self-hosted relay must not break endpoint
        // construction: bind is a local operation, the relay connection
        // is a background task (so this succeeds offline). Proves the
        // relay_urls → custom RelayMode path threads through to a valid
        // endpoint. proxy_from_env is a no-op here (env unset).
        let ep = build_relayed_endpoint(
            SecretKey::from_bytes(&[77u8; 32]),
            vec![ALPN.to_vec()],
            &RelayConfig {
                relay_urls: vec!["https://relay.corp.example:443".to_string()],
                n0_services: true,
            },
        )
        .await
        .expect("custom-relay endpoint must bind");
        assert!(
            !ep.bound_sockets().is_empty(),
            "endpoint must bind a socket"
        );
    }

    #[tokio::test]
    async fn build_sovereign_endpoint_binds_without_n0() {
        // Sovereign mode (n0 severed, no custom relay = direct-addr
        // only): must still bind a real endpoint. This is the
        // air-gapped-LAN posture — Minimal preset, relays Disabled, no
        // n0 DNS. Nothing here should touch the network at all.
        let ep = build_relayed_endpoint(
            SecretKey::from_bytes(&[78u8; 32]),
            vec![ALPN.to_vec()],
            &RelayConfig {
                relay_urls: vec![],
                n0_services: false,
            },
        )
        .await
        .expect("sovereign (no-n0) endpoint must bind");
        assert!(
            !ep.bound_sockets().is_empty(),
            "endpoint must bind a socket"
        );
    }

    /// Read the marker a bridge's loopback port forwards to. Same handshake as
    /// `read_marker_over` (send first so the peer's `accept_bi` fires), but
    /// against an ALREADY-BOUND bridge port rather than a fresh bridge — the
    /// point being that the port outlives a retarget.
    async fn read_marker_from_port(port: SocketAddr) -> Vec<u8> {
        let mut tcp = tokio::net::TcpStream::connect(port)
            .await
            .expect("connect to bridge port");
        let _ = tcp.write_all(b"ping").await;
        let _ = tcp.shutdown().await;
        let mut buf = Vec::new();
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tcp.read_to_end(&mut buf),
        )
        .await;
        read.expect("read did not time out").expect("read ok");
        buf
    }

    fn contact_for(server: &Endpoint, addrs: Vec<SocketAddr>) -> crate::PeerContact {
        crate::PeerContact {
            node_id: commonwealth_core::ids::NodeId::from_u128(9),
            addresses: vec![],
            node_pubkey: Some(NodePubkey(*server.id().as_bytes())),
            relay_url: None,
            iroh_direct_addrs: addrs,
        }
    }

    /// A peer's gossiped dial info changing must RETARGET the bridge, not
    /// rebuild it: the loopback port is what plain-TCP clients hold (ggml's
    /// rpc-server list, via the mesh's discovered worker endpoint string), and
    /// minting a new one made an unmoved peer read downstream as a stream of
    /// different workers (2026-07-25: 34207 → 40043 → 39419 → 34133 → 40021).
    /// The tunnel must still land on the NEW target — that's what the rebuild
    /// was originally protecting (the 2026-07-19 dual-restart heal deadlock), so
    /// this asserts both halves: same port, fresh destination.
    #[tokio::test]
    async fn bridge_retargets_in_place_when_peer_dial_info_changes() {
        let worker_addr = spawn_marker_listener(b"WORKER").await;
        let server_ep = hermetic_endpoint(31, vec![RPC_ALPN.to_vec()]).await;
        let mut routes = HashMap::new();
        routes.insert(RPC_ALPN.to_vec(), worker_addr);
        let _acceptor = IrohAcceptor::spawn_routed(server_ep.clone(), routes);

        let transport = IrohTransport::new(hermetic_endpoint(32, vec![]).await);

        // Tick 1: the peer's gossiped address is stale (nothing listens there).
        // The bridge binds a port; nothing is dialed until a client connects.
        let stale: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let first = transport
            .endpoints(
                &contact_for(&server_ep, vec![stale]),
                TrafficClass::RpcTensor,
            )
            .await;
        let port_before = first[0].base_url.clone();

        // Tick 2: gossip carries the peer's real address. Different dial key.
        let live = loopback_sockets(&server_ep);
        let second = transport
            .endpoints(&contact_for(&server_ep, live), TrafficClass::RpcTensor)
            .await;
        assert_eq!(
            second[0].base_url, port_before,
            "a dial-info change must hold the loopback port, not mint a new one"
        );

        // …and the tunnel now reaches the peer at its NEW address, proving the
        // held port is not a frozen bridge still dialing the dead one.
        let bridge_port: SocketAddr = second[0]
            .base_url
            .strip_prefix("http://")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            read_marker_from_port(bridge_port).await,
            b"WORKER",
            "the retargeted bridge must reach the peer's current address"
        );
    }

    /// An unchanged contact re-resolves to the SAME bridge with no rebind. This
    /// is what makes the mesh's per-tick re-mint of a known bridged worker
    /// (`sovereign_mesh::daemon::reaffirm_plan` → `Rebridge`) free: it replaces
    /// a `/status` probe that rode the same congested iroh path as the tunnel.
    #[tokio::test]
    async fn repeated_resolution_of_an_unchanged_peer_reuses_the_bridge() {
        let server_ep = hermetic_endpoint(33, vec![RPC_ALPN.to_vec()]).await;
        let transport = IrohTransport::new(hermetic_endpoint(34, vec![]).await);
        let contact = contact_for(&server_ep, loopback_sockets(&server_ep));
        let a = transport.endpoints(&contact, TrafficClass::RpcTensor).await;
        let b = transport.endpoints(&contact, TrafficClass::RpcTensor).await;
        assert_eq!(a[0].base_url, b[0].base_url, "cached bridge must be reused");
    }

    #[test]
    fn dial_key_normalizes_addr_order_and_dupes() {
        use crate::PeerContact;
        use commonwealth_core::ids::NodeId;
        let a: SocketAddr = "10.0.0.1:1000".parse().unwrap();
        let b: SocketAddr = "10.0.0.2:2000".parse().unwrap();
        let mk = |addrs: Vec<SocketAddr>| PeerContact {
            node_id: NodeId::from_u128(1),
            addresses: vec![],
            node_pubkey: None,
            relay_url: Some("https://r.example/".into()),
            iroh_direct_addrs: addrs,
        };
        // Same set, different order / dup → SAME key: a reordered gossip
        // record must NOT churn the bridge.
        assert_eq!(
            super::dial_key_for(&mk(vec![a, b])),
            super::dial_key_for(&mk(vec![b, a, b]))
        );
        // A changed port (peer restarted) → DIFFERENT key → rebuild.
        let b2: SocketAddr = "10.0.0.2:2001".parse().unwrap();
        assert_ne!(
            super::dial_key_for(&mk(vec![a, b])),
            super::dial_key_for(&mk(vec![a, b2]))
        );
    }
}
