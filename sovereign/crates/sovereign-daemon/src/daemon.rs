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

use crate::state::{AppState, NodeSeed};
use corpus_index::ingest_port::daemon::IngestPort;
use kernel_types::NodeId;
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::{InferenceProvider, StateStore};
// The candidate record lives in `sovereign_contracts::venue` (fp-1; it moved
// through `sovereign-scheduler` on domains row REVIEW-build-venue);
// re-exported here so `sovereign_mesh::daemon::InferenceVenue`
// keeps resolving while the knot's modules are still in this crate.
pub use sovereign_contracts::venue::InferenceVenue;

/// The internal-router listener bind address: loopback, always. cw-rails is
/// the node's one mesh ingress and forwards a member's request here over
/// loopback (pb-mesh-exit-transport), so a LAN caller has no business on this
/// UNAUTHENTICATED surface. `[daemon] internal_bind` named the interface
/// plaintext peers dialled; plaintext meshes are gone (phase-b-36), and a
/// non-loopback value is named in the trace as not bound.
fn internal_bind_addr(internal_bind: &str, internal_port: u16) -> std::net::SocketAddr {
    if !bind_is_loopback(internal_bind) {
        info!(
            configured = %internal_bind,
            "internal API binds loopback: cw-rails forwards members' requests over loopback, \
             so [daemon] internal_bind is not bound"
        );
    }
    std::net::SocketAddr::from(([127, 0, 0, 1], internal_port))
}

/// What the client API binds to, and what bearer token guards it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientBindPosture {
    /// The interface the client listener binds to.
    pub bind: String,
    /// Whether that interface is loopback-only.
    pub loopback: bool,
    /// The token to install in `AppState`. `None` on a loopback bind means
    /// "no auth needed, nothing remote can reach it"; `None` on a
    /// NON-loopback bind means FAIL CLOSED — the auth layer then refuses
    /// every remote caller rather than serving unauthenticated.
    pub token: Option<String>,
}

/// Is this configured interface loopback?
fn bind_is_loopback(bind: &str) -> bool {
    bind == "127.0.0.1" || bind == "::1" || bind.eq_ignore_ascii_case("localhost")
}

/// The secure-by-default posture for the client API, as one decision.
///
/// SECURE BY DEFAULT means two things at once, and they are easy to separate
/// by accident: the daemon binds loopback unless something explicitly says
/// otherwise, and a bind that is NOT loopback carries a bearer token or
/// serves nobody. Both used to live inline in `start_daemon` — a ~4700-line
/// async fn — with no extracted decision and no test, so the branch that
/// decides whether an unauthenticated listener goes on the network was
/// reachable only by starting a daemon.
///
/// An explicit non-loopback `client_bind` is the operator's, and wins on its
/// own. Mesh members never reach this listener: cw-rails is the node's one
/// mesh ingress and forwards them over loopback (pb-mesh-exit-transport), so
/// neither the mesh's encryption policy nor the retired `client-exposed`
/// marker decides this bind any more.
///
/// `resolve_token` is the env → config → generate-and-persist chain, taken as
/// a closure so this decision needs no data directory: it is called ONLY on a
/// non-loopback bind, which is itself part of the contract — a loopback
/// daemon must never mint or persist a credential it has no use for.
fn resolve_client_bind_posture(
    configured_bind: &str,
    resolve_token: impl FnOnce() -> Option<String>,
) -> ClientBindPosture {
    let bind = configured_bind.to_string();
    let loopback = bind_is_loopback(&bind);
    if loopback {
        return ClientBindPosture {
            bind,
            loopback,
            token: None,
        };
    }
    // Non-loopback: a token is mandatory. Generating one by default means an
    // operator cannot expose an unauthenticated surface by flipping the bind
    // alone — and when even that fails, the absence is REPORTED and the layer
    // refuses (ARCH §18.3), never defaulted into an open door.
    let token = resolve_token();
    match &token {
        Some(_) => info!(
            %bind,
            "client API bound non-loopback — bearer token REQUIRED for remote callers"
        ),
        None => warn!(
            %bind,
            "client API bound non-loopback but NO token could be resolved/generated — \
             remote callers will be REFUSED (fail-closed). Fix data-dir perms or set \
             daemon.client_token."
        ),
    }
    ClientBindPosture {
        bind,
        loopback,
        token,
    }
}

use crate::admin_http::ConfigDiff;
use crate::daemon_services::DaemonServices;
use crate::mcp_router;

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
    /// Bind outcome of the current serve task — see [`ClientListener`].
    client_listener: tokio::sync::watch::Sender<ClientListener>,
    /// Where to persist `mesh.json` so the daemon can auto-resume on
    /// app restart. Empty means persistence is off — but it is NOT the
    /// in-memory constructor's spelling any more: the identity key and the
    /// ring rail's journals are written under `data_dir` unconditionally,
    /// and an empty path put both in the process's working directory. See
    /// [`Self::in_memory`].
    data_dir: PathBuf,
    /// Owned scratch root for [`Self::in_memory`], deleted with the daemon.
    /// `None` for a daemon commissioned with a real `data_dir`. Behind a
    /// mutex only because `new` hands back an `Arc` and the root is attached
    /// after construction.
    _scratch: std::sync::Mutex<Option<tempfile::TempDir>>,
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
    /// Fabric's part, held on the daemon once `start` builds it (DC §4.2
    /// "Construction is staged, and parts are total").
    fabric: std::sync::RwLock<Option<Arc<sovereign_mesh::fabric::FabricPart>>>,
}

/// What became of the API listeners the serve task binds.
///
/// A closed set (ARCH §2) because callers fork on it: `daemon run` refuses
/// to report itself running until this reads `Bound`, and `Failed` is fatal
/// there. The bind itself stays best-effort INSIDE the serve task (the
/// default-port integration tests bind `:9741` under parallel contention and
/// must not be stranded) — this is how a caller that needs the truth reads
/// it without changing the task's posture. Minted 2026-09-10: the desktop
/// e2e harness's fixture daemon lost `:9741` to the operator's
/// launchd-relaunched daemon, logged "is running" anyway, and the harness's
/// port probe was answered by the stranger — a fixture ingest landed in the
/// operator's real home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientListener {
    /// The serve task has not reached its bind yet.
    Pending,
    /// Both API listeners are bound; this is the client address.
    Bound(SocketAddr),
    /// A bind failed after retries. The serve task has exited and this
    /// process serves NO client API — whatever answers on the port is
    /// someone else.
    Failed(String),
}

enum DaemonState {
    Stopped,
    Running {
        #[allow(dead_code)]
        app_state: AppState,
        client_addr: SocketAddr,
        /// The peer-facing loops. `None` means the local-only profile did
        /// not start that loop — and "did not" is not left to be inferred
        /// from a `None`: `running_services` is the authoritative census, so
        /// a declined loop and a forgotten one are distinguishable.
        ///
        /// Aborts the peer-assisted ingest handoff loop on Drop: a spawner
        /// that returns nothing can lose its `tokio::spawn` in a stray
        /// three-line diff and stay silent about it for five weeks
        /// (`ec7ca66c`, 2026-07-21 — see `auto_ingest::CollaborateHandle`).
        _collaborate_handle: Option<crate::auto_ingest::CollaborateHandle>,
        /// Aborts the plane-seal pump on Drop — the loop that seals the
        /// daemon's own namespaces. Same pattern and the same reason as
        /// `_collaborate_handle`.
        _rail_kv_pump_handle: Option<sovereign_mesh::rail_kv_pump::RailKvPumpHandle>,
        /// Stops the `ingest:v1` execute origin and its registration with
        /// cw-rails on Drop (pb-work-donor). `None` on a local-only daemon and
        /// on a node with no corpus engine.
        _work_origin_handle: Option<crate::work_origin::WorkOriginHandle>,
        _peer_origin_handle: Option<crate::peer_origin::PeerOriginHandle>,
        /// Stops renewing the guest listener's `cwth/guest/0` registration on
        /// Drop. `None` on a local-only daemon and when the guest bind failed.
        _guest_origin_handle: Option<crate::guest_origin::GuestOriginHandle>,
        /// Stops renewing the `[iroh.apps]` claims and the offer origin's
        /// registration on Drop. `None` on a local-only daemon and when the
        /// config publishes neither.
        _published_origins_handle: Option<crate::published_origins::PublishedOriginsHandle>,
        /// Stops posting the foreground deadline to cw-rails on Drop.
        _foreground_post_handle: crate::foreground_post::ForegroundPostHandle,
        /// The network posture this boot resolved, and what it produced.
        /// Read by [`EmbeddedDaemon::running_services`] — the boot
        /// assertion's instrument (ARCH §18.1).
        local_only: crate::local_only::LocalOnlyProfile,
        running_services: crate::local_only::RunningServices,
        _shutdown_tx: tokio::sync::oneshot::Sender<()>,
        /// The API-server task that owns the `:9741`/`:9742` listeners.
        /// Kept (not discarded) so `shutdown` can await its exit after
        /// dropping `_shutdown_tx`, so the listeners are released before the
        /// process reports itself stopped.
        serve_handle: tokio::task::JoinHandle<()>,
    },
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
            client_listener: tokio::sync::watch::Sender::new(ClientListener::Pending),
            data_dir,
            _scratch: std::sync::Mutex::new(None),
            self_weak: self_weak.clone(),
            services,
            setup_config: RwLock::new(setup_config),
            inference_provider: RwLock::new(provider),
            fabric: std::sync::RwLock::new(None),
        })
    }

    /// A daemon whose disk footprint dies with it. Tests that don't want to
    /// set up a tempdir; production code uses [`Self::new`] with a real
    /// `data_dir`.
    ///
    /// Until 2026-09-08 this passed an EMPTY `data_dir`, and "in memory" was
    /// false in two ways nobody noticed for months: `start_daemon` writes the
    /// identity key under `data_dir` unconditionally, and after cw-lift 4b the
    /// rail's KV pump appends every store write to `<data_dir>/rings/<ns>/`
    /// — so every in-memory daemon wrote `node_key`, `node_id` and then whole
    /// ring journals into the crate directory `cargo test` runs from. The
    /// daemon now OWNS a temp root and hands it to `new`, so the footprint is
    /// real, private, and gone on drop. `try_resume` may now find a
    /// `mesh.json` this same daemon wrote; a test that needs the "never
    /// resumes" property asserts it against its own fresh daemon.
    pub fn in_memory(setup_config: SetupConfig, services: DaemonServices) -> Arc<Self> {
        let scratch = tempfile::Builder::new()
            .prefix("svrn-in-memory-")
            .tempdir()
            .expect("a temp root for an in-memory daemon");
        let daemon = Self::new(scratch.path().to_path_buf(), setup_config, services);
        // `new` returns the daemon already behind its `Arc`, so the scratch
        // root is attached through the one field written after construction.
        *daemon._scratch.lock().unwrap_or_else(|e| e.into_inner()) = Some(scratch);
        daemon
    }

    /// cw-rails' API base, the node's mesh endpoint, from this daemon's
    /// commissioned config (`rails_client::resolve_rails_base`, the one reader).
    pub async fn rails_base(&self) -> String {
        crate::rails_client::resolve_rails_base(&self.setup_config.read().await.daemon)
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
    pub async fn resolved_ports(&self) -> (u16, u16) {
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
        // Through `binding()`, not the `entry` field: a terminal bound by mesh
        // identity has no address to report and would otherwise read as
        // "terminal, entry node: null" — a node class with no visible
        // destination, which is the opposite of why this is reported at all.
        self.setup_config
            .read()
            .await
            .node
            .binding()
            .map(|b| b.describe())
    }

    /// Borrow ingest's port this host commissioned the daemon with, if a
    /// distribution composed ingest. `reading_http` and the knowledge
    /// handlers call this; `MeshAdmin` answers `None` by construction.
    pub fn corpus_engine(&self) -> Option<&Arc<dyn IngestPort>> {
        self.services
            .serving()
            .and_then(|s| s.core.corpus_engine.as_ref())
    }

    /// Borrow ingest's atlas port, composed beside [`Self::corpus_engine`].
    pub fn atlas(&self) -> Option<&Arc<dyn corpus_engine_atlas_reader::ports::AtlasPort>> {
        self.services.serving().and_then(|s| s.core.atlas.as_ref())
    }

    /// Borrow the recipe harness this host composed beside its engine, if
    /// any. `recipe_http`'s harness route calls this and names the absence.
    pub fn recipe_harness(
        &self,
    ) -> Option<&Arc<dyn corpus_index::ingest_port::daemon::RecipeHarnessPort>> {
        self.services
            .serving()
            .and_then(|s| s.core.recipe_harness.as_ref())
    }

    /// Borrow the `StateStore` the reading surface uses to resolve
    /// `conversation-history` chunks back to their conversation. `None` only
    /// on [`DaemonServices::MeshAdmin`], which serves nothing — since
    /// daemon-convergence Phase 3 BOTH serving variants own a store.
    pub fn state_store(&self) -> Option<&Arc<dyn StateStore>> {
        self.services.serving().map(|s| &s.core.state_store)
    }

    /// `[models].context_size` from THIS daemon's own `SetupConfig` — the
    /// window its next slot load will ask for.
    ///
    /// From the config the daemon was commissioned with (and that
    /// `reload_from_setup_config` updates), NOT from
    /// `SetupConfig::load()`. A route that re-loaded the file would be
    /// reporting the config of whatever `~/.svrnmesh` the SERVING daemon
    /// can see, which is the same wrong-source mistake as a client
    /// reading its own data dir for the daemon's.
    pub async fn configured_context_size(&self) -> u32 {
        self.setup_config.read().await.effective_context_size()
    }

    /// Where this daemon dials serve, from the same commissioned config
    /// (`serve_client::resolve_serve_base`, the one reader).
    pub async fn configured_serve_base(&self) -> crate::serve_client::ServeBase {
        crate::serve_client::resolve_serve_base(&self.setup_config.read().await.node)
    }

    /// The `InferenceProvider` this daemon is serving turns on RIGHT NOW,
    /// cloned out from behind the swap lock.
    ///
    /// A clone rather than a borrow, and `async` rather than not, because
    /// `admin_reload` SWAPS this field while requests are in flight — the
    /// atomicity that makes a hot reload gapless is exactly what makes a
    /// borrow across an await point wrong. `None` on `MeshAdmin`, and also
    /// on a serving daemon whose provider has not been installed yet,
    /// which is a different fact from "no local slot" and is why the
    /// caller reports absence rather than defaulting it.
    pub async fn inference_provider(&self) -> Option<Arc<dyn InferenceProvider>> {
        self.inference_provider
            .read()
            .await
            .as_ref()
            .map(Arc::clone)
    }

    /// Borrow the `Runtime` this daemon serves turns with. `None` only on
    /// [`DaemonServices::MeshAdmin`] — the same real fork the two accessors
    /// above answer to, and the reason all three keep an `Option` where the
    /// seven deleted in Phase 2 did not: `serving()` is a genuine question,
    /// and the field one level down is not optional.
    pub fn runtime(&self) -> Option<&Arc<sovereign_core::runtime::Runtime>> {
        self.services.serving().map(|s| &s.core.runtime)
    }

    /// Borrow the `InsightService` the insight surface serves, when the
    /// commissioning host built one (sv-surface rung 6). `None` on
    /// [`DaemonServices::MeshAdmin`] and on a serving commission that
    /// supplied no service — `insight_http` renders that as a named 503,
    /// not as an unmounted route.
    pub fn insight_service(&self) -> Option<&Arc<sovereign_core::insight::InsightService>> {
        self.services
            .serving()
            .and_then(|s| s.core.insights.as_ref())
    }

    /// Borrow the recipe-project port over this daemon's `features.db`
    /// (sv-surface D6; ingest's since pb-ingest-rehome-daemon), or why there
    /// is none: `MeshAdmin`, a store that would not open, or no ingest
    /// program — `features_http` renders each as a named 503, not as a
    /// missing route.
    pub fn features_store(
        &self,
    ) -> Result<&Arc<dyn sovereign_contracts::recipe::project::RecipeProjectPort>, &str> {
        match self.services.serving() {
            Some(s) => s.core.features.as_ref().map_err(String::as_str),
            None => Err(crate::features_http::NO_FEATURES_DB),
        }
    }

    /// Borrow svrn's store behind this daemon's mounted `/mcp` surface, where
    /// its memory notes live (pb-notes-memory), when one is mounted
    /// (sv-surface rung 6). `None` on `MeshAdmin` and on a commission with no
    /// `/mcp` mount — the notes and tool-outcome routes render that as a
    /// named 503, matching `McpSurface::Unavailable`'s own refusal to
    /// conflate the two facts (ARCH §18.3).
    pub fn notes_store(&self) -> Option<&Arc<sovereign_store::sqlite::SqliteStateStore>> {
        self.services
            .serving()
            .and_then(|s| s.capability.mcp.mount())
            .map(|m| &m.notes)
    }

    /// The `[[mcp_servers]]` this daemon actually loaded — its IN-MEMORY
    /// `SetupConfig`, not a fresh `SetupConfig::load()` (sv-surface D8).
    ///
    /// The in-memory copy is the truthful one for the question "what is
    /// this daemon serving?": it is what the boot-time MCP loader read,
    /// and it advances on `POST /v1/admin/reload`. A `config.toml` edited
    /// since boot describes a daemon that does not exist yet, which is the
    /// state `reload` exists to end — reporting the file here would
    /// silently answer a different question than the caller asked.
    pub async fn configured_mcp_servers(
        &self,
    ) -> Vec<sovereign_contracts::mcp_config::McpServerConfig> {
        self.setup_config.read().await.mcp_servers.clone()
    }

    /// Every tool id in the registry behind this daemon's `/mcp` mount, or
    /// `None` when no mount was commissioned (sv-surface D8).
    ///
    /// `None` and `Some(vec![])` are different facts and both occur: no
    /// tool surface at all, versus a mounted surface with nothing in it.
    /// `mcp_config_http` renders the distinction rather than folding it
    /// into a zero count (ARCH §18.3).
    pub fn mcp_tool_ids(&self) -> Option<Vec<String>> {
        self.services
            .serving()
            .and_then(|s| s.capability.mcp.mount())
            .map(|m| m.tools.descriptors().into_iter().map(|d| d.id).collect())
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

    /// Await the serve task's bind outcome, bounded by `timeout`.
    ///
    /// `Pending` comes back ONLY on timeout — a caller that needs a verdict
    /// treats it as could-not-judge (§18.1), never as bound. The in-process
    /// tests never call this, so their best-effort posture is untouched.
    pub async fn client_listener(&self, timeout: std::time::Duration) -> ClientListener {
        let mut rx = self.client_listener.subscribe();
        let settled = rx.wait_for(|s| !matches!(s, ClientListener::Pending));
        let outcome = match tokio::time::timeout(timeout, settled).await {
            Ok(Ok(state)) => state.clone(),
            // The sender lives in `self`, so this arm is unreachable while
            // `&self` is borrowed; named rather than unwrapped.
            Ok(Err(_)) => ClientListener::Failed("daemon dropped before its listener bound".into()),
            Err(_) => ClientListener::Pending,
        };
        outcome
    }

    /// Whether the daemon is currently running.
    pub async fn is_running(&self) -> bool {
        matches!(*self.state.read().await, DaemonState::Running { .. })
    }

    /// This daemon's `NodeId`, if known. Returns `None` before the
    /// daemon has finished its create_mesh / join_mesh handshake;
    /// callers that depend on the value (e.g.
    /// `InferenceRouter::get_peer_manifest` stamping
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
    pub async fn app_state(&self) -> Option<crate::state::AppState> {
        match &*self.state.read().await {
            DaemonState::Running { app_state, .. } => Some(app_state.clone()),
            _ => None,
        }
    }

    /// Fabric's part as the daemon holds it — present while running and while
    /// parked or stopped, cleared on leave. The membership operations that
    /// answer while the daemon is `Stopped` read it here rather than through
    /// `AppStateInner.fabric`, which only exists while running (DC §4.1; DC
    /// §4.2 "Construction is staged, and parts are total").
    pub fn fabric(&self) -> Option<Arc<sovereign_mesh::fabric::FabricPart>> {
        self.fabric
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
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
    pub async fn build_yield_hook(
        &self,
    ) -> Option<std::sync::Arc<dyn corpus_engine_yield::YieldHook>> {
        let state = self.app_state().await?;
        Some(crate::yield_hook::AppStateYieldHook::new(
            state.inner.clone(),
        ))
    }

    /// Where mesh state + setup are persisted. Needed by the HTTP
    /// mesh API's rotate handler, which talks to `persist::rotate_join_key`
    /// directly rather than going through a daemon method — and by
    /// `meshapp_http`'s Wrapped route, whose GLiNER entity cards read
    /// `<data_dir>/sovereign.db` (the desktop passed
    /// `svrnmesh_root()/sovereign.db`; same file whenever the daemon owns
    /// that root, which is what the one-writer rule on
    /// `ServingCore::state_store` makes true).
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// **Shutdown** the daemon for process exit: drops the listeners and the
    /// loops. svrn persists no mesh state; cw-rails holds the node's
    /// membership across restarts (pb-mesh-exit-transport).
    pub async fn shutdown(&self) -> Result<(), MeshError> {
        let mut state = self.state.write().await;
        match std::mem::replace(&mut *state, DaemonState::Stopped) {
            DaemonState::Running {
                _shutdown_tx,
                serve_handle,
                ..
            } => {
                // Dropping the sender signals the serve task to stop.
                drop(_shutdown_tx);
                drop(state);
                // Wait for the serve task to drop its listeners before
                // returning, bounded so a wedged task cannot hang shutdown.
                if tokio::time::timeout(std::time::Duration::from_secs(2), serve_handle)
                    .await
                    .is_err()
                {
                    warn!("API-server task did not exit within 2s of shutdown signal");
                }
                info!("daemon stopped");
                Ok(())
            }
            DaemonState::Stopped => Err(MeshError::NotRunning),
        }
    }

    /// Get the Commonwealth API address (for internal use).
    pub async fn api_address(&self) -> Option<SocketAddr> {
        let state = self.state.read().await;
        match &*state {
            DaemonState::Running { client_addr, .. } => Some(*client_addr),
            DaemonState::Stopped => None,
        }
    }

    /// **What this boot actually spawned**, and the profile that decided it.
    /// `None` when the daemon is stopped.
    ///
    /// This is the falsifiable half of the local-only claim (ARCH §18.1). The
    /// named failing input: delete any one of the four gates in
    /// `start_daemon` and
    /// `local_only_boot::a_local_only_daemon_spawns_no_network_service` names
    /// the service that came back. Config alone cannot be that instrument —
    /// it says what was ASKED for, not what happened.
    pub async fn running_services(
        &self,
    ) -> Option<(
        crate::local_only::LocalOnlyProfile,
        crate::local_only::RunningServices,
    )> {
        let state = self.state.read().await;
        match &*state {
            DaemonState::Running {
                local_only,
                running_services,
                ..
            } => Some((*local_only, running_services.clone())),
            DaemonState::Stopped => None,
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
    pub async fn peer_inference_endpoints(&self) -> Vec<InferenceVenue> {
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
        // Which members, and each as a venue: the one decision serve's
        // cw-rails roster applies too (pb-serve-ranks).
        let members = sovereign_contracts::membership::inference_peers(
            app_state.membership().members().await,
            app_state.inner.fabric.identity.current(),
        );
        let mut endpoints = Vec::with_capacity(members.len());
        for m in members {
            let base_urls: Vec<String> = transport
                .endpoints(&m.dial, mesh_reach::TrafficClass::Inference)
                .await
                .into_iter()
                .map(|ep| format!("{}/v1", ep.base_url))
                .collect();
            endpoints.push(m.inference_venue(base_urls));
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
        let ingests = app_state.inner.ingest.active_ingests.read().await.len();
        (inflight, ceiling, fanout, ingests)
    }

    /// The current eligible shared-model anchors, by `NodeId`: online mesh
    /// members (including self, when self is an online anchor) that advertise
    /// `anchor.can_anchor`. This is the input to leader election for the host
    /// role — see `kernel_types::partition::should_host`. Pure read of the
    /// gossiped membership, so every anchor computes the same set and converges
    /// on the same host without coordination.
    pub async fn eligible_anchors(&self) -> Vec<kernel_types::NodeId> {
        let app_state = {
            let state = self.state.read().await;
            match &*state {
                DaemonState::Running { app_state, .. } => app_state.clone(),
                DaemonState::Stopped => return Vec::new(),
            }
        };
        // The roster decision is Fabric's (DC §4.1 "report reach"); the daemon
        // owns only the "is there a node at all" gate.
        app_state.inner.fabric.eligible_anchors().await
    }

    // ── Private ─────────────────────────────────────────

    /// Start serving: the client, internal, guest, peer and rail listeners
    /// and the loops that talk to cw-rails. Runs once, at boot. svrn holds no
    /// mesh of its own (pb-mesh-exit-transport): cw-rails is the node's one
    /// mesh endpoint and holds its one key, and svrn reads the roster and
    /// reaches peers through the ports its distribution composed
    /// (`ServingProfile::mesh`).
    pub async fn start(&self) -> Result<(), MeshError> {
        if self.is_running().await {
            return Err(MeshError::AlreadyRunning);
        }
        // Resolve the bind ports once at the top so every downstream site
        // (listener bind, auto-collaborate loop spawn) sees the same pair.
        // Defaults to (9741, 9742); see `resolved_ports` for the contract.
        let (client_port, internal_port) = self.resolved_ports().await;

        // ── The local-only profile: resolved ONCE, here, and read by every
        // gate below (ARCH §10.6). See `crate::local_only` for the census
        // that made this the deliverable.
        let local_only = {
            let c = self.setup_config.read().await;
            crate::local_only::LocalOnlyProfile::resolve(c.daemon.local_only)
        };
        // This node's id, from svrn's own `node_id` file by the one resolver
        // every stamping surface uses. `svrn mesh up`'s handover copies the
        // same file to cw-rails, so the roster names this node by it.
        let node_id = sovereign_contracts::node_identity::resolve_self_node_id(&self.data_dir);
        // The donor and its boundary probe run in cw-rails since pb-work-donor;
        // this daemon serves the `ingest:v1` execute origin below instead.
        let corpus_engine = self
            .services
            .serving()
            .and_then(|s| s.core.corpus_engine.clone());
        // Assigned inside the networked branch below. A `mut` binding rather
        // than a fifth tuple element so the gate stays the SAME `if` the four
        // loops already sit in without re-indenting sixty lines of it.
        let mut work_origin_handle: Option<crate::work_origin::WorkOriginHandle> = None;
        let mut peer_origin_handle: Option<crate::peer_origin::PeerOriginHandle> = None;
        let mut guest_origin_handle: Option<crate::guest_origin::GuestOriginHandle> = None;
        let mut published_origins_handle: Option<crate::published_origins::PublishedOriginsHandle> =
            None;
        // What this boot actually spawns, recorded at each spawn site and
        // stored on the Running variant. The profile's claim is about this
        // list, and a list is falsifiable where a config value is not
        // (ARCH §18.1).
        let mut running_services = crate::local_only::RunningServices::default();

        // Build an AppState that already knows about our CorpusEngine
        // (if one was installed via `set_corpus_engine`). Without
        // this, Commonwealth's knowledge handlers can only return
        // stubs — the whole reason Peer A couldn't see Peer B's SEP
        // corpus. Fabric builds its own private store: the node's replicated
        // KV is cw-rails', reached through `RailsKv` below (five-programs
        // fp-88, fp-111).

        // ── Fabric's values exist before its part is built ────────
        //
        // DC §4.2 "Construction is staged, and parts are total": a part is
        // constructed after everything it holds exists, and nothing is
        // installed into it afterwards. Each value below used to arrive
        // through an `install_*` call on the freshly-built `AppState`;
        // gathering them first removes the slot a reader could find unfilled.
        //
        // The shared notes convergence recorder (fix 9): the bootstrap hands
        // the SAME `Arc<ConvergenceRecord>` to the publish sink + ingest
        // poller, so `/status`'s convergence section reads the writers'
        // stamps, never a parallel copy.
        let convergence_recorder = self
            .services
            .rails()
            .map(|r| r.convergence_recorder.inner());
        // The node's mesh as the distribution composed it: cw-rails' roster
        // and reach door (`ServingProfile::mesh`). Every peer dial resolves
        // through that transport; svrn holds no key and no endpoint.
        let mesh_access = self.services.mesh().clone();
        // The ring rail's journals moved to the rails daemon's data root
        // (fp-54, §4 rule 1 — one data directory, one owner). The one-time
        // handover runs in `svrn mesh up`, before it brings cw-rails up
        // (phase-b-3, pb-rails-untether), never in this boot; this daemon
        // holds no journal and every rail read or write dials
        // `cw-rails` through the port (`rails_client::RailsRingRail`), which
        // reports ABSENCE when the rails daemon is down — never an empty
        // ledger (ARCH §18.3). The signer DOES change: the rails daemon
        // signs with its own node key (its own data dir), and a line verifies
        // because rails joined the mesh as a member with that key — see
        // commonwealth-rails' `rails_and_the_daemon_sign_with_two_keys_under_one_person`.
        let ring_rail: Option<Arc<dyn sovereign_mesh::rail_port::RingRailPort>> =
            Some(Arc::new(crate::rails_client::RailsRingRail::new(
                crate::rails_client::resolve_rails_base(&self.setup_config.read().await.daemon),
            )));
        let fabric_seed = crate::state::FabricSeed {
            convergence: convergence_recorder,
            ring_rail,
            peer_transport: crate::state::TransportReader::new(Arc::clone(&mesh_access.transport)),
            membership: Some(Arc::clone(&mesh_access.membership)),
            ..Default::default()
        };
        // Serving's provider and warmer exist before its part is built (DC §4.2
        // "Construction is staged, and parts are total"). Gathering them into a
        // `ServingSeed` and passing it to the constructor removes the
        // post-construction `Arc::get_mut` installer that could silently no-op
        // and leave `/v1/chat/completions` 503ing with `model_not_ready`.
        //
        // The OpenAI-flavour face the host handed with its provider
        // (`ServingCore::local_inference`, pb-serve-ranks) serves this node's
        // `/v1/chat/completions`, peer requests included, from the same model
        // the user would use. Without it, peer inference requests 503 because
        // the daemon's scheduler/llama-server path is empty in the embedded
        // topology.
        let serving_seed = match self
            .services
            .serving()
            .and_then(|s| s.core.local_inference.clone())
        {
            Some(adapter) => {
                info!("inference adapter: wired into /v1/chat/completions");
                crate::state::ServingSeed {
                    local_inference: Some(adapter),
                }
            }
            None => crate::state::ServingSeed::default(),
        };
        // ── Client API bind — the OpenAI-compatible public surface ────
        //
        // (SYSTEM_OVERVIEW.md §5.5.) Peers fetch `/oicp/v1/capabilities`
        // here, the Joiner's HybridProvider POSTs `/v1/chat/completions`
        // here for federated inference.
        //
        // **Trust boundary (2026-06 auth: localhost-default + bearer).**
        // `daemon.client_bind` defaults to `127.0.0.1` — secure by
        // default, single-user needs no auth. When an operator binds a
        // routable address to serve a mesh / remote clients, the
        // `client_auth` layer requires a bearer token of every
        // non-loopback caller. The posture (bind + auth) is resolved HERE,
        // before the state is built, because the token is a construction
        // argument of the node's part (DC §4.2 "Construction is staged, and
        // parts are total") — the layer reads it off `AppState` before the
        // listener binds, and nothing installs it afterwards. The internal
        // port (`:9742`) is unrelated, always binds `0.0.0.0`, and is not and
        // never was mTLS: what it requires of a caller is `internal_auth`.
        let (mut client_bind, configured_token, internal_bind) = {
            let c = self.setup_config.read().await;
            (
                c.daemon.client_bind.clone(),
                c.daemon.client_token.clone(),
                c.daemon.internal_bind.clone(),
            )
        };
        // The whole posture — bind + auth — is one decision, resolved in
        // `resolve_client_bind_posture` so it can be exercised without
        // starting a daemon. The token chain (env → config →
        // generate-and-persist) is passed as a closure and is called ONLY on
        // a non-loopback bind: a loopback daemon must not mint or persist a
        // credential it has no use for.
        let data_dir = self.data_dir.clone();
        let posture = resolve_client_bind_posture(&client_bind, move || {
            std::env::var("SOVEREIGN_CLIENT_TOKEN")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .or(configured_token)
                .or_else(|| {
                    crate::client_auth::load_or_create_client_token(&data_dir)
                        .map_err(|e| warn!("client-token persistence failed: {e}"))
                        .ok()
                })
        });
        client_bind = posture.bind;
        // The node's part exists before it is built, token and all (DC §4.2
        // "Construction is staged, and parts are total").
        let mut node_seed = NodeSeed::resolved(posture.token, &self.data_dir, &self.setup_config)
            .await
            .map_err(|e| MeshError::Config(e.to_string()))?;
        // Code's editor door, when the distribution composed code here
        // (pb-meshapp-rest); every general client surface mounts it.
        node_seed.edit_door = self
            .services
            .serving()
            .and_then(|s| s.capability.edit_door.clone());
        info!(
            code_edit_door = node_seed.edit_door.is_some(),
            "daemon: code's editor door (/v1/edit_predictions)"
        );
        // Ingest's atlas port, when the distribution composed ingest here
        // (pb-ingest-dial-daemon); the atlas routes name its absence.
        node_seed.atlas = self.atlas().cloned();
        // Fabric's part is constructed before `AppState` and held on the
        // daemon, so it survives `stop_inner` (DC §4.1; DC §4.2 "Construction
        // is staged, and parts are total"). The membership operations that
        // answer while the daemon is `Stopped` read this part rather than
        // `AppStateInner.fabric`.
        let fabric = Arc::new(sovereign_mesh::fabric::FabricPart::new(
            node_id,
            fabric_seed,
        ));
        *self.fabric.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&fabric));
        // The headless daemon hands in the ONE `RailsKv` its work atlas and
        // notes write through; the desktop and the mesh-admin one-shot dial
        // their own. Neither checks presence: a missing cw-rails is a named
        // absence on the first call (five-programs fp-88, D4).
        let kv: Arc<dyn sovereign_contracts::peer::ReplicatedKv> = match self.services.rails() {
            Some(r) => Arc::clone(&r.mesh_store),
            None => Arc::new(crate::rails_client::kv::RailsKv::new(
                node_seed.rails_base.clone(),
            )),
        };
        let app_state = AppState::new_with_fabric_and_serving_and_node(
            node_id,
            fabric,
            kv,
            corpus_engine.clone(),
            self.services
                .serving()
                .and_then(|s| s.core.in_flight_gauge.clone()),
            serving_seed,
            node_seed,
        );

        // (The former `Arc::get_mut` installer block lived here. Fabric's and
        // Serving's values are constructor arguments now, so nothing is
        // installed into a part after it is built and no ordering can silently
        // drop local inference. DC §4.2 "Construction is staged, and parts are
        // total".)

        // The daemon's own rings' roster installation died with the local
        // rail (fp-54): the journals live at the rails daemon now, and ITS
        // `MembershipRosterSource` derives every ring nobody narrowed from
        // the membership it holds. The registered-namespace list
        // (`sovereign_mesh::ring_roster::REGISTERED_NAMESPACES`) guards the
        // same rings there by derivation; the file-rostered work plane stays
        // narrowed by the file that moved with it.

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
            let hook: Arc<dyn corpus_engine_yield::YieldHook> =
                crate::yield_hook::AppStateYieldHook::new(app_state.inner.clone());
            engine.set_yield_hook(hook);
            info!("foreground-yield: hook installed on corpus engine");
            engine.set_foreground_signal(crate::yield_hook::AppStateForegroundSignal::new(
                app_state.inner.clone(),
            ));
            info!("foreground-yield: turn lease installed on corpus engine");
        }
        // The same window, published to cw-rails' donor, which yields to it
        // (pb-work-donor). With the window 0 nothing is posted.
        let foreground_post_handle = crate::foreground_post::spawn(
            app_state.clone(),
            crate::rails_client::resolve_rails_base(&self.setup_config.read().await.daemon),
        );

        // Bound peer-inference admission for headless contributors. The desktop
        // sets this from the GPU-share consent; a CLI daemon would otherwise
        // leave the AppState default (unbounded) in place — and an unbounded
        // peer fan-out is what OOM-killed the daemon. Apply the configured
        // ceiling (default 1) regardless of whether a corpus engine is present,
        // so a storage-only or inference-only node is still bounded.
        {
            let (max, reads) = {
                let cfg = self.setup_config.read().await;
                (
                    cfg.daemon.max_peer_inflight,
                    cfg.daemon.max_peer_knowledge_reads,
                )
            };
            app_state.set_contribution_max_peer_inflight(max);
            app_state.set_contribution_max_peer_knowledge_reads(reads);
            info!(
                max_peer_inflight = max,
                max_peer_knowledge_reads = reads,
                "admission: peer-inflight and knowledge-read ceilings configured"
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
            match app_state.set_local_embed_model(embed_info).await {
                Ok(()) => info!(
                    model_id = %embed_info.model_id,
                    dims = embed_info.dimensions,
                    "embed model info: published to inference store"
                ),
                Err(e) => warn!(
                    model_id = %embed_info.model_id,
                    error = %e,
                    "embed model info: NOT published to inference store"
                ),
            }
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
        // And the names claimed under those grants, for the same reason.
        let _guest_session_reaper = app_state.start_guest_session_reaper();

        // Register the locally-loaded model slots so `/v1/models`
        // answers with something meaningful instead of an empty list.
        // Without this, the OpenAI-compatible models list returns
        // `{"object":"list","data":[]}` on a freshly-set-up daemon —
        // confusing for anyone running `curl /v1/models` as a
        // post-setup health check. We register one `ModelInfo` per
        // configured slot (primary / fast / embed) with a
        // deterministic ModelId so reloads don't create duplicates.
        //
        // The slot-alias map comes from what serves: serve's residency on
        // every path a boot decided (`[models]` only where none did), where svrn
        // does not read serve's sections (seat, reviewing c0c39be03). The
        // inference_store rows and the servable-file allowlist are still
        // registered from `[models]` on both paths: their readers are a node
        // with no local inference and peer model fetch, which
        // pb-mesh-exit-mesh moves to serve's own registration.
        {
            let cfg = self.setup_config.read().await;
            let config_aliases = register_local_model_slots(&app_state, &cfg, node_id).await;
            if crate::serve_client::ServingPath::decided().is_some() {
                let slots = match self.inference_provider().await {
                    Some(provider) => provider.resident_slots(),
                    None => {
                        warn!(target: "serving_path", "boot: no provider installed; the slot-alias map is empty");
                        Vec::new()
                    }
                };
                publish_slot_aliases(
                    &app_state,
                    crate::serve_client::served_slot_aliases(&slots),
                    "serve's self-report",
                );
            } else if !config_aliases.is_empty() {
                publish_slot_aliases(&app_state, config_aliases, "[models]");
            }
        }

        // `client_bind` and the auth posture were resolved above, before the
        // state was built, so the node's part already holds the token the auth
        // layer reads. The listener address derives from the resolved bind.
        let client_addr: SocketAddr = format!("{client_bind}:{client_port}")
            .parse()
            .unwrap_or_else(|_| {
                warn!("invalid client_bind '{client_bind}'; falling back to 127.0.0.1");
                format!("127.0.0.1:{client_port}").parse().unwrap()
            });
        // Loopback: cw-rails forwards members here (`internal_bind_addr`).
        let internal_addr: SocketAddr = internal_bind_addr(&internal_bind, internal_port);

        // The holder's media-presence poll (`crate::media_presence`): the
        // reading is the mesh's rails daemon's, the report is this
        // process's own, so both halves stay loopback regardless of
        // internal_bind.
        tokio::spawn(crate::media_presence::run(
            app_state.inner.node.rails_base.clone(),
            format!("http://127.0.0.1:{internal_port}"),
        ));

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

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
            // The daemon's weights: what this machine can run, what the
            // catalog offers, what is installed, and the one job that fetches
            // any of it. Beside `admin_http` because it is the same audience
            // — a local Settings-style surface reading this daemon's
            // own state — and loopback-guarded for the same reason.
            mounted.push(crate::assets_http::assets_router(Arc::clone(&self_arc)));
            mount_names.push("assets_http");
            mounted.push(crate::reading_http::reading_router(Arc::clone(&self_arc)));
            mount_names.push("reading_http");
            // Phase 5c — the daemon answers. Built here from `Arc<Self>` like
            // the three above, not accepted from a host, so a serving daemon
            // cannot come up unable to serve a turn.
            mounted.push(crate::turn_http::turn_router(Arc::clone(&self_arc)));
            mount_names.push("turn_http");
            // sv-surface D9 — the two reads a turn leaves behind (the
            // skill registry, the last turn's provenance frame). Beside
            // `turn_http` rather than inside it: that file is the DRIVER's
            // surface, and these are reads over the serving Runtime's own
            // registers. Unconditional for the reason the routers below
            // give: a daemon with no Runtime answers a named 503, which is
            // a different fact from an unmounted route's 404 (ARCH §18.3).
            mounted.push(crate::turn_extras_http::turn_extras_router(Arc::clone(
                &self_arc,
            )));
            mount_names.push("turn_extras_http");
            // sv-surface D9a — the document-asset family. Six of
            // `commands/document_asset.rs`'s eight commands are CRUD over
            // the store a turn writes to, and read the DESKTOP's handle.
            // Unconditional for the reason the siblings give: a commission
            // with no store answers a named 503, which is a different fact
            // from an unmounted router's 404 (ARCH §18.3).
            mounted.push(crate::documents_http::documents_router(Arc::clone(
                &self_arc,
            )));
            mount_names.push("documents_http");
            // sv-surface D9a — the corpus catalogue and the notebook shelf,
            // beside `reading_http`'s status route (rung 1) rather than
            // inside it: that file is the READING surface and is within
            // sight of the §3.1 split line.
            mounted.push(crate::corpus_catalog_http::corpus_catalog_router(
                Arc::clone(&self_arc),
            ));
            mount_names.push("corpus_catalog_http");
            // thin-desktop order (2026-09-11) — the recipe-registry import and
            // the `[parameters]` read, beside the catalogue they install into.
            // Unconditional for `corpus_catalog_http`'s reason: a daemon with
            // no corpus engine answers 503 naming that, which is a different
            // fact from an unmounted router's 404.
            mounted.push(crate::recipe_http::recipe_router(Arc::clone(&self_arc)));
            mount_names.push("recipe_http");
            // sv-surface rung 6 — the insight surface. Mounted
            // unconditionally on serving daemons; a commission that built no
            // `InsightService` answers 503 with that named reason on these
            // paths, which `mount_names` below still reports as mounted.
            mounted.push(crate::insight_http::insight_router(Arc::clone(&self_arc)));
            mount_names.push("insight_http");
            // sv-surface D4 — the atlas-browse surface, beside reading_http
            // and on the same loopback posture. Unconditional for the same
            // reason: a daemon with no corpus engine answers 503 with that
            // named reason, which is a different fact from an unmounted
            // router's 404 (ARCH §18.3).
            mounted.push(crate::atlas_http::atlas_router(Arc::clone(&self_arc)));
            mount_names.push("atlas_http");
            // sv-surface D3 — the MeshApp explorer surface. Same posture and
            // the same unconditional reason as the two above: thirteen desktop
            // commands read the daemon's OWN index dir out of a second
            // process today, and a 503 naming the missing corpus engine is a
            // different fact from an unmounted route's 404.
            mounted.push(crate::meshapp_http::meshapp_router(Arc::clone(&self_arc)));
            mount_names.push("meshapp_http");
            // thin-desktop order (2026-09-11) — the enrichment-store reads:
            // the enriched-corpus inventory and the starter questions the
            // desktop used to fold from every atom it pulled over the wire.
            // Unconditional for the same reason as the three above.
            mounted.push(crate::enrich_http::enrich_router(Arc::clone(&self_arc)));
            mount_names.push("enrich_http");
            // sv-surface D6 — notes CRUD over the store the `/mcp` surface and
            // `/v1/notes/tool-outcome` already write to. Unconditional; a
            // commission whose `notes.db` would not open answers 503 naming
            // that, which is not the same fact as "no route".
            mounted.push(crate::notes_http::notes_router(Arc::clone(&self_arc)));
            mount_names.push("notes_http");
            // sv-surface D6 — the recipe-author project store. Same reason
            // again: `features.db` failing to open was a warn-and-skip the
            // daemon only wrote to a log, and this makes it a 503 a caller
            // can read.
            mounted.push(crate::features_http::features_router(Arc::clone(&self_arc)));
            mount_names.push("features_http");
            // sv-surface D8 — the living-governance surface over the
            // daemon's OWN atlas dir. Nine desktop commands opened that
            // directory's oplog from a second process, each holding its own
            // append mutex; this makes the daemon the writer. Unconditional
            // for the reason above it: no corpus engine is a named 503.
            mounted.push(crate::governance_http::governance_router(Arc::clone(
                &self_arc,
            )));
            mount_names.push("governance_http");
            // sv-surface D8 — the daemon's own external-MCP configuration
            // and the live tool counts behind its `/mcp` mount. A daemon
            // with no mount answers 200 with `mount.mounted: false`, which
            // is an ANSWER to the question rather than a failure to answer
            // it — the two config WRITE commands stay app-local, and the
            // module header records that this daemon owns no config-write
            // path to serve them over.
            mounted.push(crate::mcp_config_http::mcp_config_router(Arc::clone(
                &self_arc,
            )));
            mount_names.push("mcp_config_http");
            // sv-surface D8 — the recipe-author project COMPOSITION over
            // notes + features + the daemon's own artifact tree, the rung
            // `features_http` named and declined in D6. Unconditional; a
            // commission missing either store answers 503 naming which.
            mounted.push(crate::recipe_project_http::recipe_project_router(self_arc));
            mount_names.push("recipe_project_http");
            // sv-surface D5 — the non-watch half of `LocalCorpusManager`,
            // beside the seventeen watch routes that already serve the SAME
            // singleton. Takes no `Arc<Self>`: the manager is process state
            // (`watched_folder_runtime`), not a daemon field, which is why
            // this is the one router here built from nothing. A daemon whose
            // runtime was never installed answers 503 naming that.
            mounted.push(crate::lc_http::lc_router());
            mount_names.push("lc_http");
            // sv-surface (2026-09-11) — deep research as a daemon JOB. Takes
            // no `Arc<Self>` for `lc_http`'s reason: `launch::prepare`
            // resolves the daemon endpoint and models from `SetupConfig`
            // itself, so the router is built from nothing. A daemon whose
            // config names no models answers the capabilities route with
            // that error and `POST /v1/research` with a 400 naming it.
            mounted.push(crate::research_http::research_router());
            mount_names.push("research_http");
            for (router, name) in self
                .services
                .host_routers()
                .into_iter()
                .zip(self.services.host_router_names())
            {
                mounted.push(router);
                mount_names.push(name);
            }
        } else {
            // Mesh-admin: the venues read alone, so the setup wizard's join
            // child answers what the wizard polls (five-programs-62). The
            // `Verb` one-shots carry it too while they run.
            let self_arc = self
                .self_weak
                .upgrade()
                .expect("EmbeddedDaemon::start_daemon runs behind the Arc that owns it");
            mounted.push(crate::mesh_http::mesh_venues_router(self_arc));
            mount_names.push("mesh_venues");
        }
        info!(
            profile = self.services.label(),
            mcp = mcp_mount.is_some(),
            routers = ?mount_names,
            "daemon: client router assembled"
        );

        // The GUEST listener: a second bind of the client router, loopback-only
        // and on an ephemeral port, whose auth layer does NOT treat a loopback
        // peer as a local caller. cw-rails forwards `GUEST_ALPN` here, so a
        // `sovereign://guest/…` bearer is actually read instead of being
        // skipped by the loopback arm — see `crate::client_auth`.
        //
        // Bound HERE, before the serve task is spawned, because
        // `crate::guest_origin` below registers the resolved port and the
        // serve task runs concurrently. A bind failure is not fatal: it costs
        // guest access over the mesh and nothing else, so it is logged and
        // nothing is registered (cw-rails then refuses a guest dial rather
        // than landing it on the trusting listener).
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

        // svrn has no PEER listener since pb-mesh-exit-transport: a member
        // dialling `CLIENT_ALPN` is forwarded by cw-rails to serve's member
        // client, and a non-member on it to the guest listener above
        // (`Admit::MembersElse`, sovereign-serve rails_mesh.rs), registered
        // by `crate::guest_origin` below.

        // The rail's own listener — `rail_bind` says why it is a separate one.
        let rail_addr = sovereign_mesh::rail_bind::rail_addr(client_addr.port());
        let rail_listener = sovereign_mesh::rail_bind::bind(rail_addr).await;

        // Spawn the API servers in the background. The JoinHandle is stored
        // in `DaemonState::Running` (not discarded) so `stop_inner` can await
        // teardown — dropping the old `:9741`/`:9742` listeners — before an
        // in-process re-create (`leave_to_solo`) rebinds the same ports.
        let app_state_clone = app_state.clone();
        // The guest door — `crate::guest_door` says why it is its own bind.
        let door_state = app_state.clone();
        // The turn host for `POST /v1/guest/ask`, on both binds a guest can
        // reach: the door and the `GUEST_ALPN` forward. `AppState` carries no
        // `Runtime`, so the handler is built from this the way every other
        // turn surface in this crate is. A mesh-admin daemon upgrades fine
        // and answers 503 naming itself — the absence is reported, not
        // dressed as a 404.
        let turn_host = self.self_weak.upgrade();
        let door_turn_host = turn_host.clone();
        // The registry was resolved once, into the node's part, by
        // `NodeSeed::resolved` — the one reader of those two config keys. The
        // door serves what the rail route scopes by, because it is the same
        // value and not a second read of the same config.
        let guest_pages = door_state.guest_pages();
        let guest_bind = self.setup_config.read().await.daemon.guest_bind.clone();
        // Each start (including an in-process re-create) answers the bind
        // question afresh; the serve task publishes the outcome below.
        self.client_listener.send_replace(ClientListener::Pending);
        let listener_outcome = self.client_listener.clone();
        let serve_handle = tokio::spawn(async move {
            let mut client_router = crate::server::client_router(app_state_clone.clone());
            if let Some(m) = mcp_mount {
                // Phase 5b: a fresh `McpNotifier` with no producer is
                // fine — the daemon doesn't drive list-changed
                // notifications today (that's the per-project
                // standalone serve's job). Subscribers connect
                // harmlessly and idle until something publishes.
                client_router = client_router.merge(mcp_router::mcp_router(
                    m.tools,
                    m.notes as Arc<dyn sovereign_contracts::notes::AgentNotes>,
                    m.session_id,
                    m.code,
                    mcp_router::McpNotifier::new(),
                ));
            }
            for router in mounted {
                client_router = client_router.merge(router);
            }
            // ConnectInfo: `internal_principal_layer` reads the peer address as
            // half the "is this my own acceptor's hop" tie, and fails closed
            // without it. Same requirement the client listeners document above.
            let internal_router = crate::server::internal_router(app_state_clone.clone())
                .into_make_service_with_connect_info::<SocketAddr>();
            // The guest listener — what cw-rails' GUEST_ALPN forward serves —
            // gets the SAME guest surface as the door, pages included.
            // Without the merge a phone that tunnelled in was refused
            // /ring/ with a 403 `out_of_scope` (permits_path knows only rail
            // and API routes; pages are not in a scope's path set because
            // they are public shells — the DATA behind them is what the
            // bearer gates), so "land on the index and pick" only worked on
            // the LAN. `door_router` is the one owner of that merge; the
            // pages are grant-filtered by the index itself, exactly as they
            // are on the door's own bind.
            let guest_router = crate::guest_door::door_router(
                app_state_clone.clone(),
                guest_pages.clone(),
                turn_host,
            );
            let rail_router = crate::server::client_router_for(
                app_state_clone,
                crate::server::ClientSurface::Rail,
            );

            // A standalone code server (`svrn code mcp`) holding `:9741` is
            // not ours to stop (principle 12): the bind below fails and the
            // boot refuses, naming the address (pb-code-server).
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
                    listener_outcome.send_replace(ClientListener::Failed(e.to_string()));
                    return;
                }
            };
            let internal_listener =
                match bind_listener_with_retry(internal_addr, "internal API").await {
                    Ok(l) => l,
                    Err(e) => {
                        warn!("{e}");
                        listener_outcome.send_replace(ClientListener::Failed(e.to_string()));
                        return;
                    }
                };

            info!("Commonwealth daemon started (client: {client_addr}, internal: {internal_addr})");
            listener_outcome.send_replace(ClientListener::Bound(client_addr));

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
            // Same `ConnectInfo` reason as the guest bind: without
            // it the auth layer cannot identify the caller and fails closed
            // with a 500 on every request.
            let rail_service = rail_router.into_make_service_with_connect_info::<SocketAddr>();
            let rail_serve = async move {
                match rail_listener {
                    Some(l) => {
                        info!("ring rail listening on {rail_addr}");
                        let _ = axum::serve(l, rail_service).await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                _ = axum::serve(client_listener, client_service) => {}
                _ = axum::serve(internal_listener, internal_router) => {}
                _ = guest_serve => {}
                _ = rail_serve => {}
                _ = crate::guest_door::serve(door_state, guest_bind, guest_pages, door_turn_host) => {}
                _ = shutdown_rx => {
                    info!("Commonwealth daemon shutting down");
                }
            }
        });

        // ── The peer-facing loops, under ONE gate ──────────────────
        //
        // Gossip, admission and the ring round are cw-rails' (the node's one
        // mesh endpoint, pb-mesh-exit-transport; its copies landed with
        // pb-rails-parity). What svrn still runs here talks to cw-rails: the
        // ingest-handoff poll, the plane seal, and the two origins it
        // registers. A local-only daemon runs none of them.
        //
        // Each is recorded in `running_services` at its spawn site, so the
        // boot assertion reads what happened rather than re-deriving what
        // should have.
        let (collaborate_handle, rail_kv_pump_handle) = if local_only.is_local_only() {
            // Every one of these is a conversation with a peer, and a
            // local-only node has no other side (see `crate::local_only`'s
            // "skips the NETWORK, never the model").
            (None, None)
        } else {
            let collaborate_handle =
                crate::auto_ingest::spawn_auto_collaborate_loop(app_state.clone(), internal_port);
            running_services.record(crate::local_only::MeshService::AutoIngestCollaborate);

            // The KV drain and KV seal are cw-rails' since fp-77/fp-78 —
            // the node's KV lives there (fp-88). The `mesh-measurements`
            // and `work` seal arms stay here (five-programs-40).
            let rail_kv_pump_handle = sovereign_mesh::rail_kv_pump::spawn_plane_seal(
                app_state.inner.fabric.clone(),
                sovereign_mesh::rail_kv_pump::RAIL_KV_PUMP_INTERVAL,
            );
            running_services.record(crate::local_only::MeshService::RailKvPump);

            // The `ingest:v1` execute origin (pb-work-donor): served and
            // registered with cw-rails on the SAME networked branch the
            // donor sat on, so a local-only daemon donates no ingest work,
            // as before. The donor itself is cw-rails'. A node
            // with no corpus engine serves none, as its registry had no
            // ingest executor.
            let rails_base =
                crate::rails_client::resolve_rails_base(&self.setup_config.read().await.daemon);
            // svrn's peer routes, as an origin in cw-rails' table on the
            // same networked branch (pb-mesh-exit-transport's inbound
            // half; `crate::peer_origin` says why nothing answers through
            // it before the daemon reads cw-rails' roster).
            peer_origin_handle = crate::peer_origin::spawn(
                rails_base.clone(),
                internal_port,
                &app_state.inner.node.peer_origin_tie,
                crate::peer_origin::claims_source(app_state.clone()),
            );
            if peer_origin_handle.is_some() {
                running_services.record(crate::local_only::MeshService::PeerOrigin);
            }
            guest_origin_handle =
                guest_addr.map(|addr| crate::guest_origin::spawn(rails_base.clone(), addr.port()));
            if guest_origin_handle.is_some() {
                running_services.record(crate::local_only::MeshService::GuestOrigin);
            }
            published_origins_handle = crate::published_origins::spawn(
                rails_base.clone(),
                &self.setup_config.read().await.iroh,
            );
            if let Some(engine) = corpus_engine.clone() {
                let origin = std::sync::Arc::new(crate::work_origin::WorkOrigin::new(
                    crate::ingest_executor::IngestExecutor::new(engine),
                    app_state.self_node_id(),
                ));
                match crate::work_origin::spawn(origin, rails_base).await {
                    Ok(handle) => {
                        running_services.record(crate::local_only::MeshService::WorkOrigin);
                        work_origin_handle = Some(handle);
                    }
                    Err(e) => tracing::warn!(
                        target: crate::ingest_executor::TRACE_TARGET,
                        error = %e,
                        "work origin: no loopback port to serve it on, so no donor on this \
                         node forwards ingest work"
                    ),
                }
            }

            (Some(collaborate_handle), Some(rail_kv_pump_handle))
        };

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
        let snapshot_emitter = Arc::clone(&app_state.inner.store.contribution_emitter);
        let snapshot_engine = corpus_engine.clone();
        let (snapshot_shutdown_tx, snapshot_shutdown_rx) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            let _hold_shutdown_tx = snapshot_shutdown_tx;
            sovereign_mesh::ledger_port::run_storage_snapshot_loop(
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
                oicp_types::contributions::STORAGE_SNAPSHOT_INTERVAL,
                snapshot_shutdown_rx,
            )
            .await;
        });
        info!("StorageSnapshot loop started");

        // The contributions ledger's RetentionGc runs in cw-rails beside the
        // store it sweeps (fp-78); this daemon holds no ledger rows (fp-88).

        // Stall sweep — any non-terminal `_enrichment_state.json`
        // older than STALL_THRESHOLD_SECS is rewritten as `Stalled`
        // so the desktop chip transitions out of "starting" / "RAPTOR
        // leaves" and into "interrupted, click to retry". Cheap walk
        // of the indexes dir; runs once per daemon start and adds
        // ~tens of milliseconds at most.
        if let Some(engine) = corpus_engine.clone() {
            let indexes_dir = engine.index_dir().to_path_buf();
            match corpus_index::enrichment_state::sweep_stalled_states(&indexes_dir) {
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
                let host_state = app_state.clone();
                let host: corpus_index::ingest_port::daemon::NewsworthyHostFactory =
                    Box::new(move |corpus_id| {
                        std::sync::Arc::new(crate::newsworthy_host::MeshNewsworthyHost::new(
                            host_state, corpus_id,
                        ))
                    });
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
                if let Ok(mut slot) = app_state.inner.ingest.newsworthy_force_tick.try_write() {
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
                    let handle = engine.spawn_newsworthy_watcher(
                        host,
                        newsworthy_shutdown_rx,
                        newsworthy_force_tick_rx,
                    );
                    let _ = handle.await;
                });
                info!("WikipediaNewsworthyWatcher started");
            }
        } // freshness_enabled

        // ── The boot's own account of its network posture (ARCH §9.1) ────
        // Both halves, because a log of what STARTED cannot show what did
        // not: `spawned` is the census, `skipped` is the claim. INFO because
        // this is a lifecycle fact an operator reads once per boot (§9.2).
        info!(
            profile = local_only.label(),
            source = local_only.source().as_str(),
            spawned = ?running_services.names(),
            skipped = ?running_services.skipped_names(),
            internal_bind = %internal_addr,
            "local_only: daemon network posture resolved"
        );

        let mut state = self.state.write().await;
        *state = DaemonState::Running {
            app_state,
            client_addr,
            _collaborate_handle: collaborate_handle,
            _rail_kv_pump_handle: rail_kv_pump_handle,
            _work_origin_handle: work_origin_handle,
            _peer_origin_handle: peer_origin_handle,
            _guest_origin_handle: guest_origin_handle,
            _published_origins_handle: published_origins_handle,
            _foreground_post_handle: foreground_post_handle,
            local_only,
            running_services,
            _shutdown_tx: shutdown_tx,
            serve_handle,
        };

        Ok(())
    }
}

/// Bind a TCP listener, retrying briefly on `EADDRINUSE`.
///
/// An in-process mesh re-create (`leave_to_solo` → `create_mesh` →
/// `start_daemon`) can momentarily race the just-dropped listener socket
/// from the previous mesh. `stop_inner` already awaits the old serve task,
/// so this is belt-and-suspenders — but `SO_REUSEADDR` (which mio sets)
/// only lets a new bind past a socket in `TIME_WAIT`, NOT one still in
/// `LISTEN`, so if the old task is slow to drop we give it a few tries —
/// the host kit's `shell::bind_with_retry` (phase-b pb-shell).
///
/// On any non-`EADDRINUSE` error, or after exhausting retries, this returns
/// `MeshError::Network`; the caller (the serve task) logs it and returns
/// best-effort — a hard `start_daemon` failure here would strand the many
/// default-port tests that bind `:9741` under parallel contention.
async fn bind_listener_with_retry(
    addr: SocketAddr,
    label: &str,
) -> Result<tokio::net::TcpListener, MeshError> {
    host_kit::shell::bind_with_retry(addr, label)
        .await
        .map_err(|e| MeshError::Network(e.to_string()))
}

/// Write minimal `ModelInfo` entries into the inference store for
/// each configured local slot. The `/v1/models` handler reads from
/// this store, so without these registrations a freshly-set-up
/// daemon answers the endpoint with an empty list — misleading for
/// anyone running it as a smoke check after `sovereign setup`.
///
/// The `name` field is the file basename (stripped of `.gguf`)
/// because OpenAI-compatible clients use it as the user-visible
/// model id. The `ModelId` is a deterministic hash of the absolute
/// path so repeated calls (e.g. after an admin/reload) don't
/// accumulate duplicate entries keyed on different random IDs.
async fn register_local_model_slots(
    app_state: &AppState,
    cfg: &SetupConfig,
    node_id: NodeId,
) -> std::collections::HashMap<String, String> {
    use kernel_types::ModelId;
    use oicp_types::model_catalog::{ModelArchitecture, ModelInfo};
    use oicp_types::CapabilityProfile;
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
        return std::collections::HashMap::new();
    };

    // The slots this node advertises: the one decider serve's servable-file
    // list reads too (`sovereign_contracts::model_slots`, pb-serve-distributes).
    // Fast only when it is a distinct GGUF; each primary-pool copy and each
    // `[models.extra]` slot as its own claim, so `/v1/models` and the OICP
    // capability lookup see them.
    let slots = sovereign_contracts::model_slots::advertised_slots(models);

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
        if let Err(e) = app_state.register_model(info.clone()).await {
            tracing::warn!(
                role,
                model = %info.id,
                error = %e,
                "register_local_model_slots: the model did not reach inference_store"
            );
        } else {
            info!(
                role,
                name = %info.name,
                "registered local model in inference_store"
            );
        }

        // Add the slot alias entries. Skip extras: they're routed by
        // their slot key directly (the `[models.extra]` map already
        // gives the operator a stable name); only the canonical four
        // (primary/fast/embed/code) need alias indirection because
        // their backing GGUF can swap freely. The alias vocabulary is
        // defined ONCE in `slot_aliases::SLOT_ALIAS_POLICY` — shared
        // with `oicp_synthesis::build_self_manifest`'s advertisement
        // side so the two can't drift (the 2026-05-19 fast-alias 503).
        for key in sovereign_contracts::venue::resolution_alias_keys(role) {
            slot_aliases.insert(key, info.name.clone());
        }
    }

    // Publish the configured slot paths for `/internal/rpc-warm`'s local
    // lookup: a warm request names a file this node may already hold, and the
    // route resolves it against these paths and their directories, where every
    // shard of a split lives beside its first. The files peers FETCH are
    // serve's (model transfer, pb-serve-distributes), each shard expanded
    // there (`sovereign_compute::model_transfer::servable_for`).
    let paths: Vec<std::path::PathBuf> = slots.iter().map(|(_, p)| p.to_path_buf()).collect();
    if !paths.is_empty() {
        info!(
            files = paths.len(),
            "publishing configured model paths for the rpc-warm local lookup"
        );
        app_state.servable_model_files_reader().publish(paths);
    }
    slot_aliases
}

/// Publish the slot-alias map chat_completions and list_models resolve role
/// names through, naming where it came from.
pub(crate) fn publish_slot_aliases(
    app_state: &AppState,
    aliases: std::collections::HashMap<String, String>,
    source: &'static str,
) {
    info!(
        count = aliases.len(),
        source, "publishing slot alias map for chat_completions / list_models"
    );
    app_state.slot_aliases_reader().publish(aliases);
}

// Moved to a sibling file: inline, these put this file past its arch-gate
// slack (ARCH §3.1). `#[path]`, so the names are unchanged.
#[cfg(test)]
#[path = "tests/daemon.rs"]
mod tests;

/// Why the daemon did not start or stop. Membership refusals are cw-rails'
/// since the flip (pb-mesh-exit-transport): svrn founds, joins and admits
/// nothing, so it has none of its own.
#[derive(Debug, thiserror::Error)]
pub enum MeshError {
    #[error("Mesh daemon is already running")]
    AlreadyRunning,

    #[error("Mesh daemon is not running")]
    NotRunning,

    #[error("Configuration error: {0}")]
    Config(String),

    /// A listener that would not bind.
    #[error("Network error: {0}")]
    Network(String),
}
