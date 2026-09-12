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

use std::net::SocketAddr;
use std::path::Path;

use commonwealth_transport::iroh::{
    build_relayed_endpoint, format_dial_string, Endpoint, Forward, IrohAcceptor, IrohTransport,
    RelayConfig, ALPN, CLIENT_ALPN, GUEST_ALPN, MEDIA_ALPN, RPC_ALPN,
};
use commonwealth_transport::TrafficClass;

/// The media decisions this acceptor makes are the package crate's (moved
/// 2026-09-11, cw-lift D1): the membership consult, the identity an origin is
/// handed, the `media_allow` verdict, and the live-path reading. Re-exported
/// under their old paths so `daemon.rs`, `media_reach`, the CLI and the tests
/// keep naming them here, while a package-only rails daemon composes exactly
/// the same code (ARCH §10.6).
pub use commonwealth_media::{admits_no_one, MemberCheck, MemberIdentity, PeerTransportPath};

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
    // `Media` has no `[iroh.transport]` entry, on purpose: the other seven
    // choose between two transports, and this one has only one. The holder's
    // origin is loopback-bound and admitted by mesh key, so `= "ip"` would
    // pin the class to a transport that returns no candidates for it — a
    // config line that silently turns the feature off. Always routed.
    out.push(TrafficClass::Media);
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
) -> [(TrafficClass, &Option<String>); 7] {
    [
        (TrafficClass::Gossip, &t.gossip),
        (TrafficClass::ControlPlane, &t.control_plane),
        (TrafficClass::KnowledgeSearch, &t.knowledge_search),
        (TrafficClass::ModelTransfer, &t.model_transfer),
        (TrafficClass::Inference, &t.inference),
        (TrafficClass::StatusProbe, &t.status_probe),
        (TrafficClass::RpcTensor, &t.rpc_tensor),
    ]
}

/// Build the mesh endpoint, honoring the bench-only relay pin.
/// `SOVEREIGN_IROH_RELAY_ONLY=1` + the `iroh-relay-only` feature pins ALL
/// of this node's iroh traffic (accept + dial — one endpoint serves both)
/// to the relay path: the deterministic relay-floor posture for
/// distributed-inference measurement. Both peers must run it, or the
/// unpinned side answers over the direct path and halves the measured
/// tax. If the env is set but the feature wasn't compiled in, we log at
/// ERROR and continue unpinned — the daemon must not die, but the run's
/// measurements are invalid and the log says so.
async fn build_mesh_endpoint(
    secret: commonwealth_transport::iroh::SecretKey,
    alpns: Vec<Vec<u8>>,
    relay_cfg: &RelayConfig,
) -> Result<Endpoint, String> {
    let want_relay_only = std::env::var("SOVEREIGN_IROH_RELAY_ONLY")
        .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
        .unwrap_or(false);
    if !want_relay_only {
        return build_relayed_endpoint(secret, alpns, relay_cfg).await;
    }
    #[cfg(feature = "iroh-relay-only")]
    {
        tracing::warn!(
            target: "transport",
            "iroh(mesh): SOVEREIGN_IROH_RELAY_ONLY=1 — endpoint pinned to the \
             relay path (bench posture, both peers must run this)"
        );
        commonwealth_transport::iroh::build_relay_only_endpoint(secret, alpns, relay_cfg).await
    }
    #[cfg(not(feature = "iroh-relay-only"))]
    {
        tracing::error!(
            target: "transport",
            "SOVEREIGN_IROH_RELAY_ONLY=1 set but this build lacks the \
             `iroh-relay-only` feature — running UNPINNED; relay-floor \
             measurements from this run are INVALID. Rebuild with \
             --features iroh-relay-only."
        );
        build_relayed_endpoint(secret, alpns, relay_cfg).await
    }
}

/// The local ggml rpc-server port when this node is configured to serve
/// one (`SOVEREIGN_RPC_SERVE`, e.g. `0.0.0.0:50052` — same parse shape as
/// `routes_status::rpc_worker_port`). `None` = not an RPC worker; the
/// acceptor then neither advertises nor routes [`RPC_ALPN`].
fn rpc_serve_port() -> Option<u16> {
    sovereign_contracts::launch::RpcServe::from_env().port()
}

/// Resolve whether this daemon's iroh endpoint turns on.
///
/// The **local-only profile outranks everything** — it is the one decider for
/// "does this daemon touch the network at all" (ARCH §10.6), so a gate that
/// could turn a socket on behind its back would make it a suggestion. The
/// `require_encryption` contradiction is not resolved here by silently
/// picking a winner: `EmbeddedDaemon::start_daemon` REFUSES to boot a
/// local-only daemon on an encrypted mesh, loudly and before this is called
/// (ARCH §18.3), so the `false` returned below is only ever reached in a
/// configuration that is coherent.
///
/// Below the profile: explicit config wins; otherwise mesh participation
/// decides (the `client-exposed` marker every explicit create/join surface
/// writes) — consent-by-mesh-participation, so a meshless daemon never
/// contacts relay infrastructure. A mesh-wide `require_encryption` overrides
/// those two: an encrypted mesh cannot run without iroh (the daemon
/// hard-fails later if the endpoint won't bind, rather than silently
/// downgrading).
pub fn resolve_enabled(
    profile: crate::local_only::LocalOnlyProfile,
    cfg_enabled: Option<bool>,
    mesh_participant: bool,
    require_encryption: bool,
) -> bool {
    if profile.is_local_only() {
        return false;
    }
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

/// Every member the iroh endpoint could hold a path to, paired with what
/// membership believes and what the endpoint actually holds — the watchdog's
/// peer-path health term (note `a3f3fbff`).
///
/// A free function taking the endpoint rather than a method on
/// `EmbeddedDaemon` because the watchdog swaps its own endpoint handle on
/// rebuild: the health judgement must be made against the endpoint the
/// watchdog is holding, not against whatever the daemon state last recorded.
/// Members without a pubkey are not iroh-dialable and are left out entirely —
/// counting them would make an IP-only peer look like a lost path.
pub(crate) async fn observe_peer_paths(
    app_state: &sovereign_api::state::AppState,
    endpoint: &Endpoint,
) -> Vec<crate::iroh_watchdog::PeerPathObservation> {
    let self_id = *app_state.inner.self_node_id_swap.load_full().as_ref();
    // Clone the members out before awaiting — the codebase's
    // clone-out-then-await rule; `peer_path_snapshot` awaits per peer.
    let members: Vec<commonwealth_core::mesh::MemberRecord> = {
        let mesh = app_state.inner.mesh.read().await;
        mesh.members
            .values()
            .filter(|m| m.node_id != self_id && m.node_pubkey.is_some())
            .cloned()
            .collect()
    };
    let mut out = Vec::with_capacity(members.len());
    for m in members {
        let pubkey = m.node_pubkey.expect("filtered to Some above");
        let path = match commonwealth_transport::iroh::PublicKey::from_bytes(&pubkey.0) {
            Ok(id) => commonwealth_transport::iroh::peer_path_snapshot(endpoint, id)
                .await
                .map(|s| s.path),
            // A member whose stored pubkey is not a valid Ed25519 point can
            // never be dialed by key — a roster fault, not a transport one.
            // It reads as `None` (no record) so the census still accounts for
            // every member, and it SAYS SO rather than being absorbed into
            // the no-record population it is indistinguishable from (§18.3).
            // Harmless to the health term either way: a peer that could never
            // be dialed never enters `last_active`, so it can only ever add
            // to the total, never report a loss or arm the term.
            Err(e) => {
                tracing::warn!(
                    peer = %m.node_id,
                    name = %m.name,
                    error = %e,
                    "iroh(mesh): member pubkey is not a valid endpoint id — not dialable by \
                     key; counted as no-path, never as a lost path"
                );
                None
            }
        };
        out.push(crate::iroh_watchdog::PeerPathObservation {
            node_id: m.node_id.to_string(),
            name: m.name.clone(),
            believed_online: m.status != commonwealth_core::mesh::NodeStatus::Offline,
            path,
        });
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
    /// Whether this acceptor routes [`RPC_ALPN`] to a local ggml
    /// rpc-server — the truth behind `/status`'s `rpc_worker.iroh` flag.
    rpc_route_active: bool,
    /// Whether this acceptor routes [`MEDIA_ALPN`] to a local media origin —
    /// the fact the gossip self-stamp advertises as `origins: [media]`.
    media_route_active: bool,
}

/// The four local listeners an accepted iroh connection can be forwarded to.
///
/// A value rather than four loose arguments because the decision below reads
/// them together, and because a test wiring a real acceptor must be able to
/// build the SAME routing the daemon does — not a second copy of it (§10.6).
#[derive(Debug, Clone)]
pub struct AcceptorRoutes {
    /// The mesh-internal router (`:9742`), loopback-bound.
    pub internal: SocketAddr,
    /// The PEER bind of the client router: admits loopback (a member
    /// presents no bearer), but does not serve `/internal/*`. `None` when
    /// it failed to bind — a member is then closed, not promoted to the
    /// operator's own listener.
    ///
    /// There is deliberately no route to that listener from here. The
    /// acceptor cannot reach the operator surface at all, which is what
    /// "loopback-or-full-token" was always supposed to mean.
    pub peer: Option<SocketAddr>,
    /// The bind of the client router that does NOT admit loopback, and
    /// serves no `/internal/*`. `None` when it failed to bind — a stranger
    /// is then closed, not promoted.
    pub guest: Option<SocketAddr>,
    /// A local ggml rpc-server, when this node serves one.
    pub rpc: Option<SocketAddr>,
    /// A local HTTP media origin, when this node's operator declared one
    /// (`[iroh] media_origin`). `None` means the protocol is not advertised at
    /// all, so a dial is closed rather than left hanging on a route to nowhere.
    pub media: Option<SocketAddr>,
    /// Headers this node adds to requests reaching its OWN media origin,
    /// read once at construction from `<data_dir>/secrets/media/` (0600 files,
    /// `commonwealth_media::declared`). This is how a holder authenticates to
    /// its own Jellyfin without any viewer holding its key — see that module
    /// for why the value is not in config.
    pub media_declared: std::sync::Arc<Vec<(String, String)>>,
    /// `[iroh] media_allow`: which members may reach `media`, by name or id
    /// prefix. Empty admits every member. A non-member is refused either way.
    pub media_allow: std::sync::Arc<Vec<String>>,
}

impl AcceptorRoutes {
    /// Where an accepted connection is forwarded — the ONE place that turns
    /// `(protocol, who dialed)` into a local listener.
    ///
    /// **Holding this node's dial string is not a credential.** It is public:
    /// it rides in every invite's `dial=` and is gossiped as
    /// `MemberRecord.node_pubkey`. What the QUIC handshake proves is the
    /// dialer's key, and that is what this consults.
    ///
    /// - `CLIENT_ALPN` — a MEMBER reaches the PEER listener, which admits it
    ///   without a bearer. That is not a shortcut: peer federated inference
    ///   carries no `Authorization` header at all, and membership-by-key is
    ///   the credential it presents instead. What a member does NOT reach is
    ///   the operator's own `:9741` listener: that one serves `/internal/*`
    ///   — guest-grant minting, an 18.5 GB warmup — and its only guard is
    ///   "the caller is loopback", which this acceptor's forward hop
    ///   satisfies for free. So the peer bind serves a strictly smaller
    ///   router (`ClientSurface::Peer`), and no address in this struct
    ///   points at the operator listener at all. A stranger is NOT refused —
    ///   it is routed to the bearer-checking guest listener, the same
    ///   posture it would get calling a LAN-bound daemon. So `/status` and
    ///   `/oicp/v1/capabilities` stay reachable (a node must be able to read
    ///   those before it could hold anything), and everything else demands a
    ///   daemon token or a live guest grant. When the listener a dialer is
    ///   owed did not bind, it gets `None` — closed, never promoted.
    /// - `RPC_ALPN` — MEMBERS ONLY, with no fallback. It forwards to a raw
    ///   ggml rpc-server that speaks tensor operations and authenticates
    ///   nothing; there is no listener to downgrade a stranger to.
    /// - `GUEST_ALPN` — any dialer. A guest is by definition not a member, and
    ///   the listener behind this one reads its bearer.
    /// - `ALPN` (internal) — any dialer, DELIBERATELY. A joining node is not
    ///   yet a member and `/internal/join` is how it becomes one, so gating
    ///   this on membership would make the mesh unjoinable over its own
    ///   transport. The sensitive routes behind it carry their own
    ///   credentials (`gossip_authorized`, the join key). The rest do not, and
    ///   that gap outlives this function: closing it needs a join-only
    ///   listener for non-members, the same shape as the guest split above.
    ///   Named here so it is a known open edge and not an oversight.
    pub async fn forward_for(
        &self,
        alpn: &[u8],
        dialer: commonwealth_core::ids::NodePubkey,
        is_member: &MemberCheck,
    ) -> Option<Forward> {
        if alpn == ALPN {
            return Some(Forward::Splice(self.internal));
        }
        if alpn == GUEST_ALPN {
            return self.guest.map(Forward::Splice);
        }
        if alpn == CLIENT_ALPN {
            if is_member(dialer).await.is_some() {
                if self.peer.is_none() {
                    // Fail closed, loudly. The operator listener is not a
                    // fallback: it serves `/internal/*` to anything that
                    // reaches it from loopback, which this hop always does.
                    tracing::warn!(
                        target: "transport",
                        dialer = %hex::encode(dialer.0),
                        "iroh(mesh): CLOSED a CLIENT_ALPN dial from a member — the peer \
                         listener did not bind, and the operator listener is not a fallback"
                    );
                }
                return self.peer.map(Forward::Splice);
            }
            // Glassbox: this is the branch that used to be an unconditional
            // forward, so it must be visible when it fires (§9.1).
            tracing::info!(
                target: "transport",
                dialer = %hex::encode(dialer.0),
                downgraded = self.guest.is_some(),
                "iroh(mesh): CLIENT_ALPN dial from a non-member — routing to the                  bearer-checking listener (closing if it did not bind)"
            );
            return self.guest.map(Forward::Splice);
        }
        if alpn == MEDIA_ALPN {
            // One decider for the media dial: the origin declared here, the
            // roster's word on the dialer, and the allow-list. The three
            // refusals (no origin, non-member, outside the list) and their
            // tracing live in `commonwealth_media::admit_media`, so a rails
            // daemon with none of this acceptor under it refuses the same
            // dials for the same reasons (ARCH §10.6).
            return commonwealth_media::admit_media(
                is_member(dialer).await.as_ref(),
                dialer,
                self.media,
                &self.media_allow,
                &self.media_declared,
            );
        }
        if alpn == RPC_ALPN {
            if is_member(dialer).await.is_some() {
                return self.rpc.map(Forward::Splice);
            }
            tracing::warn!(
                target: "transport",
                dialer = %hex::encode(dialer.0),
                "iroh(mesh): REFUSED an RPC_ALPN dial from a non-member — the                  rpc-server authenticates nothing, so there is no safe downgrade"
            );
            return None;
        }
        None
    }
}

impl MeshIrohAccess {
    /// Bind the endpoint from `<data_dir>/node_key` and route by ALPN
    /// to the daemon's two loopback listeners. Returns `None` when
    /// `enabled` is false or on any bind failure (always logged).
    ///
    /// `internal_port` is this daemon's resolved internal port; the
    /// acceptor forwards to `127.0.0.1:<port>` (the listener binds
    /// `0.0.0.0`, which includes loopback). `peer_addr` and `guest_addr`
    /// are the daemon's two ephemeral loopback binds of the client
    /// router — the operator's own `:9741` listener is deliberately NOT
    /// passed in, because nothing here may forward to it. Forwarding is
    /// lazy per stream, so binding this before the listeners are up is
    /// safe — an early dial just fails and the client retries.
    pub async fn start(
        data_dir: &Path,
        internal_port: u16,
        peer_addr: Option<SocketAddr>,
        guest_addr: Option<SocketAddr>,
        media_origin: Option<SocketAddr>,
        media_allow: Vec<String>,
        member_check: MemberCheck,
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

        // A node serving a ggml rpc-server additionally accepts RPC_ALPN
        // so cross-network hosts reach the raw-TCP rpc-server through the
        // mesh tunnel (task 6). Gated on the env so consumer nodes never
        // advertise an ALPN they can't forward.
        let rpc_forward: Option<SocketAddr> = rpc_serve_port().map(|p| ([127, 0, 0, 1], p).into());
        let mut alpns = vec![ALPN.to_vec(), CLIENT_ALPN.to_vec()];
        if rpc_forward.is_some() {
            alpns.push(RPC_ALPN.to_vec());
        }
        // Advertised only when there is something behind it, for the same
        // reason RPC is: a node that negotiates a protocol it cannot forward
        // turns a closed dial into a hang.
        if let Some(origin) = media_origin {
            alpns.push(MEDIA_ALPN.to_vec());
            tracing::info!(
                target: "transport",
                media_origin = %origin,
                "iroh(mesh): serving MEDIA_ALPN to members — a peer's player reaches this \
                 origin by mesh key, with no port forwarded and no VPN"
            );
        }
        // A guest is a different principal from a peer, so it gets a different
        // protocol — forwarded to the daemon's SECOND bind of the client
        // router, the one whose auth layer does not treat this acceptor's
        // loopback forward hop as proof of a local caller. Advertised only
        // when that listener exists, so a dial on GUEST_ALPN either reaches a
        // credential-checking listener or is refused at the handshake; it can
        // never fall through to the trusting one.
        if guest_addr.is_some() {
            alpns.push(GUEST_ALPN.to_vec());
        }
        let endpoint = match build_mesh_endpoint(secret, alpns, relay_cfg).await {
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
        if let Some(rpc_addr) = rpc_forward {
            tracing::info!(
                target: "transport",
                rpc_forward = %rpc_addr,
                "iroh(mesh): accepting RPC_ALPN → local ggml rpc-server (MEMBERS ONLY)"
            );
        }
        if let Some(guest) = guest_addr {
            tracing::info!(
                target: "transport",
                guest_forward = %guest,
                "iroh(mesh): accepting GUEST_ALPN → bearer-only client listener"
            );
        }
        match peer_addr {
            Some(peer) => tracing::info!(
                target: "transport",
                peer_forward = %peer,
                "iroh(mesh): accepting CLIENT_ALPN from members → peer listener \
                 (client router minus /internal/*)"
            ),
            None => tracing::warn!(
                target: "transport",
                "iroh(mesh): no peer listener bound — CLIENT_ALPN dials from MEMBERS \
                 will be closed (federated inference from peers is off)"
            ),
        }
        let routes = AcceptorRoutes {
            internal: internal_addr,
            peer: peer_addr,
            guest: guest_addr,
            rpc: rpc_forward,
            media: media_origin,
            media_allow: std::sync::Arc::new(media_allow),
            media_declared: std::sync::Arc::new(commonwealth_media::read_declared_in(
                &commonwealth_media::dir_under(data_dir),
            )),
        };
        let acceptor =
            IrohAcceptor::spawn_admitting_forward(endpoint.clone(), move |alpn, dialer| {
                let is_member = member_check.clone();
                let routes = routes.clone();
                async move { routes.forward_for(&alpn, dialer, &is_member).await }
            });

        tracing::info!(
            endpoint_id = %endpoint.id(),
            internal_forward = %internal_addr,
            peer_forward = ?peer_addr,
            dial = %Self::dial_for(&endpoint).unwrap_or_else(|| "<no relay yet>".to_string()),
            "iroh(mesh): dial-by-key access enabled \
             (ALPN cwth/http/0 -> internal, cwth/client/0 -> peer)"
        );
        Some(MeshIrohAccess {
            endpoint,
            _acceptor: acceptor,
            rpc_route_active: rpc_forward.is_some(),
            media_route_active: media_origin.is_some(),
        })
    }

    /// Whether this acceptor routes [`RPC_ALPN`] to a local ggml
    /// rpc-server. The daemon copies this into `AppState` so `/status`
    /// advertises `rpc_worker.iroh` only when it is genuinely true.
    pub fn rpc_route_active(&self) -> bool {
        self.rpc_route_active
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
        let origins = if self.media_route_active {
            vec![commonwealth_core::capabilities::OriginKind::Media]
        } else {
            Vec::new()
        };
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
                origins: origins.clone(),
            }
        })
    }

    /// Public form of [`Self::dial_for`] for callers holding a cloned
    /// [`Endpoint`] handle (see [`Self::endpoint_handle`]) instead of
    /// the access struct — e.g. `current_invite`'s live dial read.
    pub fn dial_for_endpoint(endpoint: &Endpoint) -> Option<String> {
        Self::dial_for(endpoint)
    }

    /// The pairing string a peer would dial: `id@relay1,relay2,addr…`
    /// carrying ALL current relays + direct addrs (via [`format_dial_string`]).
    /// `None` before any reachable address is known.
    ///
    /// Emitting every relay (not just `relay_urls().next()`) is deliberate: per
    /// iroh's `connect` contract a *present-but-stale* pinned relay suppresses
    /// the pkarr discovery fallback, so a single dead relay baked into an invite
    /// would wedge the dial (the founder-unreachable bug this hardening targets).
    /// Listing all relays + direct addrs means no one stale target is fatal — the
    /// joiner races them and n0 discovery still corrects the address by node-id.
    fn dial_for(endpoint: &Endpoint) -> Option<String> {
        format_dial_string(&endpoint.addr())
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

    /// This node's iroh endpoint id (hex) — for the reachability status surface.
    pub fn endpoint_id(&self) -> String {
        self.endpoint.id().to_string()
    }

    /// The live connection path to `peer_pubkey` over iroh — the "is anyone
    /// actually on the relay?" question. Takes an [`Endpoint`] handle (see
    /// [`Self::endpoint_handle`]) so the caller needn't hold the daemon-state
    /// lock across the await. The reading itself is
    /// `commonwealth_media::path_to`, which is the one conversion from the
    /// transport's snapshot the operator surface and the health term share.
    pub async fn peer_path_on(
        endpoint: &Endpoint,
        peer_pubkey: &[u8; 32],
    ) -> Option<PeerTransportPath> {
        commonwealth_media::path_to(endpoint, peer_pubkey).await
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

    /// Every configurable class opted out leaves exactly the one that has no
    /// entry: `Media` has one transport, so there is nothing to opt it out
    /// TO. (Until 2026-09-11 this asserted `is_empty()` over seven classes.)
    #[test]
    fn opting_every_class_out_still_routes_media() {
        let out = iroh_routed_classes(&section(|t| {
            t.gossip = Some("ip".into());
            t.control_plane = Some("ip".into());
            t.knowledge_search = Some("ip".into());
            t.model_transfer = Some("ip".into());
            t.inference = Some("ip".into());
            t.status_probe = Some("ip".into());
            t.rpc_tensor = Some("ip".into());
        }));
        assert_eq!(out, vec![TrafficClass::Media]);
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

    /// The wire spelling `PeerTransportPath.path` carries is the classifier's
    /// own `as_str`, not a second vocabulary maintained here. The classifier's
    /// state table is tested where it lives (`commonwealth_transport::iroh`);
    /// this guards the ADAPTER — that the DTO keeps speaking those words.
    #[test]
    fn wire_path_spelling_comes_from_the_shared_classifier() {
        use commonwealth_transport::iroh::PeerPath;
        assert_eq!(PeerPath::Mixed.as_str(), "mixed");
        assert_eq!(PeerPath::Direct.as_str(), "direct");
        assert_eq!(PeerPath::Relayed.as_str(), "relayed");
        assert_eq!(PeerPath::Idle.as_str(), "idle");
        assert_eq!(PeerPath::Unknown.as_str(), "unknown");
    }

    #[test]
    fn resolve_enabled_matrix() {
        use crate::local_only::LocalOnlyProfile;
        let net = LocalOnlyProfile::default();
        // Explicit config wins over the participation marker…
        assert!(resolve_enabled(net, Some(true), false, false));
        assert!(!resolve_enabled(net, Some(false), true, false));
        // …absent config defers to mesh participation…
        assert!(resolve_enabled(net, None, true, false));
        assert!(!resolve_enabled(net, None, false, false));
        // …and require_encryption overrides those two, including an
        // explicit opt-out (an encrypted mesh cannot run without iroh).
        assert!(resolve_enabled(net, Some(false), false, true));
        assert!(resolve_enabled(net, None, false, true));
    }

    /// The local-only profile outranks every other input, including the
    /// explicit `[iroh] enabled = true` that beats participation, and
    /// including `require_encryption` — which cannot co-occur, because
    /// `start_daemon` refuses that pair before reaching here.
    #[test]
    fn local_only_profile_beats_every_other_iroh_input() {
        use crate::local_only::LocalOnlyProfile;
        let local = LocalOnlyProfile::decide(None, true);
        assert!(local.is_local_only());
        for cfg in [None, Some(true), Some(false)] {
            for participant in [true, false] {
                for encrypted in [true, false] {
                    assert!(
                        !resolve_enabled(local, cfg, participant, encrypted),
                        "local-only must keep iroh off for \
                         cfg={cfg:?} participant={participant} encrypted={encrypted}"
                    );
                }
            }
        }
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

    // ── who a dial reaches ──────────────────────────────────────────
    //
    // The dial string that reaches this endpoint is PUBLIC — it rides in
    // every invite's `dial=` and is gossiped as `node_pubkey`. These pin the
    // decision table that keeps holding it from being a credential.

    use commonwealth_core::ids::NodePubkey;

    const MEMBER: NodePubkey = NodePubkey([7u8; 32]);
    const STRANGER: NodePubkey = NodePubkey([9u8; 32]);

    fn member_identity() -> MemberIdentity {
        MemberIdentity {
            name: "LittleMac".into(),
            node_id: commonwealth_core::ids::NodeId::from_u128(0xB0B),
        }
    }

    fn only_the_member() -> MemberCheck {
        std::sync::Arc::new(|k| Box::pin(std::future::ready((k == MEMBER).then(member_identity))))
    }

    /// `forward_for` as a bare address, for the arms whose kind is a splice.
    async fn target_of(
        r: &AcceptorRoutes,
        alpn: &[u8],
        who: NodePubkey,
        check: &MemberCheck,
    ) -> Option<SocketAddr> {
        r.forward_for(alpn, who, check).await.map(Forward::target)
    }

    fn addr(port: u16) -> SocketAddr {
        ([127, 0, 0, 1], port).into()
    }

    fn routes() -> AcceptorRoutes {
        AcceptorRoutes {
            internal: addr(9742),
            peer: Some(addr(41001)),
            guest: Some(addr(41000)),
            rpc: Some(addr(50052)),
            media: Some(addr(8096)),
            media_allow: std::sync::Arc::new(Vec::new()),
            // Nothing declared: these cases are about WHO is admitted,
            // not what the holder adds on the way to its own origin.
            media_declared: std::sync::Arc::new(Vec::new()),
        }
    }

    /// THE fix. A stranger holding the dial string must not land on the
    /// listener that admits loopback — it lands on the one that reads a
    /// bearer, which is what a LAN caller would meet.
    #[tokio::test]
    async fn a_stranger_on_the_client_alpn_is_routed_to_the_bearer_gate() {
        let r = routes();
        assert_eq!(
            target_of(&r, CLIENT_ALPN, STRANGER, &only_the_member()).await,
            r.guest,
        );
    }

    /// And the arm that must keep working: a member presents no bearer at
    /// all on federated inference, and its key is the credential.
    ///
    /// It reaches the PEER listener, never the operator's own. Both admit a
    /// loopback caller, so the auth layer cannot tell them apart — the
    /// difference is that the peer bind does not SERVE `/internal/*`.
    #[tokio::test]
    async fn a_member_on_the_client_alpn_reaches_the_peer_listener_not_the_operators() {
        let r = routes();
        assert_eq!(
            target_of(&r, CLIENT_ALPN, MEMBER, &only_the_member()).await,
            r.peer,
        );
    }

    /// Fail CLOSED on the other side too. With no peer listener there is
    /// nothing a member may safely reach: the operator's `:9741` bind admits
    /// this acceptor's forward hop as a local caller and serves guest-grant
    /// minting to it, so it is not a fallback.
    #[tokio::test]
    async fn a_member_is_closed_rather_than_promoted_when_the_peer_listener_is_absent() {
        let r = AcceptorRoutes {
            peer: None,
            ..routes()
        };
        assert_eq!(
            target_of(&r, CLIENT_ALPN, MEMBER, &only_the_member()).await,
            None,
        );
        // The stranger arm is unaffected — it was never routed to `peer`.
        assert_eq!(
            target_of(&r, CLIENT_ALPN, STRANGER, &only_the_member()).await,
            r.guest,
        );
    }

    /// Fail CLOSED. With no bearer-checking listener there is nothing safe to
    /// downgrade a stranger to, and the trusting listener is not a fallback.
    #[tokio::test]
    async fn a_stranger_is_closed_rather_than_promoted_when_the_guest_listener_is_absent() {
        let r = AcceptorRoutes {
            guest: None,
            ..routes()
        };
        assert_eq!(
            target_of(&r, CLIENT_ALPN, STRANGER, &only_the_member()).await,
            None,
        );
        // The member arm is unaffected — this is not "refuse everything".
        assert_eq!(
            target_of(&r, CLIENT_ALPN, MEMBER, &only_the_member()).await,
            r.peer,
        );
    }

    /// The rpc-server speaks tensor operations and authenticates nothing.
    /// There is no listener to downgrade to, so a stranger is refused.
    #[tokio::test]
    async fn the_rpc_alpn_admits_members_only_and_has_no_downgrade() {
        let r = routes();
        assert_eq!(
            target_of(&r, RPC_ALPN, MEMBER, &only_the_member()).await,
            r.rpc,
        );
        assert_eq!(
            target_of(&r, RPC_ALPN, STRANGER, &only_the_member()).await,
            None,
        );
    }

    /// **A MEDIA LIBRARY IS NOT A PUBLIC SURFACE.** The origin is somebody's
    /// Jellyfin on loopback and it authenticates nothing, so — exactly like the
    /// rpc-server — a member is forwarded and a stranger is refused rather than
    /// downgraded to some other listener. The failing input is this arm
    /// returning `self.media` unconditionally, which is how a dial string that
    /// rides in every invite would become a key to the operator's films.
    #[tokio::test]
    async fn the_media_alpn_admits_members_only_and_has_no_downgrade() {
        let r = routes();
        // A member's media dial is the identity-carrying kind: the origin is
        // told who is asking, and told by the acceptor rather than the client.
        let forward = r
            .forward_for(MEDIA_ALPN, MEMBER, &only_the_member())
            .await
            .expect("a member reaches the origin");
        match &forward {
            Forward::Http { origin, headers } => {
                assert_eq!(Some(*origin), r.media);
                assert!(
                    headers.contains(&("X-Mesh-Member".to_string(), "LittleMac".to_string())),
                    "{headers:?}"
                );
                assert!(
                    headers.iter().any(|(n, _)| n == "X-Mesh-Node"),
                    "{headers:?}"
                );
            }
            other => panic!("a media dial must be an identity forward, got {other:?}"),
        }
        assert!(
            r.media.is_some(),
            "the fixture must actually serve media, or the assertion above passes on a shared None"
        );
        assert_eq!(
            target_of(&r, MEDIA_ALPN, STRANGER, &only_the_member()).await,
            None,
        );

        // A node that declares no origin advertises nothing and closes even a
        // member's dial — no route to nowhere.
        let unserved = AcceptorRoutes {
            media: None,
            ..routes()
        };
        assert_eq!(
            target_of(&unserved, MEDIA_ALPN, MEMBER, &only_the_member()).await,
            None,
        );
    }

    /// A guest is by definition not a member; the listener behind this ALPN
    /// reads its bearer, so the dialer's key decides nothing here.
    /// `[iroh] media_allow` narrows WHICH members reach the origin, by the
    /// name or id prefix the roster shows. The failing input is a member
    /// outside the list being forwarded; the control is the same member
    /// inside it, and an empty list admitting every member as before.
    #[tokio::test]
    async fn the_media_allow_list_admits_by_name_or_id_prefix_and_refuses_the_rest() {
        let allow = |entries: &[&str]| AcceptorRoutes {
            media_allow: std::sync::Arc::new(entries.iter().map(|s| s.to_string()).collect()),
            // Nothing declared: these cases are about WHO is admitted,
            // not what the holder adds on the way to its own origin.
            media_declared: std::sync::Arc::new(Vec::new()),
            ..routes()
        };
        assert!(
            allow(&["SomeoneElse"])
                .forward_for(MEDIA_ALPN, MEMBER, &only_the_member())
                .await
                .is_none(),
            "a member the list does not name is closed"
        );
        assert!(allow(&["LittleMac"])
            .forward_for(MEDIA_ALPN, MEMBER, &only_the_member())
            .await
            .is_some());
        let id_prefix = member_identity().node_id.to_string();
        assert!(allow(&[&id_prefix[..12]])
            .forward_for(MEDIA_ALPN, MEMBER, &only_the_member())
            .await
            .is_some());
        assert!(allow(&[])
            .forward_for(MEDIA_ALPN, MEMBER, &only_the_member())
            .await
            .is_some());
        // The list never widens: a stranger stays closed however it is named.
        assert!(allow(&["LittleMac"])
            .forward_for(MEDIA_ALPN, STRANGER, &only_the_member())
            .await
            .is_none());
    }

    #[tokio::test]
    async fn the_guest_alpn_admits_any_dialer_because_its_listener_checks() {
        let r = routes();
        assert_eq!(
            target_of(&r, GUEST_ALPN, STRANGER, &only_the_member()).await,
            r.guest,
        );
    }

    /// Deliberate, and the reason it is deliberate is load-bearing: a JOINING
    /// node is not a member yet, and `/internal/join` is how it stops being a
    /// stranger. Gating this would make the mesh unjoinable over its own
    /// transport.
    #[tokio::test]
    async fn the_internal_alpn_stays_open_so_a_joiner_can_become_a_member() {
        let r = routes();
        assert_eq!(
            target_of(&r, ALPN, STRANGER, &only_the_member()).await,
            Some(r.internal),
        );
    }

    /// An ALPN nobody routes is closed, never sent to whatever is nearest.
    #[tokio::test]
    async fn an_unknown_alpn_is_closed() {
        let r = routes();
        assert_eq!(
            target_of(&r, b"cwth/not-a-protocol/0", MEMBER, &only_the_member()).await,
            None,
        );
    }

    /// The safe default: a host that never wired a real check treats every
    /// dialer as a stranger. Forgetting cannot widen access.
    #[tokio::test]
    async fn the_default_check_admits_no_one() {
        let r = routes();
        assert_eq!(
            target_of(&r, CLIENT_ALPN, MEMBER, &admits_no_one()).await,
            r.guest,
        );
        assert_eq!(
            target_of(&r, RPC_ALPN, MEMBER, &admits_no_one()).await,
            None,
        );
    }
}
