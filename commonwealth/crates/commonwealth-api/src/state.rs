use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use arc_swap::ArcSwap;
use tokio::sync::RwLock;

use std::pin::Pin;

use async_trait::async_trait;
use commonwealth_app::registry::AppRegistry;
use commonwealth_app::proxy::AppPortMap;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::{Mesh, NodeStatus};
use commonwealth_inference::oicp::ProviderManifest;
use commonwealth_inference::model_aliases::ModelAliasTable;
use commonwealth_inference::store_adapter::InferenceStateStore;
use commonwealth_core::ids::HandoffId;
use commonwealth_knowledge::store_adapter::KnowledgeStateStore;
use commonwealth_knowledge::WorkQueueManager;
use commonwealth_state::{
    ActivityEmitter, ContributionEmitter, MeshStore, PeerPreferenceStore,
};
use corpus_engine::CorpusEngine;
use futures::Stream;

use crate::openai_types::{ChatCompletionRequest, ChatCompletionResponse, StreamFrame};

/// In-process inference service that fulfils chat-completions
/// requests without spawning separate `llama-server` processes.
/// The Sovereign desktop embeds its local `EmbeddedLlamaCpp` as
/// one of these — so when a peer POSTs `/v1/chat/completions` at
/// our `:9741`, we reply with model output from Sovereign's own
/// loaded weights, same as if the user had typed the query
/// locally. The standalone Commonwealth daemon leaves this `None`
/// and uses the orchestrator-spawned llama-server path instead.
#[async_trait]
pub trait LocalInferenceService: Send + Sync {
    /// One-shot chat completion (non-streaming). Called when the
    /// incoming request did NOT set `stream: true`.
    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, String>;

    /// Streaming chat completion. Yields a sequence of typed
    /// [`StreamFrame`]s — `Token(piece)` for each text delta and a
    /// terminal `Finish { reason, usage }` carrying the OpenAI
    /// `finish_reason` (`Stop` / `Length` / `Cancelled` / etc.).
    /// `serve_local_stream` translates this into OpenAI-shaped SSE
    /// chunks, with a final empty-delta chunk that surfaces the
    /// real `finish_reason` to the wire.
    ///
    /// The typed shape replaces the legacy `Stream<Item = Result<
    /// String, String>>`: that surface couldn't tell the bridge
    /// whether the model stopped naturally or hit `max_tokens`, so
    /// every truncation rendered identically to a clean stop.
    /// Streams MUST end with either `Finish` or `Error`;
    /// `serve_local_stream` treats a closed channel without a
    /// terminal frame as `Cancelled`.
    async fn chat_completion_stream(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>, String>;

    /// Provider manifest for `/oicp/v1/capabilities`. Peers fetch
    /// this to know what capabilities this node advertises — the
    /// MeshAwareSelector on the client side uses it to pick a
    /// backend. Returning `None` falls through to the scheduler-
    /// based manifest path.
    fn provider_manifest(&self) -> Option<ProviderManifest>;

    /// Produce an embedding vector for `input`. Called by the HTTP
    /// `/v1/embeddings` handler. Returns the raw vector the provider
    /// emitted — the handler is responsible for wrapping it in the
    /// OpenAI response envelope.
    async fn embed(&self, input: &str) -> Result<Vec<f32>, String>;

    /// Add (or replace) an operator-declared additional chat slot at
    /// runtime. Returns the model_id (advertised name) of the loaded
    /// slot, or an error string when the underlying provider doesn't
    /// support runtime slot mutation. Backed by
    /// `InferenceProvider::load_extra_slot`. Routes:
    /// `POST /internal/models/load`.
    async fn load_extra_slot(
        &self,
        slot_name: String,
        path: std::path::PathBuf,
        context_size: u32,
    ) -> Result<String, String> {
        let _ = (slot_name, path, context_size);
        Err("this local inference service does not support runtime \
             slot load — only the embedded llama.cpp service does"
            .to_string())
    }

    /// Drop an operator-declared additional chat slot. `Ok(Some(id))`
    /// when a slot was removed; `Ok(None)` when no slot with that
    /// name was loaded; `Err(...)` on backends that don't support
    /// the operation. Routes: `POST /internal/models/unload`.
    async fn unload_extra_slot(&self, slot_name: &str) -> Result<Option<String>, String> {
        let _ = slot_name;
        Err("this local inference service does not support runtime \
             slot unload — only the embedded llama.cpp service does"
            .to_string())
    }

    /// List currently-loaded extras as `(slot_name, model_id)` pairs.
    /// Empty by default. Routes: `GET /internal/models/inventory`.
    async fn extras_inventory(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Eagerly load the primary chat slot so the next chat-completions
    /// request doesn't pay the lazy-load tax. Idempotent. Default
    /// returns success without doing work — backends that don't
    /// manage local slots have nothing to warm.
    /// Route: `POST /internal/inference/warmup`.
    async fn warmup_primary(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Callback the route handlers fire whenever they mutate `Mesh` —
/// `/internal/join` (accepting a new member), `/internal/gossip`
/// (merging a peer's view). `sovereign-mesh::EmbeddedDaemon` installs
/// a hook that persists `mesh.json` synchronously so a restart within
/// the gossip interval never forgets a mutation. Tests leave this
/// `None` and rely on their assertions without touching disk.
pub type MeshMutationHook = std::sync::Arc<
    dyn Fn(&Mesh, NodeId) + Send + Sync,
>;

/// Shared application state for all API handlers.
#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<AppStateInner>,
}

pub struct AppStateInner {
    /// Internal storage. Use [`AppStateInner::self_node_id`] to read
    /// (returns `NodeId` by value) — direct field access is hidden
    /// behind a method so callers always go through the load.
    ///
    /// Backed by `ArcSwap` so the `join_mesh` adoption flow can swap
    /// the placeholder ID for the founder-assigned ID atomically
    /// after the handshake completes. Without that swap, gossip
    /// would never find our own member record (it indexes by
    /// `self_node_id`), `corpus_collaborate` would 500 with "local
    /// node not found in mesh", and partitions would never dispatch.
    pub self_node_id_swap: ArcSwap<NodeId>,
    pub mesh: RwLock<Mesh>,
    /// Inference plan, model info, ledger, and llama addresses — all via MeshStore.
    pub inference_store: InferenceStateStore,
    /// Knowledge shard plan — via MeshStore.
    pub knowledge_store: KnowledgeStateStore,
    pub model_aliases: ModelAliasTable,
    /// ATOS pipeline aliases — resolved before `model_aliases` when
    /// an incoming request carries a pipeline name like
    /// `commonwealth/sovereign-coder`. Loaded from the embedded
    /// `default_pipelines.toml` at `AppState::new` time.
    pub pipeline_aliases: commonwealth_core::pipeline_aliases::PipelineAliasTable,
    /// Dynamic slot-name aliases. Map keys are operator-friendly slot
    /// labels (`primary`, `fast`, `code`, `embed`) — possibly prefixed
    /// `commonwealth/` for namespaced lookups. Values are the GGUF
    /// stems (model_ids) currently bound to that slot.
    ///
    /// Populated at daemon boot from `SetupConfig.models.*` so an
    /// operator can write `commonwealth/primary` in opencode's config
    /// (or any other client) and have requests follow whatever GGUF
    /// the daemon happens to be loading without rewriting the client
    /// config when models swap. `ArcSwap` lets the admin reload path
    /// hot-swap the table when `[models]` changes on disk.
    ///
    /// Empty when the daemon hasn't installed slot bindings yet
    /// (early boot, or non-embedded daemons that don't own a
    /// `SetupConfig`). Resolution then falls through to the
    /// existing pipeline / model alias paths.
    pub slot_aliases: ArcSwap<std::collections::HashMap<String, String>>,
    /// Absolute paths of GGUF files this daemon will serve over
    /// `/internal/v1/models/*` to other mesh peers. Populated at
    /// daemon boot from `SetupConfig.models.*` so a friend or a
    /// fresh cloud pod can pull the model files from us instead of
    /// from R2/S3 — the friend doesn't have our bucket creds, and
    /// the cloud pod's R2 sync has been the slowest step of every
    /// fresh launch.
    ///
    /// `ArcSwap` so the admin reload path can update the list when
    /// `[models]` paths change on disk. Empty when the daemon has
    /// not installed bindings yet (early boot, or test fixtures
    /// that bypass the production `start_daemon` flow).
    ///
    /// Serving is an allowlist, not a directory browser: only paths
    /// listed here are exposed. A request whose `name` doesn't match
    /// the `file_name()` of one of these paths gets 404, even if
    /// the file exists somewhere on disk. Keeps the surface area to
    /// "files this daemon is configured to load and would have
    /// already loaded itself" — same trust boundary as the
    /// inference path.
    pub servable_model_files: ArcSwap<Vec<std::path::PathBuf>>,
    /// ATOS middleware registry. Holds one instance of each
    /// middleware the pipelines can reference by id.
    pub middleware_registry: Arc<crate::middleware::MiddlewareRegistry>,
    /// ATOS session-state store. `None` until a M4.4+ daemon wires
    /// it (tests without a MeshStore handle leave this empty; the
    /// handler skips ATOS pipeline processing when the store is
    /// absent).
    pub session_store: Option<sovereign_atos::session::SessionStore>,
    /// Repository root the Commonwealth daemon is anchored to —
    /// the directory that contains `.sovereign/features/`. Used by
    /// ApprovalGate for git lookups and by ContextInjector for
    /// reading spec.md. `None` when the daemon wasn't started in a
    /// repo-like context (degrades ATOS pipelines to a noop).
    pub repo_root: Option<std::path::PathBuf>,
    pub corpus_engine: Option<Arc<CorpusEngine>>,
    /// Distributed KV store for mesh apps.
    pub mesh_store: Arc<MeshStore>,
    /// Registry of known mesh apps (gossiped).
    pub app_registry: Arc<AppRegistry>,
    /// Map of locally running app ports for the proxy layer.
    pub app_port_map: AppPortMap,
    /// Corpus IDs currently being actively ingested on this node.
    /// Prevents the auto-collaborate loop from firing a second
    /// `collaborate` call while a live ingest task is writing chunks.
    pub active_ingests: RwLock<HashSet<String>>,
    /// Latest `IngestProgress` observed for each active corpus.
    /// Populated by the daemon-side ingest spawn's progress callback
    /// so the Desktop UI can poll `GET /internal/corpus/progress`
    /// instead of taking a Tauri-event-only path that dies when the
    /// app closes mid-ingest. Entries are retained until either a
    /// terminal phase (`Complete`) overwrites them or an explicit
    /// cancel wipes the corpus.
    pub corpus_progress: RwLock<HashMap<String, corpus_engine::IngestProgress>>,
    /// Operator-triggered tick channel for the `wikipedia-newsworthy`
    /// freshness watcher. Installed by the embedded daemon when (and
    /// only when) the watcher spawns; `None` in tests and on daemons
    /// without a corpus engine. The `POST /internal/newsworthy/tick`
    /// route grabs this sender to fire one tick on demand, bypassing
    /// the 24h interval — the only path operators have to recover
    /// from a stale snapshot or kick off the first portal ingest
    /// after becoming leader.
    pub newsworthy_force_tick:
        RwLock<Option<tokio::sync::mpsc::Sender<()>>>,
    /// Current inference availability (0.0–1.0). Written by sovereign-server's
    /// ActivityReporter via POST /internal/node/activity; read by gossip each
    /// round to populate NodeCapabilities.inference_availability. Default 1.0.
    pub local_inference_availability: RwLock<f32>,
    /// Hard capability gate: true iff the daemon's startup probe confirmed
    /// the configured model can be loaded. false (default) means this node
    /// joins as storage-only and is excluded from inference routing.
    pub local_inference_capable: std::sync::atomic::AtomicBool,
    /// Optional callback fired after any `Mesh` mutation by the
    /// route handlers. Set by the embedded daemon to the
    /// `persist::save` function so `/internal/join` accepts survive
    /// a founder restart immediately (not just on the next gossip
    /// tick). `None` in tests and in the standalone Commonwealth
    /// daemon, where persistence is managed elsewhere.
    pub on_mesh_mutation: Option<MeshMutationHook>,
    /// Optional in-process inference service. When Sovereign embeds
    /// the daemon, this is a wrapper over its `EmbeddedLlamaCpp` so
    /// `/v1/chat/completions` serves peer requests from the same
    /// model the local user would use. `None` in the standalone
    /// Commonwealth daemon — that path routes via the orchestrator
    /// to spawned `llama-server` processes instead.
    pub local_inference: Option<std::sync::Arc<dyn LocalInferenceService>>,
    /// Pull-based corpus ingestion work queues keyed by `HandoffId`.
    /// The coordinator's `corpus_collaborate` handler populates this with
    /// a unit list; peers pull units via `POST /internal/corpus/next_unit`.
    /// Only coordinators hold entries here — peer nodes never mutate it.
    /// See `commonwealth-knowledge::work_queue` for the full design.
    pub work_queue: Arc<WorkQueueManager>,
    /// Handoff IDs for which this node is currently running a pull loop
    /// (as a peer). Prevents `auto_ingest` from spawning duplicate pull
    /// loops when the same open handoff is seen across multiple gossip ticks.
    pub active_pull_loops: RwLock<HashSet<HandoffId>>,
    /// Unix-seconds timestamp of the last foreground inference request
    /// observed at `chat_completions`. `0` means "never touched" — the
    /// initial state at boot. Bumped via [`AppState::bump_foreground_active`]
    /// and read by the corpus-engine `YieldHook` impl to decide whether
    /// background ingest workers should pause before the next embed
    /// batch / enrichment phase. Plain atomic — no lock contention on
    /// the hot read path.
    pub foreground_last_active_ts: std::sync::atomic::AtomicI64,
    /// Yield window in seconds. While `now - last_active < window`, the
    /// daemon's `YieldHook` returns `should_yield = true`. `0` disables
    /// the feature (tests, hosts that never want background work to
    /// pause). Configured via `~/.config/sovereign/config.toml`'s
    /// `daemon.yield_to_foreground_secs` and stuffed in here at
    /// startup; the desktop Settings tab can rewrite it at runtime
    /// without a daemon restart.
    pub yield_window_secs: std::sync::atomic::AtomicU64,

    /// Mesh quiesce flag. When `true`, the auto-collaborate loop
    /// (`sovereign-mesh::auto_ingest`) skips peer-pull discovery and
    /// dispatch on every tick — this node neither pulls work assigned
    /// by other coordinators nor dispatches its own queue to peers.
    /// Initial value is set from the `SOVEREIGN_DISABLE_AUTO_COLLAB`
    /// env var at boot (preserves the existing operator escape hatch);
    /// `POST /internal/mesh/quiesce` flips it at runtime without
    /// requiring a daemon restart. Reads on the hot path are a single
    /// relaxed atomic load.
    pub mesh_quiesced: std::sync::atomic::AtomicBool,

    /// Maximum concurrent peer-served inference requests this node
    /// admits at once. The admission middleware (see `crate::admission`)
    /// reads this on every peer request and 503s when the count is
    /// at-or-above the cap. `usize::MAX` (default) disables the cap;
    /// `0` rejects all peer work (equivalent to
    /// `SOVEREIGN_DISABLE_PEER_INFERENCE=1`, but with a runtime
    /// toggle). Set via `POST /internal/contribution/ceiling`.
    pub contribution_max_peer_inflight: std::sync::atomic::AtomicUsize,

    /// Currently in-flight peer requests gated by the admission
    /// middleware. Incremented at admit time, decremented when the
    /// response future drops (RAII via `PeerInflightGuard`). Reads
    /// are relaxed: the count is approximate under contention but
    /// monotonically converges and the worst-case race is one extra
    /// peer request admitted past the cap — acceptable.
    pub peer_inflight_count: std::sync::atomic::AtomicUsize,

    /// Unix-seconds expiry for a runtime contribution pause. `0`
    /// means not paused. `now >= paused_until` means the pause has
    /// expired; the middleware simply compares without writing the
    /// field. Settable via `POST /internal/contribution/pause`.
    pub contribution_paused_until: std::sync::atomic::AtomicI64,

    /// When `true`, peer-served requests honour the foreground-yield
    /// window just like ingest workers do — a peer chat that lands
    /// during the window after a local turn 503s with `Retry-After`
    /// rather than competing with the user for the GPU. Default
    /// `true`; the setting is exposed via the same Settings surface
    /// as the foreground-yield window itself.
    pub yield_peers_to_foreground: std::sync::atomic::AtomicBool,

    /// Per-batch ingest throttle. Encoded as fixed-point ‰ (parts
    /// per thousand) so we can represent fractional levels without
    /// floats. `1000` = full speed (no post-batch sleep — the legacy
    /// behaviour and the default). `500` = duty-cycle 50% (sleep
    /// after each batch equal to the batch's wall time, halving
    /// effective throughput while leaving the GPU/CPU unblocked
    /// in between). `0` is rejected by the setter — use the pause
    /// route to fully stop a corpus.
    pub ingest_throttle_milli: std::sync::atomic::AtomicU32,

    /// User-set ceiling on how much disk Sovereign is allowed to use
    /// for corpus storage (sum of `~/.sovereign/indexes/*`). Encoded
    /// as bytes; `0` is the sentinel for "no budget — use whatever
    /// disk says is free". The desktop Settings panel writes this at
    /// boot (computed from free disk on first launch, then persisted
    /// in `desktop.toml`) and via `POST /internal/storage/budget`.
    ///
    /// The enforcement point is `sovereign-mesh::capabilities::
    /// build_local_capabilities`, which clamps the gossiped
    /// `free_storage_gb` (both the static `HardwareProfile` field and
    /// the live `AvailableResources` reading) to
    /// `min(actual_free, max(0, budget − used))`. Every existing
    /// scheduler (`knowledge_assignment::assign_knowledge_shards`,
    /// the three `plan_collaborative_ingestion*` planners) reads
    /// that one value to decide what to assign here, so clamping it
    /// at the publish boundary makes the budget self-enforcing
    /// across the whole mesh — peers won't push us shards that
    /// would breach the budget, and our own local install path
    /// already gates on the same number.
    pub storage_budget_bytes: std::sync::atomic::AtomicU64,

    /// Most recent observation of how much of the budget the corpus
    /// engine is currently using on disk. Updated each gossip tick
    /// from `CorpusEngine::installed_indexes()` (already walked once
    /// per tick to publish `hosted_corpora`, so no extra IO). Read
    /// by `GET /internal/storage/budget` to drive the desktop's
    /// "X of Y GB used" indicator without forcing the UI to re-walk
    /// the index directory.
    pub storage_used_bytes: std::sync::atomic::AtomicU64,

    /// Dimensional contribution emitter. Each route handler records
    /// `LedgerEvent`s through this on completion (per write site
    /// listed in the Mesh Health design). Cheap to clone; emission
    /// is `tokio::spawn`-friendly. The emitter holds its own handle
    /// to `MeshStore` so it survives `AppState` clones and can be
    /// passed into spawned tasks without lifetime gymnastics.
    pub contribution_emitter: ContributionEmitter,

    /// Local Activity ledger emitter. Records this daemon's own
    /// resource work — tokens served to local clients, embeddings
    /// produced, chunks ingested/enriched, newsworthy fetches — in
    /// Sovereign's vocabulary, for the glassbox "Activity & Sharing"
    /// surface. Unlike `contribution_emitter`, its records are
    /// **local-only and never gossip** (written under the
    /// `activity-private` namespace). Cheap to clone; shares the same
    /// underlying `MeshStore`. See `commonwealth_core::activity`.
    pub activity_emitter: ActivityEmitter,

    /// Per-peer preference store (Ostrom sanctions). Local-only,
    /// never gossiped — see
    /// `commonwealth_state::peer_preferences` for the structural
    /// invariants. The manifest endpoint reads this on every
    /// fetch to apply per-requester affinity multipliers.
    pub peer_preferences: PeerPreferenceStore,

    /// Shared in-flight counter for local-serve inference. Installed
    /// once by the daemon bootstrap after `MeshInferenceProvider::new`
    /// returns. Read by the gossip emitter
    /// (`sovereign-mesh::capabilities::build_local_capabilities`) on
    /// every tick to populate
    /// [`commonwealth_core::capabilities::NodeCapabilities::current_in_flight`].
    ///
    /// Lifecycle:
    /// * Cold start: MIP creates its own private `Arc<AtomicU32>`,
    ///   then the bootstrap calls
    ///   [`AppState::install_in_flight_publisher`] with that Arc.
    ///   `OnceLock::set` succeeds on the first call.
    /// * Hot reload (`replace_models_and_reload`): the new MIP is
    ///   constructed via [`MeshInferenceProvider::with_in_flight_publisher`]
    ///   passing the *already-installed* Arc back in. The OnceLock
    ///   is unchanged; old MIP guards and new MIP guards share the
    ///   same atomic, so the counter stays accurate across the swap.
    ///
    /// Empty in tests and on storage-only nodes that never construct
    /// a `MeshInferenceProvider`; gossip then emits
    /// `current_in_flight: None`, which is the legacy / "no signal"
    /// behaviour every scoring path handles correctly.
    pub local_in_flight_publisher:
        std::sync::OnceLock<Arc<std::sync::atomic::AtomicU32>>,
}

impl AppState {
    /// Resolve a slot-name alias (`primary`, `fast`, `code`, `embed`,
    /// or any of those prefixed `commonwealth/`) to the concrete model
    /// id (GGUF stem) currently bound to that slot. Returns `None`
    /// when the input is not a registered slot alias — callers fall
    /// through to the next resolution layer.
    ///
    /// Lookup is exact-match on both the bare alias and the
    /// `commonwealth/`-namespaced form. We pre-register both forms at
    /// install time so the lookup is a single map probe regardless of
    /// which form the client sent.
    pub fn resolve_slot_alias(&self, model_name: &str) -> Option<String> {
        let map = self.inner.slot_aliases.load();
        map.get(model_name).cloned()
    }
    /// Replace the slot alias table atomically. Daemon startup calls
    /// this once after `SetupConfig` is loaded; the admin reload path
    /// calls it again whenever `[models]` changes on disk so clients
    /// using `commonwealth/primary` follow the swap without restart.
    pub fn install_slot_aliases(
        &self,
        aliases: std::collections::HashMap<String, String>,
    ) {
        self.inner.slot_aliases.store(Arc::new(aliases));
    }
    /// Return a snapshot of the registered slot alias names (both
    /// bare and `commonwealth/`-prefixed) suitable for inclusion in
    /// `/v1/models`. Stable order: alphabetical, deterministic.
    pub fn slot_alias_names(&self) -> Vec<String> {
        let map = self.inner.slot_aliases.load();
        let mut names: Vec<String> = map.keys().cloned().collect();
        names.sort();
        names
    }

    /// Replace the servable-model-files allowlist atomically. Daemon
    /// startup calls this once after the slot table is built; the
    /// admin reload path calls it again on `[models]` change. Each
    /// path should be absolute (no `..`/symlink trickery) — the
    /// serve handler matches only on `file_name()` and reads the
    /// canonical path, but feeding it relative inputs would still
    /// be a footgun for whoever calls it next.
    pub fn install_servable_model_files(
        &self,
        files: Vec<std::path::PathBuf>,
    ) {
        self.inner.servable_model_files.store(Arc::new(files));
    }


    pub fn new(self_node_id: NodeId, mesh: Mesh) -> Self {
        // Test-support constructor (callers in tests/ + the test-harness);
        // in-memory MeshStore creation is infallible — fail-fast is correct.
        #[allow(clippy::expect_used)]
        let mesh_store = Arc::new(
            MeshStore::in_memory().expect("in-memory MeshStore failed"),
        );
        Self::new_with_platform(self_node_id, mesh, mesh_store, Arc::new(AppRegistry::new()))
    }

    /// Create state with explicit platform components (used by the daemon).
    pub fn new_with_platform(
        self_node_id: NodeId,
        mesh: Mesh,
        mesh_store: Arc<MeshStore>,
        app_registry: Arc<AppRegistry>,
    ) -> Self {
        Self::new_with_platform_and_engine(
            self_node_id,
            mesh,
            mesh_store,
            app_registry,
            None,
        )
    }

    /// Create state with an optional `CorpusEngine` attached. The
    /// engine is what the knowledge routes (`/v1/knowledge/search`
    /// and `/internal/knowledge/search`) query to turn a request
    /// into scored chunks. When `None` (default), the knowledge
    /// routes behave as if this node hosts no corpora — the path
    /// that used to yield the `is_stub: "true"` placeholder. The
    /// `sovereign-mesh::EmbeddedDaemon` passes `Some(engine)` so
    /// the in-process daemon has something real to search.
    pub fn new_with_platform_and_engine(
        self_node_id: NodeId,
        mesh: Mesh,
        mesh_store: Arc<MeshStore>,
        app_registry: Arc<AppRegistry>,
        corpus_engine: Option<Arc<CorpusEngine>>,
    ) -> Self {
        let inference_store = InferenceStateStore::new(Arc::clone(&mesh_store), self_node_id);
        let knowledge_store = KnowledgeStateStore::new(Arc::clone(&mesh_store), self_node_id);
        let contribution_emitter =
            ContributionEmitter::new((*mesh_store).clone(), self_node_id);
        let activity_emitter =
            ActivityEmitter::new((*mesh_store).clone(), self_node_id);
        let peer_preferences =
            PeerPreferenceStore::new((*mesh_store).clone(), self_node_id);
        // ATOS middleware registry with the M4 core four implementations
        // registered under their TOML ids. The wiring is intentionally
        // additive — operators deploying a stock Commonwealth daemon
        // get the full stack without extra config; tests that want a
        // bare daemon can build a minimal registry themselves.
        let mut middleware_registry = crate::middleware::MiddlewareRegistry::new();
        middleware_registry.register(Arc::new(crate::middleware::ApprovalGate::new()));
        // 2026-05-22: ContextInjector + ToolInjector descriptor lists
        // were previously pulled from `sovereign_tools::manifest`, a
        // global static that forced commonwealth-api to drag the
        // tree-sitter grammar crates through every downstream binary.
        // They're now injected at construction time. AppState
        // constructs them with `Vec::new()` because the registry of
        // available tools lives in the daemon host (sovereign-cli-atos,
        // sovereign-desktop, sovereign-server) — those wire the real
        // descriptors via the `with_tool_descriptors` shim below the
        // platform constructors.
        middleware_registry.register(Arc::new(crate::middleware::ContextInjector::empty()));
        middleware_registry.register(Arc::new(crate::middleware::ToolInjector::empty()));
        middleware_registry.register(Arc::new(crate::middleware::ArtifactSurface::new()));
        middleware_registry.register(Arc::new(crate::middleware::SessionBriefing::new()));
        // Phase 7.2: per-turn DecisionExtractor mines assistant
        // responses for decision-shaped phrases on `post_process`,
        // then on the next turn either persists as
        // `source='extracted'` or drops on a user correction
        // phrase. Lives at the END of the chain so it observes
        // the response after every other middleware has had its
        // say. Stateless beyond `MiddlewareSession.pending_decision`,
        // which is already plumbed through routes_inference's
        // session round-trip.
        middleware_registry
            .register(Arc::new(crate::middleware::DecisionExtractor::new()));
        // `read_only_enforcer` is the red-team alias's gate. For M4
        // it shares the ApprovalGate implementation under a distinct
        // id — M5 splits them if the behavior actually diverges.
        let read_only = Arc::new(crate::middleware::ApprovalGate::new());
        middleware_registry.register(read_only);

        // Session store is wired up when the daemon has a MeshStore
        // in hand. The handler falls back to legacy routing when
        // this is None.
        let session_store = Some(sovereign_atos::session::SessionStore::new(
            (*mesh_store).clone(),
            self_node_id,
        ));
        Self {
            inner: Arc::new(AppStateInner {
                self_node_id_swap: ArcSwap::from_pointee(self_node_id),
                mesh: RwLock::new(mesh),
                inference_store,
                knowledge_store,
                model_aliases: ModelAliasTable::default_table(),
                pipeline_aliases:
                    commonwealth_core::pipeline_aliases::PipelineAliasTable::default_table(),
                slot_aliases: ArcSwap::from_pointee(std::collections::HashMap::new()),
                servable_model_files: ArcSwap::from_pointee(Vec::new()),
                middleware_registry: Arc::new(middleware_registry),
                session_store,
                repo_root: std::env::current_dir().ok(),
                corpus_engine,
                mesh_store,
                app_registry,
                app_port_map: AppPortMap::new(),
                active_ingests: RwLock::new(HashSet::new()),
                corpus_progress: RwLock::new(HashMap::new()),
                newsworthy_force_tick: RwLock::new(None),
                local_inference_availability: RwLock::new(1.0_f32),
                local_inference_capable: std::sync::atomic::AtomicBool::new(false),
                on_mesh_mutation: None,
                local_inference: None,
                work_queue: Arc::new(WorkQueueManager::new()),
                active_pull_loops: RwLock::new(HashSet::new()),
                // 0 sentinel = no foreground activity observed yet.
                // The yield hook treats 0 as "never active", regardless
                // of the window — so a fresh boot doesn't accidentally
                // pause ingest before the first chat request.
                foreground_last_active_ts: std::sync::atomic::AtomicI64::new(0),
                // 0 = disabled. The daemon constructor overrides this
                // from config (`daemon.yield_to_foreground_secs`,
                // default 60) before AppState is shared.
                yield_window_secs: std::sync::atomic::AtomicU64::new(0),
                // false = full peer collaboration. Daemon startup
                // overrides this from `SOVEREIGN_DISABLE_AUTO_COLLAB`
                // when set, preserving the env-var escape hatch.
                mesh_quiesced: std::sync::atomic::AtomicBool::new(false),
                // usize::MAX = unlimited. The desktop overwrites this
                // at boot with the user's persisted setting (default
                // matched to their consent-dialog choice in W4);
                // headless / CLI daemons leave it unlimited so they
                // don't surprise their operators.
                contribution_max_peer_inflight: std::sync::atomic::AtomicUsize::new(
                    usize::MAX,
                ),
                peer_inflight_count: std::sync::atomic::AtomicUsize::new(0),
                // 0 = not paused. Wall-clock unix-seconds expiry when
                // a user-initiated pause is active.
                contribution_paused_until: std::sync::atomic::AtomicI64::new(0),
                // Default on: foreground-yield gates peer requests
                // too, not just ingest. The "press send mid-chat and
                // the GPU is pinned by a peer's enrich job" failure
                // mode is exactly what this prevents.
                yield_peers_to_foreground: std::sync::atomic::AtomicBool::new(true),
                // 1000 ‰ = full speed; ingest pipeline pays one atomic
                // load per batch and otherwise behaves identically to
                // the pre-throttle build.
                ingest_throttle_milli: std::sync::atomic::AtomicU32::new(1000),
                // 0 = unlimited (no clamp). The desktop overwrites
                // this at boot with either the persisted user choice
                // or a computed default; CLI/standalone daemons leave
                // it at 0 so headless servers don't surprise their
                // operators with a budget they didn't set.
                storage_budget_bytes: std::sync::atomic::AtomicU64::new(0),
                storage_used_bytes: std::sync::atomic::AtomicU64::new(0),
                contribution_emitter,
                activity_emitter,
                peer_preferences,
                local_in_flight_publisher: std::sync::OnceLock::new(),
            }),
        }
    }

    /// Install the local MIP's in-flight publisher. One-shot: if the
    /// OnceLock is already set, the new `publisher` is dropped and
    /// the existing Arc stays in place. This is the load-bearing
    /// invariant the hot-reload path relies on — it must NOT clobber
    /// the publisher the cold-start path installed, or live guards
    /// from the old MIP would decrement an Arc nobody reads.
    pub fn install_in_flight_publisher(
        &self,
        publisher: Arc<std::sync::atomic::AtomicU32>,
    ) {
        let _ = self.inner.local_in_flight_publisher.set(publisher);
    }

    /// Borrow the installed in-flight publisher Arc. Hot-reload path
    /// calls this to pass the same Arc into
    /// `MeshInferenceProvider::with_in_flight_publisher`. `None` when
    /// the bootstrap hasn't run an install yet (test harnesses,
    /// storage-only nodes).
    pub fn in_flight_publisher(
        &self,
    ) -> Option<Arc<std::sync::atomic::AtomicU32>> {
        self.inner.local_in_flight_publisher.get().cloned()
    }

    /// Read the current local in-flight count if a publisher has been
    /// installed. `None` on nodes that never wired one through
    /// (storage-only, test harnesses without `MeshInferenceProvider`).
    /// Gossip serialises this directly into
    /// `NodeCapabilities.current_in_flight`.
    pub fn current_local_in_flight(&self) -> Option<u32> {
        self.inner
            .local_in_flight_publisher
            .get()
            .map(|p| p.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// Spawn the coordinator's pull-based work-queue reaper. Must be called
    /// once per daemon process after `new_with_platform_and_engine` so
    /// leases whose heartbeats lapse get re-queued. Tests that don't use
    /// the queue can skip this — the queue is dormant until a handoff is
    /// registered. Returns the JoinHandle so the caller can abort at
    /// shutdown, though the process normally exits before the handle
    /// would matter.
    pub fn start_work_queue_reaper(&self) -> tokio::task::JoinHandle<()> {
        Arc::clone(&self.inner.work_queue).spawn_reaper()
    }

    /// This node's NodeId, by value. Cheap (atomic load + Arc deref).
    /// Use everywhere instead of the old field access — `join_mesh`
    /// swaps this when adopting a founder-assigned ID, and the field
    /// access path would always see the placeholder.
    pub fn self_node_id(&self) -> NodeId {
        **self.inner.self_node_id_swap.load()
    }

    /// Replace this node's `self_node_id` (atomic). Called by
    /// `join_mesh` after the founder assigns us a NodeId during the
    /// handshake. Cheap pointer swap; concurrent readers see either
    /// the old or new value but never garbage.
    pub fn set_self_node_id(&self, new_id: NodeId) {
        self.inner.self_node_id_swap.store(Arc::new(new_id));
    }

    /// Record whether this node's model probe succeeded at startup.
    /// Called by the daemon after `probe_inference_capability()` completes,
    /// before mDNS announce or the first gossip round.
    pub fn set_local_inference_capable(&self, capable: bool) {
        self.inner
            .local_inference_capable
            .store(capable, std::sync::atomic::Ordering::Relaxed);
    }

    /// Read the current hard capability gate for use in gossip payloads.
    pub fn local_inference_capable(&self) -> bool {
        self.inner
            .local_inference_capable
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Install the in-process inference service. Same Arc-get_mut
    /// contract as `with_mesh_mutation_hook` — call before cloning
    /// AppState into the HTTP servers.
    pub fn with_local_inference(
        mut self,
        service: std::sync::Arc<dyn LocalInferenceService>,
    ) -> Self {
        match Arc::get_mut(&mut self.inner) {
            Some(inner) => {
                inner.local_inference = Some(service);
            }
            None => {
                tracing::error!(
                    strong_count = Arc::strong_count(&self.inner),
                    "with_local_inference called on shared AppState — \
                     local inference NOT installed and /v1/chat/completions \
                     will 503 every request with model_not_ready. \
                     Likely cause: another caller cloned AppState.inner \
                     (e.g. AppStateYieldHook::new) before this point. \
                     Move the with_* installer above any inner.clone() in \
                     EmbeddedDaemon::start_daemon."
                );
            }
        }
        self
    }

    /// Install the mutation hook on an Arc not yet cloned. Called
    /// by `sovereign-mesh::EmbeddedDaemon` right after constructing
    /// its `AppState`, before handing the `Clone`d state to the HTTP
    /// servers. If the Arc has already been cloned (should not
    /// happen in normal use), this is a no-op with a warning so the
    /// daemon keeps running rather than panicking.
    pub fn with_mesh_mutation_hook(mut self, hook: MeshMutationHook) -> Self {
        match Arc::get_mut(&mut self.inner) {
            Some(inner) => {
                inner.on_mesh_mutation = Some(hook);
            }
            None => {
                tracing::error!(
                    strong_count = Arc::strong_count(&self.inner),
                    "with_mesh_mutation_hook called on shared AppState — \
                     persistence hook NOT installed; on-join persistence \
                     falls back to 10s gossip-loop cadence. \
                     Likely cause: another caller cloned AppState.inner \
                     (e.g. AppStateYieldHook::new) before this point. \
                     Move the with_* installer above any inner.clone() in \
                     EmbeddedDaemon::start_daemon."
                );
            }
        }
        self
    }

    /// Register a model as available on the mesh.
    pub fn register_model(&self, model: commonwealth_inference::model::ModelInfo) {
        self.inner.inference_store.set_model_info(&model);
    }

    /// Set the address of a llama-server for a model (after orchestrator spawns it).
    pub fn set_llama_server_address(
        &self,
        model_id: commonwealth_core::ids::ModelId,
        address: String,
    ) {
        self.inner.inference_store.set_llama_address(model_id, &address);
    }

    /// Get the llama-server address for a model.
    pub fn get_llama_server_address(
        &self,
        model_id: commonwealth_core::ids::ModelId,
    ) -> Option<String> {
        self.inner.inference_store.get_llama_address(model_id)
    }

    /// Get the default model (first in the inference plan).
    pub fn default_model_id(&self) -> Option<commonwealth_core::ids::ModelId> {
        self.inner
            .inference_store
            .get_plan()
            .and_then(|p| p.model_plans.first().map(|mp| mp.model))
    }

    /// Update this node's inference availability. Called by sovereign-server's
    /// ActivityReporter after a level transition; gossip picks up the new value
    /// on its next 10-second round.
    pub async fn update_local_availability(&self, availability: f32) {
        *self.inner.local_inference_availability.write().await = availability;
        tracing::debug!(availability, "inference_availability updated by sovereign-server");
    }

    /// Record that a foreground inference request just landed. Called
    /// from the `chat_completions` handler before slot dispatch so any
    /// background ingest workers polling `should_yield_to_foreground`
    /// will see a fresh timestamp on their next checkpoint. Cheap
    /// (atomic store, Relaxed ordering — readers don't need
    /// happens-before, just monotonic-enough-for-comparison).
    pub fn bump_foreground_active(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.inner
            .foreground_last_active_ts
            .store(now, std::sync::atomic::Ordering::Relaxed);
    }

    /// Read whether ingest workers should currently pause for
    /// foreground inference. `true` iff the yield window is positive
    /// AND a foreground request landed within `window` seconds. The
    /// `0` last-active sentinel always returns `false` (a fresh boot
    /// shouldn't pause before the first user request).
    pub fn should_yield_to_foreground(&self) -> bool {
        let window = self
            .inner
            .yield_window_secs
            .load(std::sync::atomic::Ordering::Relaxed);
        if window == 0 {
            return false;
        }
        let last = self
            .inner
            .foreground_last_active_ts
            .load(std::sync::atomic::Ordering::Relaxed);
        if last == 0 {
            return false;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let elapsed = now.saturating_sub(last);
        elapsed >= 0 && (elapsed as u64) < window
    }

    /// Seconds remaining in the current yield window, when one is
    /// active. Returns `None` when not currently yielding (window=0,
    /// never-active sentinel, or window already expired). Useful for
    /// progress messages and the `/internal/daemon/foreground_state`
    /// introspection route.
    pub fn seconds_until_foreground_idle(&self) -> Option<u64> {
        let window = self
            .inner
            .yield_window_secs
            .load(std::sync::atomic::Ordering::Relaxed);
        if window == 0 {
            return None;
        }
        let last = self
            .inner
            .foreground_last_active_ts
            .load(std::sync::atomic::Ordering::Relaxed);
        if last == 0 {
            return None;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let elapsed = now.saturating_sub(last);
        if elapsed < 0 {
            return Some(window);
        }
        let elapsed = elapsed as u64;
        if elapsed >= window {
            None
        } else {
            Some(window - elapsed)
        }
    }

    /// Replace the yield window at runtime. The daemon constructor
    /// calls this once with the configured value; the desktop's
    /// Settings tab calls it on user toggle. Setting `0` disables
    /// the feature entirely.
    pub fn set_yield_window_secs(&self, secs: u64) {
        self.inner
            .yield_window_secs
            .store(secs, std::sync::atomic::Ordering::Relaxed);
    }

    /// Read the configured yield window (seconds). `0` means disabled.
    pub fn yield_window_secs(&self) -> u64 {
        self.inner
            .yield_window_secs
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Read the last-foreground-active unix timestamp. `0` when the
    /// daemon has not yet served a chat request. Surfaced via
    /// `/internal/daemon/foreground_state` so operators can confirm
    /// the feature is actually wired during contention triage.
    pub fn foreground_last_active_ts(&self) -> i64 {
        self.inner
            .foreground_last_active_ts
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Read the mesh-quiesce flag. `true` means the auto-collaborate
    /// loop is suppressed: this node will not pull peer-assigned work
    /// and will not dispatch to peers on this tick.
    pub fn mesh_quiesced(&self) -> bool {
        self.inner
            .mesh_quiesced
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Flip the mesh-quiesce flag at runtime. Set to `true` when the
    /// operator wants to stop participating in shared ingests
    /// (foreground inference contention, focused work session).
    /// Reset to `false` to rejoin the auto-collaborate loop.
    pub fn set_mesh_quiesced(&self, quiesced: bool) {
        self.inner
            .mesh_quiesced
            .store(quiesced, std::sync::atomic::Ordering::Relaxed);
    }

    /// Set the maximum concurrent peer-served inference requests this
    /// node will admit. `usize::MAX` disables the cap. `0` rejects all
    /// peer work. Settings UI / `/internal/contribution/ceiling`.
    pub fn set_contribution_max_peer_inflight(&self, max: usize) {
        self.inner
            .contribution_max_peer_inflight
            .store(max, std::sync::atomic::Ordering::Relaxed);
    }

    /// Read the configured peer-inflight ceiling.
    pub fn contribution_max_peer_inflight(&self) -> usize {
        self.inner
            .contribution_max_peer_inflight
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Read the current in-flight peer request count (approximate
    /// under contention — see field docs).
    pub fn peer_inflight_count(&self) -> usize {
        self.inner
            .peer_inflight_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Set a runtime contribution pause that expires at the given
    /// unix-seconds timestamp. `0` clears any active pause. Caller
    /// (the Settings UI / `/internal/contribution/pause`) is
    /// responsible for computing the expiry — the admission layer
    /// just compares against `now()` on each peer request.
    pub fn set_contribution_paused_until(&self, expiry_unix: i64) {
        self.inner
            .contribution_paused_until
            .store(expiry_unix, std::sync::atomic::Ordering::Relaxed);
    }

    /// Read the contribution-pause expiry (unix seconds). `0` means
    /// not paused.
    pub fn contribution_paused_until(&self) -> i64 {
        self.inner
            .contribution_paused_until
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Seconds until the active pause expires. `Some(0)` is never
    /// returned — `None` means "not currently paused."
    pub fn seconds_until_unpaused(&self) -> Option<u64> {
        let expiry = self.contribution_paused_until();
        if expiry == 0 {
            return None;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let remaining = expiry.saturating_sub(now);
        if remaining <= 0 {
            None
        } else {
            Some(remaining as u64)
        }
    }

    /// When `true`, peer-served requests respect the foreground-yield
    /// window. Default `true` — this is the load-bearing setting for
    /// "the user pressed send and the GPU isn't pinned by peer work."
    pub fn yield_peers_to_foreground(&self) -> bool {
        self.inner
            .yield_peers_to_foreground
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Toggle whether peer-served requests respect the foreground-
    /// yield window.
    pub fn set_yield_peers_to_foreground(&self, on: bool) {
        self.inner
            .yield_peers_to_foreground
            .store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// Try to admit a peer-served request. Returns a `PeerInflightGuard`
    /// that increments `peer_inflight_count` for the caller and
    /// decrements it on drop. Returns an `AdmissionRejection` (mapped
    /// to 503 by the middleware) when the request shouldn't be served
    /// right now — pause active, foreground yield, or ceiling reached.
    ///
    /// Order matters: pause checked first (the most explicit "no"),
    /// then yield (user is actively using their machine), then
    /// ceiling (we're already serving as much as configured). The
    /// `retry_after_secs` field is the requester's hint for how long
    /// to wait before trying another peer; the load balancer will
    /// usually pick a different peer immediately anyway.
    pub fn admit_peer_request(
        &self,
    ) -> Result<crate::admission::PeerInflightGuard, crate::admission::AdmissionRejection>
    {
        use crate::admission::{AdmissionReason, AdmissionRejection, PeerInflightGuard};
        use std::sync::atomic::Ordering;

        if let Some(remaining) = self.seconds_until_unpaused() {
            return Err(AdmissionRejection {
                error: "contribution paused".into(),
                reason: AdmissionReason::Paused,
                retry_after_secs: remaining.max(1),
            });
        }
        if self.yield_peers_to_foreground() {
            if let Some(remaining) = self.seconds_until_foreground_idle() {
                return Err(AdmissionRejection {
                    error: "local user active".into(),
                    reason: AdmissionReason::YieldedToLocal,
                    retry_after_secs: remaining.max(1),
                });
            }
        }
        let cap = self.contribution_max_peer_inflight();
        // Pre-increment then check — atomic-correct against
        // concurrent admit calls. Overshoot is bounded by the number
        // of racers and corrected by the saturating decrement below.
        let previous = self
            .inner
            .peer_inflight_count
            .fetch_add(1, Ordering::Relaxed);
        if previous >= cap {
            self.inner
                .peer_inflight_count
                .fetch_sub(1, Ordering::Relaxed);
            return Err(AdmissionRejection {
                error: "peer concurrency ceiling reached".into(),
                reason: AdmissionReason::CeilingExceeded,
                // Short backoff; capacity may free as soon as one
                // in-flight request completes.
                retry_after_secs: 2,
            });
        }
        Ok(PeerInflightGuard::new(std::sync::Arc::clone(&self.inner)))
    }

    /// Read the per-batch ingest throttle factor as a normalised
    /// `f32` in `(0.0, 1.0]`. `1.0` = full speed (no post-batch
    /// sleep). Clamped on read so callers can use the value
    /// directly as a sleep multiplier.
    pub fn ingest_throttle_factor(&self) -> f32 {
        let raw = self
            .inner
            .ingest_throttle_milli
            .load(std::sync::atomic::Ordering::Relaxed);
        ((raw.max(1) as f32) / 1000.0).clamp(0.001, 1.0)
    }

    /// Set the throttle factor. Caller passes a value in `(0.0, 1.0]`;
    /// `0.0` is rejected (use the pause route to fully stop) and
    /// values >`1.0` are clamped. Returns the value actually stored.
    pub fn set_ingest_throttle_factor(&self, factor: f32) -> Result<f32, String> {
        if !factor.is_finite() || factor <= 0.0 {
            return Err(
                "throttle_factor must be > 0; use /internal/corpus/pause to fully stop".into(),
            );
        }
        let clamped = factor.min(1.0);
        let milli = (clamped * 1000.0).round().clamp(1.0, 1000.0) as u32;
        self.inner
            .ingest_throttle_milli
            .store(milli, std::sync::atomic::Ordering::Relaxed);
        Ok(milli as f32 / 1000.0)
    }

    /// Read the configured storage budget in bytes. Returns `None`
    /// when no budget is set (the underlying atomic is `0`), in which
    /// case the gossiped `free_storage_gb` is whatever the disk
    /// reports — no budget clamp.
    pub fn storage_budget_bytes(&self) -> Option<u64> {
        let raw = self
            .inner
            .storage_budget_bytes
            .load(std::sync::atomic::Ordering::Relaxed);
        (raw > 0).then_some(raw)
    }

    /// Set the storage budget. `None` (or `Some(0)`) clears the
    /// budget — the publish path falls back to raw free disk. Values
    /// below 1 GiB are rejected: anything tighter than that and the
    /// scheduler will essentially refuse work the moment the engine
    /// metadata directory grows past the threshold.
    pub fn set_storage_budget_bytes(&self, budget: Option<u64>) -> Result<(), String> {
        const MIN_BUDGET: u64 = 1_073_741_824; // 1 GiB.
        let raw = match budget {
            None | Some(0) => 0,
            Some(n) if n < MIN_BUDGET => {
                return Err(format!(
                    "storage budget must be either unset or ≥ 1 GiB ({MIN_BUDGET} bytes); got {n}"
                ))
            }
            Some(n) => n,
        };
        self.inner
            .storage_budget_bytes
            .store(raw, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// Most recent observation of disk consumed by installed corpora.
    /// Updated by the gossip-tick capabilities builder.
    pub fn storage_used_bytes(&self) -> u64 {
        self.inner
            .storage_used_bytes
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Update the cached storage usage. Called once per gossip tick
    /// from the capabilities builder; the value is what
    /// `GET /internal/storage/budget` reports back to the desktop.
    pub fn set_storage_used_bytes(&self, used: u64) {
        self.inner
            .storage_used_bytes
            .store(used, std::sync::atomic::Ordering::Relaxed);
    }

    /// Bytes the budget allows above current usage. `None` when no
    /// budget is set (no clamping). Saturates at zero when usage has
    /// already exceeded the budget — the capabilities builder turns
    /// that into a published `free_storage_gb` of 0, which makes the
    /// schedulers stop assigning new shards here.
    pub fn storage_remaining_bytes(&self) -> Option<u64> {
        let budget = self.storage_budget_bytes()?;
        let used = self.storage_used_bytes();
        Some(budget.saturating_sub(used))
    }

    /// Count online members.
    pub async fn online_member_count(&self) -> usize {
        let mesh = self.inner.mesh.read().await;
        mesh.members
            .values()
            .filter(|m| m.status == NodeStatus::Online || m.status == NodeStatus::Busy)
            .count()
    }

    /// Total member count.
    pub async fn total_member_count(&self) -> usize {
        let mesh = self.inner.mesh.read().await;
        mesh.members.len()
    }
}

#[cfg(test)]
pub fn test_app_state() -> AppState {
    use commonwealth_core::ids::MeshId;
    use commonwealth_core::mesh::Mesh;
    use std::collections::HashMap;
    let mesh = Mesh {
        id: MeshId::from_u128(1),
        name: "Test Mesh".into(),
        join_key_hash: [0u8; 32],
        members: HashMap::new(),
        peers: vec![],
    };
    AppState::new(NodeId::from_u128(1), mesh)
}
