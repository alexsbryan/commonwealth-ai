// SPDX-License-Identifier: AGPL-3.0-or-later
//! Embedded Commonwealth daemon lifecycle management.
//!
//! The daemon runs in-process within Sovereign — no separate binary needed.
//! It starts when the user creates or joins a mesh, and stops when they leave.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{info, warn};

use commonwealth_api::state::{AppState, LocalInferenceService};
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::Mesh;
use commonwealth_core::oicp::EmbedModelInfo;
use commonwealth_discovery::mdns::{BrowseHandle, DiscoveredPeer, MdnsDiscovery};
use commonwealth_discovery::membership;
use corpus_engine::CorpusEngine;
use corpus_engine_notes::NoteStore;
use sovereign_core::registry::ToolRegistry;
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

use crate::admin_http::{ConfigDiff, ProviderFactory};
use crate::deep_link::DeepLink;
use crate::gossip::{self, GossipHandle};
use crate::mcp_router;
use crate::mesh_discovery::{local_ip_candidates, reachable_addresses};
use crate::persist;
use crate::state::MeshState;

/// Per-session MCP mount for the embedded daemon. When set, the daemon
/// merges `mcp_router::mcp_router(...)` into its `:9741` client router
/// so `/mcp`, `/mcp/message`, and `/mcp/stats` share the port with the
/// OpenAI-compatible `/v1/*` endpoints.
#[derive(Clone)]
struct McpMount {
    tools: Arc<ToolRegistry>,
    notes: Arc<NoteStore>,
    session_id: String,
}

/// The embedded Commonwealth daemon, managed by Sovereign's UI.
pub struct EmbeddedDaemon {
    state: Arc<RwLock<DaemonState>>,
    /// Where to persist `mesh.json` so the daemon can auto-resume on
    /// app restart. Set once at construction.
    data_dir: PathBuf,
    /// The CorpusEngine this daemon consults when peers gossip-query
    /// our knowledge over `/internal/knowledge/search`, and when we
    /// publish our own `hosted_corpora` on gossip rounds.
    ///
    /// Held in an RwLock<Option<_>> because Sovereign's bootstrap
    /// constructs the daemon *before* it builds the engine (the
    /// engine needs an `EmbedFn` that isn't ready until the fast
    /// model has loaded). The desktop calls `set_corpus_engine`
    /// during bootstrap just before `try_resume`, so by the time
    /// the daemon is Running the engine is always present. Tests
    /// and the CLI's mesh subcommands keep it `None`.
    corpus_engine: RwLock<Option<Arc<CorpusEngine>>>,
    /// The Sovereign `InferenceProvider` that answers peer chat
    /// completions hitting our `/v1/chat/completions`. Same
    /// injection timing as `corpus_engine`: set during desktop
    /// bootstrap before the daemon is started. When this is
    /// absent, the daemon's handler falls through to the
    /// scheduler/llama-server path (which is empty in the
    /// Sovereign+mesh embed, so peer inference just 503s).
    inference_provider: RwLock<Option<Arc<dyn InferenceProvider>>>,
    /// Embedding model metadata advertised to mesh peers for
    /// collaborative ingestion compatibility checks. Derived at
    /// bootstrap from the loaded embed slot's actual dimensions,
    /// pooling strategy, and config-specified `embed_family`.
    /// `None` when no embed model is configured.
    embed_model: RwLock<Option<EmbedModelInfo>>,
    /// Optional MCP tool-server mount. When present, the client
    /// router on `:9741` additionally serves `/mcp`, `/mcp/message`,
    /// and `/mcp/stats`. Set at bootstrap via [`Self::set_mcp`].
    /// `None` means the daemon only serves `/v1/*` (inference, OICP
    /// capabilities, knowledge search) — no code-intelligence tools.
    mcp: RwLock<Option<McpMount>>,
    /// Pre-built axum `Router` merged into the client listener at
    /// `start_daemon` time. The daemon can't hand out an `Arc<Self>`
    /// from a `&self` method, so the caller (who already owns the
    /// outer `Arc<EmbeddedDaemon>`) builds `mesh_http::mesh_router`
    /// externally and stashes it here. `None` means the mesh HTTP
    /// surface is disabled — tests and legacy callers skip it.
    mesh_http_router: RwLock<Option<axum::Router>>,
    /// Pre-built axum `Router` for the admin HTTP surface
    /// (`POST /v1/admin/reload`). Same installation pattern as
    /// `mesh_http_router`: the CLI/desktop builds
    /// `admin_http::admin_router(Arc::clone(&daemon))` and hands it
    /// here before `start_daemon`. `None` means no admin surface —
    /// consumers must `kickstart`/`systemctl restart` to apply config
    /// changes.
    admin_http_router: RwLock<Option<axum::Router>>,
    /// Same pattern — project_http router for `/v1/projects/*`.
    /// Owned by the CLI / desktop side which holds the Reindexer.
    project_http_router: RwLock<Option<axum::Router>>,
    /// Knowledge-view HTTP router (`POST /v1/knowledge/landscape_digest`).
    /// Built by `sovereign-cli`'s daemon bootstrap once the
    /// `KnowledgeViewManager` exists; merged into the client listener
    /// at `start_daemon` time. `None` in tests / paths without the
    /// manager — the endpoint then 404s, which the desktop's
    /// `MeshLandscapeDigestClient` handles by inserting an empty
    /// digest list (identical to KnowledgeView=off).
    knowledge_view_http_router: RwLock<Option<axum::Router>>,
    /// Watched-folder HTTP router (`/internal/corpus/watch/...`).
    /// Reads the `watched_folder_runtime` singleton internally —
    /// `EmbeddedDaemon` doesn't carry the manager directly.
    corpus_watch_http_router: RwLock<Option<axum::Router>>,
    /// Reading-surface HTTP router
    /// (`/internal/corpus/{corpus_id}/chunks/...`,
    /// `/internal/corpus/{corpus_id}/atoms/...`). Built by
    /// `reading_http::reading_router(daemon_arc)` and installed by
    /// the bootstrap before `start_daemon`. `None` means the desktop
    /// reading surface won't be reachable — the chat UI still works
    /// (citation popovers fall back to the legacy path).
    reading_http_router: RwLock<Option<axum::Router>>,
    /// In-memory copy of the `SetupConfig` the daemon booted with.
    /// `admin_http::reload` diffs this against the file on disk so it
    /// knows which fields actually changed. Updated in place after a
    /// successful reload. `None` in tests / legacy callers that skip
    /// `set_setup_config`.
    setup_config: RwLock<Option<SetupConfig>>,
    /// How to rebuild an `InferenceProvider` when `models.*` changes
    /// during a reload. The daemon itself can't import
    /// `sovereign-inference` model loading without layering
    /// violations; the CLI/desktop provide a concrete factory at
    /// startup.
    provider_factory: RwLock<Option<Arc<dyn ProviderFactory>>>,
    /// Cached plaintext of the active mesh's join key, mirroring
    /// `<data_dir>/join_key.secret`. The hash is one-way, so without
    /// this the share UI couldn't render the invite link after the
    /// app restarts. Set on `create_mesh` / `join_mesh` /
    /// `try_resume`; refreshed on `set_join_key` (called by the
    /// rotate handler); cleared on `stop`.
    join_key_plaintext: RwLock<Option<String>>,
    /// Optional `StateStore` handle used by the reading-surface HTTP
    /// router to resolve conversation-history chunks back to their
    /// owning conversation (title, updated_at). The daemon doesn't
    /// otherwise need a state store — search/inference goes through
    /// the runtime — so this slot is only set by the desktop's
    /// bootstrap when it wants reading-surface conversation
    /// rendering. `None` means conversation chunks render with no
    /// title metadata; the surface still shows the chunk text.
    state_store: RwLock<Option<Arc<dyn StateStore>>>,
    /// Optional `MeshStore` injected by the bootstrap. When set, the
    /// daemon uses this for `AppState.mesh_store` instead of building
    /// its own in-memory instance — letting other subsystems (e.g.
    /// the work atlas) write into the SAME store the gossip layer
    /// publishes from. Same injection timing as `set_corpus_engine`:
    /// set during bootstrap before `start_daemon`. When `None`,
    /// `start_daemon` falls back to the legacy in-memory MeshStore.
    mesh_store: RwLock<Option<Arc<commonwealth_state::MeshStore>>>,
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
        _shutdown_tx: tokio::sync::oneshot::Sender<()>,
        /// Server-half iroh endpoint + acceptor (Track W, W1 — see
        /// `crate::iroh_access`). `None` unless iroh is enabled
        /// (explicit config or mesh participation). Read live by
        /// invite generation (`create_mesh_with` / `current_invite`)
        /// for the dial string; its Drop ties the acceptor to the
        /// Running variant, so leaving the mesh / stopping the daemon
        /// also stops accepting dial-by-key traffic, same pattern as
        /// `_browse_handle`.
        iroh_access: Option<crate::iroh_access::MeshIrohAccess>,
    },
}

/// Distinguishes "user wants to leave the mesh" from "process is
/// being shut down gracefully". Both stop the in-memory daemon, but
/// only Leave wipes the on-disk persistence — Shutdown preserves it
/// so the next launch resumes into the same mesh.
#[derive(Debug, Clone, Copy)]
enum StopMode {
    Leave,
    Shutdown,
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

/// Result of joining an existing mesh.
pub struct JoinMeshResult {
    pub mesh_name: String,
    pub node_id: String,
    /// This node's own client-API token once exposed — so the joiner
    /// can in turn admit further peers/clients. See `CreateMeshResult`.
    pub client_token: Option<String>,
}

impl EmbeddedDaemon {
    /// Construct a daemon that persists its running-mesh state to
    /// `data_dir/mesh.json`. Call [`try_resume`](Self::try_resume)
    /// once at app start to re-attach to a previously-created mesh.
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            state: Arc::new(RwLock::new(DaemonState::Stopped)),
            data_dir,
            corpus_engine: RwLock::new(None),
            inference_provider: RwLock::new(None),
            embed_model: RwLock::new(None),
            mcp: RwLock::new(None),
            mesh_http_router: RwLock::new(None),
            admin_http_router: RwLock::new(None),
            project_http_router: RwLock::new(None),
            knowledge_view_http_router: RwLock::new(None),
            corpus_watch_http_router: RwLock::new(None),
            reading_http_router: RwLock::new(None),
            setup_config: RwLock::new(None),
            provider_factory: RwLock::new(None),
            join_key_plaintext: RwLock::new(None),
            state_store: RwLock::new(None),
            mesh_store: RwLock::new(None),
        }
    }

    /// Inject a `MeshStore` for the daemon to use as `AppState.mesh_store`.
    /// Call before `start_daemon`. Lets the bootstrap pre-construct a
    /// shared `Arc<MeshStore>` and hand the same handle to other
    /// subsystems (e.g. the work atlas) so writes from those modules
    /// reach the gossip layer's `all_entries_for_gossip` enumeration.
    pub async fn set_mesh_store(&self, store: Arc<commonwealth_state::MeshStore>) {
        *self.mesh_store.write().await = Some(store);
    }

    /// Legacy constructor that doesn't persist — use only in tests
    /// where a tempdir isn't worth setting up. Production code must
    /// prefer `new(data_dir)`.
    pub fn new_in_memory() -> Self {
        Self {
            state: Arc::new(RwLock::new(DaemonState::Stopped)),
            data_dir: PathBuf::new(),
            corpus_engine: RwLock::new(None),
            inference_provider: RwLock::new(None),
            embed_model: RwLock::new(None),
            mcp: RwLock::new(None),
            mesh_http_router: RwLock::new(None),
            admin_http_router: RwLock::new(None),
            project_http_router: RwLock::new(None),
            knowledge_view_http_router: RwLock::new(None),
            corpus_watch_http_router: RwLock::new(None),
            reading_http_router: RwLock::new(None),
            setup_config: RwLock::new(None),
            provider_factory: RwLock::new(None),
            join_key_plaintext: RwLock::new(None),
            state_store: RwLock::new(None),
            mesh_store: RwLock::new(None),
        }
    }

    /// Install an MCP tool mount so the client router on `:9741` also
    /// serves `/mcp`, `/mcp/message`, and `/mcp/stats`. Call once
    /// during bootstrap *before* `try_resume` / `create_mesh` /
    /// `join_mesh`. Passing a session id (e.g. `serve-<uuid>`) groups
    /// per-process tool calls in `NoteStore::log_tool_call`.
    pub async fn set_mcp(
        &self,
        tools: Arc<ToolRegistry>,
        notes: Arc<NoteStore>,
        session_id: String,
    ) {
        *self.mcp.write().await = Some(McpMount {
            tools,
            notes,
            session_id,
        });
    }

    /// Install the mesh HTTP API so `/v1/mesh/status` + `create` +
    /// `join` + `rotate` + `leave` are served on the same `:9741`
    /// listener. The caller builds the router with
    /// [`mesh_http::mesh_router(Arc::clone(&daemon_arc))`]
    /// (which captures the `Arc<EmbeddedDaemon>` the caller owns) and
    /// hands it here. We can't build the router internally from
    /// `&self` because axum handlers need an `Arc<Self>` and this
    /// method can't conjure one.
    ///
    /// Call once at bootstrap, before `start_daemon`. Calling again
    /// later replaces the previously installed router; the change
    /// won't take effect until the next `start_daemon` cycle.
    pub async fn install_mesh_http_router(&self, router: axum::Router) {
        *self.mesh_http_router.write().await = Some(router);
    }

    /// Install the admin HTTP router (`POST /v1/admin/reload`). Same
    /// installation shape as [`install_mesh_http_router`] — the caller
    /// builds `admin_http::admin_router(Arc::clone(&daemon))` and hands
    /// it here. Must be called before `start_daemon` for the route to
    /// be live; a later install affects only the next restart.
    /// Install the project HTTP router (`GET /v1/projects`,
    /// register / unregister / rebuild). Same shape as
    /// [`install_admin_http_router`].
    pub async fn install_project_http_router(&self, router: axum::Router) {
        *self.project_http_router.write().await = Some(router);
    }

    pub async fn install_admin_http_router(&self, router: axum::Router) {
        *self.admin_http_router.write().await = Some(router);
    }

    /// Install the knowledge-view HTTP router
    /// (`POST /v1/knowledge/landscape_digest`). Same shape as
    /// [`install_admin_http_router`] — caller builds
    /// `landscape_digest_http::landscape_digest_router(Arc::clone(&mgr))`
    /// and hands it here. `None` (no install) means the endpoint is
    /// not exposed; an attached desktop's HTTP client soft-fails to
    /// an empty digest list in that case.
    pub async fn install_knowledge_view_http_router(&self, router: axum::Router) {
        *self.knowledge_view_http_router.write().await = Some(router);
    }

    /// Install the watched-folder HTTP router
    /// (`/internal/corpus/watch/...`). Same pattern as
    /// [`install_knowledge_view_http_router`]: caller builds
    /// `corpus_watch_http::corpus_watch_router()` and hands it here.
    /// Must be called before `start_daemon` for the routes to bind;
    /// a later install affects only the next restart.
    pub async fn install_corpus_watch_http_router(&self, router: axum::Router) {
        *self.corpus_watch_http_router.write().await = Some(router);
    }

    /// Install the reading-surface HTTP router
    /// (`/internal/corpus/{corpus}/chunks/...` and
    /// `/internal/corpus/{corpus}/atoms/...`). Same lifecycle as
    /// the other `install_*_http_router` setters: caller builds
    /// `reading_http::reading_router(Arc::clone(&daemon))` and
    /// hands it here before `start_daemon`. Loopback-guarded.
    pub async fn install_reading_http_router(&self, router: axum::Router) {
        *self.reading_http_router.write().await = Some(router);
    }

    /// Record the `SetupConfig` this daemon booted with. The admin
    /// reload handler diffs future on-disk states against this value
    /// to figure out which fields actually changed. Called once by
    /// `sovereign daemon run` right after it loads the config, and
    /// again after every successful reload so the in-memory baseline
    /// moves forward.
    pub async fn set_setup_config(&self, cfg: SetupConfig) {
        *self.setup_config.write().await = Some(cfg);
    }

    /// Resolve the `(client_port, internal_port)` pair this daemon
    /// should bind and advertise. Pulls from
    /// `setup_config.daemon.{client_port, internal_port}` when a
    /// config has been installed (`set_setup_config`); otherwise
    /// returns the historic `(9741, 9742)` defaults.
    ///
    /// Use this in every place that previously hardcoded 9741 or
    /// 9742 for *this* daemon's binding decisions: `create_mesh`,
    /// `join_mesh`, `start_daemon`'s listener bind, the mDNS
    /// announce, and the auto-collaborate loop's spawn.
    ///
    /// **Scope note (peer-side uniformity).** The peer-targeting
    /// rewrites in `peer_inference_endpoints` and
    /// `auto_ingest`'s candidate-URL builder still assume every
    /// peer uses the same port pair as this daemon — they apply
    /// `client_port` from `resolved_ports` to all peers
    /// uniformly. Mixed-port mesh deployments need a wire-protocol
    /// change (a `client_port` field on `MemberRecord`) and are
    /// tracked separately in §10.1.
    pub(crate) async fn resolved_ports(&self) -> (u16, u16) {
        if let Some(cfg) = self.setup_config.read().await.as_ref() {
            (cfg.daemon.client_port, cfg.daemon.internal_port)
        } else {
            (9741, 9742)
        }
    }

    /// Install the `ProviderFactory` the admin reload handler uses to
    /// rebuild an `InferenceProvider` when `models.*` fields change.
    /// Without one, a reload that touches model paths fails at the
    /// HTTP layer rather than silently swallowing the change.
    pub async fn set_provider_factory(&self, factory: Arc<dyn ProviderFactory>) {
        *self.provider_factory.write().await = Some(factory);
    }

    /// Re-read `SetupConfig` from disk (or from `config_path_override`
    /// if supplied by a test), diff against the in-memory baseline,
    /// and apply whatever is hot-reloadable. Returns the per-field
    /// report the HTTP layer serialises as [`ReloadResponse`].
    ///
    /// Semantics:
    /// - `models.*` changes → rebuild the provider via
    ///   [`set_provider_factory`]'s factory, then swap atomically
    ///   through [`set_inference_provider`]. In-flight requests
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
        let current = self
            .setup_config
            .read()
            .await
            .clone()
            .ok_or_else(|| "no SetupConfig installed on this daemon".to_string())?;

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
            let factory = self
                .provider_factory
                .read()
                .await
                .clone()
                .ok_or_else(|| "models changed but no ProviderFactory installed".to_string())?;
            let new_provider = factory.build_provider(&fresh).await?;
            self.set_inference_provider(new_provider).await;
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
        *self.setup_config.write().await = Some(fresh);

        Ok(crate::admin_http::ReloadResponse {
            reloaded_fields: reloaded,
            restart_required_fields,
            restart_required,
        })
    }

    /// Install a `CorpusEngine` so that when the daemon starts, its
    /// `AppState` has something to search — without this, the
    /// handlers on `/v1/knowledge/search` and
    /// `/internal/knowledge/search` return 503 and peers asking us
    /// for philosophy passages see an empty mesh. Call once, during
    /// Sovereign's bootstrap, *before* `try_resume` / `create_mesh`
    /// / `join_mesh` so the first gossip round that runs after
    /// startup already advertises our real `hosted_corpora`.
    ///
    /// If called while the daemon is already running, the engine is
    /// swapped in — useful when bootstrap rebuilds the engine mid-
    /// session (e.g. the user changes the embed model). Existing
    /// Arc<AppState> instances captured by running HTTP tasks keep
    /// the old engine; the next created `AppState` (after a
    /// `stop` + restart) will pick up the new one.
    pub async fn set_corpus_engine(&self, engine: Arc<CorpusEngine>) {
        *self.corpus_engine.write().await = Some(engine);
    }

    /// Borrow the currently-installed `CorpusEngine`, if any. Used
    /// by HTTP routers (notably `reading_http`) that need to fetch
    /// chunks on demand without taking ownership. Returns `None`
    /// when bootstrap hasn't installed an engine yet.
    pub async fn corpus_engine(&self) -> Option<Arc<CorpusEngine>> {
        self.corpus_engine.read().await.clone()
    }

    /// Install the `StateStore` the reading-surface HTTP router uses
    /// to look up conversation metadata when serving
    /// `conversation-history` chunks. Same injection-timing
    /// expectations as `set_corpus_engine`: call from the desktop
    /// bootstrap once the SQLite store is open, before
    /// `start_daemon`. Tests / CLI mesh paths skip this and the
    /// reading surface degrades gracefully (chunk text without a
    /// resolved conversation title).
    pub async fn set_state_store(&self, store: Arc<dyn StateStore>) {
        *self.state_store.write().await = Some(store);
    }

    /// Borrow the currently-installed `StateStore`, if any.
    /// `reading_http` calls this when a chunk's `corpus_id` is
    /// `"conversation-history"` to resolve `source_doc_id` →
    /// conversation title.
    pub async fn state_store(&self) -> Option<Arc<dyn StateStore>> {
        self.state_store.read().await.clone()
    }

    /// Install the `InferenceProvider` that answers peer chat
    /// completions. Same injection timing as `set_corpus_engine`:
    /// call during desktop bootstrap, before any mesh start. The
    /// same provider Sovereign uses for the local user's chats —
    /// a peer asking us for synthesis gets the same quality a
    /// local user would.
    pub async fn set_inference_provider(&self, provider: Arc<dyn InferenceProvider>) {
        *self.inference_provider.write().await = Some(provider);
    }

    /// Record the embedding model metadata so that when the daemon starts,
    /// the Commonwealth `AppState` can advertise the correct model to peers
    /// evaluating collaborative ingestion compatibility. Call during desktop
    /// bootstrap, after the embed model has been probed for actual dimensions.
    pub async fn set_embed_model_info(&self, info: EmbedModelInfo) {
        *self.embed_model.write().await = Some(info);
    }

    fn persistence_enabled(&self) -> bool {
        !self.data_dir.as_os_str().is_empty()
    }

    /// If a mesh has been persisted from a previous session, start
    /// the daemon with that mesh so mDNS advertises immediately and
    /// existing members can reconnect without the user recreating.
    /// No-op if no persisted file exists or if persistence is
    /// disabled (the `new_in_memory` constructor).
    pub async fn try_resume(&self) -> Result<bool, MeshError> {
        if !self.persistence_enabled() {
            return Ok(false);
        }
        if self.is_running().await {
            return Ok(false);
        }
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
            let endpoint = {
                let state = self.state.read().await;
                match &*state {
                    DaemonState::Running {
                        app_state,
                        iroh_access: Some(access),
                        ..
                    } => {
                        if expires_at.is_some() {
                            app_state.set_join_key_expiry(expires_at);
                        }
                        Some(access.endpoint_handle())
                    }
                    _ => None,
                }
            };
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
                if let Err(e) = persist::save(&self.data_dir, &live, node_id) {
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
            // Snapshot member count under the read lock. We pull
            // `app_state` here even though we don't keep a handle
            // to it past the check — refusing without observing
            // the live mesh would either always-refuse or
            // always-allow, both worse than this honest probe.
            let live_members: Option<(String, usize)> = {
                let state = self.state.read().await;
                match &*state {
                    DaemonState::Running { app_state, .. } => {
                        let mesh = app_state.inner.mesh.read().await;
                        Some((mesh.name.clone(), mesh.members.len()))
                    }
                    DaemonState::Stopped => None,
                }
            };
            if let Some((mesh_name_now, member_count)) = live_members {
                if member_count > 1 {
                    tracing::warn!(
                        mesh_name = %mesh_name_now,
                        members = member_count,
                        "join_mesh: refusing to auto-leave a populated mesh"
                    );
                    return Err(MeshError::AlreadyInPopulatedMesh {
                        mesh_name: mesh_name_now,
                        members: member_count,
                    });
                }
                tracing::info!(
                    mesh_name = %mesh_name_now,
                    "join_mesh: daemon is in a solo mesh — auto-leaving before joining"
                );
            }
            // Solo mesh (or daemon already stopped — leave is a
            // no-op then). Safe to clear state and proceed with
            // the new handshake.
            let _ = self.leave().await;
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
            commonwealth_transport::identity::sign_join_proof(
                &identity_key,
                &stable_id,
                node_name,
            ),
        ));

        // Self-hosted relays (if configured) for the join's one-shot
        // iroh endpoint, so a joiner behind a firewall that blocks n0's
        // relays reaches the founder via the fleet's own relay (W4).
        // Empty = n0 default.
        let join_relay_urls: Vec<String> = {
            let guard = self.setup_config.read().await;
            guard
                .as_ref()
                .map(|c| c.iroh.relay_urls.clone())
                .unwrap_or_default()
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
                &join_relay_urls,
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
                iroh_dial
                    .as_deref()
                    .map(|d| (d, identity_key.to_bytes())),
                &join_relay_urls,
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
                // Tear down the placeholder daemon so the next attempt
                // from the UI doesn't hit AlreadyRunning. Use leave —
                // we never persisted the placeholder mesh, so leave's
                // clear is a no-op against persistence and matches the
                // user-facing intent ("the join failed, go back to no-mesh").
                let _ = self.leave().await;
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
                if let Err(e) = persist::save(&self.data_dir, &live, adopted_node_id) {
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
            DaemonState::Running { _shutdown_tx, .. } => {
                // Dropping the sender signals the daemon to shut down.
                drop(_shutdown_tx);
                // Drop the write guard before touching the filesystem
                // — persistence shouldn't gate the in-memory stop.
                drop(state);
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
                }
                if matches!(mode, StopMode::Leave) {
                    *self.join_key_plaintext.write().await = None;
                }
                match mode {
                    StopMode::Leave => info!("mesh daemon stopped (left mesh)"),
                    StopMode::Shutdown => info!("mesh daemon stopped (preserving mesh state)"),
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
        // The exp param mirrors the ARMED founder-side expiry — read,
        // never re-armed here, or every status poll would extend the
        // invite forever. Rotation is what re-arms (see mesh_http's
        // rotate handler).
        let expires_at = if require_encryption {
            app_state.join_key_expiry()
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

    /// Replace the in-memory cached plaintext join key. Called by
    /// the rotate HTTP handler after `persist::rotate_join_key` so
    /// the next status poll surfaces the new link without needing
    /// a daemon restart.
    pub async fn set_join_key(&self, key: String) {
        *self.join_key_plaintext.write().await = Some(key);
    }

    /// Arm a fresh invite TTL for an ENCRYPTED mesh's rotated key.
    /// Rotation is the one place a TTL gets re-armed — `current_invite`
    /// only READS the armed expiry (re-arming on status polls would
    /// extend the invite forever). No-op (returns `None`) on a
    /// plaintext mesh or a stopped daemon.
    pub async fn rearm_join_key_expiry(&self) -> Option<u64> {
        let state = self.state.read().await;
        let app_state = match &*state {
            DaemonState::Running { app_state, .. } => app_state.clone(),
            DaemonState::Stopped => return None,
        };
        drop(state);
        if !app_state.inner.mesh.read().await.require_encryption {
            return None;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let expires_at = now + INVITE_TTL_SECS;
        app_state.set_join_key_expiry(Some(expires_at));
        Some(expires_at)
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
            DaemonState::Running { mdns, .. } => {
                mdns.as_ref().map(|m| m.discovered_peers()).unwrap_or_default()
            }
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

    pub async fn discover_rpc_workers(&self) -> Vec<String> {
        let app_state = {
            let state = self.state.read().await;
            match &*state {
                DaemonState::Running { app_state, .. } => app_state.clone(),
                DaemonState::Stopped => return Vec::new(),
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
            Err(_) => return Vec::new(),
        };

        let mut out = Vec::new();
        for m in members {
            let name = m.name.clone();
            let probes = transport
                .endpoints(
                    &commonwealth_transport::peer_contact(&m),
                    commonwealth_transport::TrafficClass::StatusProbe,
                )
                .await;
            for probe in &probes {
                let status_url = format!("{}/status", probe.base_url);
                // The RPC worker speaks raw TCP at `host:port` — an
                // IP-overlay address by construction, so derive the
                // host from the probe URL's authority. A future
                // identity-keyed transport must keep rpc-server
                // traffic on the IP overlay (or a tunnel proxy);
                // see the commonwealth-transport docs.
                let Some(host) = probe
                    .base_url
                    .strip_prefix("http://")
                    .and_then(|a| a.rsplit_once(':'))
                    .map(|(host, _)| host.to_string())
                else {
                    continue;
                };
                if let Ok(resp) = client.get(&status_url).send().await {
                    if resp.status().is_success() {
                        if let Ok(json) = resp.json::<serde_json::Value>().await {
                            if let Some(port) = json
                                .get("rpc_worker")
                                .and_then(|w| w.get("port"))
                                .and_then(|p| p.as_u64())
                            {
                                let ep = format!("{host}:{port}");
                                tracing::debug!(peer = %name, endpoint = %ep, "discovered mesh RPC worker");
                                out.push(ep);
                                break; // one reachable address per peer suffices
                            }
                        }
                    }
                }
            }
        }
        out
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
        let corpus_engine = self.corpus_engine.read().await.clone();
        let mesh_store = match self.mesh_store.read().await.clone() {
            Some(provided) => provided,
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
            app_state.with_local_inference(adapter).with_rpc_shard_warmer(warmer)
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
            if let Some(cfg) = self.setup_config.read().await.as_ref() {
                let secs = cfg.daemon.yield_to_foreground_secs;
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
        if let Some(cfg) = self.setup_config.read().await.as_ref() {
            let max = cfg.daemon.max_peer_inflight;
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
        if let Some(embed_info) = self.embed_model.read().await.as_ref() {
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

        // Register the locally-loaded model slots so `/v1/models`
        // answers with something meaningful instead of an empty list.
        // Without this, the OpenAI-compatible models list returns
        // `{"object":"list","data":[]}` on a freshly-set-up daemon —
        // confusing for anyone running `curl /v1/models` as a
        // post-setup health check. We register one `ModelInfo` per
        // configured slot (primary / fast / embed) with a
        // deterministic ModelId so reloads don't create duplicates.
        if let Some(cfg) = self.setup_config.read().await.as_ref() {
            register_local_model_slots(&app_state, cfg, node_id);
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
            let guard = self.setup_config.read().await;
            match guard.as_ref() {
                Some(c) => (
                    c.daemon.client_bind.clone(),
                    c.daemon.client_token.clone(),
                    c.daemon.internal_bind.clone(),
                ),
                None => ("127.0.0.1".to_string(), None, "0.0.0.0".to_string()),
            }
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
        let client_addr: SocketAddr =
            format!("{client_bind}:{client_port}").parse().unwrap_or_else(|_| {
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
            let guard = self.setup_config.read().await;
            mdns_enabled_effective(guard.as_ref().map(|c| c.discovery.mdns).unwrap_or(true))
        };
        let (mdns, browse_handle): (Option<Arc<MdnsDiscovery>>, Option<BrowseHandle>) =
            if mdns_enabled {
                let mdns =
                    MdnsDiscovery::new(node_id, &mesh_id_hex, &mesh_name, &node_name, internal_port)
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

        // Snapshot the MCP mount + installed mesh HTTP router (if any)
        // before moving app_state into the spawn. Both are cheap:
        // Option<McpMount> clones 3 Arc bumps, Option<axum::Router>
        // clones internal Arcs.
        let mcp_mount = self.mcp.read().await.clone();
        let mesh_http = self.mesh_http_router.read().await.clone();
        let admin_http = self.admin_http_router.read().await.clone();
        let project_http = self.project_http_router.read().await.clone();
        let knowledge_view_http = self.knowledge_view_http_router.read().await.clone();
        let corpus_watch_http = self.corpus_watch_http_router.read().await.clone();
        let reading_http = self.reading_http_router.read().await.clone();

        // Spawn the API servers in the background.
        let app_state_clone = app_state.clone();
        tokio::spawn(async move {
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
            if let Some(mesh_http_router) = mesh_http {
                client_router = client_router.merge(mesh_http_router);
            }
            if let Some(admin_http_router) = admin_http {
                client_router = client_router.merge(admin_http_router);
            }
            if let Some(project_http_router) = project_http {
                client_router = client_router.merge(project_http_router);
            }
            if let Some(knowledge_view_http_router) = knowledge_view_http {
                client_router = client_router.merge(knowledge_view_http_router);
            }
            if let Some(corpus_watch_http_router) = corpus_watch_http {
                client_router = client_router.merge(corpus_watch_http_router);
            }
            if let Some(reading_http_router) = reading_http {
                client_router = client_router.merge(reading_http_router);
            }
            let internal_router = commonwealth_api::server::internal_router(app_state_clone);

            // Phase 3 takeover: a `sovereign init` invocation may
            // have spawned a standalone `sovereign serve` background
            // process holding `:9741`. Before we bind, look for the
            // pid pointer in `~/.sovereign/server.pid` and SIGTERM
            // the process so we can take ownership of the port. If
            // the daemon was started by a service manager (launchd,
            // systemd) on a fresh boot, the pointer file won't exist
            // and this is a no-op.
            takeover_standalone_serve_if_present();

            let client_listener = match tokio::net::TcpListener::bind(client_addr).await {
                Ok(l) => l,
                Err(e) => {
                    warn!("Failed to bind client API on {client_addr}: {e}");
                    return;
                }
            };
            let internal_listener = match tokio::net::TcpListener::bind(internal_addr).await {
                Ok(l) => l,
                Err(e) => {
                    warn!("Failed to bind internal API on {internal_addr}: {e}");
                    return;
                }
            };

            info!("Commonwealth daemon started (client: {client_addr}, internal: {internal_addr})");

            // Enumerate local non-loopback IPs and log them so the
            // founder can copy one into a `?relay=<IP>` query param
            // if mDNS doesn't reach the joiner (e.g. WiFi AP
            // isolation, router multicast filtering, different
            // subnets). Matches the exact workaround documented in
            // the Tailscale section of the crate README.
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
            tokio::select! {
                _ = axum::serve(client_listener, client_service) => {}
                _ = axum::serve(internal_listener, internal_router) => {}
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
            .as_ref()
            .map(|cfg| cfg.daemon.freshness_watchers_enabled)
            .unwrap_or(true);
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
        let (cfg_iroh_enabled, iroh_transport_cfg, iroh_relay_urls) = {
            let guard = self.setup_config.read().await;
            match guard.as_ref() {
                Some(c) => (
                    c.iroh.enabled,
                    c.iroh.transport.clone(),
                    c.iroh.relay_urls.clone(),
                ),
                None => (None, Default::default(), Vec::new()),
            }
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
        let iroh_access = crate::iroh_access::MeshIrohAccess::start(
            &self.data_dir,
            internal_port,
            client_port,
            iroh_enabled,
            &iroh_relay_urls,
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
                commonwealth_transport::TrafficClass::ALL.into_iter().collect(),
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
            app_state.install_self_iroh_dialinfo(access.dial_info_provider());

            // W3: when `[iroh.transport]` flips one or more classes to
            // iroh, re-install a `RoutedTransport` that routes those
            // classes over iroh (dialing from THIS endpoint) and falls
            // back to the `ip_transport` above per dial. No flip => the
            // plain `IpTransport` install above stands, unchanged.
            if !iroh_routed_classes.is_empty() {
                let iroh_t: Arc<dyn commonwealth_transport::PeerTransport> =
                    Arc::new(access.client_transport());
                let mut per_class = std::collections::HashMap::new();
                for class in &iroh_routed_classes {
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

        let mut state = self.state.write().await;
        *state = DaemonState::Running {
            app_state,
            mesh_state,
            client_addr,
            mdns,
            _browse_handle: browse_handle,
            _gossip_handle: gossip_handle,
            _shutdown_tx: shutdown_tx,
            iroh_access,
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

/// Write minimal `ModelInfo` entries into the inference store for
/// each configured local slot. The `/v1/models` handler reads from
/// this store, so without these registrations a freshly-set-up
/// Phase 3 takeover: when the daemon is starting, look for a PID
/// file written by `sovereign serve --background` (which `sovereign
/// init` invokes before the user gets around to running the
/// daemon). If we find a live process, SIGTERM it and wait briefly
/// so the port is free by the time we bind. The pid pointer lives
/// at `~/.sovereign/server.pid` so this works regardless of which
/// project directory the daemon is launched from.
///
/// This is best-effort. Failures are logged at info level and the
/// caller proceeds — if the port really is held by something the
/// daemon can't displace, the subsequent `bind()` will fail loudly
/// with the actual error. We don't want this helper to be a
/// hard-stop in the daemon path.
fn takeover_standalone_serve_if_present() {
    let Some(home) = dirs::home_dir() else { return };
    let pid_path = home.join(".sovereign").join("server.pid");
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

    let mut slots: Vec<(String, &std::path::Path)> = vec![
        ("primary".into(), cfg.models.primary.as_path()),
        ("embed".into(), cfg.models.embed.as_path()),
    ];
    // Mesh-advertise fast only when it's a distinct GGUF. If the
    // primary subsumes the fast role, a separate "fast" advertisement
    // would mislead peers into thinking there are two chat models on
    // this node when there's actually one.
    if cfg.models.has_explicit_fast() {
        slots.push(("fast".into(), cfg.models.fast_path()));
    }
    if let Some(code_path) = cfg.models.code.as_ref() {
        slots.push(("code".into(), code_path.as_path()));
    }
    // Multi-primary pool: register N additional primary-class slots so
    // a high-VRAM host (e.g. MI300X 192 GB) can serve concurrent
    // chat-completion requests without queueing against a single slot.
    // Each pool member is registered under `primary_<i>` and points at
    // the same GGUF; the OICP capability advertiser surfaces them as
    // distinct claims so the scheduler can dispatch round-robin.
    if let Some(pool) = cfg.models.primary_pool.as_ref() {
        for i in 0..pool.copies {
            slots.push((format!("primary_{i}"), pool.path.as_path()));
        }
    }
    // Operator-declared additional chat slots from `[models.extra]`
    // also need to land in `inference_store` so `/v1/models`
    // advertises them. Without this entry, clients sending
    // `model: "<extras-stem>"` would see a 404 from the OICP
    // capability lookup before the slot picker ever runs.
    for (slot_name, path) in cfg.models.extra.iter() {
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
    let mut servable: Vec<std::path::PathBuf> = Vec::new();
    let mut seen: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    for (_, path) in &slots {
        let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if seen.insert(canon.clone()) {
            servable.push(canon);
        }
    }
    if !servable.is_empty() {
        info!(
            files = servable.len(),
            "installing servable model files allowlist for peer fetch"
        );
        app_state.install_servable_model_files(servable);
    }
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

impl Default for EmbeddedDaemon {
    /// In-memory default — useful for tests and quick scripts, but
    /// never used from the desktop app which calls
    /// `EmbeddedDaemon::new(data_dir)` to get persistence.
    fn default() -> Self {
        Self::new_in_memory()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::setup_config::{DaemonSection, DataSection, ModelsSection, SetupConfig};
    use std::path::PathBuf;

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
            id: commonwealth_core::ids::MeshId::generate(),
            name: "test".into(),
            join_key_hash: [0u8; 32],
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
            models: ModelsSection {
                primary: PathBuf::from("/m/qwen3-coder-30b.gguf"),
                fast: Some(PathBuf::from("/m/qwen3-1.7b.gguf")),
                embed: PathBuf::from("/m/qwen3-embedding-0.6b.gguf"),
                code: None,
                context_size: None,
                max_extras_memory_gb: None,
                extra: std::collections::BTreeMap::new(),
                primary_pool: None,
            },
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

    /// `join_mesh` was called on a daemon already in a populated mesh
    /// (members beyond self). The auto-leave-then-join shortcut is
    /// fine for switching out of a freshly-created solo mesh, but
    /// against a real mesh it would persist::clear before attempting
    /// the new handshake — if that handshake then failed for any
    /// reason (bad key, no peer, network), the user is left with the
    /// old mesh's `mesh.json` already deleted on disk. Refuse early
    /// so the destructive step never runs without an explicit
    /// `mesh leave` from the caller.
    #[error(
        "Cannot auto-switch meshes: already in '{mesh_name}' with {members} member(s). \
         Run `sovereign mesh leave` first if you intend to switch."
    )]
    AlreadyInPopulatedMesh { mesh_name: String, members: usize },
}
