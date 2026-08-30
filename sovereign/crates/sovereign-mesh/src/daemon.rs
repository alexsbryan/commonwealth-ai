// SPDX-License-Identifier: AGPL-3.0-or-later
//! Embedded Commonwealth daemon lifecycle management.
//!
//! The daemon runs in-process within Sovereign — no separate binary needed.
//! It starts when the user creates or joins a mesh, and stops when they leave.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use tokio::sync::RwLock;
use tracing::{info, warn};

use commonwealth_api::state::{AppState, LocalInferenceService};
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::Mesh;
use commonwealth_discovery::mdns::{BrowseHandle, DiscoveredPeer, MdnsDiscovery};
use commonwealth_discovery::membership;
use corpus_engine::CorpusEngine;
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::{InferenceProvider, StateStore};

/// Short-lived TTL stamped into an ENCRYPTED mesh's invite link. The
/// founder enforces it at the join handler, so a leaked link is useless
/// after this window. 24h balances "share in a group chat, everyone
/// joins today" against replay exposure; multiple joiners are fine
/// within the window (TTL, not single-use).
const INVITE_TTL_SECS: u64 = 24 * 60 * 60;

/// The internal-router listener bind address. Under an ENCRYPTED mesh
/// (WS-C receiver lockout) it is loopback-only — the iroh acceptor,
/// which forwards to this loopback listener, is the sole network path
/// in (including for `/internal/join`), so a plaintext LAN caller is
/// refused. A plaintext mesh keeps the historical `0.0.0.0` bind.
fn internal_bind_addr(
    require_encryption: bool,
    internal_bind: &str,
    internal_port: u16,
) -> std::net::SocketAddr {
    // Encryption forces loopback regardless of the configured interface:
    // the iroh acceptor is the sole network ingress on an encrypted mesh.
    let host = if require_encryption {
        "127.0.0.1"
    } else {
        internal_bind
    };
    format!("{host}:{internal_port}")
        .parse()
        .unwrap_or_else(|_| {
            warn!("invalid [daemon] internal_bind '{internal_bind}'; falling back to 0.0.0.0");
            format!("0.0.0.0:{internal_port}")
                .parse()
                .expect("0.0.0.0 bind addr is always valid")
        })
}

/// Effective mDNS-on decision: the `[discovery] mdns` config flag, with
/// `SOVEREIGN_DISABLE_MDNS` (`=1`/`=true`) as a force-off override for
/// container/VPC deploys whose network namespace can't bind the multicast
/// socket. Config-on + env-unset reproduces the historical behaviour.
fn mdns_enabled_effective(cfg_mdns: bool) -> bool {
    let env_force_off = std::env::var("SOVEREIGN_DISABLE_MDNS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    cfg_mdns && !env_force_off
}

use crate::admin_http::ConfigDiff;
use crate::daemon_services::DaemonServices;
use crate::deep_link::DeepLink;
use crate::gossip::{self, GossipHandle};
use crate::mcp_router;
use crate::mesh_discovery::{local_ip_candidates, reachable_addresses};
use crate::persist;
use crate::state::MeshState;

/// The embedded Commonwealth daemon — the ONE daemon implementation, shared
/// by `sovereign daemon run`, the desktop's Local mode, and `svrn mesh`.
///
/// **Everything a host supplies arrives as one value.** Until 2026-08-24 this
/// struct carried 17 `RwLock<Option<T>>` slots punched in afterwards by 10
/// `set_*` and 7 `install_*_router` methods; a slot a host forgot was
/// indistinguishable from a slot a host deliberately declined, and the route
/// behind it silently 404'd. [`DaemonServices`] replaces all seventeen — see
/// `daemon_services`'s module docs for the pair-independence pass
/// (`quality/TOPOLOGY.md` §4) that produced its three variants.
///
/// The two `RwLock<Option<…>>` that remain are **derived from the variant, not
/// settable by a host**: both are seeded at construction and mutated only by
/// `POST /v1/admin/reload`, which is itself reachable only on the variant that
/// carries a `ProviderFactory`.
pub struct EmbeddedDaemon {
    state: Arc<RwLock<DaemonState>>,
    /// Where to persist `mesh.json` so the daemon can auto-resume on
    /// app restart. Empty means persistence is off (the in-memory
    /// test constructor).
    data_dir: PathBuf,
    /// This daemon's own `Arc`, captured by `Arc::new_cyclic` at
    /// construction. It is what lets `start_daemon` build the three routers
    /// that are pure functions of the daemon — mesh, admin, reading — instead
    /// of accepting them from a host that might not pass them. A serving
    /// daemon therefore cannot be missing its own control surface.
    self_weak: Weak<EmbeddedDaemon>,
    /// What this host is and everything it supplies. Immutable for the
    /// daemon's lifetime.
    services: DaemonServices,
    /// The config this daemon booted with. **Not** an `Option`:
    /// `SetupConfig::unconfigured()` is byte-identical to every fallback this file
    /// used to apply when the slot was `None` (`9741`/`9742`, loopback client
    /// bind, `0.0.0.0` internal bind, no token), and `register_local_model_slots`
    /// skips empty paths — so "no config installed" was never a distinct state,
    /// only an unnamed one. `admin_http::reload` diffs the file on disk against
    /// this value and advances it on success.
    setup_config: RwLock<SetupConfig>,
    /// The provider that answers peer chat completions on
    /// `/v1/chat/completions`. Seeded from [`Self::services`]; swapped in place
    /// by `reload_from_setup_config` when `models.*` changes on disk, which is
    /// why it is behind a lock rather than living in the variant. `None`
    /// exactly on [`DaemonServices::MeshAdmin`], which has no inference role —
    /// never because a host forgot to install one.
    inference_provider: RwLock<Option<Arc<dyn InferenceProvider>>>,
    /// Cached plaintext of the active mesh's join key, mirroring
    /// `<data_dir>/join_key.secret`. The hash is one-way, so without
    /// this the share UI couldn't render the invite link after the
    /// app restarts. Genuine runtime state: set on `create_mesh` /
    /// `join_mesh` / `try_resume`; refreshed on `set_join_key` (called
    /// by the rotate handler); cleared on `stop`.
    join_key_plaintext: RwLock<Option<String>>,
    /// Endpoint→NodeId directory for discovered RPC workers: which mesh
    /// member owns each raw `ip:port` ggml-RPC endpoint. Written by
    /// [`Self::discover_rpc_workers`] at the moment the endpoint string is
    /// derived — the one place identity and endpoint meet before identity
    /// is dropped into the bare-string RPC layer. Read by the warm
    /// orchestrator (`rpc_warm_http`) to resolve a worker's mesh transport
    /// (iroh bridge on an encrypted mesh) instead of reverse-parsing an IP
    /// from the endpoint string. Entries are never pruned: resolution
    /// re-reads the live membership, so a mapping for a vanished worker is
    /// inert. `std::sync` lock — never held across an await.
    rpc_endpoint_nodes: std::sync::RwLock<std::collections::HashMap<String, NodeId>>,
    /// Per-node sticky endpoint choice for RPC-worker discovery — the hysteresis
    /// state that stops a single transient direct-ip probe miss from flipping a
    /// worker's transport identity (direct-ip ↔ iroh-bridge loopback). Both the
    /// eligibility tracker and the reload loop key on the endpoint the discovery
    /// tick returns, so an unheld flip reads as a flap + full re-settle and
    /// collapses a live distribution to local-only (observed 2026-07-19, 122B
    /// e2e). Keyed by the worker's stable mesh node_id. `std::sync` lock — never
    /// held across an await (read to a clone, decide, write the result).
    rpc_worker_sticky: std::sync::RwLock<std::collections::HashMap<NodeId, StickyEndpoint>>,
    /// Peers we have EVER confirmed an RPC worker on, and when.
    ///
    /// Independent of `rpc_worker_sticky` on purpose. That map's hold budget is
    /// about endpoint STABILITY and it drops a bridged worker on its first miss
    /// (`sticky_endpoint`), so using it as the "do we know this peer?" set would
    /// make an unconfirmed hold last exactly one tick — useless for the bridged,
    /// multi-tick starvation that is the actual 2026-07-28 incident. This map
    /// answers a different question: have we ever seen a worker here, so that an
    /// unanswered probe is reportable as `unconfirmed` rather than as absence.
    rpc_worker_last_seen: std::sync::RwLock<std::collections::HashMap<NodeId, std::time::Instant>>,
}

enum DaemonState {
    Stopped,
    Running {
        #[allow(dead_code)]
        app_state: AppState,
        mesh_state: Arc<RwLock<MeshState>>,
        client_addr: SocketAddr,
        /// Live mDNS advertiser + discovery — kept to drive
        /// `discovered_peers()` and (in Phase B) the join handshake.
        /// `None` when mDNS is disabled (`[discovery] mdns = false` /
        /// `SOVEREIGN_DISABLE_MDNS`) — the daemon then forms the mesh from
        /// static seeds only and never advertises/browses.
        mdns: Option<Arc<MdnsDiscovery>>,
        /// Dropping this handle stops the background browse task.
        /// Underscore-prefixed because it's held purely for its Drop
        /// impl. `None` when mDNS is disabled (no browse task to stop).
        _browse_handle: Option<BrowseHandle>,
        /// Aborts the gossip heartbeat loop on Drop. Same pattern
        /// as `_browse_handle` — tying the task's lifetime to the
        /// Running variant means stopping the daemon also stops
        /// gossip; no explicit teardown.
        _gossip_handle: GossipHandle,
        /// Aborts the peer-assisted ingest handoff loop on Drop. Same pattern
        /// as `_gossip_handle`, and held for the same second reason the gossip
        /// one is not: a spawner that returns nothing can lose its
        /// `tokio::spawn` in a stray three-line diff and stay silent about it
        /// for five weeks (`ec7ca66c`, 2026-07-21 — see
        /// `auto_ingest::CollaborateHandle`).
        _collaborate_handle: crate::auto_ingest::CollaborateHandle,
        _shutdown_tx: tokio::sync::oneshot::Sender<()>,
        /// The API-server task that owns the `:9741`/`:9742` listeners.
        /// Kept (not discarded) so `stop_inner` can await its exit after
        /// dropping `_shutdown_tx`, guaranteeing the listeners are fully
        /// released before an in-process re-create (`leave_to_solo`)
        /// rebinds the same ports — otherwise the rebind races the
        /// still-`LISTEN`ing socket and hits EADDRINUSE.
        serve_handle: tokio::task::JoinHandle<()>,
        /// Server-half iroh endpoint + acceptor (Track W, W1 — see
        /// `crate::iroh_access`). `None` unless iroh is enabled
        /// (explicit config or mesh participation). Read live by
        /// invite generation (`create_mesh_with` / `current_invite`)
        /// for the dial string; its Drop ties the acceptor to the
        /// Running variant, so leaving the mesh / stopping the daemon
        /// also stops accepting dial-by-key traffic, same pattern as
        /// `_browse_handle`.
        iroh_access: Option<crate::iroh_access::MeshIrohAccess>,
        /// Founder reachability watchdog (Track W hardening): polls relay-home +
        /// self-discovery health and self-heals (nudge → relay bounce → endpoint
        /// rebuild) with no daemon restart. `None` when iroh is disabled. Aborts
        /// its task on Drop (tied to the Running variant, like `_gossip_handle`);
        /// also read by `self_reachability()` for the status surface.
        reachability_watchdog: Option<crate::iroh_watchdog::WatchdogHandle>,
    },
}

/// (Re)install a built `MeshIrohAccess` into `app_state`: publish this node's
/// dial info for the gossip self-stamp (W2), and — when any traffic class routes
/// over iroh — (re)install the `RoutedTransport` that dials from this endpoint
/// (W3). Both installs are RwLock-based and re-runnable at runtime, which is what
/// lets the reachability watchdog swap in a fresh endpoint without a daemon
/// restart. Called by `start_daemon` and by the watchdog's rebuild closure.
pub(crate) fn install_iroh_access(
    app_state: &AppState,
    access: &crate::iroh_access::MeshIrohAccess,
    iroh_routed_classes: &[commonwealth_transport::TrafficClass],
    iroh_required_classes: &std::collections::HashSet<commonwealth_transport::TrafficClass>,
    ip_transport: &Arc<dyn commonwealth_transport::PeerTransport>,
    require_encryption: bool,
) {
    app_state.install_self_iroh_dialinfo(access.dial_info_provider());
    app_state.set_rpc_iroh_accept(access.rpc_route_active());
    if !iroh_routed_classes.is_empty() {
        let iroh_t: Arc<dyn commonwealth_transport::PeerTransport> =
            Arc::new(access.client_transport());
        let mut per_class = std::collections::HashMap::new();
        for class in iroh_routed_classes {
            per_class.insert(*class, iroh_t.clone());
        }
        app_state.install_peer_transport(Arc::new(
            commonwealth_transport::RoutedTransport::with_required(
                per_class,
                ip_transport.clone(),
                iroh_required_classes.clone(),
            ),
        ));
        info!(
            routed = ?iroh_routed_classes.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            required = ?iroh_required_classes.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            require_encryption,
            "iroh(mesh): routing classes over iroh (required classes have NO \
             plaintext fallback; dial fails closed if a peer has no encrypted path)"
        );
    }
}

/// Distinguishes "user wants to leave the mesh" from "process is
/// being shut down gracefully". Both stop the in-memory daemon, but
/// only Leave wipes the on-disk persistence — Shutdown preserves it
/// so the next launch resumes into the same mesh.
#[derive(Debug, Clone, Copy)]
enum StopMode {
    Leave,
    Shutdown,
    /// Set this mesh down without giving it up: persistence is preserved
    /// exactly as `Shutdown` does, and the caller announces `Offline` (never a
    /// `removed_at` tombstone) so peers see us step away rather than depart.
    /// The listeners are dropped so the next mesh can rebind them.
    Park,
}

/// Result of creating a new mesh.
pub struct CreateMeshResult {
    pub mesh_name: String,
    pub join_key: String,
    pub join_link: String,
    /// The client-API bearer token a joining peer / remote client must
    /// present, surfaced beside the join key on the invite screen.
    /// `Some` once the daemon is exposed (bound non-loopback); `None`
    /// for a loopback-only daemon (no remote access, no token).
    pub client_token: Option<String>,
}

/// One peer's live iroh connection path (H2 observability). `path` is
/// `None` when the endpoint has no record of this peer yet.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IrohPeerPath {
    pub node_id: NodeId,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub path: Option<crate::iroh_access::PeerTransportPath>,
}

/// The founder's OWN iroh reachability (Track W hardening), for
/// `/v1/mesh/status.self_reachability` and the desktop "Reachable /
/// Reconnecting" indicator. Flattens the reachability watchdog's live health
/// snapshot so the wire object is one flat record.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SelfReachability {
    /// This node's current dial-by-key string (all relays + direct addrs), or
    /// `None` before any reachable address is known.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dial: Option<String>,
    /// iroh endpoint id (hex).
    pub endpoint_id: String,
    /// Live watchdog health: relay-homed, discovery probe, recovery history.
    #[serde(flatten)]
    pub health: crate::iroh_watchdog::ReachabilityStatus,
}

/// Result of joining an existing mesh.
pub struct JoinMeshResult {
    pub mesh_name: String,
    pub node_id: String,
    /// This node's own client-API token once exposed — so the joiner
    /// can in turn admit further peers/clients. See `CreateMeshResult`.
    pub client_token: Option<String>,
}

impl EmbeddedDaemon {
    /// The only constructor. **Total**: the host names which of the three
    /// live shapes it is and supplies everything that shape needs, in one
    /// value, before the daemon exists. There is no window in which a request
    /// can observe a half-wired daemon, and no slot a host can forget.
    ///
    /// `data_dir` is where `mesh.json` is persisted so the daemon can
    /// auto-resume on restart; an empty path disables persistence (see
    /// [`Self::in_memory`]). Call [`try_resume`](Self::try_resume) once at
    /// app start to re-attach to a previously-created mesh.
    ///
    /// Returns an `Arc` because construction goes through `Arc::new_cyclic`:
    /// the daemon keeps a `Weak` to itself so it can build its own mesh,
    /// admin and reading routers at start. Those three used to be installed
    /// by each host — and the desktop installed a different subset from the
    /// CLI daemon, which is exactly the divergence this constructor retires.
    pub fn new(
        data_dir: PathBuf,
        setup_config: SetupConfig,
        services: DaemonServices,
    ) -> Arc<Self> {
        let provider = services
            .serving()
            .map(|s| Arc::clone(&s.core.inference_provider));

        // Answer the variant question ONCE, at the top.
        //
        // This block used to ask it SEVEN times through seven `Option`-
        // returning accessors. `DaemonServices` exposed ten; two are real
        // forks (`serving`, `rails`) and eight are artifactual,
        // wrapping fields that are not optional one level down
        // (`quality/TOPOLOGY.md §10`). Reading through an artifactual one means
        // a click lands on `.map()` rather than on a struct field — and worse,
        // it STACKS a meaningless outer `Option` on top of the genuinely
        // meaningful inner absence that `McpSurface` and `EmbedAdvertisement`
        // carry with a reason (§18.3). Matching once leaves only the absences
        // that mean something.
        match services.serving() {
            // `MeshAdmin` has no serving role at all. That emptiness is the
            // shape, not a set of holes.
            None => info!(
                profile = services.label(),
                "daemon: commissioned with no serving role"
            ),
            Some(serving) => {
                info!(
                    profile = services.label(),
                    host_routers = services.host_routers().len(),
                    // Structural, not probed. `ServingCore` holds both as plain
                    // `Arc`s, so a serving daemon cannot lack either — the old
                    // `.is_some()` pair could never report false here, which is
                    // exactly what made them read as checks rather than facts.
                    corpus_engine = true,
                    inference = true,
                    // Structural since Phase 3 as well: the crossing this line
                    // used to report (`Desktop` has a store, `Headless` does
                    // not) is closed, so every serving daemon has one and the
                    // `.is_some()` here could no longer report false either.
                    state_store = true,
                    "daemon: commissioned"
                );
                match &serving.capability.mcp {
                    crate::daemon_services::McpSurface::Mounted(m) => {
                        info!(tools = m.tools.count(), "daemon: /mcp will be mounted");
                    }
                    crate::daemon_services::McpSurface::Unavailable { reason } => {
                        warn!(%reason, "daemon: /mcp NOT mounted");
                    }
                }
                if let crate::daemon_services::EmbedAdvertisement::Unavailable { reason } =
                    &serving.advertise_embed
                {
                    warn!(
                        %reason,
                        "daemon: no embed model advertised — peers will NOT route \
                         collaborative ingestion to this node"
                    );
                }
            }
        }
        Arc::new_cyclic(|self_weak| Self {
            state: Arc::new(RwLock::new(DaemonState::Stopped)),
            data_dir,
            self_weak: self_weak.clone(),
            services,
            setup_config: RwLock::new(setup_config),
            inference_provider: RwLock::new(provider),
            join_key_plaintext: RwLock::new(None),
            rpc_endpoint_nodes: std::sync::RwLock::new(std::collections::HashMap::new()),
            rpc_worker_sticky: std::sync::RwLock::new(std::collections::HashMap::new()),
            rpc_worker_last_seen: std::sync::RwLock::new(std::collections::HashMap::new()),
        })
    }

    /// A daemon with persistence disabled — no `mesh.json` is written and
    /// `try_resume` always answers `false`. Tests that don't want to set up a
    /// tempdir; production code uses [`Self::new`] with a real `data_dir`.
    pub fn in_memory(setup_config: SetupConfig, services: DaemonServices) -> Arc<Self> {
        Self::new(PathBuf::new(), setup_config, services)
    }

    /// What this daemon is and what its host gave it. Read by
    /// `start_daemon`, by the HTTP routers, and by `/status`.
    pub fn services(&self) -> &DaemonServices {
        &self.services
    }

    /// Resolve the `(client_port, internal_port)` pair this daemon should
    /// bind and advertise, from the config it was commissioned with.
    ///
    /// Use this in every place that previously hardcoded 9741 or 9742 for
    /// *this* daemon's binding decisions: `create_mesh`, `join_mesh`,
    /// `start_daemon`'s listener bind, the mDNS announce, and the
    /// auto-collaborate loop's spawn.
    ///
    /// **Scope note (peer-side uniformity).** The peer-targeting rewrites in
    /// `peer_inference_endpoints` and `auto_ingest`'s candidate-URL builder
    /// still assume every peer uses the same port pair as this daemon — they
    /// apply `client_port` from `resolved_ports` to all peers uniformly.
    /// Mixed-port mesh deployments need a wire-protocol change (a
    /// `client_port` field on `MemberRecord`) and are tracked separately in
    /// §10.1.
    pub(crate) async fn resolved_ports(&self) -> (u16, u16) {
        let cfg = self.setup_config.read().await;
        (cfg.daemon.client_port, cfg.daemon.internal_port)
    }

    /// What kind of participant this node is, read LIVE from the daemon's
    /// `SetupConfig` rather than from a copy taken at boot.
    ///
    /// Derived on every read, deliberately (§7.5): the class is already a
    /// judgement over two config fields, and caching it here would make a
    /// third fact that can disagree with the two — exactly what
    /// `SetupConfig::node_class`'s own doc rules out. The read is a `RwLock`
    /// borrow of a struct the daemon already owns, so there is nothing to
    /// amortise.
    ///
    /// Surfaced on `GET /v1/mesh/status` because it answers a question the
    /// manifest cannot: after `build_self_manifest` began gating candidacy on
    /// residency, a terminal and a holder whose models failed to load BOTH
    /// advertise nothing, and "holds nothing by design" and "should hold
    /// something and does not" are different verdicts that must not collapse
    /// into one (§18.2).
    pub async fn node_class(&self) -> sovereign_core::setup_config::NodeClass {
        self.setup_config.read().await.node_class()
    }

    /// The entry node a `terminal` forwards to, or `None` on a holder.
    /// Reported beside [`node_class`](Self::node_class) so an operator reading
    /// "terminal" can see WHERE its turns go without opening `config.toml`.
    pub async fn entry_node(&self) -> Option<String> {
        self.setup_config.read().await.node.entry.clone()
    }

    /// Borrow the `CorpusEngine` this host commissioned the daemon with, if
    /// its variant carries one. `reading_http` and the knowledge handlers
    /// call this; `MeshAdmin` answers `None` by construction.
    pub fn corpus_engine(&self) -> Option<&Arc<CorpusEngine>> {
        self.services.serving().map(|s| &s.core.corpus_engine)
    }

    /// Borrow the `StateStore` the reading surface uses to resolve
    /// `conversation-history` chunks back to their conversation. `None` only
    /// on [`DaemonServices::MeshAdmin`], which serves nothing — since
    /// daemon-convergence Phase 3 BOTH serving variants own a store.
    pub fn state_store(&self) -> Option<&Arc<dyn StateStore>> {
        self.services.serving().map(|s| &s.core.state_store)
    }

    /// Borrow the `Runtime` this daemon serves turns with. `None` only on
    /// [`DaemonServices::MeshAdmin`] — the same real fork the two accessors
    /// above answer to, and the reason all three keep an `Option` where the
    /// seven deleted in Phase 2 did not: `serving()` is a genuine question,
    /// and the field one level down is not optional.
    pub fn runtime(&self) -> Option<&Arc<sovereign_core::runtime::Runtime>> {
        self.services.serving().map(|s| &s.core.runtime)
    }

    /// Swap the serving `InferenceProvider`. Private on purpose: the ONLY
    /// caller is `reload_from_setup_config`, which is itself reachable only
    /// on the variant that carries a `ProviderFactory`. A host cannot install
    /// a provider after construction — it names one in its
    /// [`DaemonServices`] or it has none.
    async fn swap_inference_provider(&self, provider: Arc<dyn InferenceProvider>) {
        *self.inference_provider.write().await = Some(provider);
    }

    /// Re-read `SetupConfig` from disk (or from `config_path_override`
    /// if supplied by a test), diff against the in-memory baseline,
    /// and apply whatever is hot-reloadable. Returns the per-field
    /// report the HTTP layer serialises as [`ReloadResponse`].
    ///
    /// Semantics:
    /// - `models.*` changes → rebuild the provider via
    ///   the variant's `ProviderFactory`, then swap atomically
    ///   through the private provider slot. In-flight requests
    ///   holding the old `Arc` continue against it; new ones see
    ///   the new provider.
    /// - `daemon.client_port` / `daemon.internal_port` / `data.dir`
    ///   changes → reported as `restart_required_fields`. The
    ///   handler doesn't rebind or reopen anything; rebinding while
    ///   serving requests risks losing them and reopening SQLite
    ///   handles mid-flight is unsafe.
    /// - Identical files → no-op, empty `reloaded_fields`.
    ///
    /// The baseline `SetupConfig` is advanced to the fresh value
    /// only when the reload succeeds end-to-end, so a provider
    /// rebuild failure leaves the daemon in its pre-reload state
    /// for a retry.
    pub async fn reload_from_setup_config(
        &self,
        config_path_override: Option<&Path>,
    ) -> Result<crate::admin_http::ReloadResponse, String> {
        let current = self.setup_config.read().await.clone();

        let fresh = match config_path_override {
            Some(p) => SetupConfig::load_from(p)?,
            None => SetupConfig::load()?,
        };

        let diff = ConfigDiff::diff(&current, &fresh);
        if diff.is_noop() {
            return Ok(crate::admin_http::ReloadResponse {
                reloaded_fields: vec![],
                restart_required_fields: vec![],
                restart_required: false,
            });
        }

        let mut reloaded: Vec<String> = vec![];

        if !diff.models_changed.is_empty() {
            // No factory means this variant cannot rebuild a provider — the
            // desktop's daemon has never carried one. Name the variant in
            // the refusal rather than reporting a missing installation
            // (ARCH §18.3): nothing is missing, this shape has no factory.
            let factory = self
                .services
                .rails()
                .map(|r| Arc::clone(&r.provider_factory))
                .ok_or_else(|| {
                    format!(
                        "models changed but the `{}` daemon profile carries no ProviderFactory — \
                     restart to apply model changes",
                        self.services.label()
                    )
                })?;
            let new_provider = factory.build_provider(&fresh).await?;
            self.swap_inference_provider(new_provider).await;
            for f in &diff.models_changed {
                reloaded.push((*f).to_string());
            }
            info!(
                changed = ?diff.models_changed,
                "admin_reload: inference provider swapped"
            );
        }

        let restart_required_fields: Vec<String> = diff
            .restart_required
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let restart_required = !restart_required_fields.is_empty();

        // Advance the baseline only after successful application.
        // Fields that require restart are still recorded here —
        // otherwise a subsequent reload would keep reporting them
        // as "changed" even though the caller already acknowledged
        // them.
        *self.setup_config.write().await = fresh;

        Ok(crate::admin_http::ReloadResponse {
            reloaded_fields: reloaded,
            restart_required_fields,
            restart_required,
        })
    }

    pub(crate) fn persistence_enabled(&self) -> bool {
        !self.data_dir.as_os_str().is_empty()
    }

    /// If a mesh has been persisted from a previous session, start
    /// the daemon with that mesh so mDNS advertises immediately and
    /// existing members can reconnect without the user recreating.
    /// No-op if no persisted file exists or if persistence is
    /// disabled (the [`in_memory`](Self::in_memory) constructor).
    pub async fn try_resume(&self) -> Result<bool, MeshError> {
        if !self.persistence_enabled() {
            return Ok(false);
        }
        if self.is_running().await {
            return Ok(false);
        }
        // One-time move of a pre-multi-mesh layout into `meshes/<id>/`, which
        // also derives the `mesh_secret` this node will gossip. Idempotent, so
        // it is cheap to attempt on every boot and there is no flag to forget.
        if let Err(e) = persist::migrate_legacy_layout(&self.data_dir) {
            warn!(error = %e, "mesh: legacy layout migration failed; continuing");
        }
        self.resume_active().await
    }

    /// Bring up whichever mesh `<data_dir>/active` names.
    ///
    /// This is the second half of both [`Self::try_resume`] and
    /// [`Self::switch_mesh`] — a resume and a switch differ only in whether
    /// the pointer moved first, which is exactly why re-entering a mesh whose
    /// roster is still on disk costs no handshake and no invite.
    async fn resume_active(&self) -> Result<bool, MeshError> {
        let loaded = match persist::load(&self.data_dir) {
            Ok(Some(p)) => p,
            Ok(None) => return Ok(false),
            Err(e) => {
                warn!(
                    error = %e,
                    "mesh.json failed to load — ignoring, starting clean"
                );
                return Ok(false);
            }
        };
        let (mesh, self_node_id) = loaded.into_live();
        let mesh_name = mesh.name.clone();
        self.start_daemon(mesh, self_node_id).await?;
        // Restore the cached plaintext so the share UI can render the
        // invite link immediately on this launch — without it, users
        // would see a member roster but no way to invite anyone new.
        match persist::load_join_key(&self.data_dir) {
            Ok(Some(key)) => {
                *self.join_key_plaintext.write().await = Some(key);
            }
            Ok(None) => {
                // Pre-existing mesh from before this feature shipped.
                // Active-mesh view will hide the invite card; the
                // user can still rotate to recover a shareable link.
                tracing::info!(
                    "resumed mesh has no cached join_key.secret \
                     — share card disabled until next rotate"
                );
            }
            Err(e) => warn!(error = %e, "failed to read join_key.secret on resume"),
        }
        info!(mesh_name, "resumed mesh from persisted state");
        // A resumed mesh may have peers cached from a prior session.
        // Kick off an immediate gossip sweep so their `last_seen`
        // gets refreshed (or decayed) within ~2s of the app opening,
        // rather than showing the user a stale roster for the first
        // DEFAULT_GOSSIP_INTERVAL.
        self.trigger_initial_sync().await;
        Ok(true)
    }

    /// Every mesh this node is a member of — the active one and the parked
    /// ones. Read straight off disk so it answers even when stopped.
    pub fn known_meshes(&self) -> Vec<persist::PersistedMesh> {
        if !self.persistence_enabled() {
            return Vec::new();
        }
        persist::list_known(&self.data_dir)
    }

    /// Set the active mesh down and bring another one up, without giving up
    /// membership in either.
    ///
    /// Re-entering the mesh we park costs nothing later: `mesh.json` keeps the
    /// roster and the `mesh_secret`, and gossip authenticates on that secret,
    /// so coming back is a resume rather than a join. No invite is redeemed,
    /// no founder is involved, and an expired invite is irrelevant.
    ///
    /// `target` matches a mesh id (hex, full or unique prefix) or a mesh name,
    /// because an operator types the name and a script has the id.
    pub async fn switch_mesh(&self, target: &str) -> Result<String, MeshError> {
        let known = self.known_meshes();
        let found = persist::resolve_known(&known, target)
            .ok_or_else(|| MeshError::UnknownMesh(target.to_string()))?;

        if persist::active_mesh_id(&self.data_dir).as_ref() == Some(&found.mesh_id) {
            return Err(MeshError::MeshAlreadyActive(found.name.clone()));
        }
        let target_name = found.name.clone();
        let target_id = found.mesh_id;

        // Tell the mesh we are stepping out BEFORE the listeners drop, and say
        // "offline", not "departed" — a `removed_at` tombstone would read as a
        // leave, and we intend to come back.
        if let Some(app_state) = self.app_state().await {
            crate::gossip::announce_presence_change(
                &app_state,
                crate::gossip::PresenceChange::Parked,
            )
            .await;
        }
        if self.is_running().await {
            self.stop_inner(StopMode::Park).await?;
        }

        persist::set_active(&self.data_dir, &target_id)
            .map_err(|e| MeshError::Config(format!("could not set active mesh: {e}")))?;

        match self.resume_active().await {
            Ok(true) => {
                info!(mesh = %target_name, "mesh: switched");
                Ok(target_name)
            }
            Ok(false) => Err(MeshError::Config(format!(
                "'{target_name}' is listed but its mesh.json could not be read"
            ))),
            Err(e) => Err(e),
        }
    }

    /// Drop a PARKED mesh from disk. Refuses on the active one — switch or
    /// leave first, so "forget" can never strand the active pointer.
    pub fn forget_mesh(&self, target: &str) -> Result<String, MeshError> {
        let known = self.known_meshes();
        // Same resolver as `switch_mesh`. It used not to be: forget refused the
        // id prefix switch accepted, so a reference that could switch a mesh
        // could not forget it.
        let found = persist::resolve_known(&known, target)
            .ok_or_else(|| MeshError::UnknownMesh(target.to_string()))?;
        persist::forget(&self.data_dir, &found.mesh_id)
            .map_err(|e| MeshError::Config(e.to_string()))?;
        Ok(found.name.clone())
    }

    /// Any ONLINE peer whose credential generation we have not observed since
    /// this daemon started. Drives the one confirmation round `rotate_invite`
    /// runs before it is willing to refuse — see there for why.
    ///
    /// Reads `None`, not `false`: a peer we merged from and found pre-split is
    /// already answered, and re-gossiping will not change it. Only genuine
    /// absence is worth a round-trip.
    async fn has_unconfirmed_online_peers(&self, app_state: &AppState) -> bool {
        let mesh = app_state.inner.mesh.read().await;
        let self_id = app_state.self_node_id();
        mesh.members.values().any(|m| {
            m.node_id != self_id
                && m.is_active()
                && m.status == commonwealth_core::mesh::NodeStatus::Online
                && app_state.peer_split_generation(m.node_id).is_none()
        })
    }

    /// Whether the daemon is currently running.
    pub async fn is_running(&self) -> bool {
        matches!(*self.state.read().await, DaemonState::Running { .. })
    }

    /// This daemon's `NodeId`, if known. Returns `None` before the
    /// daemon has finished its create_mesh / join_mesh handshake;
    /// callers that depend on the value (e.g.
    /// `MeshInferenceProvider::get_peer_manifest` stamping
    /// `X-Node-Id` for peer-preference matching) skip the
    /// dependent behaviour gracefully when this is `None`.
    pub async fn self_node_id(&self) -> Option<NodeId> {
        match &*self.state.read().await {
            DaemonState::Running { app_state, .. } => Some(app_state.self_node_id()),
            _ => None,
        }
    }

    /// Clone the running `AppState` for callers that need access
    /// to `peer_preferences`, `contribution_emitter`, or other
    /// in-process daemon state. Returns `None` when the daemon
    /// has not yet started (no mesh created/joined).
    ///
    /// `AppState` is `Clone` over an `Arc<AppStateInner>`, so this
    /// is cheap and the returned handle survives any subsequent
    /// state transitions.
    pub async fn app_state(&self) -> Option<commonwealth_api::state::AppState> {
        match &*self.state.read().await {
            DaemonState::Running { app_state, .. } => Some(app_state.clone()),
            _ => None,
        }
    }

    /// Opt this daemon into serving REMOTE callers — the explicit
    /// `mesh create`/`join` action (NOT the silent solo-mesh auto-
    /// create). Persists the `client-exposed` marker so the bind is
    /// `0.0.0.0` (+ bearer token required) on this and every future
    /// start. Call BEFORE `create_mesh`/`join_mesh` when the daemon is
    /// not yet running, so `start_daemon` binds wide on first start
    /// with no restart; when called against an already-running daemon
    /// (attach mode) the new posture takes effect on the next restart
    /// (`client_bind` is a restart-required field).
    pub fn expose_client_api(&self) {
        if let Err(e) = persist::set_client_exposed(&self.data_dir) {
            warn!(error = %e, "failed to persist client-exposed marker — mesh may bind loopback-only");
        }
    }

    /// The running daemon's installed client-API bearer token, if any.
    /// `None` when not running or bound loopback-only (no token).
    /// Surfaced on the invite screen beside the join key.
    pub async fn running_client_token(&self) -> Option<String> {
        match &*self.state.read().await {
            DaemonState::Running { app_state, .. } => {
                app_state.client_token().map(|t| t.to_string())
            }
            _ => None,
        }
    }

    /// Build a `YieldHook` backed by the running daemon's `AppState`.
    /// Returns `None` when the daemon hasn't started yet. Lives here
    /// so callers in `sovereign-cli` (which depends on this crate but
    /// not on `commonwealth-api`) can install foreground back-pressure
    /// on the lint/test watchers without taking a direct
    /// `commonwealth-api` dep.
    pub async fn build_yield_hook(&self) -> Option<std::sync::Arc<dyn corpus_engine::YieldHook>> {
        let state = self.app_state().await?;
        Some(commonwealth_api::yield_hook::AppStateYieldHook::new(
            state.inner.clone(),
        ))
    }

    /// Where mesh state + setup are persisted. Needed by the HTTP
    /// mesh API's rotate handler, which talks to `persist::rotate_join_key`
    /// directly rather than going through a daemon method.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Create a new mesh and start the daemon (plaintext/default mode).
    /// Thin wrapper over [`Self::create_mesh_with`] so the existing
    /// callers (CLI, HTTP, tests) stay unchanged; the desktop create
    /// flow calls `create_mesh_with` to set the encryption policy.
    pub async fn create_mesh(
        &self,
        mesh_name: &str,
        node_name: &str,
    ) -> Result<CreateMeshResult, MeshError> {
        self.create_mesh_with(mesh_name, node_name, false).await
    }

    /// Create a new mesh with an explicit mesh-wide encryption policy.
    /// `require_encryption = true` seeds [`commonwealth_core::mesh::Mesh::require_encryption`];
    /// every joiner inherits it via the join snapshot and gossip.
    pub async fn create_mesh_with(
        &self,
        mesh_name: &str,
        node_name: &str,
        require_encryption: bool,
    ) -> Result<CreateMeshResult, MeshError> {
        if self.is_running().await {
            return Err(MeshError::AlreadyRunning);
        }

        let (_, internal_port) = self.resolved_ports().await;
        // Use routable local IPs rather than `0.0.0.0:port`. The wildcard
        // bind is correct for the listener, but storing it on our
        // `MemberRecord.addresses` means peers receiving our gossip would
        // try to dial `0.0.0.0`, which on macOS resolves to 127.0.0.1 —
        // they'd hit themselves instead of us. See `reachable_addresses`.
        let addrs = reachable_addresses(internal_port);

        // Use this install's stable NodeId (persisted at
        // `<data_dir>/node_id`). Without this, every `create_mesh`
        // would stamp a fresh random ID, so rejoining users would
        // appear as new peers every time their mesh.json got wiped.
        let stable_id = persist::load_or_generate_self_node_id(&self.data_dir);
        // Identity key lives beside node_id; its pubkey rides in the
        // founder's MemberRecord so the trust ring is dial-by-key
        // ready. The seed at `<data_dir>/node_key` doubles as the
        // future iroh SecretKey.
        let identity_key =
            commonwealth_transport::identity::load_or_generate_node_key(&self.data_dir);
        let (mesh, join_key) = membership::init_mesh_with_identity(
            mesh_name,
            node_name,
            addrs,
            stable_id,
            Some(commonwealth_transport::identity::node_pubkey(&identity_key)),
            require_encryption,
        );
        let node_id = stable_id;
        let _ = mesh
            .members
            .keys()
            .next()
            .copied()
            .ok_or_else(|| MeshError::Config("no node in mesh".into()))?;

        // Plaintext link by default; rebuilt AFTER `start_daemon`
        // below once the founder's iroh endpoint has bound and learned
        // a dial string — for BOTH mesh kinds. Encrypted: the dial
        // rides `iroh=` + a TTL, and the join runs over a key-verified
        // QUIC tunnel, never plaintext. Plaintext: the dial rides
        // `dial=` (no TTL) so a no-VPN joiner can reach this founder
        // by key, with IP/mDNS fallback intact.
        let mut join_link = crate::deep_link::build_join_link(
            &join_key,
            None, // relay_hint — local network for now
            Some(mesh_name),
            None,
            false,
            None,
        );

        self.start_daemon(mesh, node_id).await?;

        // Stamp the founder's dial-by-key string into the invite. The
        // iroh endpoint is up now for any mesh-participating daemon
        // (auto-enable via the client-exposed marker), and hard-failed
        // already if an encrypted mesh couldn't bind it. Encrypted
        // additionally arms the founder-side TTL check.
        {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let expires_at = require_encryption.then_some(now + INVITE_TTL_SECS);
            // Clone the endpoint handle out of the state lock — the
            // relay wait below must not hold the daemon-state read
            // lock across its await.
            let (endpoint, app_state_for_ttl) = {
                let state = self.state.read().await;
                match &*state {
                    DaemonState::Running {
                        app_state,
                        iroh_access: Some(access),
                        ..
                    } => (Some(access.endpoint_handle()), Some(app_state.clone())),
                    DaemonState::Running { app_state, .. } => (None, Some(app_state.clone())),
                    _ => (None, None),
                }
            };
            // Arm the TTL on the MESH, not on this node's AppState — it has to
            // gossip and persist, or it is enforced only here and only until
            // the next restart. Written after the state lock is dropped: the
            // mesh guard is async and must not be taken inside the match.
            if let (Some(exp), Some(app_state)) = (expires_at, app_state_for_ttl) {
                app_state.inner.mesh.write().await.invite_expires_at = Some(exp);
            }
            let dial = match &endpoint {
                Some(ep) => {
                    crate::iroh_access::MeshIrohAccess::wait_for_relay(
                        ep,
                        std::time::Duration::from_secs(8),
                    )
                    .await
                }
                None => None,
            };
            match dial {
                Some(dial) => {
                    join_link = crate::deep_link::build_join_link(
                        &join_key,
                        None,
                        Some(mesh_name),
                        Some(dial.as_str()),
                        require_encryption,
                        expires_at,
                    );
                }
                None if require_encryption => {
                    warn!(
                        "encrypted mesh created but the iroh endpoint has no dial \
                         string yet — invite omits the encrypted dial path; \
                         re-share once a relay/address is discovered"
                    );
                }
                None => {
                    if endpoint.is_some() {
                        warn!(
                            "mesh created but the iroh endpoint has no dial string \
                             yet — invite is IP/mDNS-only; a later status read \
                             (current_invite) picks the dial up live"
                        );
                    }
                }
            }
        }

        // Persist *after* start_daemon succeeds so we never leave a
        // mesh.json that points at a daemon that never bound.
        if self.persistence_enabled() {
            if let DaemonState::Running { app_state, .. } = &*self.state.read().await {
                let live = app_state.inner.mesh.read().await.clone();
                // ESTABLISHING call: creating a mesh is one of exactly two acts
                // that make a mesh this node's active one. `save` alone no
                // longer moves the pointer — see `persist::save`.
                if let Err(e) = persist::save_and_activate(&self.data_dir, &live, node_id) {
                    warn!(error = %e, "mesh.json write failed — mesh is in-memory only");
                }
            }
            if let Err(e) = persist::save_join_key(&self.data_dir, &join_key) {
                warn!(
                    error = %e,
                    "join_key.secret write failed — share UI will be empty after restart"
                );
            }
        }
        *self.join_key_plaintext.write().await = Some(join_key.clone());

        info!(mesh_name, "mesh created, daemon started");
        // On create there are no peers yet, but fire initial_sync
        // anyway — it touches our own last_seen to "now" so the
        // very first gossip exchange we later receive has a fresh
        // self record to merge against.
        self.trigger_initial_sync().await;

        Ok(CreateMeshResult {
            mesh_name: mesh_name.to_string(),
            join_key,
            join_link,
            client_token: self.running_client_token().await,
        })
    }

    /// Join an existing mesh from a deep link and start the daemon.
    ///
    /// Flow:
    ///   1. Validate the join-key format.
    ///   2. Start the daemon with a *placeholder* mesh so mDNS
    ///      advertises us and the browse task populates the peers
    ///      table. Chicken-and-egg: mDNS needs a `mesh_id` to
    ///      advertise a service, but we don't know the founder's
    ///      mesh_id until the handshake completes.
    ///   3. Call `perform_join` — scans mDNS for peers whose TXT
    ///      `name` matches the URL, POSTs `/internal/join` with
    ///      the raw key to each, returns the founder's authoritative
    ///      mesh on the first 200.
    ///   4. Swap the placeholder mesh in `AppState` for the adopted
    ///      one. Gossip takes over from here.
    ///
    /// The mDNS TXT record keeps advertising the placeholder
    /// `mesh_id` until the next daemon restart — cosmetic: peers
    /// match on `name`, not mesh_id, so nothing breaks.
    pub async fn join_mesh(
        &self,
        link: &DeepLink,
        node_name: &str,
    ) -> Result<JoinMeshResult, MeshError> {
        // Auto-leave the existing mesh ONLY if it's an auto-created
        // solo mesh (just the founder, no other members). Populated
        // meshes (members > 1) require an explicit `mesh leave` from
        // the caller before joining a new one.
        //
        // Why the gate exists: `self.leave()` calls
        // `persist::clear()` which deletes `mesh.json` AND
        // `join_key.secret` from disk BEFORE the handshake runs.
        // If the handshake then fails (bad key, no peer accepting,
        // network blip, daemon listener fails to re-bind), the user
        // is left without the original mesh on disk. For a solo
        // auto-created mesh that's fine — `mesh create` rebuilds it
        // in 100 ms. For a real, populated mesh it silently
        // destroys peer relationships the user can't recover from
        // local state alone. (See HANDOFF_WS2_MESH_FANOUT.md note
        // on the 2026-05-10 incident.)
        //
        // Why auto-leave still applies for solos: after `sovereign
        // setup`, `daemon_cmd.rs` auto-creates a solo mesh at boot
        // so the daemon has a valid state to gossip from. If the
        // user then pastes a real invite, they expect "join the new
        // mesh", not "AlreadyRunning error, please run leave first".
        // The solo case is harmless to auto-leave.
        if self.is_running().await {
            // PARK the mesh we are in; never destroy it. This is the step that
            // makes a node capable of holding more than one membership — every
            // other multi-mesh surface (`known_meshes`, `mesh list|switch|
            // forget`, the desktop `MeshList`) reads state that only ever comes
            // into existence here.
            //
            // Until 2026-08-27 this branch auto-left a solo mesh and refused a
            // populated one, so `persist::clear` deleted the outgoing
            // `mesh.json` and a second membership could not exist outside
            // tests. The switcher was complete and unreachable.
            //
            // Parking is safe where leaving was not: the outgoing mesh keeps
            // its own `meshes/<id>/` directory and join key, `persist::save`
            // re-points `active` at the mesh we are about to join, and the
            // roster we set down is exactly the one `mesh switch` resumes. The
            // 2026-05-10 incident this branch was written for — a join
            // silently destroying peer relationships the user could not
            // recover from local state — cannot happen when nothing is
            // deleted.
            let parked: Option<String> = {
                let state = self.state.read().await;
                match &*state {
                    DaemonState::Running { app_state, .. } => {
                        Some(app_state.inner.mesh.read().await.name.clone())
                    }
                    DaemonState::Stopped => None,
                }
            };
            // Say "offline", not "departed", before the listeners drop: a
            // `removed_at` tombstone would tell peers we left, and we have not.
            if let Some(app_state) = self.app_state().await {
                crate::gossip::announce_presence_change(
                    &app_state,
                    crate::gossip::PresenceChange::Parked,
                )
                .await;
            }
            self.stop_inner(StopMode::Park).await?;
            if let Some(name) = parked {
                tracing::info!(
                    parked_mesh = %name,
                    "join_mesh: parked the current mesh — it stays joined and \
                     `svrn mesh switch` returns to it"
                );
            }
        }

        let (join_key, url_mesh_name, relay_hint, iroh_dial, invite_encrypted) = match link {
            DeepLink::Join {
                join_key,
                mesh_name,
                relay_hint,
                iroh_dial,
                encrypted,
                ..
            } => (
                join_key.clone(),
                mesh_name.clone(),
                relay_hint.clone(),
                iroh_dial.clone(),
                *encrypted,
            ),
            // A guest link is deliberately NOT joinable. Refusing here with a
            // message that names the right command is the whole difference
            // between "this link is broken" and "you pasted the other kind" —
            // and joining on a guest link would hand membership to someone the
            // issuer meant to lend one model to.
            DeepLink::Guest { .. } => {
                return Err(MeshError::InvalidJoinKey(
                    "that is a guest link, not an invite — it grants use of a \
                     node's models without joining its mesh. Use `svrn mesh use \
                     <link>` instead."
                        .to_string(),
                ))
            }
        };
        let mesh_name = url_mesh_name
            .clone()
            .unwrap_or_else(|| "Joined Mesh".to_string());

        membership::validate_join_key_format(&join_key)
            .map_err(|e| MeshError::InvalidJoinKey(e.to_string()))?;

        let (_, internal_port) = self.resolved_ports().await;
        // Same rationale as create_mesh: we must advertise routable IPs
        // in our MemberRecord, not a wildcard, so the founder can reach
        // us back during gossip rounds after the initial handshake.
        let addrs = reachable_addresses(internal_port);

        // Step 2 — placeholder mesh so mDNS has something to advertise.
        //
        // Use the persisted stable NodeId (not a fresh one). The
        // founder will honour this during the handshake via the
        // `proposed_node_id` wire field, so after adoption our
        // identity in the mesh matches the one we'll advertise in
        // every future rejoin. Without this, each rejoin would
        // assign us a new founder-side NodeId and leave zombie
        // entries in the mesh.members roster.
        let stable_id = persist::load_or_generate_self_node_id(&self.data_dir);
        let (mut placeholder_mesh, _throwaway_key) =
            membership::init_mesh_with_node_id(&mesh_name, node_name, addrs.clone(), stable_id);
        // An ENCRYPTED-mesh invite (dial via `iroh=`) brings the
        // joiner up in encrypted mode from the start: its transport
        // enforces no-plaintext immediately and already matches the
        // (encrypted) mesh we adopt after the handshake — no post-join
        // restart needed. A plaintext invite's `dial=` does NOT trip
        // this: it only offers a no-VPN path to the founder, the mesh
        // itself stays plaintext.
        if invite_encrypted {
            placeholder_mesh.require_encryption = true;
        }
        let placeholder_node_id = stable_id;

        self.start_daemon(placeholder_mesh, placeholder_node_id)
            .await?;

        // Step 3 — handshake. Clone the Arc<MdnsDiscovery> so we don't
        // hold the DaemonState lock for the ~5s the handshake may take.
        let mdns = {
            let state = self.state.read().await;
            match &*state {
                DaemonState::Running { mdns, .. } => mdns.clone(),
                DaemonState::Stopped => unreachable!("just started above"),
            }
        };

        // Identity: present our pubkey with a proof of possession
        // bound to (stable_id, node_name). The founder records the
        // key in our MemberRecord; pre-identity founders ignore the
        // extra fields (serde-default on their side).
        let identity_key =
            commonwealth_transport::identity::load_or_generate_node_key(&self.data_dir);
        let identity = Some((
            commonwealth_transport::identity::node_pubkey(&identity_key),
            commonwealth_transport::identity::sign_join_proof(&identity_key, &stable_id, node_name),
        ));

        // Relay/discovery posture (if configured) for the join's
        // one-shot iroh endpoint, so a joiner behind a firewall that
        // blocks n0's relays reaches the founder via the fleet's own
        // relay (W4), or with n0 fully severed (H1). Default = n0.
        let join_relay_cfg: commonwealth_transport::iroh::RelayConfig = {
            let c = self.setup_config.read().await;
            commonwealth_transport::iroh::RelayConfig::from_parts(
                c.iroh.relay_urls.clone(),
                c.iroh.discovery.as_deref(),
            )
        };
        let handshake = if let (Some(dial), true) = (iroh_dial.as_deref(), invite_encrypted) {
            // ENCRYPTED join: dial the founder by key over iroh and
            // tunnel `/internal/join` through the QUIC bridge — the join
            // secret never crosses the wire in plaintext, and the joiner
            // cryptographically verifies it reached the real founder.
            // Fail closed: no mDNS / plaintext fallback for an encrypted
            // mesh. (The on-wire handshake is validated on two boxes.)
            // A plaintext invite's `dial=` takes the perform_join path
            // below — prefer-iroh, fail-soft (W2c).
            crate::join::perform_encrypted_join(
                dial,
                &join_key,
                node_name,
                addrs,
                identity_key.to_bytes(),
                &join_relay_cfg,
                Some(stable_id),
                identity,
            )
            .await
        } else {
            crate::join::perform_join(
                &mesh_name,
                &join_key,
                node_name,
                addrs,
                // A plaintext invite's `dial=` connect code: dial the
                // founder by key first (no shared IP route needed),
                // fall back to the hint + mDNS below.
                iroh_dial.as_deref().map(|d| (d, identity_key.to_bytes())),
                &join_relay_cfg,
                relay_hint.as_deref(),
                mdns.as_deref(),
                std::time::Duration::from_secs(5),
                // Propose our stable NodeId. Founder keeps it if free
                // or matches our name; else mints a fresh one (first
                // join from a new machine to this mesh).
                Some(stable_id),
                identity,
            )
            .await
        };

        let handshake = match handshake {
            Ok(h) => h,
            Err(e) => {
                // A failed join (bad key, peer offline, network blip) must NOT
                // strand the client API on :9741 — that was the recurring
                // "daemon alive but :9741 down" wedge.
                //
                // Roll back to the mesh we PARKED on the way in. Nothing was
                // destroyed and `persist::save` never ran for the mesh we
                // failed to join, so `active` still names the parked one and
                // resuming it is exact: same roster, same join key, same id.
                //
                // This used to `leave_to_solo()`, which was right only while
                // the pre-flight destroyed the outgoing mesh — there was
                // nothing to go back TO, so a fresh solo mesh was the least-bad
                // landing. Re-soloing now would mint a THIRD mesh and orphan
                // the parked one, which is the clobber this path exists to
                // prevent.
                match self.resume_active().await {
                    Ok(true) => {
                        info!("join rollback: resumed the parked mesh after a failed handshake");
                    }
                    // No parked mesh to go back to (a first-ever join from a
                    // meshless daemon). Solo is the correct landing there, and
                    // is what keeps :9741 bound.
                    Ok(false) => {
                        if let Err(re) = self.leave_to_solo().await {
                            warn!(
                                error = %re,
                                "join rollback: no parked mesh and re-solo failed \
                                 — daemon may be left meshless"
                            );
                        }
                    }
                    Err(re) => {
                        warn!(
                            error = %re,
                            "join rollback: parked mesh could not be resumed \
                             — daemon may be left meshless"
                        );
                    }
                }
                return Err(MeshError::Network(e.to_string()));
            }
        };

        // Step 4 — adopt the founder's authoritative mesh.
        let adopted_node_id = handshake.assigned_node_id;
        {
            let state = self.state.read().await;
            if let DaemonState::Running {
                app_state,
                mesh_state,
                ..
            } = &*state
            {
                *app_state.inner.mesh.write().await = handshake.mesh;
                // Swap our `self_node_id` from the placeholder we
                // generated locally for mDNS to the founder-assigned
                // ID. Without this, every component that indexes by
                // self_node_id (gossip's own-record update,
                // corpus_collaborate's "find me in members",
                // auto_ingest's peer filter) would hit the
                // placeholder which doesn't exist in the adopted
                // mesh — manifesting as `local node not found in
                // mesh` 500s and gossip log spam every 10s.
                app_state.set_self_node_id(adopted_node_id);
                *mesh_state.write().await = MeshState::from_app_state(app_state).await;
            }
        }

        // Persist the adopted mesh so the next app start resumes
        // automatically. Without this, joiners would have to paste
        // the link again every launch.
        if self.persistence_enabled() {
            if let DaemonState::Running { app_state, .. } = &*self.state.read().await {
                let live = app_state.inner.mesh.read().await.clone();
                // The other ESTABLISHING call. Joining a second mesh PARKS the
                // first (P1) rather than leaving it, so the pointer move is the
                // whole switch — it must be explicit here, not a side effect of
                // whichever code path happened to persist last.
                if let Err(e) = persist::save_and_activate(&self.data_dir, &live, adopted_node_id) {
                    warn!(error = %e, "mesh.json write failed — joined mesh is in-memory only");
                }
            }
            // Cache the joiner-side plaintext too — they're equally
            // entitled to re-share the invite they used to get in.
            if let Err(e) = persist::save_join_key(&self.data_dir, &join_key) {
                warn!(
                    error = %e,
                    "join_key.secret write failed — share UI will be empty after restart"
                );
            }
        }
        *self.join_key_plaintext.write().await = Some(join_key.clone());

        info!(mesh_name, node_id = %adopted_node_id, "joined mesh, daemon started");
        // Fire a gossip round immediately so the founder (and any
        // other existing members in the adopted snapshot) learn
        // about us right away — the handshake registered us on
        // the founder, but other peers still need to find out.
        self.trigger_initial_sync().await;

        Ok(JoinMeshResult {
            mesh_name,
            node_id: adopted_node_id.to_string(),
            client_token: self.running_client_token().await,
        })
    }

    /// **Leave** the mesh: stop the daemon AND delete the persisted
    /// state so the next launch doesn't auto-resume. The UI's "Leave"
    /// button and `POST /v1/mesh/leave` invoke this. Internal callers
    /// switching meshes (`join_mesh`'s auto-leave) also use it.
    ///
    /// Distinct from [`shutdown`](Self::shutdown) which is intended
    /// for graceful process exit (SIGTERM/SIGINT) and PRESERVES the
    /// persisted state. Conflating the two means a Ctrl-C wipes the
    /// mesh — the regression that left Machine A creating a fresh
    /// solo mesh on every restart.
    pub async fn leave(&self) -> Result<(), MeshError> {
        // Best-effort: announce departure so online peers tombstone us mesh-wide
        // (gossiped `removed_at`) instead of re-learning our stale live record on
        // their next round. Then tear down + clear local state.
        if let Some(app_state) = self.app_state().await {
            crate::gossip::announce_departure(&app_state).await;
        }
        self.stop_inner(StopMode::Leave).await
    }

    /// Leave the current mesh and immediately re-create a fresh **solo**
    /// mesh in this SAME process, rebinding `:9741`/`:9742`.
    ///
    /// This is the user-initiated "Leave" behavior: a node that leaves a
    /// populated mesh returns to being its own solo mesh, with the client
    /// API staying available on the same process — no restart, no model
    /// reload, no dependency on a service manager to relaunch us. Both the
    /// `POST /v1/mesh/leave` HTTP handler and the desktop Local-mode leave
    /// command call this.
    ///
    /// Distinct from [`leave`](Self::leave), which only tears down and
    /// clears persistence: `join_mesh`'s auto-leave uses that so it can
    /// switch meshes without bouncing back to solo. `leave()` sets the
    /// state to `Stopped` and (via `stop_inner`) awaits the old listener
    /// task's exit, so the `create_mesh` below binds `:9741` cleanly
    /// instead of racing the just-dropped socket.
    pub async fn leave_to_solo(&self) -> Result<(), MeshError> {
        self.leave().await?;
        // Mirror the standalone daemon's boot-time solo mesh
        // (`{hostname}'s Mesh`, node = hostname) so a re-solo looks
        // identical to a fresh launch.
        let host = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "sovereign-node".to_string());
        self.create_mesh(&format!("{host}'s Mesh"), &host).await?;
        Ok(())
    }

    /// **Shutdown** the daemon for process exit. Stops gossip,
    /// mDNS, and the HTTP listener, but PRESERVES `mesh.json` and
    /// `join_key.secret` so the next launch resumes into the same
    /// mesh. Use this in SIGTERM/SIGINT handlers — never to "leave".
    pub async fn shutdown(&self) -> Result<(), MeshError> {
        self.stop_inner(StopMode::Shutdown).await
    }

    /// Backwards-compatible alias for the old API. Deprecated —
    /// callers should pick [`leave`](Self::leave) or
    /// [`shutdown`](Self::shutdown) explicitly so the persistence
    /// intent is unambiguous. Defaulting to leave-semantics
    /// preserves pre-rename behavior for any caller we missed.
    #[deprecated = "use leave() for /v1/mesh/leave or shutdown() for graceful process exit"]
    pub async fn stop(&self) -> Result<(), MeshError> {
        self.leave().await
    }

    async fn stop_inner(&self, mode: StopMode) -> Result<(), MeshError> {
        let mut state = self.state.write().await;
        match std::mem::replace(&mut *state, DaemonState::Stopped) {
            DaemonState::Running {
                _shutdown_tx,
                serve_handle,
                ..
            } => {
                // Dropping the sender signals the daemon to shut down.
                drop(_shutdown_tx);
                // Drop the write guard before touching the filesystem
                // — persistence shouldn't gate the in-memory stop.
                drop(state);
                // Wait for the API-server task to actually observe the
                // shutdown signal and drop its `:9741`/`:9742` listeners
                // before we return. Without this, a follow-on in-process
                // re-create (`leave_to_solo` → `create_mesh` → `start_daemon`)
                // races the just-dropped sockets: SO_REUSEADDR lets a new
                // bind past a socket in TIME_WAIT but NOT one still in
                // LISTEN, so an unsynchronised rebind can hit EADDRINUSE.
                // Bounded so a wedged serve task can never hang `leave()`.
                if tokio::time::timeout(std::time::Duration::from_secs(2), serve_handle)
                    .await
                    .is_err()
                {
                    warn!(
                        "API-server task did not exit within 2s of shutdown signal; \
                         a subsequent rebind of :9741/:9742 may briefly fail"
                    );
                }
                if matches!(mode, StopMode::Leave) && self.persistence_enabled() {
                    if let Err(e) = persist::clear(&self.data_dir) {
                        warn!(
                            error = %e,
                            "mesh.json could not be deleted on leave; \
                             it may auto-resume on next launch"
                        );
                    }
                    if let Err(e) = persist::clear_join_key(&self.data_dir) {
                        warn!(
                            error = %e,
                            "join_key.secret could not be deleted on leave"
                        );
                    }
                    // Re-secure: leaving the mesh drops the remote-serving
                    // posture, so the next start binds loopback-only again.
                    if let Err(e) = persist::clear_client_exposed(&self.data_dir) {
                        warn!(error = %e, "client-exposed marker could not be cleared on leave");
                    }
                    // The pointer goes LAST, and it has to go at all: the three
                    // deletions above resolve their targets THROUGH `active`, and
                    // `active` used to survive every leave — still naming a
                    // directory whose `mesh.json` we had just removed. Boot looked
                    // healthy (`load` returns None, `resume_active` returns false)
                    // while `forget` refused that mesh forever, because forget
                    // refuses the ACTIVE one and nothing could move the pointer off
                    // it. `persist::clear_active` was written for exactly this and
                    // had no caller anywhere in the workspace.
                    let departed = persist::active_mesh_id(&self.data_dir);
                    if let Err(e) = persist::clear_active(&self.data_dir) {
                        warn!(error = %e, "active-mesh pointer could not be cleared on leave");
                    }
                    // …and with the pointer gone, drop the husk. Leaving already
                    // deleted everything inside it, so what remains is residue, and
                    // `list_known` cannot show it (no mesh.json) which means
                    // `forget` could never be aimed at it either.
                    if let Some(id) = departed {
                        if let Err(e) = persist::forget(&self.data_dir, &id) {
                            warn!(error = %e, "left mesh's directory could not be removed");
                        }
                    }
                }
                if matches!(mode, StopMode::Leave | StopMode::Park) {
                    // Park clears the cache but not the file: the plaintext is
                    // per-mesh on disk now, and `resume_active` reloads
                    // whichever mesh comes up next.
                    *self.join_key_plaintext.write().await = None;
                }
                match mode {
                    StopMode::Leave => info!("mesh daemon stopped (left mesh)"),
                    StopMode::Shutdown => info!("mesh daemon stopped (preserving mesh state)"),
                    StopMode::Park => info!("mesh daemon stopped (parked; state preserved)"),
                }
                Ok(())
            }
            DaemonState::Stopped => Err(MeshError::NotRunning),
        }
    }

    /// Get the current mesh state for UI display.
    ///
    /// Rebuilds the snapshot from the live `AppState` on every call
    /// rather than returning a cached value. The `/internal/join`
    /// handler on the founder side mutates `app_state.inner.mesh`
    /// directly — if this returned a stale snapshot (the original
    /// implementation did) the UI's poll never saw new members land
    /// until the daemon restarted, which looked exactly like the
    /// handshake silently failing. Rebuilding is cheap (a walk over
    /// `mesh.members` + derived aggregations) relative to the poll
    /// cadence (5s from MeshSettings, 3s from diagnostics).
    pub async fn mesh_state(&self) -> Option<MeshState> {
        let state = self.state.read().await;
        match &*state {
            DaemonState::Running {
                app_state,
                mesh_state,
                ..
            } => {
                let fresh = MeshState::from_app_state(app_state).await;
                // Gated heartbeat: log at info only when the member
                // count actually changed, else debug. The UI polls
                // every 5s; an unchanging mesh would spam the info
                // stream otherwise. The "changed" case is the
                // operator-meaningful signal — "a member came
                // online" / "a member went offline" — which stays
                // visible.
                let prior = mesh_state.read().await.clone();
                let changed = prior.status.members_total != fresh.status.members_total
                    || prior.status.members_online != fresh.status.members_online;
                if changed {
                    tracing::info!(
                        members = fresh.status.members_total,
                        online = fresh.status.members_online,
                        prior_online = prior.status.members_online,
                        "mesh_state: membership or online-count changed"
                    );
                } else {
                    tracing::debug!(
                        members = fresh.status.members_total,
                        online = fresh.status.members_online,
                        "mesh_state: unchanged heartbeat"
                    );
                }
                // Keep the cached snapshot in sync too, so anything
                // still reading it directly stays current.
                *mesh_state.write().await = fresh.clone();
                Some(fresh)
            }
            DaemonState::Stopped => None,
        }
    }

    /// Current shareable invite for the active mesh.
    ///
    /// Returns `(join_key, join_link)` when the daemon is running
    /// and the plaintext key is cached (set on `create_mesh` /
    /// `join_mesh` / restored from disk on `try_resume`). Returns
    /// `None` when:
    ///   - the daemon is stopped (no mesh)
    ///   - the daemon resumed an older mesh from before this cache
    ///     existed (the share UI hides the invite card and prompts
    ///     a rotate to recover a link)
    ///
    /// The `join_link` is reconstructed on demand from the cached
    /// key + the current mesh name via [`crate::deep_link::build_join_link`],
    /// so a mesh rename (if we ever add it) is automatically picked
    /// up without invalidating the secret file.
    pub async fn current_invite(&self) -> Option<(String, String)> {
        let key = self.join_key_plaintext.read().await.clone()?;
        let state = self.state.read().await;
        let (app_state, endpoint) = match &*state {
            DaemonState::Running {
                app_state,
                iroh_access,
                ..
            } => (
                app_state.clone(),
                iroh_access.as_ref().map(|a| a.endpoint_handle()),
            ),
            DaemonState::Stopped => return None,
        };
        drop(state);
        let (mesh_name, require_encryption) = {
            let mesh = app_state.inner.mesh.read().await;
            (mesh.name.clone(), mesh.require_encryption)
        };
        // Live-read the dial string on every call — the desktop's
        // status poll merges this in, so the share card upgrades
        // itself as the relay connects (and a rotated invite keeps its
        // no-VPN path; this closed the old rotation-loses-the-dial
        // wart). No relay wait here: polls repeat.
        let dial =
            endpoint.and_then(|ep| crate::iroh_access::MeshIrohAccess::dial_for_endpoint(&ep));
        // The exp param mirrors the armed expiry — read, never re-armed here,
        // or every status poll would extend the invite forever. Rotation is
        // what re-arms (see `rotate_invite`). Read from the mesh so a member
        // that did not personally mint the invite still renders the real TTL.
        let expires_at = if require_encryption {
            app_state.inner.mesh.read().await.invite_expires_at
        } else {
            None
        };
        let link = crate::deep_link::build_join_link(
            &key,
            None,
            Some(&mesh_name),
            dial.as_deref(),
            require_encryption,
            expires_at,
        );
        Some((key, link))
    }

    /// H2 observability: per-peer iroh connection path (`direct` /
    /// `relayed` / `mixed` / `idle`), for the operator surface
    /// (`/v1/mesh/status.iroh_transport`, `sovereign mesh transport`).
    /// Empty when iroh isn't running (no endpoint) — the mesh is on the
    /// IP path, nothing to report here. Only members with a known
    /// pubkey are queried (an iroh peer must have one).
    pub async fn iroh_transport_snapshot(&self) -> Vec<IrohPeerPath> {
        let state = self.state.read().await;
        let (app_state, endpoint) = match &*state {
            DaemonState::Running {
                app_state,
                iroh_access: Some(access),
                ..
            } => (app_state.clone(), access.endpoint_handle()),
            _ => return Vec::new(),
        };
        drop(state);
        let self_id = *app_state.inner.self_node_id_swap.load_full().as_ref();
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
            let path = crate::iroh_access::MeshIrohAccess::peer_path_on(&endpoint, &pubkey.0).await;
            out.push(IrohPeerPath {
                node_id: m.node_id,
                name: m.name.clone(),
                path,
            });
        }
        out
    }

    /// The founder's OWN iroh reachability (Track W): is this node relay-homed +
    /// discoverable, and what has the self-heal watchdog done? `None` when iroh
    /// isn't running (mesh on the IP path). Clones the endpoint id / dial and the
    /// watchdog status handle out of the state lock BEFORE awaiting, per the
    /// codebase's clone-out-then-await rule.
    pub async fn self_reachability(&self) -> Option<SelfReachability> {
        let (dial, endpoint_id, status_arc) = {
            let state = self.state.read().await;
            match &*state {
                DaemonState::Running {
                    iroh_access: Some(access),
                    reachability_watchdog,
                    ..
                } => (
                    access.dial_string(),
                    access.endpoint_id(),
                    reachability_watchdog.as_ref().map(|w| w.status_arc()),
                ),
                _ => return None,
            }
        };
        let health = match status_arc {
            Some(arc) => arc.read().await.clone(),
            None => crate::iroh_watchdog::ReachabilityStatus::default(),
        };
        Some(SelfReachability {
            dial,
            endpoint_id,
            health,
        })
    }

    /// Replace the in-memory cached plaintext join key. Called by
    /// the rotate HTTP handler after `persist::rotate_join_key` so
    /// the next status poll surfaces the new link without needing
    /// a daemon restart.
    pub async fn set_join_key(&self, key: String) {
        *self.join_key_plaintext.write().await = Some(key);
    }

    /// Rotate the invite credential. **The one and only implementation of
    /// what rotation means** (ARCH §10.6).
    ///
    /// Before the credential split this was spread across three callers that
    /// each did a different amount of the job — the CLI wrote only disk, the
    /// HTTP handler additionally refreshed the cached plaintext and re-armed a
    /// node-local TTL, the desktop bypassed the daemon entirely — and *none*
    /// of them wrote the live `Mesh`. Two failures fell out of that: the
    /// gossip loop re-persists the in-memory mesh every round, so the new hash
    /// on disk was silently reverted within seconds while `join_key.secret`
    /// kept the new plaintext (the operator was left holding an invite that
    /// hashed to nothing the mesh accepted); and if a restart landed inside
    /// that window instead, the rotator came back with a hash no peer shared
    /// and partitioned itself symmetrically.
    ///
    /// Now: mutate the live mesh first, persist from it, and let the ordinary
    /// gossip round carry it. Rotation cannot touch `mesh_secret` — that is
    /// enforced by [`Mesh::rotate_invite_key`] not being able to name the
    /// field — so it can no longer partition anyone.
    ///
    /// `force` overrides the pre-split-peer refusal below. It must be typed;
    /// silently partitioning a peer is the substitution ARCH §18.3 forbids.
    pub async fn rotate_invite(&self, force: bool) -> Result<RotatedInvite, MeshError> {
        let app_state = self.app_state().await.ok_or(MeshError::NotRunning)?;

        let new_key = commonwealth_discovery::membership::generate_join_key();
        let new_hash = commonwealth_discovery::membership::hash_join_key(&new_key);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // Never refuse on an instrument that has not been run (ARCH §18.4).
        // The confirmation map is in-memory, so every restart empties it — and
        // a rotate seconds after boot then reported a fully-migrated fleet as
        // "still on a pre-split build" when the truth was that this daemon had
        // not spoken to anyone yet. One round costs about one RTT per peer and
        // answers exactly the question the guard is about to ask. Run it BEFORE
        // taking the write lock: the round takes it too.
        if !force && self.has_unconfirmed_online_peers(&app_state).await {
            info!("rotate: peers unconfirmed since boot — one gossip round before deciding");
            if let Err(e) =
                gossip::run_one_round(&app_state, gossip::DEFAULT_OFFLINE_THRESHOLD).await
            {
                warn!(
                    error = %e,
                    "rotate: the confirmation round failed; deciding on what we already know"
                );
            }
        }

        let (mesh_name, expires_at, self_id) = {
            let mut mesh = app_state.inner.mesh.write().await;

            // A peer still on a pre-split build authorizes gossip on
            // `invite_key_hash` (the compat arm in `Mesh::gossip_authorized`),
            // so rotating now would drop exactly those nodes out of the mesh.
            // Name them and refuse rather than partition them quietly.
            if !force {
                // Refuse unless every online peer is CONFIRMED post-split.
                //
                // The confirmation comes from `AppState::peer_confirmed_post_split`,
                // which is fed by the gossip round from `MergeReport::peer_pre_split`.
                // Unknown counts as unsafe: a peer we have not gossiped with
                // since boot may be on either build, and rotating on the
                // optimistic assumption is precisely the silent partition this
                // whole change exists to remove (ARCH §18.3 — never substitute
                // a success-shaped answer for an absent one).
                //
                // This read is deliberately NOT `mesh.mesh_secret`. That is OUR
                // credential; it says nothing about any peer, and testing it
                // here made the guard inert on every migrated node — the exact
                // failure the guard was written to prevent.
                // Why a rotate was refused is otherwise unanswerable from a
                // deployed daemon: the guard's input is an in-memory map, not
                // anything on disk or in the roster.
                //
                // Classify in THREE values, not two. "We merged from it and it
                // offered neither a proof nor a secret" and "we have not merged
                // from it at all" are different facts with different remedies —
                // upgrade that node, versus wait one round — and collapsing
                // them is what made the refusal tell operators their fleet was
                // un-migrated when it was not. `peer_confirmed_post_split` is
                // still the right SAFETY read (unknown is unsafe); it is the
                // wrong DIAGNOSTIC one.
                let mut pre_split: Vec<String> = Vec::new();
                let mut unconfirmed: Vec<String> = Vec::new();
                for m in mesh.members.values() {
                    if m.node_id == app_state.self_node_id() {
                        continue;
                    }
                    let generation = app_state.peer_split_generation(m.node_id);
                    tracing::debug!(
                        peer = %m.node_id,
                        name = %m.name,
                        status = ?m.status,
                        active = m.is_active(),
                        generation = ?generation,
                        "rotate: pre-split check"
                    );
                    let online =
                        m.is_active() && m.status == commonwealth_core::mesh::NodeStatus::Online;
                    if !online {
                        continue;
                    }
                    match generation {
                        Some(true) => {}
                        Some(false) => pre_split.push(m.name.clone()),
                        None => unconfirmed.push(m.name.clone()),
                    }
                }
                if !pre_split.is_empty() || !unconfirmed.is_empty() {
                    return Err(MeshError::RotateWouldPartition {
                        pre_split,
                        unconfirmed,
                    });
                }
            }

            let expires_at = mesh.require_encryption.then_some(now + INVITE_TTL_SECS);
            mesh.rotate_invite_key(new_hash, expires_at);
            (mesh.name.clone(), expires_at, app_state.self_node_id())
        };

        // Persist FROM the live mesh, so disk and memory agree and the next
        // gossip round has nothing to revert.
        if self.persistence_enabled() {
            let mesh = app_state.inner.mesh.read().await;
            if let Err(e) = persist::save(&self.data_dir, &mesh, self_id) {
                warn!(error = %e, "rotate: mesh.json could not be written");
            }
            if let Err(e) = persist::save_join_key(&self.data_dir, &new_key) {
                warn!(error = %e, "rotate: join_key.secret could not be written");
            }
        }
        *self.join_key_plaintext.write().await = Some(new_key.clone());

        info!(
            mesh_name,
            expires_at = ?expires_at,
            "rotate: invite key rotated; mesh_secret untouched"
        );
        Ok(RotatedInvite {
            mesh_name,
            join_key: new_key,
            expires_at,
        })
    }

    /// Get the Commonwealth API address (for internal use).
    pub async fn api_address(&self) -> Option<SocketAddr> {
        let state = self.state.read().await;
        match &*state {
            DaemonState::Running { client_addr, .. } => Some(*client_addr),
            DaemonState::Stopped => None,
        }
    }

    /// Snapshot of peers discovered via mDNS on the local network.
    /// Empty when the daemon is stopped or no peers have advertised
    /// on `_commonwealth._tcp.local.` yet.
    pub async fn discovered_peers(&self) -> Vec<DiscoveredPeer> {
        let state = self.state.read().await;
        match &*state {
            DaemonState::Running { mdns, .. } => mdns
                .as_ref()
                .map(|m| m.discovered_peers())
                .unwrap_or_default(),
            DaemonState::Stopped => Vec::new(),
        }
    }

    /// Endpoints for peer nodes that are currently online and
    /// reachable for federated inference. Each entry lists all of
    /// the peer's advertised addresses in the order the `MeshInference`
    /// wrapper should try them (routable IPs first, link-local
    /// filtered out).
    ///
    /// Empty when the daemon is stopped, when we're solo, or when
    /// every peer is offline — callers should fall back to local
    /// inference in any of those cases.
    pub async fn peer_inference_endpoints(&self) -> Vec<PeerInferenceEndpoint> {
        let state = self.state.read().await;
        let app_state = match &*state {
            DaemonState::Running { app_state, .. } => app_state.clone(),
            DaemonState::Stopped => return Vec::new(),
        };
        drop(state);
        // The PeerTransport seam resolves dial candidates: for the
        // Inference class, `IpTransport` rewrites each gossiped
        // address's port (the peer's *internal* port — that's what
        // the join handshake targets) to the *client* port and sorts
        // by `peer_addr::rank` so the inference fallback chain in
        // `peer_inference.rs` tries IPv4 (typically Tailscale CGNAT)
        // before IPv6 ULA. The uniform-port assumption (every peer's
        // client API on the same `client_port` as ours, pending a
        // `MemberRecord.client_port` wire field — §10.1) lives in
        // the transport's construction at `start_daemon`.
        let transport = app_state.peer_transport();
        let members: Vec<commonwealth_core::mesh::MemberRecord> = {
            let mesh = app_state.inner.mesh.read().await;
            let self_id = *app_state.inner.self_node_id_swap.load_full().as_ref();
            mesh.members
                .values()
                .filter(|m| m.node_id != self_id)
                .filter(|m| {
                    matches!(
                        m.status,
                        commonwealth_core::mesh::NodeStatus::Online
                            | commonwealth_core::mesh::NodeStatus::Busy
                    )
                })
                .filter(|m| m.is_dialable())
                .cloned()
                .collect()
        };
        let mut endpoints = Vec::with_capacity(members.len());
        for m in members {
            let base_urls: Vec<String> = transport
                .endpoints(
                    &commonwealth_transport::peer_contact(&m),
                    commonwealth_transport::TrafficClass::Inference,
                )
                .await
                .into_iter()
                .map(|ep| format!("{}/v1", ep.base_url))
                .collect();
            endpoints.push(PeerInferenceEndpoint {
                node_id: m.node_id,
                name: m.name.clone(),
                base_urls,
                system_ram_gb: m.capabilities.hardware.system_ram_gb,
                benchmark: m.capabilities.benchmark.clone(),
                current_in_flight: m.capabilities.current_in_flight,
                inference_availability: Some(m.capabilities.inference_availability),
                // P2 provenance: the LWW event time on the member
                // record is exactly the age of the two load signals
                // above — they arrive on the same gossip payload.
                gossip_last_seen_unix: m.last_seen,
                // Mesh peers always use the default plain-HTTP transport
                // — TLS pinning is reserved for ephemeral worker pods,
                // which surface through `PinnedWorkerEndpointSource` in
                // a separate path.
                transport: None,
            });
        }
        endpoints
    }

    /// Auto-discover mesh RPC inference workers: probe each online peer's
    /// `/status` for an advertised `rpc_worker.port` and return reachable
    /// `ip:port` RPC endpoints. Fed to the embedded engine's worker provider so
    /// a host needs no manual `SOVEREIGN_RPC_WORKERS`. Best-effort — peers that
    /// don't respond or aren't serving a worker are simply omitted.
    /// HTTP-observable admission + fan-out + ingest signals for the mesh-soak
    /// invariant checker: `(peer_inflight_current, peer_inflight_ceiling,
    /// fanout_inflight_current, active_corpus_ingests)`. Cheap lock/atomic reads
    /// on a non-hot path; `(0, 0, 0, 0)` when the daemon isn't Running (nothing
    /// in flight).
    pub async fn glassbox_signals(&self) -> (usize, usize, usize, usize) {
        let app_state = {
            let state = self.state.read().await;
            match &*state {
                DaemonState::Running { app_state, .. } => app_state.clone(),
                DaemonState::Stopped => return (0, 0, 0, 0),
            }
        };
        let inflight = app_state.peer_inflight_count();
        let ceiling = app_state.contribution_max_peer_inflight();
        let fanout = app_state.fanout_inflight_count();
        let ingests = app_state.inner.active_ingests.read().await.len();
        (inflight, ceiling, fanout, ingests)
    }

    /// The current eligible shared-model anchors, by `NodeId`: online mesh
    /// members (including self, when self is an online anchor) that advertise
    /// `anchor.can_anchor`. This is the input to leader election for the host
    /// role — see `commonwealth_core::partition::should_host`. Pure read of the
    /// gossiped membership, so every anchor computes the same set and converges
    /// on the same host without coordination.
    pub async fn eligible_anchors(&self) -> Vec<commonwealth_core::ids::NodeId> {
        let app_state = {
            let state = self.state.read().await;
            match &*state {
                DaemonState::Running { app_state, .. } => app_state.clone(),
                DaemonState::Stopped => return Vec::new(),
            }
        };
        let mesh = app_state.inner.mesh.read().await;
        mesh.members
            .values()
            .filter(|m| {
                matches!(
                    m.status,
                    commonwealth_core::mesh::NodeStatus::Online
                        | commonwealth_core::mesh::NodeStatus::Busy
                )
            })
            .filter(|m| m.capabilities.anchor.as_ref().is_some_and(|a| a.can_anchor))
            .map(|m| m.node_id)
            .collect()
    }

    pub async fn discover_rpc_workers(&self) -> crate::worker_eligibility::DiscoveryOutcome {
        // The raw-TCP rpc-server needs the peer's DIRECT IP. The `/status` probe
        // URL host is unreliable for this: when `status_probe` is routed over iroh,
        // the probe authority is a loopback proxy (`127.0.0.1:<ephemeral>`), which
        // is NOT where the peer's rpc-server listens. Derive the endpoint from the
        // member's advertised IPs instead — prefer private-LAN (lowest latency for
        // per-layer activation traffic), then CGNAT/Tailscale, then anything else —
        // and reachability-probe each so we only return an openable socket.
        async fn reachable_rpc_endpoint(addresses: &[SocketAddr], rpc_port: u16) -> Option<String> {
            fn rank(ip: &std::net::IpAddr) -> u8 {
                match ip {
                    std::net::IpAddr::V4(v) if v.is_private() => 0,
                    std::net::IpAddr::V4(v)
                        if v.octets()[0] == 100 && (v.octets()[1] & 0xC0) == 0x40 =>
                    {
                        1
                    }
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
        let app_state = {
            let state = self.state.read().await;
            match &*state {
                DaemonState::Running { app_state, .. } => app_state.clone(),
                // The scan did not run at all — `scanned: false` says this tick
                // is evidence about NOTHING, rather than silently reading as
                // "every worker is gone".
                DaemonState::Stopped => {
                    return crate::worker_eligibility::DiscoveryOutcome::default()
                }
            }
        };
        let transport = app_state.peer_transport();
        let members: Vec<commonwealth_core::mesh::MemberRecord> = {
            let mesh = app_state.inner.mesh.read().await;
            let self_id = *app_state.inner.self_node_id_swap.load_full().as_ref();
            mesh.members
                .values()
                .filter(|m| m.node_id != self_id)
                .filter(|m| {
                    matches!(
                        m.status,
                        commonwealth_core::mesh::NodeStatus::Online
                            | commonwealth_core::mesh::NodeStatus::Busy
                    )
                })
                .filter(|m| m.is_dialable())
                // Anchor-tier gate: only pull peers that declare themselves
                // shared-model anchors into the RPC layer-split. A peer that
                // explicitly advertises `can_anchor = false` is a consumer and
                // is excluded; legacy peers (no `anchor` field) get the benefit
                // of the doubt — they're still gated downstream by whether they
                // actually advertise an `rpc_worker` port.
                .filter(|m| m.capabilities.anchor.as_ref().is_none_or(|a| a.can_anchor))
                .cloned()
                .collect()
        };

        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(800))
            .build()
        {
            Ok(c) => c,
            Err(_) => return crate::worker_eligibility::DiscoveryOutcome::default(),
        };

        // Node ids of the currently gossip-Online members — used to prune sticky
        // endpoints for peers that have since gone offline, so a peer that changed
        // address while away is re-probed fresh on its return rather than
        // re-affirmed from stale cache.
        let online_ids: std::collections::HashSet<NodeId> =
            members.iter().map(|m| m.node_id).collect();
        // Snapshot of the worker history, read once: membership here is what
        // makes an unanswered probe reportable as "no statement" instead of
        // "absent".
        let known_workers: std::collections::HashMap<NodeId, std::time::Instant> = self
            .rpc_worker_last_seen
            .read()
            .map(|m| m.clone())
            .unwrap_or_default();
        let polled = members.len();
        let mut unconfirmed: Vec<NodeId> = Vec::new();
        let mut out = Vec::new();
        for m in members {
            let name = m.name.clone();
            let node_id = m.node_id;
            // Stickiness identity + hold budget, read once up front — it decides
            // whether we even need the heavy `/status` re-probe this tick.
            let prev = self
                .rpc_worker_sticky
                .read()
                .ok()
                .and_then(|sticky| sticky.get(&node_id).cloned());
            let flip_threshold = rpc_endpoint_flip_threshold();

            // Fresh discovery for THIS tick — the endpoint ggml should dial, if we
            // can confirm it now. Stays `None` when the probe fails; because `m`
            // already passed the gossip-Online + dialable filter above, a `None`
            // means a transient probe blip on a LIVE peer, not a death, and the
            // stickiness guard below holds the last-good direct-ip rather than
            // dropping the worker and collapsing a live distribution. Two flap
            // sources feed this, both observed mid-decode 2026-07-19: a 600ms
            // direct-ip miss, and a starved `/status` probe (3 straight misses at
            // ~774ms gossip RTT under decode load) that dropped the peer entirely.
            let mut fresh: Option<(String, String)> = None;
            match reaffirm_plan(prev.as_ref(), rpc_tunnel_mode()) {
                // KNOWN direct-ip worker still gossip-Online (it passed the Online +
                // dialable membership filter above): re-affirm its cached endpoint
                // WITHOUT any network probe. Measured 2026-07-19: under active decode
                // the RPC tensor traffic saturates the shared Wi-Fi link, so EVERY
                // probe to the worker — /status HTTP *and* a raw TCP connect — times
                // out for the whole inference and, after `flip_threshold` misses,
                // drops a worker that is in fact alive and serving (tensors flowed at
                // ~8.7 tok/s while both probe types failed 3× straight, yet gossip
                // reach to the same peer stayed 58–143ms throughout). Gossip rides a
                // separate path + a looser budget and survives that load, so
                // gossip-Online membership IS the liveness signal for a known worker.
                // A moved endpoint is re-learned on the next Offline→Online cycle
                // (sticky is pruned for offline nodes after the loop); a dead
                // rpc-server with live gossip surfaces via the ggml RPC connection
                // failing → supervised reload (P0.4), not a discovery probe.
                Reaffirm::Held => {
                    fresh = prev.as_ref().map(|p| (p.endpoint.clone(), p.via.clone()));
                }
                // KNOWN bridged worker: re-mint its loopback endpoint straight from
                // the transport's bridge cache — same gossip-as-liveness argument,
                // and the `/status` probe it replaces rides the SAME iroh path as
                // the tunnel it would be checking (so decode load starves it on a
                // worker that is serving fine, and a non-direct endpoint has no
                // stickiness to survive the miss — see `reaffirm_plan`).
                Reaffirm::Rebridge => {
                    fresh = bridge_rpc_endpoint(&transport, &m).await;
                }
                Reaffirm::FullProbe => {}
            }
            if fresh.is_none() {
                // UNKNOWN worker (initial discovery), a probe-host worker, or a
                // bridged one whose iroh path just vanished (it may have moved onto
                // the LAN): run the full `/status` probe + endpoint selection.
                let probes = transport
                    .endpoints(
                        &commonwealth_transport::peer_contact(&m),
                        commonwealth_transport::TrafficClass::StatusProbe,
                    )
                    .await;
                for probe in &probes {
                    let status_url = format!("{}/status", probe.base_url);
                    // Fallback host only: the RPC worker speaks raw TCP and needs an
                    // IP-overlay address, but when `status_probe` is routed over iroh
                    // this probe authority is a loopback proxy (`127.0.0.1`). We
                    // prefer a direct member IP below (`reachable_rpc_endpoint`) and
                    // use this parsed probe host only when no advertised IP is reachable.
                    let Some(host) = probe
                        .base_url
                        .strip_prefix("http://")
                        .and_then(|a| a.rsplit_once(':'))
                        .map(|(host, _)| host.to_string())
                    else {
                        continue;
                    };
                    let Ok(resp) = client.get(&status_url).send().await else {
                        continue; // /status timed out — leave `fresh` None (blip guard below)
                    };
                    if !resp.status().is_success() {
                        continue;
                    }
                    let Ok(json) = resp.json::<serde_json::Value>().await else {
                        continue;
                    };
                    let Some(port) = json
                        .get("rpc_worker")
                        .and_then(|w| w.get("port"))
                        .and_then(|p| p.as_u64())
                    else {
                        continue;
                    };
                    let rpc_port = port as u16;
                    let iroh_advertised = json
                        .get("rpc_worker")
                        .and_then(|w| w.get("iroh"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let mode = rpc_tunnel_mode();
                    let allow_bridge = iroh_advertised && mode != RpcTunnelMode::Never;

                    // Choose the endpoint ggml will dial. Direct raw TCP to a member
                    // IP is the LAN fast path; the iroh bridge is the cross-network
                    // path; the parsed probe host is the last resort. `SOVEREIGN_RPC_TUNNEL`
                    // = `always` prefers the bridge; `never` opts out of bridging.
                    let mut sel: Option<(String, String)> = None;
                    if allow_bridge && mode == RpcTunnelMode::Always {
                        sel = bridge_rpc_endpoint(&transport, &m).await;
                    }
                    if sel.is_none() {
                        sel = reachable_rpc_endpoint(&m.addresses, rpc_port)
                            .await
                            .map(|d| (d, "direct-ip".to_string()));
                    }
                    if sel.is_none() && allow_bridge {
                        sel = bridge_rpc_endpoint(&transport, &m).await;
                    }
                    if sel.is_none() {
                        sel = Some((format!("{host}:{rpc_port}"), "probe-host".to_string()));
                    }
                    fresh = sel;
                    break; // one reachable address per peer suffices
                }
            }
            let Some(choice) = sticky_endpoint(prev.as_ref(), fresh, flip_threshold) else {
                // Nothing to hold (no prior direct-ip, or the hold budget is
                // spent). We could not CONFIRM a worker here — which is not the
                // same statement as "there is no worker here". The peer passed
                // the gossip-Online + dialable filter above, so if we have ever
                // seen a worker on it, report the tick as unconfirmed and let
                // the eligibility layer hold its prior state (bounded by
                // `absence_grace`) rather than record a flap.
                if let Ok(mut sticky) = self.rpc_worker_sticky.write() {
                    sticky.remove(&node_id);
                }
                if known_workers.contains_key(&node_id) {
                    unconfirmed.push(node_id);
                }
                continue;
            };

            if choice.direct_misses > 0 {
                tracing::info!(
                    peer = %name,
                    endpoint = %choice.endpoint,
                    via = %choice.via,
                    miss = choice.direct_misses,
                    flip_threshold,
                    "rpc-discovery: probe miss on a gossip-Online worker — holding last-good endpoint (transient-blip guard)"
                );
            } else if choice.via == "probe-host" {
                tracing::warn!(
                    peer = %name,
                    endpoint = %choice.endpoint,
                    "no reachable direct IP and no iroh bridge for RPC worker; falling back to probe host (may be an iroh loopback proxy — distribution likely to fail)"
                );
            } else {
                tracing::info!(
                    peer = %name,
                    endpoint = %choice.endpoint,
                    via = %choice.via,
                    "discovered mesh RPC worker"
                );
            }

            // Record which member owns this endpoint BEFORE identity is dropped
            // into the bare-string RPC layer — the warm orchestrator resolves the
            // worker's mesh transport through this.
            if let Ok(mut dir) = self.rpc_endpoint_nodes.write() {
                dir.insert(choice.endpoint.clone(), node_id);
            }
            if let Ok(mut sticky) = self.rpc_worker_sticky.write() {
                sticky.insert(node_id, choice.clone());
            }
            if let Ok(mut seen) = self.rpc_worker_last_seen.write() {
                seen.insert(node_id, std::time::Instant::now());
            }
            out.push(crate::worker_eligibility::DiscoveredWorker {
                node_id,
                endpoint: choice.endpoint,
            });
        }
        // Prune sticky endpoints for peers that are no longer gossip-Online, so a
        // returning peer with a changed address is re-probed fresh (see `online_ids`).
        if let Ok(mut sticky) = self.rpc_worker_sticky.write() {
            sticky.retain(|nid, _| online_ids.contains(nid));
        }
        // Same pruning for the worker-history map, and it is load-bearing: a
        // peer gossip has dropped is no longer "known", so it stops being
        // eligible for an unconfirmed hold and its absence becomes POSITIVE
        // evidence on the next tick. That is what keeps `kill -9` of a worker
        // daemon converging at the pre-2026-07-28 speed (P0.4 acceptance).
        if let Ok(mut seen) = self.rpc_worker_last_seen.write() {
            seen.retain(|nid, _| online_ids.contains(nid));
        }
        crate::worker_eligibility::DiscoveryOutcome {
            workers: out,
            unconfirmed,
            // Engagement is the CALLER's knowledge (only the discovery loop
            // knows what its compute child is doing) — folded in there.
            engaged: Vec::new(),
            polled,
            scanned: true,
        }
    }

    /// Which mesh member owns `endpoint` (a discovered `ip:port` ggml-RPC
    /// worker endpoint), if discovery recorded one. Env-configured workers
    /// (`SOVEREIGN_RPC_WORKERS`) have no entry — callers fall back to raw-IP
    /// addressing for those.
    pub fn rpc_endpoint_node(&self, endpoint: &str) -> Option<NodeId> {
        self.rpc_endpoint_nodes
            .read()
            .ok()
            .and_then(|dir| dir.get(endpoint).copied())
    }

    /// Ordered dial candidates for `node`'s internal HTTP surface under the
    /// `ModelTransfer` traffic class (rpc-warm pushes, GGUF/shard pulls) —
    /// the mesh transport's view: on an iroh-routed mesh the first candidate
    /// is a loopback bridge that tunnels to the peer, with raw-IP fallback
    /// candidates after. Empty when the daemon isn't Running or the node has
    /// left the membership.
    pub async fn model_transfer_endpoints(
        &self,
        node: NodeId,
    ) -> Vec<commonwealth_transport::PeerEndpoint> {
        let app_state = {
            let state = self.state.read().await;
            match &*state {
                DaemonState::Running { app_state, .. } => app_state.clone(),
                DaemonState::Stopped => return Vec::new(),
            }
        };
        let member = {
            let mesh = app_state.inner.mesh.read().await;
            mesh.members.get(&node).cloned()
        };
        let Some(member) = member else {
            return Vec::new();
        };
        app_state
            .peer_transport()
            .endpoints(
                &commonwealth_transport::peer_contact(&member),
                commonwealth_transport::TrafficClass::ModelTransfer,
            )
            .await
    }

    /// Feedback that `endpoint` carried a successful ModelTransfer call to
    /// `node` — lets the transport promote it for future dials (the same
    /// last-working cache gossip benefits from).
    pub async fn note_model_transfer_success(
        &self,
        node: NodeId,
        endpoint: &commonwealth_transport::PeerEndpoint,
    ) {
        let state = self.state.read().await;
        if let DaemonState::Running { app_state, .. } = &*state {
            app_state.peer_transport().note_success(
                node,
                commonwealth_transport::TrafficClass::ModelTransfer,
                endpoint,
            );
        }
    }

    // ── Private ─────────────────────────────────────────

    async fn start_daemon(&self, mesh: Mesh, node_id: NodeId) -> Result<(), MeshError> {
        // Resolve the bind/announce ports once at the top so every
        // downstream site (listener bind, mDNS announce, auto-
        // collaborate loop spawn) sees the same pair. Defaults to
        // (9741, 9742); operator config via `set_setup_config`
        // overrides — see `resolved_ports` for the contract.
        let (client_port, internal_port) = self.resolved_ports().await;

        // mesh_id as hex — broadcast in mDNS TXT records so peers on
        // the LAN can tell which mesh this node belongs to. Public by
        // design (knowing the mesh_id isn't sufficient to join;
        // accessing members still requires the join_key).
        let mesh_id_hex = hex::encode(mesh.id.as_bytes());
        let mesh_name = mesh.name.clone();
        // Mesh-wide encryption policy, captured before `mesh` is moved
        // into `app_state` below. Drives BOTH the receiver-side
        // plaintext lockout (listener binds, WS-C) and the require-mode
        // iroh transport install (WS-B) further down.
        let require_encryption = mesh.require_encryption;
        let node_name = mesh
            .members
            .get(&node_id)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| node_id.to_string());

        // Build an AppState that already knows about our CorpusEngine
        // (if one was installed via `set_corpus_engine`). Without
        // this, Commonwealth's knowledge handlers can only return
        // stubs — the whole reason Peer A couldn't see Peer B's SEP
        // corpus. The MeshStore defaults to in-memory; bootstraps
        // that want shared access (e.g. the work atlas reading from
        // the same store gossip publishes from) inject one via
        // `set_mesh_store` before this point. Long-term persistence
        // for the legacy mesh state still flows through `mesh.json`.
        let corpus_engine = self
            .services
            .serving()
            .map(|s| Arc::clone(&s.core.corpus_engine));
        let mesh_store = match self.services.rails().map(|r| &r.mesh_store) {
            Some(provided) => Arc::clone(provided),
            // Only the headless daemon carries a shared store; the desktop and
            // the mesh-admin one-shot get a private in-memory one, which is
            // what their variants declare by not having the field.
            None => Arc::new(
                commonwealth_state::MeshStore::in_memory().expect("in-memory MeshStore failed"),
            ),
        };
        let app_registry = Arc::new(commonwealth_app::registry::AppRegistry::new());
        let app_state = AppState::new_with_platform_and_engine(
            node_id,
            mesh,
            mesh_store,
            app_registry,
            corpus_engine.clone(),
        );

        // Install the shared notes convergence recorder (fix 9) into
        // the freshly-built AppState — same injection discipline as
        // `mesh_store` above: the bootstrap hands the SAME
        // `Arc<ConvergenceRecord>` to the publish sink + ingest
        // poller, so `/status`'s convergence section reads the
        // writers' stamps, never a parallel copy.
        if let Some(recorder) = self.services.rails().map(|r| &r.convergence_recorder) {
            app_state
                .inner
                .install_convergence_recorder(Arc::clone(recorder));
        }

        // Route every peer dial through an `IpTransport` configured
        // with OUR resolved client port (the `AppState::new*` default
        // assumes 9741) — this is where the uniform-port assumption
        // for the Inference/StatusProbe port rewrite is anchored.
        // RwLock-based installer, so it is exempt from the
        // `Arc::get_mut` ordering constraint documented below.
        //
        // Bound to a variable because W3 may RE-install a
        // `RoutedTransport` over iroh later in this fn (after the iroh
        // endpoint binds), reusing THIS `IpTransport` as the fallback
        // default. Until then — and in every non-iroh deployment — this
        // is the one and only install, byte-identical to before.
        let ip_transport: Arc<dyn commonwealth_transport::PeerTransport> =
            Arc::new(commonwealth_transport::IpTransport::new(client_port));
        app_state.install_peer_transport(ip_transport.clone());

        // Publish this install's identity pubkey (key beside node_id
        // at `<data_dir>/node_key`; same unconditional
        // load-or-generate posture as the stable NodeId). Gossip
        // stamps it into our MemberRecord every round, which is also
        // the in-place upgrade path for meshes created before
        // identity keys existed.
        {
            let identity_key =
                commonwealth_transport::identity::load_or_generate_node_key(&self.data_dir);
            app_state.install_self_node_pubkey(commonwealth_transport::identity::node_pubkey(
                &identity_key,
            ));
            // Install the dial-info signer (WS-D anti-downgrade): the
            // gossip self-stamp uses it to sign our reachability so only
            // we can change our own dial info. The key stays captured in
            // the closure — AppState never holds raw key material.
            let signing_key = identity_key.clone();
            app_state.install_self_dial_signer(Arc::new(
                move |version, relay: Option<String>, addrs: Vec<std::net::SocketAddr>| {
                    commonwealth_transport::identity::sign_dial_info(
                        &signing_key,
                        version,
                        relay.as_deref(),
                        &addrs,
                    )
                },
            ));
        }

        // ── Order is load-bearing ─────────────────────────────────
        //
        // The `with_*` installers below mutate `AppStateInner`
        // through `Arc::get_mut`, which silently fails (with a
        // tracing::warn!) the moment any other code clones
        // `app_state.inner`. The YieldHook construction
        // (`AppStateYieldHook::new(app_state.inner.clone())`) and
        // the embed-info publication (`app_state.inner.inference_store
        // .set_local_embed_model(...)`) both bump the Arc strong
        // count, so we must run ALL `with_*` installers BEFORE any
        // of those.
        //
        // Inverting this order silently breaks
        // `/v1/chat/completions` — the orchestrator path is taken
        // and every request 503s with `model_not_ready` — and
        // breaks mesh persistence on join (falls back to the 10s
        // gossip-loop cadence). See `with_local_inference` and
        // `with_mesh_mutation_hook` in
        // commonwealth-api/src/state.rs.

        // If Sovereign installed an InferenceProvider, wrap it in
        // the OpenAI-flavour adapter so this node's
        // `/v1/chat/completions` serves peer requests directly
        // from the same local model the user would use. Without
        // this, peer inference requests 503 because the daemon's
        // scheduler/llama-server path is empty in the embedded
        // topology.
        let app_state = if let Some(provider) = self.inference_provider.read().await.as_ref() {
            let adapter: Arc<dyn LocalInferenceService> = Arc::new(
                crate::inference_adapter::SovereignInferenceAdapter::new(provider.clone()),
            );
            info!("inference adapter: wired into /v1/chat/completions");
            // Worker side of distributed-inference auto-warm: this node can seed
            // its RPC tensor cache with a shard on request (`POST /internal/
            // rpc-warm`). Installed alongside local inference — a node that can
            // serve chat can serve as an RPC worker. See `rpc_warm_http`.
            let warmer: Arc<dyn commonwealth_api::state::RpcShardWarmer> =
                Arc::new(crate::rpc_warm_http::MeshRpcShardWarmer::new());
            app_state
                .with_local_inference(adapter)
                .with_rpc_shard_warmer(warmer)
        } else {
            app_state
        };

        // Install the persistence hook that fires on every Mesh
        // mutation from a route handler (`/internal/join`,
        // `/internal/gossip`). This closes the race window where
        // the founder accepts a new member but crashes before the
        // next 10s gossip-loop re-persist fires, forgetting the
        // joiner on restart.
        let app_state = if self.persistence_enabled() {
            let data_dir = self.data_dir.clone();
            let hook: commonwealth_api::state::MeshMutationHook = Arc::new(
                move |mesh: &commonwealth_core::mesh::Mesh, self_id: NodeId| {
                    if let Err(e) = persist::save(&data_dir, mesh, self_id) {
                        tracing::warn!(
                            error = %e,
                            "mesh_mutation_hook: persist failed"
                        );
                    }
                },
            );
            app_state.with_mesh_mutation_hook(hook)
        } else {
            app_state
        };

        // ── End of Arc::get_mut-sensitive block ───────────────────
        // Everything below is free to clone `app_state.inner`.

        // Apply foreground-yield config from setup_config and install
        // the AppState-backed YieldHook on the corpus engine.
        //
        // The hook is a thin Arc<AppStateInner> wrapper. Cloning
        // `app_state.inner` here bumps the Arc strong count.
        //
        // When `yield_to_foreground_secs = 0` the hook still gets
        // wired but `should_yield` short-circuits to false — so the
        // ingest pipeline pays only the cost of one rwlock read +
        // one atomic load per embed batch when the feature is off.
        if let Some(engine) = corpus_engine.as_ref() {
            {
                let secs = self
                    .setup_config
                    .read()
                    .await
                    .daemon
                    .yield_to_foreground_secs;
                app_state.set_yield_window_secs(secs);
                info!(
                    yield_to_foreground_secs = secs,
                    "foreground-yield: window configured"
                );
            }
            let hook: Arc<dyn corpus_engine::YieldHook> =
                commonwealth_api::yield_hook::AppStateYieldHook::new(app_state.inner.clone());
            engine.set_yield_hook(hook);
            info!("foreground-yield: hook installed on corpus engine");
        }

        // Bound peer-inference admission for headless contributors. The desktop
        // sets this from the GPU-share consent; a CLI daemon would otherwise
        // leave the AppState default (unbounded) in place — and an unbounded
        // peer fan-out is what OOM-killed the daemon. Apply the configured
        // ceiling (default 1) regardless of whether a corpus engine is present,
        // so a storage-only or inference-only node is still bounded.
        {
            let max = self.setup_config.read().await.daemon.max_peer_inflight;
            app_state.set_contribution_max_peer_inflight(max);
            info!(
                max_peer_inflight = max,
                "admission: peer-inflight ceiling configured"
            );
        }

        // Publish embed model info so the collaborative ingestion planner
        // can compare this node's embedding model against candidates'.
        // Without this, `get_local_embed_model()` returns None and the
        // collaborate handler falls back to the qwen3-embedding-0.6b default,
        // which won't match a peer running a different model.
        if let Some(embed_info) = self
            .services
            .serving()
            .and_then(|s| s.advertise_embed.info())
        {
            app_state
                .inner
                .inference_store
                .set_local_embed_model(embed_info);
            info!(
                model_id = %embed_info.model_id,
                dims = embed_info.dimensions,
                "embed model info: published to inference store"
            );
        }

        // Start the pull-based work-queue reaper. Dormant until a handoff
        // gets registered via `corpus_collaborate` with the pull-based flag;
        // always-on so we don't have to race the first `register` call.
        let _reaper = app_state.start_work_queue_reaper();

        // Sweep lapsed guest grants. Auth already fails closed on an expired
        // grant (`GuestGrantStore::live` evaluates expiry per read), so this
        // bounds the map rather than enforcing the TTL — but a `drain_dead`
        // with no caller is exactly the shape that left `ingest_grant`'s
        // expiry unenforced, so it gets a caller at birth.
        let _guest_reaper = app_state.start_guest_grant_reaper();

        // Register the locally-loaded model slots so `/v1/models`
        // answers with something meaningful instead of an empty list.
        // Without this, the OpenAI-compatible models list returns
        // `{"object":"list","data":[]}` on a freshly-set-up daemon —
        // confusing for anyone running `curl /v1/models` as a
        // post-setup health check. We register one `ModelInfo` per
        // configured slot (primary / fast / embed) with a
        // deterministic ModelId so reloads don't create duplicates.
        {
            let cfg = self.setup_config.read().await;
            register_local_model_slots(&app_state, &cfg, node_id);
        }

        // Client API bind — the OpenAI-compatible public surface
        // (SYSTEM_OVERVIEW.md §5.5). Peers fetch `/oicp/v1/capabilities`
        // here, the Joiner's HybridProvider POSTs `/v1/chat/completions`
        // here for federated inference, and mesh apps federate via
        // `/v1/apps/*`.
        //
        // **Trust boundary (2026-06 auth: localhost-default + bearer).**
        // `daemon.client_bind` defaults to `127.0.0.1` — secure by
        // default, single-user needs no auth. When an operator binds a
        // routable address to serve a mesh / remote clients, the
        // `client_auth` layer requires a bearer token of every
        // non-loopback caller. We resolve + install that token here so
        // the layer (which reads it from `AppState`) has it before the
        // first request. The internal port (`:9742`, mTLS) is unrelated
        // and always binds `0.0.0.0`.
        let (mut client_bind, configured_token, internal_bind) = {
            let c = self.setup_config.read().await;
            (
                c.daemon.client_bind.clone(),
                c.daemon.client_token.clone(),
                c.daemon.internal_bind.clone(),
            )
        };
        let mut bind_is_loopback = client_bind == "127.0.0.1"
            || client_bind == "::1"
            || client_bind.eq_ignore_ascii_case("localhost");
        // The `client-exposed` marker (written by `expose_client_api`
        // on an explicit `mesh create`/`join`) bumps a loopback default
        // to `0.0.0.0`. An explicit non-loopback `client_bind` in config
        // already wins on its own; this only promotes the default, so
        // the silent solo-mesh stays loopback (no marker) while a shared
        // mesh is reachable across restarts (marker persists).
        if bind_is_loopback && persist::client_exposed(&self.data_dir) {
            client_bind = "0.0.0.0".to_string();
            bind_is_loopback = false;
        }
        // Receiver-side lockout (WS-C): an ENCRYPTED mesh closes its
        // plaintext ingress entirely. Force the client bind back to
        // loopback even if the client-exposed marker or config asked for
        // `0.0.0.0` — remote peers reach `/v1` over the key-authenticated
        // iroh acceptor (which forwards to this loopback listener), never
        // plaintext. Overrides the marker bump above.
        if require_encryption && !bind_is_loopback {
            info!(
                "encrypted mesh: forcing client API to loopback-only — remote \
                 access is via the iroh acceptor (key-authenticated)"
            );
            client_bind = "127.0.0.1".to_string();
            bind_is_loopback = true;
        }
        if bind_is_loopback {
            app_state.install_client_token(None);
        } else {
            // Non-loopback: a token is mandatory. Precedence: env →
            // config → auto-generate+persist. Generating-by-default
            // means an operator can't accidentally expose an
            // unauthenticated surface by flipping the bind alone.
            let token = std::env::var("SOVEREIGN_CLIENT_TOKEN")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .or(configured_token)
                .or_else(|| {
                    commonwealth_transport::identity::load_or_create_client_token(&self.data_dir)
                        .map_err(|e| warn!("client-token persistence failed: {e}"))
                        .ok()
                });
            match token {
                Some(tok) => {
                    info!(
                        bind = %client_bind,
                        "client API bound non-loopback — bearer token REQUIRED for \
                         remote callers (token at {}/client-token; or set \
                         daemon.client_token / SOVEREIGN_CLIENT_TOKEN)",
                        self.data_dir.display()
                    );
                    app_state.install_client_token(Some(tok.into()));
                }
                None => {
                    // Could not obtain a token at all — fail closed:
                    // install None so the layer refuses every remote
                    // caller (loopback still works) rather than serving
                    // unauthenticated.
                    warn!(
                        bind = %client_bind,
                        "client API bound non-loopback but NO token could be \
                         resolved/generated — remote callers will be REFUSED \
                         (fail-closed). Fix data-dir perms or set \
                         daemon.client_token."
                    );
                    app_state.install_client_token(None);
                }
            }
        }
        let client_addr: SocketAddr = format!("{client_bind}:{client_port}")
            .parse()
            .unwrap_or_else(|_| {
                warn!("invalid client_bind '{client_bind}'; falling back to 127.0.0.1");
                format!("127.0.0.1:{client_port}").parse().unwrap()
            });
        // Receiver-side lockout (WS-C): under encryption the internal
        // router is loopback-only too — the iroh acceptor (which forwards
        // here) is the sole network path in, including for
        // `/internal/join`. Plaintext LAN callers get connection-refused.
        let internal_addr: SocketAddr =
            internal_bind_addr(require_encryption, &internal_bind, internal_port);

        let mesh_state = Arc::new(RwLock::new(MeshState::from_app_state(&app_state).await));

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        // Register on mDNS and start browsing — but only when discovery
        // is enabled. Both are load-bearing on a LAN: advertise lets
        // remote peers find us; browse populates the discovered-peers
        // table that `perform_join` (Phase B) uses to locate handshake
        // targets. On a VPC/hardened host (`[discovery] mdns = false` or
        // `SOVEREIGN_DISABLE_MDNS`) we skip both — and crucially never
        // touch the multicast socket, whose bind is otherwise fatal at
        // boot — forming the mesh from static seeds (`?relay=` /
        // `[discovery] seed_addrs`) instead.
        let mdns_enabled = {
            let c = self.setup_config.read().await;
            mdns_enabled_effective(c.discovery.mdns)
        };
        let (mdns, browse_handle): (Option<Arc<MdnsDiscovery>>, Option<BrowseHandle>) =
            if mdns_enabled {
                let mdns = MdnsDiscovery::new(
                    node_id,
                    &mesh_id_hex,
                    &mesh_name,
                    &node_name,
                    internal_port,
                )
                .map_err(|e| MeshError::Network(format!("mDNS register failed: {e}")))?;
                let mdns = Arc::new(mdns);
                // A 32-slot channel is plenty — the browse loop pushes on
                // ServiceResolved and we don't actively consume. If the
                // buffer fills (many peers on a busy LAN), the background
                // task drops extras; the discovered-peers hash map is
                // still authoritative.
                let (peer_tx, _peer_rx) = tokio::sync::mpsc::channel::<DiscoveredPeer>(32);
                let browse_handle = mdns
                    .browse(peer_tx)
                    .map_err(|e| MeshError::Network(format!("mDNS browse failed: {e}")))?;
                (Some(mdns), Some(browse_handle))
            } else {
                info!(
                    "mesh: mDNS discovery disabled — forming mesh from static \
                     seeds only (no multicast advertise/browse)"
                );
                (None, None)
            };

        // Assemble every router this daemon serves, before moving `app_state`
        // into the spawn. Cheap: `axum::Router` clones internal Arcs.
        //
        // The three routers that are pure functions of `Arc<Self>` — mesh,
        // admin, reading — are built HERE from `self_weak`, not accepted from
        // a host. That is the fix for the divergence this file used to
        // document as ordinary: the desktop installed a different subset from
        // the CLI daemon, and nothing could tell a declined router from a
        // forgotten one. A serving daemon now always has its own control
        // surface, and `mount_names` prints exactly what it has.
        let mcp_mount = self
            .services
            .serving()
            .and_then(|s| s.capability.mcp.mount())
            .cloned();
        let mut mounted: Vec<axum::Router> = Vec::new();
        let mut mount_names: Vec<&'static str> = Vec::new();
        if self.services.serves_host_surface() {
            let self_arc = self
                .self_weak
                .upgrade()
                .expect("EmbeddedDaemon::start_daemon runs behind the Arc that owns it");
            mounted.push(crate::mesh_http::mesh_router(Arc::clone(&self_arc)));
            mount_names.push("mesh_http");
            mounted.push(crate::admin_http::admin_router(Arc::clone(&self_arc)));
            mount_names.push("admin_http");
            mounted.push(crate::reading_http::reading_router(Arc::clone(&self_arc)));
            mount_names.push("reading_http");
            // Phase 5c — the daemon answers. Built here from `Arc<Self>` like
            // the three above, not accepted from a host, so a serving daemon
            // cannot come up unable to serve a turn.
            mounted.push(crate::turn_http::turn_router(self_arc));
            mount_names.push("turn_http");
            for (router, name) in self
                .services
                .host_routers()
                .into_iter()
                .zip(self.services.host_router_names())
            {
                mounted.push(router);
                mount_names.push(name);
            }
        }
        info!(
            profile = self.services.label(),
            mcp = mcp_mount.is_some(),
            routers = ?mount_names,
            "daemon: client router assembled"
        );

        // The GUEST listener: a second bind of the client router, loopback-only
        // and on an ephemeral port, whose auth layer does NOT treat a loopback
        // peer as a local caller. The iroh acceptor forwards `GUEST_ALPN`
        // here, so a `sovereign://guest/…` bearer is actually read instead of
        // being skipped by the loopback arm — see
        // `commonwealth_api::client_auth`.
        //
        // Bound HERE, before the serve task is spawned, because
        // `MeshIrohAccess::start` below needs the resolved port and the serve
        // task runs concurrently. A bind failure is not fatal: it costs guest
        // access over iroh and nothing else, so it is logged and the ALPN goes
        // unadvertised (a guest dial is then refused at the handshake rather
        // than silently landing on the trusting listener).
        //
        // It deliberately serves the BARE client router: no MCP, no mounted
        // host surfaces. A guest's scope reaches `/v1/models` and
        // `/v1/chat/completions`; mounting less than `permits_path` allows is
        // free defence in depth.
        let (guest_listener, guest_addr) = match tokio::net::TcpListener::bind(("127.0.0.1", 0u16))
            .await
        {
            Ok(l) => match l.local_addr() {
                Ok(a) => (Some(l), Some(a)),
                Err(e) => {
                    warn!("guest listener bound but has no local address ({e}) — guest links over iroh disabled");
                    (None, None)
                }
            },
            Err(e) => {
                warn!(
                    "guest listener could not bind loopback ({e}) — guest links over iroh disabled"
                );
                (None, None)
            }
        };

        // The PEER listener: the third bind of the client router, and the
        // only one the acceptor forwards a MEMBER to. It admits a loopback
        // caller exactly as `:9741` does — a peer's federated inference
        // carries no `Authorization` header at all, and its key is the
        // credential the QUIC handshake already proved — but it serves
        // `ClientSurface::Peer`, which mounts no `/internal/*`.
        //
        // Why a separate bind rather than a guard on those routes: the
        // acceptor forwards by `TcpStream::connect("127.0.0.1")`, so on any
        // listener it feeds, "is the caller loopback" cannot distinguish a
        // real local caller from the forward hop. A loopback guard there
        // would read as a fix and gate nothing. Until 2026-08-28 a member
        // landed on `:9741` and could POST `/internal/guest/grant` — mint a
        // credential for an outsider on someone else's node — with nothing
        // presented. See note `3d2f1ae0`.
        //
        // A bind failure costs federated inference FROM peers over iroh and
        // nothing else; `forward_for` then closes a member's dial rather
        // than promoting it to the operator listener.
        let (peer_listener, peer_addr) = match tokio::net::TcpListener::bind(("127.0.0.1", 0u16))
            .await
        {
            Ok(l) => match l.local_addr() {
                Ok(a) => (Some(l), Some(a)),
                Err(e) => {
                    warn!("peer listener bound but has no local address ({e}) — peer inference over iroh disabled");
                    (None, None)
                }
            },
            Err(e) => {
                warn!(
                    "peer listener could not bind loopback ({e}) — peer inference over iroh disabled"
                );
                (None, None)
            }
        };

        // Spawn the API servers in the background. The JoinHandle is stored
        // in `DaemonState::Running` (not discarded) so `stop_inner` can await
        // teardown — dropping the old `:9741`/`:9742` listeners — before an
        // in-process re-create (`leave_to_solo`) rebinds the same ports.
        let app_state_clone = app_state.clone();
        let serve_handle = tokio::spawn(async move {
            let mut client_router =
                commonwealth_api::server::client_router(app_state_clone.clone());
            if let Some(m) = mcp_mount {
                // Phase 5: daemon path leaves the spec-presence gate
                // off (`FeatureRoot::new(None)`) so `tools/list`
                // continues to advertise every exposed tool. Per-
                // request gating via the registered project root is a
                // follow-up — the embedded daemon serves many projects
                // and we don't yet plumb per-request feature_root.
                //
                // Phase 5b: a fresh `McpNotifier` with no producer is
                // fine — the daemon doesn't drive list-changed
                // notifications today (that's the per-project
                // standalone serve's job). Subscribers connect
                // harmlessly and idle until something publishes.
                client_router = client_router.merge(mcp_router::mcp_router(
                    m.tools,
                    m.notes,
                    m.session_id,
                    mcp_router::FeatureRoot::new(None),
                    mcp_router::McpNotifier::new(),
                ));
            }
            for router in mounted {
                client_router = client_router.merge(router);
            }
            let internal_router =
                commonwealth_api::server::internal_router(app_state_clone.clone());
            let peer_router = commonwealth_api::server::client_router_for(
                app_state_clone.clone(),
                commonwealth_api::server::ClientSurface::Peer,
            );
            let guest_router = commonwealth_api::server::client_router_for(
                app_state_clone,
                commonwealth_api::server::ClientSurface::Guest,
            );

            // Phase 3 takeover: a `sovereign init` invocation may have
            // spawned a standalone `sovereign serve` process holding `:9741`.
            // SIGTERM it (via the `~/.svrnmesh/server.pid` pointer) so we can
            // take ownership of the port; a no-op on a service-manager boot
            // where the pointer file doesn't exist.
            takeover_standalone_serve_if_present();

            // Bind with a short EADDRINUSE retry: an in-process re-create
            // (`leave_to_solo`) can momentarily race the previous mesh's
            // just-dropped socket. `stop_inner` already awaits the old serve
            // task via `serve_handle` before the rebind, so this retry is
            // belt-and-suspenders. A bind that STILL fails is logged and the
            // task returns — best-effort, matching long-standing behavior (a
            // hard error here would strand the many default-port tests that
            // bind `:9741` under parallel contention).
            let client_listener = match bind_listener_with_retry(client_addr, "client API").await {
                Ok(l) => l,
                Err(e) => {
                    warn!("{e}");
                    return;
                }
            };
            let internal_listener =
                match bind_listener_with_retry(internal_addr, "internal API").await {
                    Ok(l) => l,
                    Err(e) => {
                        warn!("{e}");
                        return;
                    }
                };

            info!("Commonwealth daemon started (client: {client_addr}, internal: {internal_addr})");

            // Enumerate local non-loopback IPs so the founder can copy one
            // into a `?relay=<IP>` query param if mDNS doesn't reach the
            // joiner (WiFi AP isolation, multicast filtering, different
            // subnets). Matches the crate README's Tailscale workaround.
            for iface in local_ip_candidates() {
                info!(
                    ip = %iface,
                    "mesh: reachable at this address — share as \
                     `?relay={iface}:9742` if mDNS fails"
                );
            }

            // CRITICAL: the client router contains handlers that
            // extract `ConnectInfo<SocketAddr>` (mesh_http, admin_http,
            // mcp_router) to enforce a loopback-only guard on admin
            // surfaces. Bare `axum::serve(listener, router)` does NOT
            // register a ConnectInfo service factory, so every such
            // handler rejects with 500 "Missing request extension" —
            // breaking the guards for legitimate localhost callers
            // AND defeating the security boundary for remote callers
            // (they also get 500, but the extractor failure is a
            // foot-gun waiting for a router refactor to flip it to
            // fail-open). Always use `.into_make_service_with_connect_info`
            // on this listener. Regression test:
            // `admin_http::tests::loopback_guard_works_under_production_listener_shape`.
            let client_service = client_router.into_make_service_with_connect_info::<SocketAddr>();
            // `ConnectInfo` matters here for the same reason it does on the
            // client listener, and one reason more: without it the guest layer
            // cannot identify the caller at all and fails closed with a 500 on
            // every guest request.
            let guest_service = guest_router.into_make_service_with_connect_info::<SocketAddr>();
            // Same reason again: without `ConnectInfo` the auth layer cannot
            // identify the caller and fails closed with a 500.
            let peer_service = peer_router.into_make_service_with_connect_info::<SocketAddr>();
            // A daemon whose guest listener failed to bind still serves
            // everything else; `pending()` just never resolves that arm.
            let guest_serve = async move {
                match guest_listener {
                    Some(l) => {
                        let _ = axum::serve(l, guest_service).await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            let peer_serve = async move {
                match peer_listener {
                    Some(l) => {
                        let _ = axum::serve(l, peer_service).await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                _ = axum::serve(client_listener, client_service) => {}
                _ = axum::serve(internal_listener, internal_router) => {}
                _ = guest_serve => {}
                _ = peer_serve => {}
                _ = shutdown_rx => {
                    info!("Commonwealth daemon shutting down");
                }
            }
        });

        // Spawn the gossip heartbeat task. It uses `app_state` via a
        // clone (cheap Arc bump) so it stays live independently of
        // the Running variant's ownership. Aborted on daemon stop by
        // the `_gossip_handle: Drop` → `JoinHandle::abort()`.
        //
        // Log at spawn site (synchronous to `start_daemon`) — the
        // matching "gossip: loop started" info inside the task fires
        // when the runtime first polls the future, which can be
        // later. Seeing "spawning gossip loop" but NOT "loop
        // started" means the task is queued but starved; seeing
        // NEITHER means the binary predates this code and a rebuild
        // is required.
        info!("spawning gossip loop");
        // Hand `data_dir` to the gossip loop so it can re-persist
        // mesh.json after every round — catching the Founder's
        // /internal/join mutation (which mutates in-memory but used
        // to leave the on-disk snapshot stale, so a Founder restart
        // forgot every Joiner and Joiners had to rejoin each time).
        let persist_dir = if self.persistence_enabled() {
            Some(self.data_dir.clone())
        } else {
            None
        };
        let gossip_handle = gossip::spawn_gossip_loop(
            app_state.clone(),
            gossip::DEFAULT_GOSSIP_INTERVAL,
            gossip::DEFAULT_OFFLINE_THRESHOLD,
            persist_dir,
        );

        let collaborate_handle =
            crate::auto_ingest::spawn_auto_collaborate_loop(app_state.clone(), internal_port);

        // Re-spawn any solo corpus ingest the daemon was running before
        // restart. The mesh auto-collaborate loop above only handles
        // peer-driven dispatch; a solo Wikipedia install that was
        // mid-stream when launchd restarted us has no other waker.
        // Without this hook the on-disk state stays "in progress"
        // forever and the desktop banner pretends progress is happening
        // while the embed slot is idle. See `auto_resume.rs` docstring.
        crate::auto_resume::spawn_resume_in_progress_ingests(app_state.clone());

        // Hourly StorageSnapshot ledger emission. Without this, the
        // dimensional ledger has no signal for "what corpora is each
        // peer hosting" — the merge-leader pull path emits
        // `ShardTransferred`, but until a corpus has been served
        // there's nothing for the UI to render. The first tick of
        // `tokio::time::interval` runs immediately, so a
        // freshly-restarted daemon emits one snapshot at boot AND
        // every interval after.
        //
        // The loop owns its own `watch` channel; the sender is moved
        // into the spawned task so it stays alive for the task's
        // lifetime. When the runtime drops the task at process
        // exit, the sender drops with it. Mirrors the gossip
        // loop's "live for the whole daemon" model without needing
        // to thread a new field into `DaemonState::Running`.
        let snapshot_emitter = app_state.inner.contribution_emitter.clone();
        let snapshot_engine = corpus_engine.clone();
        let (snapshot_shutdown_tx, snapshot_shutdown_rx) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            let _hold_shutdown_tx = snapshot_shutdown_tx;
            commonwealth_state::contributions::run_storage_snapshot_loop(
                snapshot_emitter,
                move || {
                    let engine = snapshot_engine.clone();
                    async move {
                        let Some(engine) = engine else {
                            return Vec::new();
                        };
                        match engine.installed_indexes().await {
                            Ok(list) => list
                                .into_iter()
                                .filter(|i| i.mesh_sharing)
                                .map(|i| (i.corpus_id, i.index_size_bytes as f64 / 1e9))
                                .collect(),
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "storage_snapshot: installed_indexes failed"
                                );
                                Vec::new()
                            }
                        }
                    }
                },
                commonwealth_state::contributions::STORAGE_SNAPSHOT_INTERVAL,
                snapshot_shutdown_rx,
            )
            .await;
        });
        info!("StorageSnapshot loop started");

        // ── Contributions-ledger retention GC ─────────────────────
        //
        // The ledger is append-only: `ContributionEmitter::record`
        // writes one ~220 B `MeshStore` row per served request, under a
        // key carrying an origin+time+seq suffix so LWW never collapses
        // two events. Nothing ever deleted them on this daemon —
        // `RetentionGc` was constructed only by `commonwealth-daemon`.
        // At 10k requests/day that is ~2 MB/day of rows that gossip
        // then replicates as a full snapshot on every round, walking
        // toward `MAX_REQUEST_BODY_BYTES` (8 MiB) and, before that, the
        // 3s POST timeout (MESH_SCALE_100_USERS_1000_CORPORA.md §7.2).
        //
        // TTL is the AGGREGATION WINDOW, not a second independently
        // chosen number: every reader
        // (`current_contributions`, `commonwealth balance`) aggregates
        // over `DEFAULT_WINDOW_DAYS`, so a row older than that is
        // provably invisible to every reader. One decider (§10.6) —
        // widen the window and the retention follows.
        //
        // SCOPED to the contributions app on purpose. This daemon's
        // `MeshStore` also carries processed-shards dedup markers and
        // `corpus-engine/handoff:*` records that are written once and
        // never rewritten; a whole-store age sweep would delete those
        // and re-open completed ingest work. See `RetentionGc::app_scope`.
        let gc_store = app_state.inner.mesh_store.clone();
        let ledger_ttl_secs =
            u64::from(commonwealth_core::contributions::DEFAULT_WINDOW_DAYS).saturating_mul(86_400);
        let (gc_shutdown_tx, gc_shutdown_rx) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            let _hold_shutdown_tx = gc_shutdown_tx;
            commonwealth_state::RetentionGc::new(
                gc_store,
                ledger_ttl_secs,
                commonwealth_state::contributions::STORAGE_SNAPSHOT_INTERVAL,
            )
            .scoped_to_app(commonwealth_state::CONTRIBUTIONS_APP_ID)
            .run(gc_shutdown_rx)
            .await;
        });
        info!(
            app_scope = commonwealth_state::CONTRIBUTIONS_APP_ID,
            ttl_days = commonwealth_core::contributions::DEFAULT_WINDOW_DAYS,
            interval_secs = commonwealth_state::contributions::STORAGE_SNAPSHOT_INTERVAL.as_secs(),
            "RetentionGc started (contributions ledger)"
        );

        // Stall sweep — any non-terminal `_enrichment_state.json`
        // older than STALL_THRESHOLD_SECS is rewritten as `Stalled`
        // so the desktop chip transitions out of "starting" / "RAPTOR
        // leaves" and into "interrupted, click to retry". Cheap walk
        // of the indexes dir; runs once per daemon start and adds
        // ~tens of milliseconds at most.
        if let Some(engine) = corpus_engine.clone() {
            let indexes_dir = engine.index_dir().to_path_buf();
            match corpus_engine::enrichment::state::sweep_stalled_states(&indexes_dir) {
                Ok(corpora) if !corpora.is_empty() => {
                    info!(
                        count = corpora.len(),
                        corpora = ?corpora,
                        "enrichment_stall_sweep: marked previously-running enrichments as Stalled"
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    error = %e,
                    "enrichment_stall_sweep failed; UI may show stale 'starting' until next manual retry"
                ),
            }
        }

        // ── wikipedia-newsworthy freshness daemon ─────────────────
        // Spawned only when a CorpusEngine handle is available — the
        // watcher's whole point is reindexing into the parent
        // `wikipedia` corpus, which requires the engine. Watcher reads
        // mesh membership for leader/owner via `MeshNewsworthyHost`,
        // shares the daemon's tokio runtime, and listens to the same
        // shutdown channel pattern as RetentionGc/storage-snapshot so
        // it terminates cleanly on `EmbeddedDaemon::stop`.
        //
        // Gated on `[daemon] freshness_watchers_enabled` (default
        // true). Operators flip it to false for measurement runs —
        // e.g. the Enron Phase 5 baseline — where the per-tick
        // wikipedia atlas-rebuild streams ~1.88M chunks through the
        // enrichment LLM and contends with foreground ingest. The
        // yield hook fires only on user-facing inference, not on
        // background enrichment, so a config-level toggle is the
        // clean lever. Future freshness watchers (sec-edgar, etc.)
        // inherit the same gate.
        let freshness_enabled = self
            .setup_config
            .read()
            .await
            .daemon
            .freshness_watchers_enabled;
        if !freshness_enabled {
            info!(
                "freshness watchers skipped — [daemon].freshness_watchers_enabled = false in config.toml"
            );
        }
        if freshness_enabled {
            if let Some(engine) = corpus_engine.clone() {
                let newsworthy_config =
                    corpus_engine::update::newsworthy_watcher::NewsworthyConfig::default();
                let host: std::sync::Arc<
                    dyn corpus_engine::update::newsworthy_watcher::NewsworthyHost,
                > = std::sync::Arc::new(crate::newsworthy_host::MeshNewsworthyHost::new(
                    app_state.clone(),
                    newsworthy_config.corpus_id.clone(),
                ));
                let mw_client: std::sync::Arc<
                    dyn corpus_engine::update::newsworthy_watcher::MediaWikiClient,
                > = std::sync::Arc::new(
                    corpus_engine::update::newsworthy_watcher::HttpMediaWikiClient {
                        base_url: "https://en.wikipedia.org/w/api.php".to_string(),
                        user_agent: "commonwealth-ai/0.1 (newsworthy)".to_string(),
                        http: reqwest::Client::new(),
                    },
                );
                let watcher = std::sync::Arc::new(
                    corpus_engine::update::newsworthy_watcher::WikipediaNewsworthyWatcher::new(
                        host,
                        engine,
                        mw_client,
                        newsworthy_config,
                    ),
                );
                let (newsworthy_shutdown_tx, newsworthy_shutdown_rx) =
                    tokio::sync::watch::channel(false);
                // Operator-triggered tick channel. Capacity 4 is plenty —
                // ticks coalesce on the watcher side (one in flight at a
                // time), so a burst of /internal/newsworthy/tick POSTs
                // collapses to "one extra tick after the current one
                // finishes". Sender is published on AppState so the route
                // handler can fire without holding a watcher handle.
                let (newsworthy_force_tick_tx, newsworthy_force_tick_rx) =
                    tokio::sync::mpsc::channel::<()>(4);
                if let Ok(mut slot) = app_state.inner.newsworthy_force_tick.try_write() {
                    *slot = Some(newsworthy_force_tick_tx);
                }
                // Wrap `watcher.spawn` in another `tokio::spawn` so the
                // sender is moved INTO the wrapping task's async block
                // (mirroring the storage-snapshot loop above). Earlier
                // attempts bound `let _hold = sender` directly in this
                // function — but that scope ends as soon as
                // `start_daemon` returns a few lines down, dropping the
                // sender, which causes the watcher's
                // `shutdown_rx.changed()` arm to fire on Err before the
                // jitter window completes. The watcher would log
                // `newsworthy.watcher_starting` and then silently exit
                // without ever ticking. Moving the bind inside the
                // wrapping async task keeps the sender alive for as long
                // as the watcher's `JoinHandle` is being awaited — i.e.
                // for the daemon's lifetime under normal operation.
                tokio::spawn(async move {
                    let _hold_shutdown_tx = newsworthy_shutdown_tx;
                    let handle = watcher.spawn(newsworthy_shutdown_rx, newsworthy_force_tick_rx);
                    let _ = handle.await;
                });
                info!("WikipediaNewsworthyWatcher started");
            }
        } // freshness_enabled

        // W1 (TRANSPORT_MIGRATION.md): bind a dial-by-key endpoint
        // (server half) when `[iroh] enabled`. Uses the SAME node_key
        // identity gossip already publishes as `MemberRecord
        // .node_pubkey`, so a known member is a dialable member. The
        // acceptor routes by negotiated ALPN to the loopback client /
        // internal listeners bound above. Strictly additive: a bind
        // failure logs and yields `None`, leaving the `IpTransport`
        // path untouched. Forwarding is lazy per stream, so binding
        // after the listener spawn (which races to bind) is safe.
        let (cfg_iroh_enabled, iroh_transport_cfg, iroh_relay_cfg) = {
            let c = self.setup_config.read().await;
            (
                c.iroh.enabled,
                c.iroh.transport.clone(),
                commonwealth_transport::iroh::RelayConfig::from_parts(
                    c.iroh.relay_urls.clone(),
                    c.iroh.discovery.as_deref(),
                ),
            )
        };
        // Enablement is tri-state: explicit `[iroh] enabled` wins;
        // otherwise mesh participation decides — the `client-exposed`
        // marker every explicit create/join surface writes (and
        // `leave()` clears), so joining a mesh turns iroh on and a
        // meshless daemon never contacts relays. The mesh-wide
        // encryption policy still FORCES iroh on: an encrypted mesh
        // must be dialable by key and must dial peers by key.
        let iroh_enabled = crate::iroh_access::resolve_enabled(
            cfg_iroh_enabled,
            persist::client_exposed(&self.data_dir),
            require_encryption,
        );
        // Who the acceptor will treat as a member. Reads the LIVE mesh on every
        // dial rather than a snapshot: a node that left must lose reachability
        // with its membership, and one that just joined must gain it without a
        // restart. `removed_at` tombstones are excluded here and nowhere else.
        let member_check: crate::iroh_access::MemberCheck = {
            let app_state = app_state.clone();
            Arc::new(move |dialer: commonwealth_core::ids::NodePubkey| {
                let app_state = app_state.clone();
                Box::pin(async move {
                    let mesh = app_state.inner.mesh.read().await;
                    mesh.members
                        .values()
                        .any(|m| m.removed_at.is_none() && m.node_pubkey == Some(dialer))
                })
            })
        };
        let iroh_access = crate::iroh_access::MeshIrohAccess::start(
            &self.data_dir,
            internal_port,
            peer_addr,
            guest_addr,
            member_check.clone(),
            iroh_enabled,
            &iroh_relay_cfg,
        )
        .await;
        // Which classes route over iroh, and which of those are
        // REQUIRED (no plaintext fallback). Under `require_encryption`
        // the policy is the driver: every class routes over iroh AND is
        // required. Otherwise iroh-first is the default for EVERY class
        // with no required classes (prefer-iroh, fall back to IP per
        // dial); `[iroh.transport] <class> = "ip"` opts a class out.
        let (iroh_routed_classes, iroh_required_classes): (
            Vec<commonwealth_transport::TrafficClass>,
            std::collections::HashSet<commonwealth_transport::TrafficClass>,
        ) = if require_encryption {
            (
                commonwealth_transport::TrafficClass::ALL.to_vec(),
                commonwealth_transport::TrafficClass::ALL
                    .into_iter()
                    .collect(),
            )
        } else {
            (
                crate::iroh_access::iroh_routed_classes(&iroh_transport_cfg),
                std::collections::HashSet::new(),
            )
        };
        // W2: publish our own dial info so peers can reach us by key.
        // The gossip self-stamp pulls this each round and writes
        // relay_url + iroh_direct_addrs into our `MemberRecord` — the
        // "membership = dialability" collapse. RwLock-based install, so
        // it's exempt from the `Arc::get_mut` ordering constraint above.
        if let Some(access) = &iroh_access {
            install_iroh_access(
                &app_state,
                access,
                &iroh_routed_classes,
                &iroh_required_classes,
                &ip_transport,
                require_encryption,
            );
        } else if require_encryption {
            // The mesh-wide policy demands encryption but the iroh
            // endpoint failed to bind — we cannot enforce no-plaintext,
            // so refuse to start rather than silently downgrade. This is
            // the WS-B hard-fail: "encryption required but iroh unbound".
            return Err(MeshError::Config(
                "mesh requires encryption but the iroh endpoint failed to bind; \
                 refusing to start on a plaintext transport"
                    .into(),
            ));
        } else if crate::iroh_access::has_explicit_iroh_routes(&iroh_transport_cfg) {
            // Under opt-out semantics `iroh_routed_classes` is non-empty
            // even for an empty section, so this warning keys off
            // explicit `"iroh"` entries — someone wrote config that
            // cannot take effect while the endpoint is off.
            warn!(
                "iroh(mesh): [iroh.transport] names iroh for one or more classes but the \
                 iroh endpoint is off — staying on IP. Set [iroh] enabled=true to activate."
            );
        }

        // Founder reachability watchdog (Track W hardening): spawn only when the
        // iroh endpoint is up. It self-heals a wedged relay/discovery layer
        // (nudge → relay bounce → in-process endpoint rebuild) so an idle founder
        // never silently becomes undialable — no daemon restart required. The
        // rebuild closure lives here (not in the watchdog) so all DaemonState
        // mutation stays in this module; it re-runs `install_iroh_access` against
        // the fresh endpoint, exactly as start does.
        let reachability_watchdog = iroh_access.as_ref().map(|access| {
            let endpoint = access.endpoint_handle();
            let state = self.state.clone();
            let data_dir = self.data_dir.clone();
            let relay_cfg = iroh_relay_cfg.clone();
            let ip_tx = ip_transport.clone();
            let routed = iroh_routed_classes.clone();
            let required = iroh_required_classes.clone();
            let member_check = member_check.clone();
            let rebuild: crate::iroh_watchdog::RebuildFn = Arc::new(move || {
                let state = state.clone();
                let data_dir = data_dir.clone();
                let relay_cfg = relay_cfg.clone();
                let ip_tx = ip_tx.clone();
                let routed = routed.clone();
                let required = required.clone();
                let member_check = member_check.clone();
                Box::pin(async move {
                    let new = crate::iroh_access::MeshIrohAccess::start(
                        &data_dir,
                        internal_port,
                        peer_addr,
                        guest_addr,
                        member_check.clone(),
                        iroh_enabled,
                        &relay_cfg,
                    )
                    .await
                    .ok_or_else(|| {
                        "endpoint rebuild: start() returned None (bind failed or disabled)"
                            .to_string()
                    })?;
                    let new_ep = new.endpoint_handle();
                    // Swap the endpoint + re-run the installs under the write lock.
                    // No `.await` is held across the lock (start() already ran).
                    let mut guard = state.write().await;
                    if let DaemonState::Running {
                        iroh_access,
                        app_state,
                        ..
                    } = &mut *guard
                    {
                        install_iroh_access(
                            app_state,
                            &new,
                            &routed,
                            &required,
                            &ip_tx,
                            require_encryption,
                        );
                        *iroh_access = Some(new);
                        Ok(new_ep)
                    } else {
                        Err("endpoint rebuild: daemon no longer Running".to_string())
                    }
                })
                    as std::pin::Pin<
                        Box<
                            dyn std::future::Future<
                                    Output = Result<commonwealth_transport::iroh::Endpoint, String>,
                                > + Send,
                        >,
                    >
            });
            let mut cfg = crate::iroh_watchdog::WatchdogConfig::from_env();
            cfg.self_probe = iroh_relay_cfg.n0_services;
            // Relay-home is a health signal only when this node actually uses a
            // relay (n0 or a configured one). A relay-less LAN/air-gapped node
            // (netns soak) is reachable by direct addrs — don't rebuild-loop it.
            cfg.relays_expected =
                iroh_relay_cfg.n0_services || !iroh_relay_cfg.relay_urls.is_empty();
            crate::iroh_watchdog::spawn(endpoint, rebuild, cfg)
        });

        let mut state = self.state.write().await;
        *state = DaemonState::Running {
            app_state,
            mesh_state,
            client_addr,
            mdns,
            _browse_handle: browse_handle,
            _gossip_handle: gossip_handle,
            _collaborate_handle: collaborate_handle,
            _shutdown_tx: shutdown_tx,
            serve_handle,
            iroh_access,
            reachability_watchdog,
        };

        Ok(())
    }

    /// Fire a bounded initial gossip round so a freshly-resumed or
    /// freshly-joined daemon reconciles with peers within ~2s
    /// instead of waiting a full `DEFAULT_GOSSIP_INTERVAL`. Callers
    /// invoke this after each of `create_mesh` / `join_mesh` /
    /// `try_resume` returns.
    async fn trigger_initial_sync(&self) {
        let state = self.state.read().await;
        if let DaemonState::Running { app_state, .. } = &*state {
            gossip::initial_sync(
                app_state,
                gossip::DEFAULT_OFFLINE_THRESHOLD,
                std::time::Duration::from_secs(2),
            )
            .await;
        }
    }
}

/// Bind a TCP listener, retrying briefly on `EADDRINUSE`.
///
/// An in-process mesh re-create (`leave_to_solo` → `create_mesh` →
/// `start_daemon`) can momentarily race the just-dropped listener socket
/// from the previous mesh. `stop_inner` already awaits the old serve task,
/// so this is belt-and-suspenders — but `SO_REUSEADDR` (which mio sets)
/// only lets a new bind past a socket in `TIME_WAIT`, NOT one still in
/// `LISTEN`, so if the old task is slow to drop we give it a few tries.
///
/// On any non-`EADDRINUSE` error, or after exhausting retries, this returns
/// `MeshError::Network`; the caller (the serve task) logs it and returns
/// best-effort — a hard `start_daemon` failure here would strand the many
/// default-port tests that bind `:9741` under parallel contention.
async fn bind_listener_with_retry(
    addr: SocketAddr,
    label: &str,
) -> Result<tokio::net::TcpListener, MeshError> {
    const ATTEMPTS: usize = 5;
    const BACKOFF: std::time::Duration = std::time::Duration::from_millis(100);
    let mut last_err: Option<std::io::Error> = None;
    for attempt in 1..=ATTEMPTS {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => return Ok(listener),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                warn!(
                    %addr, attempt, attempts = ATTEMPTS,
                    "bind {label}: address in use — retrying in {}ms (old listener \
                     may still be releasing)",
                    BACKOFF.as_millis()
                );
                last_err = Some(e);
                tokio::time::sleep(BACKOFF).await;
            }
            Err(e) => {
                return Err(MeshError::Network(format!(
                    "bind {label} on {addr} failed: {e}"
                )));
            }
        }
    }
    Err(MeshError::Network(format!(
        "bind {label} on {addr} failed after {ATTEMPTS} attempts: {}",
        last_err
            .map(|e| e.to_string())
            .unwrap_or_else(|| "address in use".to_string())
    )))
}

/// Write minimal `ModelInfo` entries into the inference store for
/// each configured local slot. The `/v1/models` handler reads from
/// this store, so without these registrations a freshly-set-up
/// Phase 3 takeover: when the daemon is starting, look for a PID
/// file written by `sovereign serve --background` (which `sovereign
/// init` invokes before the user gets around to running the
/// daemon). If we find a live process, SIGTERM it and wait briefly
/// so the port is free by the time we bind. The pid pointer lives
/// at `~/.svrnmesh/server.pid` so this works regardless of which
/// project directory the daemon is launched from.
///
/// This is best-effort. Failures are logged at info level and the
/// caller proceeds — if the port really is held by something the
/// daemon can't displace, the subsequent `bind()` will fail loudly
/// with the actual error. We don't want this helper to be a
/// hard-stop in the daemon path.
fn takeover_standalone_serve_if_present() {
    let pid_path = sovereign_contracts::rebrand::svrnmesh_root().join("server.pid");
    takeover_serve_at(&pid_path);
}

/// Takeover, parameterized over the pid-pointer path. Split from the
/// HOME-resolving wrapper above so unit tests can exercise the
/// stale-pid / malformed-pid / self-pid branches against a tempdir
/// without mutating `$HOME` (which would race across cargo's
/// threaded test runner).
fn takeover_serve_at(pid_path: &Path) {
    let Ok(contents) = std::fs::read_to_string(pid_path) else {
        return; // No file is the common case: clean boot, no prior init.
    };
    let Ok(pid) = contents.trim().parse::<i32>() else {
        warn!(path = %pid_path.display(), "takeover: malformed pid file");
        let _ = std::fs::remove_file(pid_path);
        return;
    };
    if pid == std::process::id() as i32 {
        // We somehow inherited our own pid file (shouldn't happen
        // in production, but possible in tests where the same
        // binary writes the pointer and then becomes the daemon).
        let _ = std::fs::remove_file(pid_path);
        return;
    }
    let killed = std::process::Command::new("/bin/kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if killed {
        info!(pid, "daemon: signalled standalone serve to release :9741");
        // Give the child a moment to release the listener. axum's
        // graceful-shutdown is fast; 1s is plenty in practice. We
        // could poll the port instead, but on slow CI this would
        // over-engineer the wait — the bind() retry below catches
        // anything we miss.
        std::thread::sleep(std::time::Duration::from_millis(1000));
    } else {
        info!(pid, "daemon: stale serve pid file (process gone) — cleared");
    }
    let _ = std::fs::remove_file(pid_path);
}

/// daemon answers the endpoint with an empty list — misleading for
/// anyone running it as a smoke check after `sovereign setup`.
///
/// The `name` field is the file basename (stripped of `.gguf`)
/// because OpenAI-compatible clients use it as the user-visible
/// model id. The `ModelId` is a deterministic hash of the absolute
/// path so repeated calls (e.g. after an admin/reload) don't
/// accumulate duplicate entries keyed on different random IDs.
fn register_local_model_slots(app_state: &AppState, cfg: &SetupConfig, node_id: NodeId) {
    use commonwealth_core::ids::ModelId;
    use commonwealth_inference::model::{ModelArchitecture, ModelInfo};
    use commonwealth_inference::oicp::CapabilityProfile;
    use std::collections::HashMap;
    use std::hash::{DefaultHasher, Hash, Hasher};

    // A node with no `[models]` registers no slots — the mirror of
    // `capacity::build_slots_from_config`'s early return, which that function's
    // doc asks to be kept in sync. Registering nothing is what makes a
    // `terminal` honest end to end: no local slot in the store, and therefore
    // nothing for `build_self_manifest` to advertise to peers.
    let Some(models) = cfg.models.as_ref() else {
        tracing::info!(
            node = %node_id,
            "register_local_model_slots: no [models] — terminal node, registering none"
        );
        return;
    };

    let mut slots: Vec<(String, &std::path::Path)> = vec![
        ("primary".into(), models.primary.as_path()),
        ("embed".into(), models.embed.as_path()),
    ];
    // Mesh-advertise fast only when it's a distinct GGUF. If the
    // primary subsumes the fast role, a separate "fast" advertisement
    // would mislead peers into thinking there are two chat models on
    // this node when there's actually one.
    if models.has_explicit_fast() {
        slots.push(("fast".into(), models.fast_path()));
    }
    if let Some(code_path) = models.code.as_ref() {
        slots.push(("code".into(), code_path.as_path()));
    }
    // Multi-primary pool: register N additional primary-class slots so
    // a high-VRAM host (e.g. MI300X 192 GB) can serve concurrent
    // chat-completion requests without queueing against a single slot.
    // Each pool member is registered under `primary_<i>` and points at
    // the same GGUF; the OICP capability advertiser surfaces them as
    // distinct claims so the scheduler can dispatch round-robin.
    if let Some(pool) = models.primary_pool.as_ref() {
        for i in 0..pool.copies {
            slots.push((format!("primary_{i}"), pool.path.as_path()));
        }
    }
    // Operator-declared additional chat slots from `[models.extra]`
    // also need to land in `inference_store` so `/v1/models`
    // advertises them. Without this entry, clients sending
    // `model: "<extras-stem>"` would see a 404 from the OICP
    // capability lookup before the slot picker ever runs.
    for (slot_name, path) in models.extra.iter() {
        slots.push((format!("extras:{slot_name}"), path.as_path()));
    }

    // Build a slot-name → model_id map so OpenAI-shape clients can
    // address slots by role (`primary`, `fast`, `code`) instead of
    // GGUF stem. The same stem is registered under both the bare
    // alias (`primary`) and a `commonwealth/`-namespaced form so
    // opencode's provider/model addressing convention works without
    // the operator hand-curating their `provider.commonwealth.models`
    // map. Code-slot also gets a `coder` synonym since OICP's hint
    // vocabulary calls the capability `code` while operators
    // colloquially say "coder".
    let mut slot_aliases: HashMap<String, String> = HashMap::new();

    for (role, path) in &slots {
        let role: &str = role.as_str();
        let Some(file_name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let name = file_name.trim_end_matches(".gguf").to_string();

        // Deterministic ID: a 128-bit hash of the absolute path. Two
        // calls with the same path produce the same ModelId (matters
        // for reload — we want to update the entry, not add a twin).
        let mut h = DefaultHasher::new();
        path.hash(&mut h);
        let lo = h.finish();
        let mut h = DefaultHasher::new();
        role.hash(&mut h);
        path.hash(&mut h);
        let hi = h.finish();
        let id = ModelId::from_u128((u128::from(hi) << 64) | u128::from(lo));

        // Leave `available_on` empty. JSON map keys must be strings,
        // but `NodeId` serializes as a byte array — populating this
        // HashMap makes `serde_json::to_vec` (write path) succeed but
        // `serde_json::from_slice` (read path in `list_models`) fail,
        // so entries silently vanish from `/v1/models`. The scheduler
        // recomputes availability from live gossip anyway.
        let _ = node_id; // keep the parameter meaningful for callers
        let available_on = HashMap::new();

        let info = ModelInfo {
            id,
            name,
            repo: String::new(), // local file — no upstream repo
            file: file_name.to_string(),
            size_bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            total_layers: 0, // unknown without loading — scheduler tolerates 0
            architecture: ModelArchitecture::Other,
            available_on,
            oicp_capabilities: CapabilityProfile::default(),
            quantization: String::new(),
            min_memory_gb: 0,
            preferred_memory_gb: 0,
            supports_parallel_instances: false,
            supports_pipeline_shard: false,
        };
        app_state.inner.inference_store.set_model_info(&info);
        info!(
            role,
            name = %info.name,
            "registered local model in inference_store"
        );

        // Add the slot alias entries. Skip extras: they're routed by
        // their slot key directly (the `[models.extra]` map already
        // gives the operator a stable name); only the canonical four
        // (primary/fast/embed/code) need alias indirection because
        // their backing GGUF can swap freely. The alias vocabulary is
        // defined ONCE in `slot_aliases::SLOT_ALIAS_POLICY` — shared
        // with `oicp_synthesis::build_self_manifest`'s advertisement
        // side so the two can't drift (the 2026-05-19 fast-alias 503).
        for key in crate::slot_aliases::resolution_alias_keys(role) {
            slot_aliases.insert(key, info.name.clone());
        }
    }

    if !slot_aliases.is_empty() {
        info!(
            count = slot_aliases.len(),
            "installing slot alias map for chat_completions / list_models"
        );
        app_state.install_slot_aliases(slot_aliases);
    }

    // Install the servable-model-files allowlist so peers can
    // pull these GGUFs via `/internal/v1/models/list` +
    // `/internal/v1/models/file/:name`. Dedup by canonical path
    // — `primary_pool` slots all point at the same file as the
    // primary slot, and there's no point advertising it three
    // times. See `commonwealth-api::routes_internal::model_files`.
    //
    // A slot path that names one shard of a SPLIT GGUF is expanded to the
    // whole shard set. Config names only `…-00001-of-0000N.gguf`, so without
    // this the host advertises (and `serve_model_file` will serve) shard 1
    // alone and 404s the rest — which strands any worker that does not
    // already hold every shard on disk. Both warm paths die there: the
    // default whole-GGUF fetch on `NotAdvertised`, the byte-range fetch on
    // "range GET failed on all sources". The failure is never-wedge safe
    // (warm falls back to local-only), so it presents not as an error but as
    // a big model mysteriously refusing to distribute. Found 2026-07-31
    // sizing a 5-shard 155 GB DeepSeek-V4-Flash split; every earlier
    // acceptance masked it by having all shards on every node.
    let paths: Vec<std::path::PathBuf> = slots.iter().map(|(_, p)| p.to_path_buf()).collect();
    let servable = servable_model_files(&paths);
    if !servable.is_empty() {
        info!(
            files = servable.len(),
            "installing servable model files allowlist for peer fetch"
        );
        app_state.install_servable_model_files(servable);
    }
}

/// The set of files peers may fetch, derived from the configured slot paths:
/// every shard of a split GGUF, canonicalized, deduped, in slot order.
///
/// Split expansion is the load-bearing part. Config names one shard
/// (`…-00001-of-0000N.gguf`); `shard_files` turns that into the whole set when
/// — and only when — every sibling is actually on disk, so we never advertise
/// a file we cannot serve. Dedup matters because `primary_pool` slots all
/// point at the same GGUF.
fn servable_model_files(slot_paths: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    let mut seen: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    for path in slot_paths {
        let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let shards = sovereign_inference::embedded::shard_files(&canon);
        if shards.len() > 1 {
            info!(
                shards = shards.len(),
                model = %canon.display(),
                "split GGUF: advertising all shards for peer fetch"
            );
        }
        for shard in shards {
            if seen.insert(shard.clone()) {
                out.push(shard);
            }
        }
    }
    out
}

/// A peer's inference service, as seen by the local
/// `MeshInferenceProvider`. One per online, non-self member at the
/// moment `peer_inference_endpoints()` was called.
#[derive(Debug, Clone)]
pub struct PeerInferenceEndpoint {
    pub node_id: NodeId,
    pub name: String,
    /// Candidate base URLs in try-order. Each is a
    /// `http://<ip>:9741/v1` prefix ready to hand to
    /// `RemoteApiProvider::new`. Multiple when the peer is
    /// dual-homed (WiFi + Tailscale); the wrapper tries them in
    /// order until one succeeds — same policy as gossip + fan-out.
    pub base_urls: Vec<String>,
    /// Peer's gossiped `system_ram_gb`. Used as a crude-but-
    /// correct-direction signal in the v1 routing heuristic:
    /// only route synthesis to a peer whose RAM exceeds ours, so
    /// a big-box Founder+small-box Joiner pair does the right
    /// thing without us implementing full OICP manifest scoring
    /// up-front. Proper per-model OICP matching is the Stage 2.1
    /// follow-up.
    pub system_ram_gb: u32,
    /// Peer's gossiped baseline-model benchmark. Feeds the
    /// throughput-extrapolation path in [`oicp::throughput_factor`]
    /// when we score the peer's manifest. `None` when the peer is
    /// running an older daemon (no benchmark field) or hasn't
    /// completed its startup probe yet — in either case the
    /// scheduler falls back to observation-only throughput scoring,
    /// which degrades to neutral 1.0 below the sample threshold.
    pub benchmark: Option<sovereign_core::oicp::BenchmarkResult>,
    /// Peer's gossiped self-reported concurrent inference count.
    /// Authoritative: peers count requests they serve from their
    /// own local user — traffic the founder never originated and
    /// `peer_observations[name].in_flight` is structurally blind
    /// to. Used by `select_peer` to override the founder-local view
    /// when present. `None` for older peers (gossip field absent);
    /// scoring falls back to `peer_observations` in that case.
    /// See `sovereign/docs/MESH_LOAD_AWARENESS.md`.
    pub current_in_flight: Option<u32>,
    /// Peer's gossiped `inference_availability` (0.0–1.0; 1.0 =
    /// fully idle, written by the peer's ActivityReporter).
    /// Multiplied into the OICP score (clamped to ≥0.2 so a busy
    /// peer stays routable) — adopted 2026-06-10; the signal was
    /// previously gossiped but ignored by routing.
    pub inference_availability: Option<f32>,
    /// `MemberRecord::last_seen` for the gossip record the two load
    /// signals above were read from (unix seconds; `0` = unknown).
    ///
    /// This is the **staleness** half of the two-field pair that P2
    /// of `docs/specs/SCHEDULER_QUALITY.md` exists to measure. F1 is
    /// that a decider sees its own load exactly and every peer's a
    /// full anti-entropy round or more late; a load value without
    /// its age cannot distinguish "the hub is idle" from "the hub
    /// was idle thirty seconds ago." Nothing routes on this — the
    /// scorer never reads it — but every decision record stamps
    /// `now - last_seen` next to the value it scored, which turns
    /// F1's dead time from a modelled 10–30s hypothesis into a
    /// measured distribution, and gives the Tier-1 simulator its
    /// most load-bearing parameter as data instead of a guess.
    pub gossip_last_seen_unix: u64,
    /// How to actually open a connection to this endpoint.
    ///
    /// `None` is the default mesh transport — plain HTTP to `base_urls`,
    /// gossip-issued bearer (or no bearer). `Some(transport)` means
    /// route through a TLS-pinned `reqwest::Client` carrying the
    /// owner-signed `WorkerToken`, the way ephemeral worker pods are
    /// authenticated. See `crate::pinned_transport`.
    ///
    /// The scoring, manifest fetch, throughput tracking, and fan-out
    /// fallback paths in `peer_inference.rs` are oblivious to this
    /// field — they only consume `node_id`, `name`, `base_urls`, and
    /// the load signals. The hot-path call site that actually opens
    /// the HTTP connection is the only place that branches on it.
    /// Spec: `sovereign/docs/PINNED_WORKER_AS_INFERENCE_PEER.md`.
    pub transport: Option<crate::pinned_transport::PinnedTransport>,
}

/// How RPC-worker discovery uses the iroh bridge for ggml's raw-TCP
/// endpoint (`SOVEREIGN_RPC_TUNNEL`): `auto` (default) bridges only when
/// no direct member IP answers; `always` prefers the bridge (E2E forcing,
/// known-cross-network meshes); `never` disables bridging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RpcTunnelMode {
    Auto,
    Always,
    Never,
}

/// Pure parse — unit-testable without touching the process environment.
fn rpc_tunnel_mode_from(v: Option<&str>) -> RpcTunnelMode {
    match v.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("always") => RpcTunnelMode::Always,
        Some("never") | Some("off") | Some("0") => RpcTunnelMode::Never,
        None | Some("") | Some("auto") => RpcTunnelMode::Auto,
        Some(other) => {
            tracing::warn!(
                value = %other,
                "SOVEREIGN_RPC_TUNNEL: unknown value, using `auto` (accepted: auto|always|never)"
            );
            RpcTunnelMode::Auto
        }
    }
}

fn rpc_tunnel_mode() -> RpcTunnelMode {
    rpc_tunnel_mode_from(std::env::var("SOVEREIGN_RPC_TUNNEL").ok().as_deref())
}

/// A worker endpoint choice carried across discovery ticks so a single transient
/// probe miss can't flip a healthy worker's transport identity. `direct_misses`
/// counts consecutive ticks a *proven* direct-ip endpoint was unreachable while
/// we held it (reset the moment direct-ip answers again).
#[derive(Debug, Clone, PartialEq)]
struct StickyEndpoint {
    endpoint: String,
    via: String,
    direct_misses: u32,
}

impl StickyEndpoint {
    /// A direct raw-TCP endpoint to a member IP — the only transport we hold
    /// through a blip. The `via` label is the source of truth (set at selection).
    fn is_direct(&self) -> bool {
        self.via == "direct-ip"
    }

    /// A loopback endpoint served by an iroh bridge to the peer.
    fn is_bridge(&self) -> bool {
        self.via.starts_with("iroh-bridge")
    }
}

/// How a discovery tick should re-establish the endpoint of a peer we already
/// hold a choice for. Split out from the IO so the "never re-probe a known
/// worker over the link its own tensors are saturating" rule is a unit-testable
/// policy rather than a branch buried in a 200-line async method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reaffirm {
    /// Re-use the held endpoint verbatim — no network at all.
    Held,
    /// Re-resolve the peer's iroh bridge. Loopback-local against the transport's
    /// bridge cache: no WAN round-trip, so congestion can't starve it.
    Rebridge,
    /// Nothing worth re-affirming — run the full `/status` probe.
    FullProbe,
}

/// The re-affirm policy for one peer, given last tick's held choice.
///
/// Both known-worker cases rest on the SAME evidence: the peer already passed
/// this tick's gossip-Online + dialable membership filter, and gossip rides a
/// separate path with a looser budget than any probe we could run here. What a
/// probe would add is not liveness but noise — it rides the very link the RPC
/// tensor traffic is saturating.
///
/// For a bridged worker that noise was load-bearing (2026-07-26 tunnel e2e): the
/// `/status` probe travels the same iroh path as the tunnel, and each timeout
/// left `fresh = None`, which `sticky_endpoint` turns into "worker absent" for a
/// non-direct endpoint — read downstream as a flap. The endpoint never moved
/// (`127.0.0.1:40021` for six straight minutes) yet the tracker logged
/// `flaps=9 quarantine_count=5 cooldown_secs=300`, excluding a peer that was
/// serving the whole time. Re-minting the bridge instead touches only loopback.
///
/// A dead rpc-server behind live gossip is NOT this function's problem in either
/// case — it surfaces when ggml's RPC connection fails, via supervised reload
/// (DAEMON_RESILIENCE P0.4), not via a discovery probe.
///
/// **Stated trade-off:** a bridged worker is never re-probed for a direct IP, so
/// under `auto` a peer that fell back to the tunnel stays on it rather than
/// upgrading back to raw LAN TCP. This is deliberate and narrow: `auto` prefers
/// direct-ip at selection, so becoming bridged at all means direct was
/// unreachable at first sight; cross-network peers (the case this path exists
/// for) can never be direct; `always` wants the tunnel by definition; and the
/// pin clears on the peer's next Offline→Online cycle, which prunes stickiness.
/// The upgrade probe is deferrable, but if added it must be an UPGRADE ONLY —
/// its failure may never drop the worker, or it re-opens the flap this closed.
fn reaffirm_plan(prev: Option<&StickyEndpoint>, tunnel: RpcTunnelMode) -> Reaffirm {
    match prev {
        Some(p) if p.is_direct() => Reaffirm::Held,
        // `never` means the operator has opted out of bridging; re-probe so the
        // worker can move to a direct address (or drop out) rather than be
        // pinned to a tunnel we're no longer allowed to use.
        Some(p) if p.is_bridge() && tunnel != RpcTunnelMode::Never => Reaffirm::Rebridge,
        _ => Reaffirm::FullProbe,
    }
}

/// Consecutive direct-ip probe misses tolerated before a worker's endpoint is
/// allowed to flip to a fallback transport (iroh-bridge / probe-host) or be
/// dropped. Default 3 — roughly three ~15s discovery ticks (~45s) of a proven
/// direct-ip being unreachable before we treat the address as durably changed.
/// Env-overridable for pathological links; clamped to ≥1 (0 would disable the
/// guard and re-introduce the flip-on-one-miss bug).
fn rpc_endpoint_flip_threshold() -> u32 {
    std::env::var("SOVEREIGN_RPC_ENDPOINT_FLIP_THRESHOLD")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(3)
}

/// Hysteresis over the per-tick endpoint selection: a proven **direct-ip**
/// endpoint is not demoted to a fallback — nor dropped — on a transient miss.
/// We hold it for up to `flip_threshold` consecutive misses so a single
/// congested-Wi-Fi probe timeout can't flip the endpoint STRING that the
/// eligibility tracker and the reload loop key on (which reads as a flap +
/// full re-settle → live distribution collapses to local-only, 2026-07-19
/// 122B e2e).
///
/// - `prev`: last tick's held choice for this node (`None` on first sight).
/// - `fresh`: what raw probing selected THIS tick — `(endpoint, via)` — or
///   `None` when nothing was reachable at all.
///
/// Returns the choice to advertise this tick, or `None` to drop the worker.
/// A bridge/probe-host worker (no proven direct-ip to protect) is dropped the
/// moment it's unreachable — only direct-ip gets the hold.
fn sticky_endpoint(
    prev: Option<&StickyEndpoint>,
    fresh: Option<(String, String)>,
    flip_threshold: u32,
) -> Option<StickyEndpoint> {
    // Would holding `prev` for one more miss stay within budget?
    let can_hold = |p: &StickyEndpoint| p.is_direct() && p.direct_misses + 1 < flip_threshold;
    let held = |p: &StickyEndpoint| StickyEndpoint {
        endpoint: p.endpoint.clone(),
        via: p.via.clone(),
        direct_misses: p.direct_misses + 1,
    };
    match fresh {
        // Direct-ip verified reachable this tick — always take it, reset misses.
        Some((endpoint, via)) if via == "direct-ip" => Some(StickyEndpoint {
            endpoint,
            via,
            direct_misses: 0,
        }),
        // A fallback was selected → direct-ip missed. Hold the proven direct-ip
        // through the blip if we can; otherwise accept the fallback.
        Some((endpoint, via)) => match prev {
            Some(p) if can_hold(p) => Some(held(p)),
            _ => Some(StickyEndpoint {
                endpoint,
                via,
                direct_misses: 0,
            }),
        },
        // Nothing reachable at all. Hold a proven direct-ip through a transient
        // total miss; otherwise the worker is gone this tick.
        None => match prev {
            Some(p) if can_hold(p) => Some(held(p)),
            _ => None,
        },
    }
}

/// Mint (or reuse — the transport caches one bridge per peer per ALPN) a
/// bridge-local endpoint for `member`'s ggml rpc-server via the
/// `RpcTensor` traffic class. Returns `("127.0.0.1:<port>", via_label)` —
/// the scheme is stripped because ggml dials the authority verbatim.
/// `None` when the transport has no iroh path to the peer (plaintext
/// mesh, no pubkey, class pinned to ip).
///
/// Deliberately NOT TCP-probed: a loopback bridge accepts instantly
/// regardless of whether the peer is dialable, so a connect probe is a
/// false positive by construction. The peer's gossip-Online status (a
/// prerequisite for reaching this code) plus the eligibility settle gate
/// is the liveness evidence — the same ≤1-discovery-tick exposure window
/// raw-TCP workers already have.
async fn bridge_rpc_endpoint(
    transport: &Arc<dyn commonwealth_transport::PeerTransport>,
    member: &commonwealth_core::mesh::MemberRecord,
) -> Option<(String, String)> {
    let candidates = transport
        .endpoints(
            &commonwealth_transport::peer_contact(member),
            commonwealth_transport::TrafficClass::RpcTensor,
        )
        .await;
    let ep = candidates.into_iter().next()?;
    let authority = ep.base_url.strip_prefix("http://")?.to_string();
    // The bridge hands back a loopback authority; anything else means a
    // transport misroute — refuse rather than hand ggml a bad endpoint.
    let addr: std::net::SocketAddr = authority.parse().ok()?;
    if !addr.ip().is_loopback() {
        tracing::warn!(
            endpoint = %authority,
            label = %ep.label,
            "rpc bridge endpoint is not loopback — refusing (transport misroute?)"
        );
        return None;
    }
    Some((authority, format!("iroh-bridge:{}", ep.label)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::setup_config::{DaemonSection, DataSection, ModelsSection, SetupConfig};
    use std::path::PathBuf;

    fn direct(ep: &str) -> Option<(String, String)> {
        Some((ep.to_string(), "direct-ip".to_string()))
    }
    fn bridge(ep: &str) -> Option<(String, String)> {
        Some((ep.to_string(), "iroh-bridge:x".to_string()))
    }

    #[test]
    fn sticky_takes_fresh_direct_ip_immediately() {
        // First-ever sight of a verified direct-ip: no prior, take it, misses=0.
        let s = sticky_endpoint(None, direct("10.0.0.9:50052"), 3).unwrap();
        assert_eq!(s.endpoint, "10.0.0.9:50052");
        assert!(s.is_direct());
        assert_eq!(s.direct_misses, 0);
    }

    /// The regression this file's split-expansion comment describes: a slot
    /// configured at shard 1 of a split GGUF must make ALL shards servable.
    /// Advertising only shard 1 404s the rest, which strands any worker that
    /// doesn't already hold the whole model — and because warm failure is
    /// never-wedge safe, it surfaces as "the big model won't distribute"
    /// rather than as an error.
    #[test]
    fn servable_files_expand_a_split_gguf_to_every_shard() {
        let dir = tempfile::tempdir().unwrap();
        let mk = |name: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, b"x").unwrap();
            p
        };
        let s1 = mk("big-00001-of-00003.gguf");
        let s2 = mk("big-00002-of-00003.gguf");
        let s3 = mk("big-00003-of-00003.gguf");
        let solo = mk("embed.gguf");

        // Config names shard 1 only; all three must become servable.
        let got = servable_model_files(&[s1.clone(), solo.clone()]);
        let canon = |p: &std::path::PathBuf| p.canonicalize().unwrap();
        assert_eq!(
            got,
            vec![canon(&s1), canon(&s2), canon(&s3), canon(&solo)],
            "split slot must advertise every shard, in order, then the solo slot"
        );

        // Dedup: primary_pool points several slots at the same GGUF.
        let got = servable_model_files(&[s1.clone(), s1.clone(), solo.clone()]);
        assert_eq!(
            got.len(),
            4,
            "same model twice must not be advertised twice"
        );
    }

    /// Never advertise what we cannot serve: with a sibling absent,
    /// `shard_files` refuses to guess, so we fall back to the named file.
    #[test]
    fn servable_files_do_not_guess_missing_shards() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t-00001-of-00002.gguf");
        std::fs::write(&p, b"x").unwrap();
        let got = servable_model_files(&[p.clone()]);
        assert_eq!(got, vec![p.canonicalize().unwrap()]);
    }

    #[test]
    fn sticky_holds_direct_ip_through_transient_misses_then_flips() {
        // The 2026-07-19 flap in miniature: a proven direct-ip must NOT flip to
        // the bridge on one miss — hold it until the threshold, THEN flip.
        let flip = 3;
        let s0 = sticky_endpoint(None, direct("10.0.0.5:50052"), flip).unwrap();
        // Miss 1: bridge offered, but hold direct-ip.
        let s1 = sticky_endpoint(Some(&s0), bridge("127.0.0.1:40001"), flip).unwrap();
        assert_eq!(s1.endpoint, "10.0.0.5:50052", "must not flip on one miss");
        assert!(s1.is_direct());
        assert_eq!(s1.direct_misses, 1);
        // Miss 2: still holding (2 < 3).
        let s2 = sticky_endpoint(Some(&s1), bridge("127.0.0.1:40001"), flip).unwrap();
        assert_eq!(s2.endpoint, "10.0.0.5:50052");
        assert_eq!(s2.direct_misses, 2);
        // Miss 3 reaches the threshold — NOW accept the bridge (durable change).
        let s3 = sticky_endpoint(Some(&s2), bridge("127.0.0.1:40001"), flip).unwrap();
        assert_eq!(s3.endpoint, "127.0.0.1:40001");
        assert_eq!(s3.via, "iroh-bridge:x");
        assert_eq!(s3.direct_misses, 0);
    }

    #[test]
    fn sticky_direct_ip_recovery_resets_miss_count() {
        let flip = 3;
        let s0 = sticky_endpoint(None, direct("10.0.0.5:50052"), flip).unwrap();
        let s1 = sticky_endpoint(Some(&s0), None, flip).unwrap(); // total miss → hold
        assert_eq!(s1.direct_misses, 1);
        // Direct-ip answers again → back to a clean slate.
        let s2 = sticky_endpoint(Some(&s1), direct("10.0.0.5:50052"), flip).unwrap();
        assert!(s2.is_direct());
        assert_eq!(s2.direct_misses, 0);
    }

    #[test]
    fn sticky_drops_a_non_direct_worker_when_unreachable() {
        // A bridge-only worker (no proven direct-ip to protect) is dropped the
        // moment it's unreachable — nothing to hold.
        let bridge_only = StickyEndpoint {
            endpoint: "127.0.0.1:1".to_string(),
            via: "iroh-bridge:x".to_string(),
            direct_misses: 0,
        };
        assert!(sticky_endpoint(Some(&bridge_only), None, 3).is_none());
    }

    fn held(via: &str) -> StickyEndpoint {
        StickyEndpoint {
            endpoint: "127.0.0.1:40021".to_string(),
            via: via.to_string(),
            direct_misses: 0,
        }
    }

    #[test]
    fn reaffirm_probes_only_what_it_has_never_seen() {
        use RpcTunnelMode::*;
        // First sight of a peer: nothing held, so the full probe is the only way
        // to learn whether it serves an RPC worker at all.
        assert_eq!(reaffirm_plan(None, Auto), Reaffirm::FullProbe);
        // A proven direct-ip is re-affirmed from cache (2026-07-19 guard).
        assert_eq!(
            reaffirm_plan(Some(&held("direct-ip")), Auto),
            Reaffirm::Held
        );
        // A probe-host fallback is a last resort, not evidence of anything —
        // keep re-probing so it can be promoted to a real transport.
        assert_eq!(
            reaffirm_plan(Some(&held("probe-host")), Auto),
            Reaffirm::FullProbe
        );
    }

    #[test]
    fn reaffirm_never_reprobes_a_known_bridged_worker_over_its_own_tunnel() {
        // THE 2026-07-26 REGRESSION. A bridged worker was re-probed via
        // `/status` every tick; that probe rides the same iroh path as the
        // tunnel, so under load it timed out, `fresh` went None, and
        // `sticky_endpoint` drops a non-direct endpoint on a miss (asserted in
        // `sticky_drops_a_non_direct_worker_when_unreachable`) — which the
        // eligibility tracker reads as a flap. Observed: endpoint pinned at
        // 127.0.0.1:40021 for six minutes while flaps climbed to 9 and the
        // cooldown compounded to 300s, excluding a peer that was serving.
        for via in ["iroh-bridge:x", "iroh-bridge:iroh:127.0.0.1:40021→86627fd5"] {
            assert_eq!(
                reaffirm_plan(Some(&held(via)), RpcTunnelMode::Auto),
                Reaffirm::Rebridge,
                "{via} must be re-minted from the local bridge cache, never re-probed"
            );
            assert_eq!(
                reaffirm_plan(Some(&held(via)), RpcTunnelMode::Always),
                Reaffirm::Rebridge
            );
        }
    }

    #[test]
    fn reaffirm_respects_an_operator_opting_out_of_bridging() {
        // `SOVEREIGN_RPC_TUNNEL=never` withdraws permission to tunnel. Holding a
        // bridge endpoint would pin the worker to a transport we may no longer
        // use, so re-probe: it either surfaces at a direct address or drops out.
        assert_eq!(
            reaffirm_plan(Some(&held("iroh-bridge:x")), RpcTunnelMode::Never),
            Reaffirm::FullProbe
        );
        // The direct-ip hold is unaffected by the tunnel knob.
        assert_eq!(
            reaffirm_plan(Some(&held("direct-ip")), RpcTunnelMode::Never),
            Reaffirm::Held
        );
    }

    #[test]
    fn sticky_flip_threshold_one_disables_the_hold() {
        // threshold 1 = flip on the first miss (the pre-guard behaviour), so the
        // env knob's floor is a conscious opt-out, not a silent no-op.
        let s0 = sticky_endpoint(None, direct("10.0.0.5:50052"), 1).unwrap();
        let s1 = sticky_endpoint(Some(&s0), bridge("127.0.0.1:2"), 1).unwrap();
        assert_eq!(s1.endpoint, "127.0.0.1:2", "threshold 1 flips immediately");
    }

    #[test]
    fn rpc_tunnel_mode_parses_the_documented_values() {
        use RpcTunnelMode::*;
        assert_eq!(rpc_tunnel_mode_from(None), Auto);
        assert_eq!(rpc_tunnel_mode_from(Some("")), Auto);
        assert_eq!(rpc_tunnel_mode_from(Some("auto")), Auto);
        assert_eq!(rpc_tunnel_mode_from(Some("ALWAYS")), Always);
        assert_eq!(rpc_tunnel_mode_from(Some(" always ")), Always);
        assert_eq!(rpc_tunnel_mode_from(Some("never")), Never);
        assert_eq!(rpc_tunnel_mode_from(Some("off")), Never);
        assert_eq!(rpc_tunnel_mode_from(Some("0")), Never);
        // Unknown values degrade to the safe default, never panic.
        assert_eq!(rpc_tunnel_mode_from(Some("banana")), Auto);
    }

    #[test]
    fn rpc_endpoint_directory_records_and_resolves() {
        // The warm orchestrator resolves worker identity through this
        // directory; an unknown endpoint (env-configured worker) is None so
        // callers fall back to raw-IP addressing.
        let daemon = EmbeddedDaemon::in_memory(
            SetupConfig::unconfigured(),
            crate::daemon_services::DaemonServices::mesh_admin(),
        );
        assert_eq!(daemon.rpc_endpoint_node("10.0.0.7:50052"), None);

        let node = NodeId::from_u128(42);
        daemon
            .rpc_endpoint_nodes
            .write()
            .unwrap()
            .insert("10.0.0.7:50052".to_string(), node);
        assert_eq!(daemon.rpc_endpoint_node("10.0.0.7:50052"), Some(node));
        // Re-discovery overwrites in place — same endpoint, later owner wins.
        let other = NodeId::from_u128(43);
        daemon
            .rpc_endpoint_nodes
            .write()
            .unwrap()
            .insert("10.0.0.7:50052".to_string(), other);
        assert_eq!(daemon.rpc_endpoint_node("10.0.0.7:50052"), Some(other));
    }

    #[test]
    fn internal_bind_is_loopback_only_under_encryption() {
        // WS-C receiver lockout: an encrypted mesh binds the internal
        // router loopback-only (iroh acceptor is the sole network path);
        // a plaintext mesh keeps the historical wildcard bind.
        let encrypted = internal_bind_addr(true, "0.0.0.0", 9742);
        assert!(
            encrypted.ip().is_loopback(),
            "encrypted mesh must bind internal router loopback-only, got {encrypted}"
        );
        assert_eq!(encrypted.port(), 9742);

        let plaintext = internal_bind_addr(false, "0.0.0.0", 9742);
        assert!(
            plaintext.ip().is_unspecified(),
            "plaintext mesh keeps the 0.0.0.0 internal bind, got {plaintext}"
        );

        // A configured private bind is honoured on a plaintext mesh...
        let pinned = internal_bind_addr(false, "10.0.1.4", 9742);
        assert_eq!(pinned.to_string(), "10.0.1.4:9742");
        // ...but encryption still forces loopback, ignoring the config.
        let pinned_encrypted = internal_bind_addr(true, "10.0.1.4", 9742);
        assert!(pinned_encrypted.ip().is_loopback());
    }

    /// Regression for: after `sovereign setup`, `GET /v1/models`
    /// returned `{"data":[]}`. Root cause was that the daemon never
    /// registered its loaded model slots into `inference_store`, so
    /// Commonwealth's handler had nothing to list.
    #[test]
    fn register_local_model_slots_writes_info_for_all_three_slots() {
        use commonwealth_api::state::AppState;
        use commonwealth_core::mesh::Mesh;

        let mesh = Mesh {
            mesh_secret: [0u8; 32],
            invite_expires_at: None,
            id: commonwealth_core::ids::MeshId::generate(),
            name: "test".into(),
            invite_key_hash: [0u8; 32],
            invite_version: 0,
            require_encryption: false,
            members: Default::default(),
            peers: vec![],
        };
        let node_id = commonwealth_core::ids::NodeId::generate();
        let mesh_store = Arc::new(commonwealth_state::MeshStore::in_memory().unwrap());
        let app_registry = Arc::new(commonwealth_app::registry::AppRegistry::new());
        let app_state =
            AppState::new_with_platform_and_engine(node_id, mesh, mesh_store, app_registry, None);

        let cfg = SetupConfig {
            compute: Default::default(),
            search: Default::default(),
            models: Some(ModelsSection {
                primary: PathBuf::from("/m/qwen3-coder-30b.gguf"),
                fast: Some(PathBuf::from("/m/qwen3-1.7b.gguf")),
                embed: PathBuf::from("/m/qwen3-embedding-0.6b.gguf"),
                code: None,
                context_size: None,
                fast_context_size: None,
                max_extras_memory_gb: None,
                extra: std::collections::BTreeMap::new(),
                primary_pool: None,
                edit: None,
            }),
            node: Default::default(),
            daemon: DaemonSection::default(),
            data: DataSection::default(),
            watched_folders: Default::default(),
            memory: Default::default(),
            iroh: Default::default(),
            shared_model: Default::default(),
            discovery: Default::default(),
            mcp_servers: Vec::new(),
        };

        register_local_model_slots(&app_state, &cfg, node_id);

        let models = app_state.inner.inference_store.list_models();
        assert_eq!(
            models.len(),
            3,
            "primary/fast/embed must each produce one ModelInfo"
        );
        let names: std::collections::HashSet<String> =
            models.values().map(|m| m.name.clone()).collect();
        assert!(names.contains("qwen3-coder-30b"));
        assert!(names.contains("qwen3-1.7b"));
        assert!(names.contains("qwen3-embedding-0.6b"));

        // Second call with the same config must not duplicate entries
        // (deterministic ModelId per slot + path).
        register_local_model_slots(&app_state, &cfg, node_id);
        let models2 = app_state.inner.inference_store.list_models();
        assert_eq!(
            models2.len(),
            3,
            "re-registering same config must upsert, not duplicate"
        );
    }
}

#[cfg(test)]
mod takeover_tests {
    //! Unit tests for `takeover_serve_at` — Phase 3 daemon-takeover of
    //! the standalone `sovereign serve --background` process. We
    //! exercise the deterministic branches (no file, malformed pid,
    //! self-pid) here. The real-process SIGTERM branch needs a child
    //! to kill, which lives in the manual lifecycle verification per
    //! the Phase 3 plan.

    use super::*;

    #[test]
    fn missing_pid_file_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.pid");
        // No file exists — function must return without panicking
        // and without creating the file.
        takeover_serve_at(&path);
        assert!(!path.exists(), "takeover must not create the pid file");
    }

    #[test]
    fn malformed_pid_file_is_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.pid");
        std::fs::write(&path, "not-a-number\n").unwrap();
        takeover_serve_at(&path);
        assert!(
            !path.exists(),
            "malformed pid file must be removed so a future bind can rewrite it"
        );
    }

    #[test]
    fn self_pid_is_cleared_without_signal() {
        // The self-pid branch defends against the daemon being
        // launched in a context where it inherited its own pid file
        // (test harness, in-process spawn). The function must remove
        // the file and not attempt to SIGTERM ourselves.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.pid");
        let me = std::process::id() as i32;
        std::fs::write(&path, format!("{me}\n")).unwrap();
        takeover_serve_at(&path);
        assert!(!path.exists(), "self-pid file must be removed");
        // If the function had SIGTERM'd us, the test process would be
        // dead — reaching this assertion proves the self-skip works.
    }

    #[test]
    fn stale_pid_file_for_dead_process_is_cleared() {
        // A pid that's almost certainly not a live process. We use
        // 999_999, which is well above macOS's default pid_max and
        // Linux's default 32_768. /bin/kill returns non-zero, the
        // function logs "stale" and removes the file.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.pid");
        std::fs::write(&path, "999999\n").unwrap();
        takeover_serve_at(&path);
        assert!(
            !path.exists(),
            "stale pid file must be removed so the daemon can write a new one"
        );
    }
}

/// Prose for [`MeshError::RotateWouldPartition`]. A free function rather than a
/// format string because the right sentence depends on WHICH population is
/// non-empty, and the two remedies are different actions — "upgrade that node"
/// versus "wait one round". A single joined list could only say one of them.
fn describe_rotate_refusal(pre_split: &[String], unconfirmed: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !pre_split.is_empty() {
        parts.push(format!(
            "{} peer(s) are still on a pre-split build ({}) — upgrade them first",
            pre_split.len(),
            pre_split.join(", ")
        ));
    }
    if !unconfirmed.is_empty() {
        parts.push(format!(
            "{} peer(s) have not been confirmed since this daemon started ({}) — \
             retry after the next gossip round",
            unconfirmed.len(),
            unconfirmed.join(", ")
        ));
    }
    format!(
        "Rotating now could partition the mesh: {}. Or re-run with --force to \
         rotate anyway.",
        parts.join("; ")
    )
}

#[derive(Debug, thiserror::Error)]
pub enum MeshError {
    #[error("Mesh daemon is already running")]
    AlreadyRunning,

    #[error("Mesh daemon is not running")]
    NotRunning,

    #[error("Invalid join key: {0}")]
    InvalidJoinKey(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Network error: {0}")]
    Network(String),

    // `AlreadyInPopulatedMesh` was removed 2026-08-27. It refused a join while
    // the daemon was in a populated mesh, because `join_mesh` used to
    // `persist::clear` the outgoing mesh before the handshake and a failed
    // handshake then left the user with no mesh on disk. `join_mesh` now PARKS
    // instead of leaving, so nothing is deleted and there is no destructive
    // step to refuse in front of — and refusing was itself the reason a second
    // membership could never exist. See `tests/join_parks_not_leaves.rs`.
    /// `rotate_invite` refused because rotating now could drop an online peer
    /// out of the mesh. Refusing loudly beats partitioning quietly (ARCH
    /// §18.3); `--force` overrides.
    ///
    /// Two populations, deliberately kept apart because their remedies differ:
    /// `pre_split` peers authorize gossip on `invite_key_hash` and need
    /// UPGRADING; `unconfirmed` peers have simply not been merged from since
    /// this daemon started and need one gossip ROUND. The old single-list
    /// wording called both "still on a pre-split build", which sent operators
    /// hunting for un-migrated nodes that did not exist.
    #[error("{}", describe_rotate_refusal(pre_split, unconfirmed))]
    RotateWouldPartition {
        pre_split: Vec<String>,
        unconfirmed: Vec<String>,
    },

    /// `forget_member` matched nothing in the roster.
    #[error("No member matching '{0}' — `svrn mesh status` lists the roster")]
    UnknownMember(String),

    /// `forget_member` was pointed at this node. Retiring your own row is
    /// `svrn mesh leave`, which also tears the mesh down locally; doing it
    /// through this path would tombstone us while we keep gossiping, and the
    /// authoritative-for-self rule means every peer would ignore it anyway.
    #[error("That is this node — use `svrn mesh leave` to give up membership")]
    CannotForgetSelf,

    /// `forget_member` refused: the target is ACTIVE and ONLINE, and it is
    /// not one of a colliding pair. Retiring a member that is right there
    /// gossiping is an eviction, not a repair — and it does not even work,
    /// since the member re-announces itself with a newer `last_seen` on its
    /// next round. Refuse loudly rather than perform a no-op that reads as a
    /// success (ARCH §18.3). `--force` overrides.
    #[error(
        "'{0}' is online and not part of an endpoint-key collision — retiring it \
         would be an eviction, and its next gossip round would undo it anyway. \
         Pass --force if that is really what you mean"
    )]
    MemberStillLive(String),

    /// `switch_mesh` was given a mesh this node is not a member of.
    #[error("Not a member of any mesh matching '{0}' — `svrn mesh list` shows what is joined")]
    UnknownMesh(String),

    /// `switch_mesh` was given the mesh that is already active.
    #[error("Already active in '{0}'")]
    MeshAlreadyActive(String),
}

/// Result of [`EmbeddedDaemon::rotate_invite`].
#[derive(Debug, Clone)]
pub struct RotatedInvite {
    pub mesh_name: String,
    /// Plaintext of the freshly-minted invite key. Shown once; the mesh keeps
    /// only its hash.
    pub join_key: String,
    /// When the new invite lapses, for an encrypted mesh. `None` = no expiry.
    pub expires_at: Option<u64>,
}
