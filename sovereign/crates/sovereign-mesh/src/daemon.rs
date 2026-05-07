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
use corpus_engine::{CorpusEngine, NoteStore};
use sovereign_core::registry::ToolRegistry;
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::{InferenceProvider, StateStore};

use crate::admin_http::{ConfigDiff, ProviderFactory};
use crate::deep_link::DeepLink;
use crate::gossip::{self, GossipHandle};
use crate::mcp_router;
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
        mdns: Arc<MdnsDiscovery>,
        /// Dropping this handle stops the background browse task.
        /// Underscore-prefixed because it's held purely for its Drop
        /// impl.
        _browse_handle: BrowseHandle,
        /// Aborts the gossip heartbeat loop on Drop. Same pattern
        /// as `_browse_handle` — tying the task's lifetime to the
        /// Running variant means stopping the daemon also stops
        /// gossip; no explicit teardown.
        _gossip_handle: GossipHandle,
        _shutdown_tx: tokio::sync::oneshot::Sender<()>,
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
}

/// Result of joining an existing mesh.
pub struct JoinMeshResult {
    pub mesh_name: String,
    pub node_id: String,
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
        }
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
        *self.mcp.write().await = Some(McpMount { tools, notes, session_id });
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
                .ok_or_else(|| {
                    "models changed but no ProviderFactory installed"
                        .to_string()
                })?;
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
    pub async fn set_inference_provider(
        &self,
        provider: Arc<dyn InferenceProvider>,
    ) {
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
            DaemonState::Running { app_state, .. } => {
                Some(app_state.self_node_id())
            }
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

    /// Where mesh state + setup are persisted. Needed by the HTTP
    /// mesh API's rotate handler, which talks to `persist::rotate_join_key`
    /// directly rather than going through a daemon method.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Create a new mesh and start the daemon.
    pub async fn create_mesh(
        &self,
        mesh_name: &str,
        node_name: &str,
    ) -> Result<CreateMeshResult, MeshError> {
        if self.is_running().await {
            return Err(MeshError::AlreadyRunning);
        }

        let internal_port = 9742u16;
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
        let (mesh, join_key) = membership::init_mesh_with_node_id(
            mesh_name,
            node_name,
            addrs,
            stable_id,
        );
        let node_id = stable_id;
        let _ = mesh
            .members
            .keys()
            .next()
            .copied()
            .ok_or_else(|| MeshError::Config("no node in mesh".into()))?;

        let join_link = crate::deep_link::build_join_link(
            &join_key,
            None, // relay_hint — local network for now
            Some(mesh_name),
        );

        self.start_daemon(mesh, node_id).await?;

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
        // Auto-leave any existing mesh before joining a new one.
        //
        // Why: after `sovereign setup`, the CLI daemon
        // (`daemon_cmd.rs`) auto-creates a solo mesh at boot so it
        // has a valid state to gossip from. If the user then pastes a
        // real invite to switch meshes, this method used to error with
        // `AlreadyRunning` and force a two-step "leave, wait, join"
        // flow — but the HTTP listener is tied to the daemon task, so
        // the `leave` call takes the listener offline before `join`
        // can arrive. The result: paste-invite silently failed and the
        // user was stuck in the solo mesh.
        //
        // Switching is a valid operation, and the alternative (manual
        // ordering from the caller) isn't survivable across the
        // launchd restart race. Do the leave internally in one call
        // so callers get atomic "switch mesh" semantics.
        if self.is_running().await {
            tracing::info!(
                "join_mesh: daemon is in an existing mesh — auto-leaving before joining"
            );
            // User intent: switch meshes. Use leave (clears state)
            // not shutdown (preserves it) — without the clear, the
            // resume on next launch would race the join and we'd be
            // back in the previous mesh after a restart.
            let _ = self.leave().await;
        }

        let (join_key, url_mesh_name, relay_hint) = match link {
            DeepLink::Join {
                join_key,
                mesh_name,
                relay_hint,
            } => (join_key.clone(), mesh_name.clone(), relay_hint.clone()),
        };
        let mesh_name = url_mesh_name
            .clone()
            .unwrap_or_else(|| "Joined Mesh".to_string());

        membership::validate_join_key_format(&join_key)
            .map_err(|e| MeshError::InvalidJoinKey(e.to_string()))?;

        let internal_port = 9742u16;
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
        let (placeholder_mesh, _throwaway_key) = membership::init_mesh_with_node_id(
            &mesh_name,
            node_name,
            addrs.clone(),
            stable_id,
        );
        let placeholder_node_id = stable_id;

        self.start_daemon(placeholder_mesh, placeholder_node_id).await?;

        // Step 3 — handshake. Clone the Arc<MdnsDiscovery> so we don't
        // hold the DaemonState lock for the ~5s the handshake may take.
        let mdns = {
            let state = self.state.read().await;
            match &*state {
                DaemonState::Running { mdns, .. } => Arc::clone(mdns),
                DaemonState::Stopped => unreachable!("just started above"),
            }
        };

        let handshake = crate::join::perform_join(
            &mesh_name,
            &join_key,
            node_name,
            addrs,
            relay_hint.as_deref(),
            mdns.as_ref(),
            std::time::Duration::from_secs(5),
            // Propose our stable NodeId. Founder keeps it if free
            // or matches our name; else mints a fresh one (first
            // join from a new machine to this mesh).
            Some(stable_id),
        )
        .await;

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
                *mesh_state.write().await =
                    MeshState::from_app_state(app_state).await;
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
            DaemonState::Running { app_state, mesh_state, .. } => {
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
        let app_state = match &*state {
            DaemonState::Running { app_state, .. } => app_state.clone(),
            DaemonState::Stopped => return None,
        };
        drop(state);
        let mesh_name = app_state.inner.mesh.read().await.name.clone();
        let link = crate::deep_link::build_join_link(&key, None, Some(&mesh_name));
        Some((key, link))
    }

    /// Replace the in-memory cached plaintext join key. Called by
    /// the rotate HTTP handler after `persist::rotate_join_key` so
    /// the next status poll surfaces the new link without needing
    /// a daemon restart.
    pub async fn set_join_key(&self, key: String) {
        *self.join_key_plaintext.write().await = Some(key);
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
            DaemonState::Running { mdns, .. } => mdns.discovered_peers(),
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
        let mesh = app_state.inner.mesh.read().await;
        let self_id = app_state.inner.self_node_id_swap.load_full().as_ref().clone();
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
            .filter(|m| !m.addresses.is_empty())
            .map(|m| PeerInferenceEndpoint {
                node_id: m.node_id,
                name: m.name.clone(),
                // The client API is on port 9741 on every peer, not
                // 9742 (which is internal). The gossiped addresses
                // carry port 9742 (that's what the join handshake
                // targets). Rewrite to 9741 for inference.
                base_urls: m
                    .addresses
                    .iter()
                    .map(|addr| {
                        let ip = addr.ip();
                        if ip.is_ipv6() {
                            format!("http://[{ip}]:9741/v1")
                        } else {
                            format!("http://{ip}:9741/v1")
                        }
                    })
                    .collect(),
                system_ram_gb: m.capabilities.hardware.system_ram_gb,
                benchmark: m.capabilities.benchmark.clone(),
            })
            .collect()
    }

    // ── Private ─────────────────────────────────────────

    async fn start_daemon(
        &self,
        mesh: Mesh,
        node_id: NodeId,
    ) -> Result<(), MeshError> {
        // mesh_id as hex — broadcast in mDNS TXT records so peers on
        // the LAN can tell which mesh this node belongs to. Public by
        // design (knowing the mesh_id isn't sufficient to join;
        // accessing members still requires the join_key).
        let mesh_id_hex = hex::encode(mesh.id.as_bytes());
        let mesh_name = mesh.name.clone();
        let node_name = mesh
            .members
            .get(&node_id)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| node_id.to_string());

        // Build an AppState that already knows about our CorpusEngine
        // (if one was installed via `set_corpus_engine`). Without
        // this, Commonwealth's knowledge handlers can only return
        // stubs — the whole reason Peer A couldn't see Peer B's SEP
        // corpus. The MeshStore is in-memory here; the desktop's
        // long-term persistence story for mesh state still goes
        // through `mesh.json` on disk, not through MeshStore.
        let corpus_engine = self.corpus_engine.read().await.clone();
        let mesh_store = Arc::new(
            commonwealth_state::MeshStore::in_memory()
                .expect("in-memory MeshStore failed"),
        );
        let app_registry = Arc::new(commonwealth_app::registry::AppRegistry::new());
        let app_state = AppState::new_with_platform_and_engine(
            node_id,
            mesh,
            mesh_store,
            app_registry,
            corpus_engine.clone(),
        );

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
        let app_state = if let Some(provider) =
            self.inference_provider.read().await.as_ref()
        {
            let adapter: Arc<dyn LocalInferenceService> = Arc::new(
                crate::inference_adapter::SovereignInferenceAdapter::new(
                    provider.clone(),
                ),
            );
            info!("inference adapter: wired into /v1/chat/completions");
            app_state.with_local_inference(adapter)
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
                commonwealth_api::yield_hook::AppStateYieldHook::new(
                    app_state.inner.clone(),
                );
            engine.set_yield_hook(hook);
            info!("foreground-yield: hook installed on corpus engine");
        }

        // Publish embed model info so the collaborative ingestion planner
        // can compare this node's embedding model against candidates'.
        // Without this, `get_local_embed_model()` returns None and the
        // collaborate handler falls back to the qwen3-embedding-0.6b default,
        // which won't match a peer running a different model.
        if let Some(embed_info) = self.embed_model.read().await.as_ref() {
            app_state.inner.inference_store.set_local_embed_model(embed_info);
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

        // Client API on 0.0.0.0:9741 — this is the OpenAI-compatible
        // public surface documented in SYSTEM_OVERVIEW.md §5.5.
        // Peers fetch `/oicp/v1/capabilities` here, the Joiner's
        // HybridProvider POSTs `/v1/chat/completions` here for
        // federated inference, and mesh apps can federate via
        // `/v1/apps/*`. Was 127.0.0.1 for earlier dev builds where
        // only the in-process Tauri commands called it — that broke
        // mesh inference federation because peers couldn't reach us.
        //
        // Trust boundary: this port has no authentication today.
        // The Commonwealth security model (per glossary) is "a
        // closed trust ring" — the join_key_hash gates membership,
        // and deployment environments (Tailscale ACLs, LAN
        // firewalls) are expected to bound reachability to mesh
        // members. A future revision should add per-request auth
        // against `Mesh.join_key_hash` so a reachable-but-
        // non-member attacker can't burn our inference budget.
        let client_addr: SocketAddr = "0.0.0.0:9741".parse().unwrap();
        let internal_addr: SocketAddr = "0.0.0.0:9742".parse().unwrap();

        let mesh_state = Arc::new(RwLock::new(MeshState::from_app_state(&app_state).await));

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        // Register on mDNS and start browsing. Both are load-bearing:
        // advertise lets remote peers find us; browse populates the
        // discovered-peers table that `perform_join` (Phase B) uses
        // to locate handshake targets.
        let mdns = MdnsDiscovery::new(
            node_id,
            &mesh_id_hex,
            &mesh_name,
            &node_name,
            9742,
        )
        .map_err(|e| MeshError::Network(format!("mDNS register failed: {e}")))?;
        let mdns = Arc::new(mdns);
        // A 32-slot channel is plenty — the browse loop pushes on
        // ServiceResolved and we don't actively consume. If the buffer
        // fills (many peers on a busy LAN), the background task drops
        // extras; the discovered-peers hash map is still authoritative.
        let (peer_tx, _peer_rx) = tokio::sync::mpsc::channel::<DiscoveredPeer>(32);
        let browse_handle = mdns
            .browse(peer_tx)
            .map_err(|e| MeshError::Network(format!("mDNS browse failed: {e}")))?;

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
            let internal_router =
                commonwealth_api::server::internal_router(app_state_clone);

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
            let client_service = client_router
                .into_make_service_with_connect_info::<SocketAddr>();
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

        crate::auto_ingest::spawn_auto_collaborate_loop(app_state.clone(), 9742);

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
        let (snapshot_shutdown_tx, snapshot_shutdown_rx) =
            tokio::sync::watch::channel(false);
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
                                .map(|i| {
                                    (
                                        i.corpus_id,
                                        i.index_size_bytes as f64 / 1e9,
                                    )
                                })
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

        let mut state = self.state.write().await;
        *state = DaemonState::Running {
            app_state,
            mesh_state,
            client_addr,
            mdns,
            _browse_handle: browse_handle,
            _gossip_handle: gossip_handle,
            _shutdown_tx: shutdown_tx,
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
fn register_local_model_slots(
    app_state: &AppState,
    cfg: &SetupConfig,
    node_id: NodeId,
) {
    use commonwealth_inference::model::{ModelArchitecture, ModelInfo};
    use commonwealth_inference::oicp::CapabilityProfile;
    use commonwealth_core::ids::ModelId;
    use std::collections::HashMap;
    use std::hash::{DefaultHasher, Hash, Hasher};

    let mut slots: Vec<(String, &std::path::Path)> = vec![
        ("primary".into(), cfg.models.primary.as_path()),
        ("fast".into(), cfg.models.fast.as_path()),
        ("embed".into(), cfg.models.embed.as_path()),
    ];
    if let Some(code_path) = cfg.models.code.as_ref() {
        slots.push(("code".into(), code_path.as_path()));
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
        // their backing GGUF can swap freely.
        match role {
            "primary" | "fast" | "embed" | "code" => {
                slot_aliases.insert(role.to_string(), info.name.clone());
                slot_aliases.insert(
                    format!("commonwealth/{role}"),
                    info.name.clone(),
                );
                if role == "code" {
                    slot_aliases.insert("coder".into(), info.name.clone());
                    slot_aliases.insert(
                        "commonwealth/coder".into(),
                        info.name.clone(),
                    );
                }
            }
            _ => {}
        }
    }

    if !slot_aliases.is_empty() {
        info!(
            count = slot_aliases.len(),
            "installing slot alias map for chat_completions / list_models"
        );
        app_state.install_slot_aliases(slot_aliases);
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
}

/// One reachable address the founder can paste into the `?relay=…`
/// query param of a sovereign:// invite when mDNS won't traverse the
/// network between them and the joiner. Built from
/// `local_ip_candidates()` plus a kind classifier — the desktop UI
/// uses `kind` to recommend the best one (Tailscale > LAN > IPv6).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct RelayCandidate {
    /// Bare IP literal (no brackets for IPv6 — frontend formats it).
    pub ip: String,
    /// What kind of network this address is on. Drives the
    /// recommendation ordering and the human-readable label.
    /// One of: "tailscale", "lan", "ipv6", "other".
    pub kind: String,
    /// Pre-formatted `host:port` (or `[host]:port` for IPv6) ready
    /// to drop into `?relay=<value>`. Saves the UI from having to
    /// re-implement IPv6 bracket rules.
    pub url_fragment: String,
    /// True for the single best candidate the daemon would pick if
    /// asked to autoselect. Today: Tailscale > LAN > IPv6, first
    /// of its tier wins. The frontend pre-selects this in the
    /// invite-card relay picker.
    pub recommended: bool,
}

/// Classify an IP into a coarse "kind" the UI can render. Tailscale
/// uses the CGNAT range 100.64.0.0/10 (RFC 6598) plus an
/// `fd7a:115c:a1e0::/48` ULA for IPv6. We match on those shapes
/// rather than probing tailscaled — the daemon already runs without
/// any Tailscale dependency and this keeps it that way.
fn classify_ip(ip: &std::net::IpAddr) -> &'static str {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            // CGNAT 100.64.0.0/10 — Tailscale's tailnet range.
            if o[0] == 100 && (o[1] & 0xc0) == 64 {
                "tailscale"
            } else {
                "lan"
            }
        }
        std::net::IpAddr::V6(v6) => {
            // Tailscale ULA prefix fd7a:115c:a1e0::/48.
            let s = v6.segments();
            if s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0 {
                "tailscale"
            } else {
                "ipv6"
            }
        }
    }
}

/// Format an IP + port the way `?relay=…` expects: bare for IPv4,
/// bracketed for IPv6 so the colon separator parses unambiguously.
fn format_relay_fragment(ip: &std::net::IpAddr, port: u16) -> String {
    match ip {
        std::net::IpAddr::V4(_) => format!("{ip}:{port}"),
        std::net::IpAddr::V6(_) => format!("[{ip}]:{port}"),
    }
}

/// Build the ordered candidate list the daemon HTTP API serves and
/// the desktop UI renders. Sorted by recommendation tier so the
/// frontend can `[0]` the best one without re-sorting:
///   1. Tailscale — works across networks, peer-to-peer.
///   2. LAN       — works on the same subnet only.
///   3. IPv6 (non-Tailscale) — sometimes routable, often blocked.
///
/// Marks the first candidate as `recommended: true`. If the host has
/// no detected interfaces (no network), returns an empty Vec — the UI
/// then collapses the relay picker.
pub fn relay_candidates(internal_port: u16) -> Vec<RelayCandidate> {
    let mut tagged: Vec<(u8, RelayCandidate)> = local_ip_candidates()
        .into_iter()
        .map(|ip| {
            let kind = classify_ip(&ip);
            let tier: u8 = match kind {
                "tailscale" => 0,
                "lan" => 1,
                "ipv6" => 2,
                _ => 3,
            };
            let cand = RelayCandidate {
                ip: ip.to_string(),
                kind: kind.to_string(),
                url_fragment: format_relay_fragment(&ip, internal_port),
                recommended: false,
            };
            (tier, cand)
        })
        .collect();
    tagged.sort_by_key(|(tier, _)| *tier);
    let mut out: Vec<RelayCandidate> =
        tagged.into_iter().map(|(_, c)| c).collect();
    if let Some(first) = out.first_mut() {
        first.recommended = true;
    }
    out
}

/// Best-effort list of the host's externally-reachable IPs, so the
/// founder can copy one into `?relay=<ip>:9742` when mDNS is blocked
/// (WiFi AP isolation, multicast filtering, cross-subnet LANs).
///
/// Uses the portable "UDP-connect to a public IP without sending"
/// trick: kernel updates `local_addr` on the socket to reflect the
/// preferred outbound source address. No packets are actually sent.
/// Returns the IPv4 default-route source and, if dual-stack, the
/// IPv6 one. Skips loopback. Not exhaustive (won't enumerate VPN
/// interfaces that aren't the default route) but covers the common
/// home-WiFi and Tailscale cases.
/// Build the `Vec<SocketAddr>` we'll store in our own `MemberRecord`.
/// Each local non-loopback IP becomes `ip:port`. If no interface can
/// be discovered (e.g. no network at all), fall back to the wildcard
/// `0.0.0.0:port` — worse than useless for cross-machine gossip, but
/// at least lets a solo-on-localhost founder start up. Peers that
/// receive a wildcard address will see self-loopback behavior; the
/// warning log below makes that case visible.
fn reachable_addresses(port: u16) -> Vec<SocketAddr> {
    let ips = local_ip_candidates();
    if ips.is_empty() {
        warn!(
            port,
            "no routable local IPs discovered — falling back to \
             0.0.0.0:{port} in MemberRecord. Cross-machine gossip \
             will not work until a network interface is available."
        );
        return vec![SocketAddr::new(
            std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
            port,
        )];
    }
    ips.into_iter().map(|ip| SocketAddr::new(ip, port)).collect()
}

pub fn local_ip_candidates() -> Vec<std::net::IpAddr> {
    // Two-tier strategy:
    //
    //   Tier 1: enumerate EVERY local non-loopback interface via
    //   `if-addrs`. This is what we actually want — on a machine
    //   with both WiFi (192.168.x) and Tailscale (100.x) up, both
    //   addresses need to be published so peers can reach us via
    //   whichever one they can route to. The old default-route
    //   trick missed Tailscale entirely on dual-homed machines,
    //   which is EXACTLY the Commonwealth LAN-+-VPN topology.
    //
    //   Tier 2 (fallback): the "UDP-connect to a public IP without
    //   sending" trick. Kept for cases where `if-addrs` errors out
    //   (should never happen on darwin/linux but the contract is
    //   best-effort). Never used in practice.
    //
    // Ordering: preferred routable IPs first — link-local addresses
    // (169.254.x, fe80::) and private-ranges come after globals.
    // Rationale: the peer tries addresses in list order, so putting
    // the most reliable ones first shortens the mean fan-out path.
    let mut ips: Vec<std::net::IpAddr> = Vec::new();

    match if_addrs::get_if_addrs() {
        Ok(addrs) => {
            for iface in addrs {
                let ip = iface.ip();
                if ip.is_loopback() {
                    continue;
                }
                // Link-local addresses are useless cross-machine:
                // 169.254.x is unconfigured DHCP fallback, fe80::
                // is IPv6 link-local which can't route off the
                // local segment. Macs have lots of these from
                // Thunderbolt / virtual interfaces / utun0,1,2...
                // Including them just spams the startup log and
                // wastes fan-out attempts (reqwest dials them and
                // gets EHOSTUNREACH). Drop outright.
                let is_link_local = match ip {
                    std::net::IpAddr::V4(v4) => {
                        v4.octets()[0] == 169 && v4.octets()[1] == 254
                    }
                    std::net::IpAddr::V6(v6) => {
                        v6.segments()[0] & 0xffc0 == 0xfe80
                    }
                };
                if is_link_local {
                    continue;
                }
                ips.push(ip);
            }
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "if_addrs::get_if_addrs failed — falling back to \
                 UDP-connect default-route detection"
            );
            if let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") {
                if sock.connect("1.1.1.1:80").is_ok() {
                    if let Ok(addr) = sock.local_addr() {
                        if !addr.ip().is_loopback() {
                            ips.push(addr.ip());
                        }
                    }
                }
            }
            if let Ok(sock) = std::net::UdpSocket::bind("[::]:0") {
                if sock.connect("[2606:4700:4700::1111]:80").is_ok() {
                    if let Ok(addr) = sock.local_addr() {
                        let ip = addr.ip();
                        if !ip.is_loopback() && !ips.contains(&ip) {
                            ips.push(ip);
                        }
                    }
                }
            }
        }
    }

    ips
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
mod relay_tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn classifies_tailscale_cgnat_v4() {
        // Real Tailscale-assigned: 100.104.36.28
        let ip = IpAddr::V4(Ipv4Addr::new(100, 104, 36, 28));
        assert_eq!(classify_ip(&ip), "tailscale");
        // Boundary: 100.64.0.1 is the lowest CGNAT addr.
        assert_eq!(
            classify_ip(&IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1))),
            "tailscale"
        );
        // Boundary: 100.127.255.254 is the highest CGNAT addr.
        assert_eq!(
            classify_ip(&IpAddr::V4(Ipv4Addr::new(100, 127, 255, 254))),
            "tailscale"
        );
    }

    #[test]
    fn does_not_misclassify_neighbouring_ranges_as_tailscale() {
        // 100.63.x is NOT CGNAT; 100.128.x is NOT CGNAT.
        assert_eq!(
            classify_ip(&IpAddr::V4(Ipv4Addr::new(100, 63, 1, 1))),
            "lan"
        );
        assert_eq!(
            classify_ip(&IpAddr::V4(Ipv4Addr::new(100, 128, 1, 1))),
            "lan"
        );
    }

    #[test]
    fn classifies_typical_lan_v4_as_lan() {
        assert_eq!(
            classify_ip(&IpAddr::V4(Ipv4Addr::new(192, 168, 1, 3))),
            "lan"
        );
        assert_eq!(
            classify_ip(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5))),
            "lan"
        );
    }

    #[test]
    fn classifies_tailscale_v6_ula() {
        // fd7a:115c:a1e0::a3a:241c — real Tailscale IPv6 from the
        // user's own daemon log.
        let ip = IpAddr::V6("fd7a:115c:a1e0::a3a:241c".parse().unwrap());
        assert_eq!(classify_ip(&ip), "tailscale");
    }

    #[test]
    fn classifies_other_v6_as_ipv6_not_tailscale() {
        let ip = IpAddr::V6(Ipv6Addr::new(0x2606, 0, 0, 0, 0, 0, 0, 1));
        assert_eq!(classify_ip(&ip), "ipv6");
    }

    #[test]
    fn formats_v4_relay_fragment_without_brackets() {
        let ip = IpAddr::V4(Ipv4Addr::new(100, 104, 36, 28));
        assert_eq!(format_relay_fragment(&ip, 9742), "100.104.36.28:9742");
    }

    #[test]
    fn formats_v6_relay_fragment_with_brackets() {
        // The bracket form is what URL parsers (and `?relay=…`'s
        // own parse_join_argument) expect for IPv6.
        let ip = IpAddr::V6("fd7a:115c:a1e0::a3a:241c".parse().unwrap());
        assert_eq!(
            format_relay_fragment(&ip, 9742),
            "[fd7a:115c:a1e0::a3a:241c]:9742"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::setup_config::{
        DaemonSection, DataSection, ModelsSection, SetupConfig,
    };
    use std::path::PathBuf;

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
            members: Default::default(),
            peers: vec![],
        };
        let node_id = commonwealth_core::ids::NodeId::generate();
        let mesh_store = Arc::new(
            commonwealth_state::MeshStore::in_memory().unwrap(),
        );
        let app_registry = Arc::new(
            commonwealth_app::registry::AppRegistry::new(),
        );
        let app_state = AppState::new_with_platform_and_engine(
            node_id,
            mesh,
            mesh_store,
            app_registry,
            None,
        );

        let cfg = SetupConfig {
            models: ModelsSection {
                primary: PathBuf::from("/m/qwen3-coder-30b.gguf"),
                fast: PathBuf::from("/m/qwen3-1.7b.gguf"),
                embed: PathBuf::from("/m/qwen3-embedding-0.6b.gguf"),
                code: None,
                context_size: None,
                max_extras_memory_gb: None,
                extra: std::collections::BTreeMap::new(),
            },
            daemon: DaemonSection::default(),
            data: DataSection::default(),
            watched_folders: Default::default(),
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
}
