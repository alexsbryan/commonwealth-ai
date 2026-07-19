// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use tokio::sync::RwLock;

use std::pin::Pin;

use async_trait::async_trait;
use commonwealth_app::proxy::AppPortMap;
use commonwealth_app::registry::AppRegistry;
use commonwealth_core::fair_sched::{reciprocity_weight, SchedCore, TryGrant};
use commonwealth_core::ids::HandoffId;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::{Mesh, NodeStatus};
use commonwealth_inference::model_aliases::ModelAliasTable;
use commonwealth_inference::oicp::ProviderManifest;
use commonwealth_inference::store_adapter::InferenceStateStore;
use commonwealth_knowledge::store_adapter::KnowledgeStateStore;
use commonwealth_knowledge::{EphemeralGrantStore, VerifyReport, WorkQueueManager};
use commonwealth_state::{ActivityEmitter, ContributionEmitter, MeshStore, PeerPreferenceStore};
use corpus_engine::CorpusEngine;
use futures::Stream;

use crate::openai_types::{ChatCompletionRequest, ChatCompletionResponse, StreamFrame};

/// One inference slot's *actual* in-memory residency, as reported by
/// the embedded engine — the daemon-facing mirror of
/// `sovereign_core::traits::ResidentSlot`. Kept as its own type here
/// because `commonwealth-api` depends on `sovereign-core` but not
/// `sovereign-contracts`; the [`LocalInferenceService`] adapter maps
/// across the seam. This is the ground truth behind `/status`'s
/// `loaded` flag (the `ollama ps` analog).
/// Where a slot's weights physically live — the `/status` mirror of
/// `sovereign_core::traits::SlotPlacement`. The glassbox answer to "is this
/// model distributed across the mesh, and how is it split?", so an operator
/// never has to infer distribution from `free` deltas.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SlotPlacement {
    /// `local` | `distributed` | `stream-split` | `forming`.
    pub mode: String,
    /// Total transformer blocks the plan apportions (`0` when local).
    pub total_blocks: u32,
    /// Blocks resident on THIS node's local GPU.
    pub local_blocks: u32,
    /// Per remote RPC worker: endpoint + the block count pinned onto it.
    pub workers: Vec<WorkerPlacement>,
}

/// One remote worker's share of a distributed slot (`/status` mirror).
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkerPlacement {
    pub endpoint: String,
    pub blocks: u32,
    pub holds_output: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ResidentSlot {
    /// Role stem: `fast` | `primary` | `embed` | `code` | `rerank` |
    /// `extra:<name>` | `primary_pool`.
    pub role: String,
    /// The gguf file stem currently occupying the slot.
    pub model_id: String,
    /// `true` when the weights are resident in memory this instant.
    pub resident: bool,
    /// Resident byte footprint when the engine knows it, else `None`.
    pub size_bytes: Option<u64>,
    /// `true` when the slot is mid load/unload (residency momentarily
    /// indeterminate). Never forces a load to resolve it.
    pub transitioning: bool,
    /// Physical placement (distributed vs local + the split). `None` for
    /// non-distributable slots. Stated, never inferred.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placement: Option<SlotPlacement>,
}

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

    /// Report the real in-memory residency of every slot the embedded
    /// engine owns — the ground truth behind `/status`'s `loaded` flag
    /// and the `ollama ps` analog. Empty by default (the orchestrator
    /// path has no embedded engine to introspect; it keeps reporting
    /// residency via the `llama_addr:` store keys). Backed by
    /// `InferenceProvider::resident_slots`.
    fn resident_slots(&self) -> Vec<ResidentSlot> {
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

/// Worker side of the distributed-inference auto-warm orchestration. When a host
/// distributes a large primary across the mesh, it asks each worker (this node)
/// to seed its RPC tensor cache with ITS shard of the model — so the host's
/// subsequent `-ot` load is all `SET_TENSOR_HASH` cache hits and never streams a
/// large weight share (the upload deadlock). The impl (sovereign-mesh) holds an
/// HTTP client so it can fetch the GGUF — or, for the byte-range path, only its
/// shard's tensors — and the warm primitives from sovereign-inference. Injected
/// by the daemon; `None` on a node with no local inference.
///
/// Defined as an OPAQUE-JSON seam (`request`/return are the wire bodies, an
/// `RpcWarmShardRequest`/`RpcWarmShardResponse` defined in sovereign-mesh) so
/// commonwealth-api needn't depend on sovereign-inference's plan types — the same
/// decoupling [`LocalInferenceService`] gives the chat path. The route handler
/// resolves `model_id` → `local_model_path` against the servable allowlist (which
/// lives here) and passes it in, so the warmer can warm a model the node already
/// holds without re-fetching. Route: `POST /internal/rpc-warm`.
#[async_trait]
pub trait RpcShardWarmer: Send + Sync {
    /// `state` is this worker node's own `AppState`: the warmer resolves the
    /// HOST's fetch bases through this node's `PeerTransport` (the request may
    /// carry a `host_node_id`), so a cross-network host is reached over the
    /// mesh transport (iroh bridge) instead of a raw IP it may not route to.
    async fn warm_shard(
        &self,
        request: serde_json::Value,
        local_model_path: Option<std::path::PathBuf>,
        state: AppState,
    ) -> Result<serde_json::Value, String>;
}

/// Callback the route handlers fire whenever they mutate `Mesh` —
/// `/internal/join` (accepting a new member), `/internal/gossip`
/// (merging a peer's view). `sovereign-mesh::EmbeddedDaemon` installs
/// a hook that persists `mesh.json` synchronously so a restart within
/// the gossip interval never forgets a mutation. Tests leave this
/// `None` and rely on their assertions without touching disk.
pub type MeshMutationHook = std::sync::Arc<dyn Fn(&Mesh, NodeId) + Send + Sync>;

/// Shared application state for all API handlers.
#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<AppStateInner>,
}

/// Peer-inflight ceiling a freshly-constructed [`AppState`] starts with:
/// unbounded. This is the *pre-configuration* value only — the daemon ALWAYS
/// applies a finite ceiling at boot from `DaemonSection.max_peer_inflight`
/// (default 1, see `sovereign-mesh::daemon::start_daemon`), so a headless
/// contributor is never actually unbounded in production. Tests that build an
/// `AppState` directly inherit this and don't admit peer traffic, so the lack
/// of a bound is harmless there.
pub const DEFAULT_PEER_INFLIGHT_CEILING: usize = usize::MAX;

/// Reciprocity gain for the peer-admission per-node cap. A top contributor's
/// effective cap reaches the full ceiling; a pure consumer's stays at the
/// base. `0` would disable reciprocity (uniform cap = ceiling). Matches the
/// chat server's `[server] reciprocity_k` default.
pub const PEER_RECIPROCITY_K: f64 = 0.5;

/// Base per-node concurrency cap when rationing — a pure consumer (no
/// contribution) may hold this many peer slots at once.
const PEER_BASE_CAP: u32 = 1;

/// Derive a peer node's effective concurrency cap from the global ceiling and
/// its reciprocity weight. Not rationing (`ceiling` unbounded) → no per-node
/// limit, so the pool is shared freely (preserves the pre-existing default).
/// Rationing → a pure consumer holds [`PEER_BASE_CAP`]; a top contributor
/// (`weight → 1 + k`) may hold up to the whole ceiling.
fn effective_peer_cap(ceiling: usize, weight: f64) -> u32 {
    // `usize::MAX` is the "not rationing" sentinel (no comparison can
    // exceed it — `==` is the whole check).
    if ceiling == usize::MAX {
        return u32::MAX;
    }
    let ceiling = ceiling.min(u32::MAX as usize) as u32;
    if ceiling <= PEER_BASE_CAP || PEER_RECIPROCITY_K <= 0.0 {
        return ceiling.max(PEER_BASE_CAP);
    }
    // weight ∈ [1.0, 1.0 + k] → bonus ∈ [0, ceiling − base].
    let frac = ((weight - 1.0) / PEER_RECIPROCITY_K).clamp(0.0, 1.0);
    let bonus = (frac * f64::from(ceiling - PEER_BASE_CAP)).round() as u32;
    (PEER_BASE_CAP + bonus).clamp(PEER_BASE_CAP, ceiling)
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
    /// This node's Ed25519 identity pubkey (see
    /// `commonwealth_core::ids::NodePubkey`). Installed by the
    /// embedded daemon at startup from `<data_dir>/node_key`; `None`
    /// in tests and on daemons that don't manage an identity key.
    /// Gossip stamps it into our own `MemberRecord` every round so
    /// in-place upgrades publish the key without a rejoin.
    pub self_node_pubkey: std::sync::RwLock<Option<commonwealth_core::ids::NodePubkey>>,
    /// Provider yielding this node's CURRENT iroh dial info (relay URL
    /// + direct addrs), pulled fresh each gossip round and stamped into
    /// our own `MemberRecord` (Track W2). Type-erased so this crate
    /// needs no iroh dependency; installed by the daemon, which owns the
    /// iroh endpoint. `None` when iroh access is off. A pull-provider,
    /// not a stored snapshot, because the relay and hole-punched addrs
    /// appear and change over the endpoint's lifetime.
    #[allow(clippy::type_complexity)]
    pub self_iroh_dialinfo: std::sync::RwLock<
        Option<std::sync::Arc<dyn Fn() -> commonwealth_core::mesh::IrohDialInfo + Send + Sync>>,
    >,
    /// Unix-seconds TTL on the active mesh's join key, for an ENCRYPTED
    /// mesh whose invite is short-lived. Set by the founder when it
    /// mints the invite; the `/internal/join` handler rejects a join
    /// once `now >= this`. `None` (default) = no expiry (plaintext /
    /// legacy mesh).
    pub join_key_expires_at: std::sync::RwLock<Option<u64>>,
    /// Closure that signs this node's dial info (relay_url + direct addrs)
    /// for the gossip self-stamp — `(version, relay, addrs) -> hex sig`.
    /// The daemon installs it from the node `SigningKey`, so `AppState`
    /// never holds raw key material and `commonwealth-api` needs no crypto
    /// dependency. `None` until the daemon binds iroh. See [`crate::state::AppState::sign_dial_info`].
    #[allow(clippy::type_complexity)]
    pub self_dial_signer: std::sync::RwLock<
        Option<
            std::sync::Arc<
                dyn Fn(u64, Option<String>, Vec<std::net::SocketAddr>) -> String + Send + Sync,
            >,
        >,
    >,
    /// Bearer token required of non-loopback callers on the client API
    /// (`:9741`). `None` (the default) means "no token configured" —
    /// the [`crate::client_auth`] layer then admits ONLY loopback
    /// callers and fails closed for any remote one. The embedded daemon
    /// installs `Some(_)` via [`AppState::install_client_token`] at
    /// startup when it binds a routable (non-loopback) address. Stored
    /// in cleartext: the layer compares it byte-for-byte against the
    /// incoming `Authorization: Bearer`. Set-once-read-many.
    pub client_token: std::sync::RwLock<Option<Arc<str>>>,
    /// How this daemon reaches mesh peers — the PeerTransport seam.
    /// Every route handler that dials a peer resolves URLs through
    /// this instead of formatting `http://{ip}:{port}` inline, so a
    /// future transport (dial-by-key iroh) slots in without touching
    /// call sites. Defaults to `IpTransport::default()` (client port
    /// 9741); the embedded daemon re-installs one configured with
    /// its resolved client port via
    /// [`AppState::install_peer_transport`] at startup. Set-once-
    /// read-many: a plain `std::sync::RwLock` read per resolution.
    pub peer_transport: std::sync::RwLock<Arc<dyn commonwealth_transport::PeerTransport>>,
    /// Wall-clock source. Defaults to [`commonwealth_core::SystemClock`]; the
    /// test harness installs a per-node [`commonwealth_core::TestClock`] to
    /// drive skew scenarios deterministically. Read per timestamp (RwLock read
    /// + Arc clone), same set-once-read-many pattern as `peer_transport`.
    pub clock: std::sync::RwLock<Arc<dyn commonwealth_core::Clock>>,
    /// Local-observation liveness map: `node_id -> local-clock seconds at which
    /// we last observed this peer's gossiped record advance (or reached it
    /// directly). Offline-decay measures staleness against THIS, never the
    /// peer's own gossiped `last_seen` — so a clock-skewed peer can't flap
    /// Offline (the "~9 min flap"). Ephemeral; rebuilt after restart as gossip
    /// re-observes peers.
    pub peer_last_contact: std::sync::RwLock<std::collections::HashMap<NodeId, u64>>,
    /// ATOS middleware registry. Holds one instance of each
    /// middleware the pipelines can reference by id.
    pub middleware_registry: Arc<crate::middleware::MiddlewareRegistry>,
    /// ATOS session-state store. `None` until a M4.4+ daemon wires
    /// it (tests without a MeshStore handle leave this empty; the
    /// handler skips ATOS pipeline processing when the store is
    /// absent). ATOS-only — absent entirely in product builds.
    #[cfg(feature = "atos")]
    pub session_store: Option<sovereign_atos::session::SessionStore>,
    /// Repository root the Commonwealth daemon is anchored to —
    /// the directory that contains `.sovereign/features/`. Used by
    /// ApprovalGate for git lookups and by ContextInjector for
    /// reading spec.md. `None` when the daemon wasn't started in a
    /// repo-like context (degrades ATOS pipelines to a noop).
    pub repo_root: Option<std::path::PathBuf>,
    pub corpus_engine: Option<Arc<CorpusEngine>>,
    /// Process start instant — drives `/status`'s `process.uptime_seconds`
    /// (an uptime reset is the cheap witness that a supervised restart
    /// actually produced a fresh process).
    pub started_at: std::time::Instant,
    /// True when this node's iroh acceptor routes the RPC ALPN to a local
    /// ggml rpc-server — i.e. a cross-network host can genuinely reach our
    /// RPC worker through the mesh tunnel. Drives the additive
    /// `rpc_worker.iroh` flag on `/status`. Set by the daemon when it
    /// installs iroh access; stays false on plaintext meshes or when the
    /// iroh kill-switch is on, so we never advertise a path that isn't
    /// actually accepting.
    pub rpc_iroh_accept: std::sync::atomic::AtomicBool,
    /// Distributed KV store for mesh apps.
    pub mesh_store: Arc<MeshStore>,
    /// Registry of known mesh apps (gossiped).
    pub app_registry: Arc<AppRegistry>,
    /// Map of locally running app ports for the proxy layer.
    pub app_port_map: AppPortMap,
    /// Concurrent **outbound** peer knowledge fan-out requests in flight from
    /// this node (one per peer a knowledge search is currently querying). The
    /// glassbox companion to the inbound `peer_sched` admission gauge: it makes
    /// `BoundedFanOut` a live runtime signal, not just a source-unit-tested
    /// property of `select_fanout_corpora`. 0 when no fan-out is in progress.
    /// Maintained by `FanoutGuard` (routes_knowledge.rs), read via
    /// [`AppState::fanout_inflight_count`].
    pub fanout_inflight: std::sync::atomic::AtomicUsize,
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
    pub newsworthy_force_tick: RwLock<Option<tokio::sync::mpsc::Sender<()>>>,
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
    /// Worker-side auto-warm hook for distributed inference. Installed by the
    /// daemon alongside `local_inference`; drives `POST /internal/rpc-warm`.
    /// `None` on a node that isn't an inference worker. See [`RpcShardWarmer`].
    pub rpc_shard_warmer: Option<std::sync::Arc<dyn RpcShardWarmer>>,
    /// Pull-based corpus ingestion work queues keyed by `HandoffId`.
    /// The coordinator's `corpus_collaborate` handler populates this with
    /// a unit list; peers pull units via `POST /internal/corpus/next_unit`.
    /// Only coordinators hold entries here — peer nodes never mutate it.
    /// See `commonwealth-knowledge::work_queue` for the full design.
    pub work_queue: Arc<WorkQueueManager>,
    /// Ephemeral, renewable ingest grants — the out-of-band capability that
    /// authorizes a one-off peer-assisted ingest of an otherwise local-only
    /// corpus. Consulted at the `corpus_collaborate` kickoff gate; never
    /// persisted, never mutates on-disk corpus metadata (so the corpus's
    /// standing `mesh_sharing = false` posture is preserved throughout).
    /// See `commonwealth-knowledge::ingest_grant`.
    pub grant_store: Arc<EphemeralGrantStore>,
    /// Handoff IDs for which this node is currently running a pull loop
    /// (as a peer). Prevents `auto_ingest` from spawning duplicate pull
    /// loops when the same open handoff is seen across multiple gossip ticks.
    pub active_pull_loops: RwLock<HashSet<HandoffId>>,
    /// Post-merge verification spot-check reports, keyed by handoff. The merge
    /// coordinator writes one after re-embedding a sample of the merged corpus
    /// locally; the collaborate-status endpoint surfaces it for the desktop's
    /// glassbox "re-checked N chunks — all matched" line.
    pub verify_reports: RwLock<HashMap<HandoffId, VerifyReport>>,
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

    /// Fair admission for peer-served inference — one accounting authority
    /// (the same `SchedCore` policy the chat server uses) holding the
    /// runtime-mutable global ceiling (`slots`) AND a per-node concurrency
    /// cap, so one peer can't hog the pool even under the ceiling. `slots =
    /// usize::MAX` (default) disables the ceiling ("share freely"); `0`
    /// rejects all peer work (equivalent to `SOVEREIGN_DISABLE_PEER_INFERENCE=1`).
    /// Set via `POST /internal/contribution/ceiling`. The middleware
    /// (`crate::admission`) calls `try_grant` per peer request and 503s on
    /// refusal; the per-request `PeerInflightGuard` `release`s on drop.
    pub peer_sched: Mutex<SchedCore<NodeId>>,

    /// Cached reciprocity weight per peer node (`1.0 + k·norm(contribution)`),
    /// refreshed out-of-band from the contribution ledger by a daemon loop.
    /// Scales each node's effective concurrency cap when the operator is
    /// rationing (a finite ceiling) — a contributor may hold more slots at
    /// once. Absent nodes are neutral (`1.0`). `ArcSwap` for lock-free reads
    /// on the admission hot path.
    pub reciprocity_weights: ArcSwap<HashMap<NodeId, f64>>,

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
    /// `min(actual_free, max(0, budget − used))`. The live planner
    /// (the three `knowledge_assignment::plan_collaborative_ingestion*`
    /// variants) reads that one value to decide what to assign here
    /// (a peer at 0 is skipped outright), so clamping it
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
    pub local_in_flight_publisher: std::sync::OnceLock<Arc<std::sync::atomic::AtomicU32>>,
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
    pub fn install_slot_aliases(&self, aliases: std::collections::HashMap<String, String>) {
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
    pub fn install_servable_model_files(&self, files: Vec<std::path::PathBuf>) {
        self.inner.servable_model_files.store(Arc::new(files));
    }

    /// This node's identity pubkey, if one was installed.
    pub fn self_node_pubkey(&self) -> Option<commonwealth_core::ids::NodePubkey> {
        *self
            .inner
            .self_node_pubkey
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Install this node's identity pubkey. The embedded daemon
    /// calls this at startup after `load_or_generate_node_key`.
    pub fn install_self_node_pubkey(&self, pubkey: commonwealth_core::ids::NodePubkey) {
        *self
            .inner
            .self_node_pubkey
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pubkey);
    }

    /// This node's current iroh dial info, if a provider is installed
    /// (iroh access enabled). Pulled live from the endpoint each call.
    pub fn self_iroh_dialinfo(&self) -> Option<commonwealth_core::mesh::IrohDialInfo> {
        self.inner
            .self_iroh_dialinfo
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|provider| provider())
    }

    /// Set (or clear) the active join key's TTL. The founder calls this
    /// when it mints an encrypted-mesh invite; the join handler reads it
    /// to reject an expired join.
    pub fn set_join_key_expiry(&self, expires_at: Option<u64>) {
        *self
            .inner
            .join_key_expires_at
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = expires_at;
    }

    /// The active join key's expiry (unix-seconds), if any.
    pub fn join_key_expiry(&self) -> Option<u64> {
        *self
            .inner
            .join_key_expires_at
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Install the dial-info signing closure (daemon builds it from the
    /// node SigningKey after binding iroh).
    #[allow(clippy::type_complexity)]
    pub fn install_self_dial_signer(
        &self,
        signer: std::sync::Arc<
            dyn Fn(u64, Option<String>, Vec<std::net::SocketAddr>) -> String + Send + Sync,
        >,
    ) {
        *self
            .inner
            .self_dial_signer
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(signer);
    }

    /// Sign this node's dial info (hex), or `None` if no signer is
    /// installed (iroh disabled / pre-identity build).
    pub fn sign_dial_info(
        &self,
        version: u64,
        relay_url: Option<&str>,
        direct_addrs: &[std::net::SocketAddr],
    ) -> Option<String> {
        let signer = self
            .inner
            .self_dial_signer
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        Some(signer(
            version,
            relay_url.map(|s| s.to_string()),
            direct_addrs.to_vec(),
        ))
    }

    /// Install the provider yielding this node's live iroh dial info.
    /// The daemon calls this after binding its iroh endpoint (W2), so
    /// gossip can stamp relay_url + iroh_direct_addrs into our own
    /// `MemberRecord` every round.
    pub fn install_self_iroh_dialinfo(
        &self,
        provider: std::sync::Arc<dyn Fn() -> commonwealth_core::mesh::IrohDialInfo + Send + Sync>,
    ) {
        *self
            .inner
            .self_iroh_dialinfo
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(provider);
    }

    /// Install the client-API bearer token. The embedded daemon calls
    /// this at startup with the token from
    /// `commonwealth_transport::identity::load_or_create_client_token`
    /// ONLY when it binds a non-loopback address; loopback-only
    /// deployments leave it `None` (no secret generated, all local
    /// traffic admitted by [`crate::client_auth`]).
    pub fn install_client_token(&self, token: Option<Arc<str>>) {
        *self
            .inner
            .client_token
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = token;
    }

    /// Snapshot of the configured client-API bearer token (cheap RwLock
    /// read + Arc clone). `None` ⇒ no token configured. Read per request
    /// by the [`crate::client_auth`] layer.
    pub fn client_token(&self) -> Option<Arc<str>> {
        self.inner
            .client_token
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Record whether the iroh acceptor routes the RPC ALPN to a local
    /// ggml rpc-server (see `AppStateInner::rpc_iroh_accept`). Called by
    /// the daemon's iroh install; re-runnable (watchdog endpoint swaps).
    pub fn set_rpc_iroh_accept(&self, on: bool) {
        self.inner
            .rpc_iroh_accept
            .store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether `/status` may honestly advertise `rpc_worker.iroh: true`.
    pub fn rpc_iroh_accept(&self) -> bool {
        self.inner
            .rpc_iroh_accept
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Snapshot of the active [`PeerTransport`]. Cheap (one RwLock
    /// read + Arc clone); call per dial, don't cache across awaits —
    /// the daemon may re-install at startup.
    pub fn peer_transport(&self) -> Arc<dyn commonwealth_transport::PeerTransport> {
        self.inner
            .peer_transport
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Replace the peer transport. The embedded daemon calls this
    /// once at startup with an `IpTransport` configured for its
    /// resolved client port (the `AppState::new*` default assumes
    /// 9741). Same install-at-boot pattern as
    /// [`AppState::install_slot_aliases`].
    pub fn install_peer_transport(
        &self,
        transport: Arc<dyn commonwealth_transport::PeerTransport>,
    ) {
        *self
            .inner
            .peer_transport
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = transport;
    }

    /// Snapshot of the active [`commonwealth_core::Clock`]. Cheap (one RwLock
    /// read + Arc clone); call per timestamp, don't cache across awaits.
    pub fn clock(&self) -> Arc<dyn commonwealth_core::Clock> {
        self.inner
            .clock
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Replace the clock. The test harness calls this with a per-node
    /// `TestClock` to drive skew; production leaves the `SystemClock` default.
    pub fn install_clock(&self, clock: Arc<dyn commonwealth_core::Clock>) {
        *self
            .inner
            .clock
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = clock;
    }

    /// Record that we just observed `peer`'s liveness — its gossiped record
    /// advanced (added or LWW-updated, possibly via transitive gossip) or we
    /// reached it directly — at local time `now_secs`. The stamp is always
    /// OUR clock, so a peer's skewed `last_seen` can't drive offline-decay.
    pub fn observe_peer_contact(&self, peer: NodeId, now_secs: u64) {
        self.inner
            .peer_last_contact
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(peer, now_secs);
    }

    /// Local-observation time for `peer`, initializing it to `now_secs` (and
    /// returning that) when we have no record yet. The lazy init gives a
    /// freshly-seen peer a full threshold grace window before it can decay, so
    /// a peer learned at startup isn't decayed before we've had a chance to
    /// gossip with it.
    pub fn peer_contact_or_init(&self, peer: NodeId, now_secs: u64) -> u64 {
        *self
            .inner
            .peer_last_contact
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(peer)
            .or_insert(now_secs)
    }

    pub fn new(self_node_id: NodeId, mesh: Mesh) -> Self {
        // Test-support constructor (callers in tests/ + the test-harness);
        // in-memory MeshStore creation is infallible — fail-fast is correct.
        #[allow(clippy::expect_used)]
        let mesh_store = Arc::new(MeshStore::in_memory().expect("in-memory MeshStore failed"));
        Self::new_with_platform(self_node_id, mesh, mesh_store, Arc::new(AppRegistry::new()))
    }

    /// Create state with explicit platform components (used by the daemon).
    pub fn new_with_platform(
        self_node_id: NodeId,
        mesh: Mesh,
        mesh_store: Arc<MeshStore>,
        app_registry: Arc<AppRegistry>,
    ) -> Self {
        Self::new_with_platform_and_engine(self_node_id, mesh, mesh_store, app_registry, None)
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
        let contribution_emitter = ContributionEmitter::new((*mesh_store).clone(), self_node_id);
        let activity_emitter = ActivityEmitter::new((*mesh_store).clone(), self_node_id);
        let peer_preferences = PeerPreferenceStore::new((*mesh_store).clone(), self_node_id);
        // ATOS middleware registry with the M4 core four implementations
        // registered under their TOML ids. The wiring is intentionally
        // additive — operators deploying a stock Commonwealth daemon
        // get the full stack without extra config; tests that want a
        // bare daemon can build a minimal registry themselves.
        let mut middleware_registry = crate::middleware::MiddlewareRegistry::new();
        #[cfg(feature = "atos")]
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
        #[cfg(feature = "atos")]
        middleware_registry.register(Arc::new(crate::middleware::ContextInjector::empty()));
        middleware_registry.register(Arc::new(crate::middleware::ToolInjector::empty()));
        #[cfg(feature = "atos")]
        middleware_registry.register(Arc::new(crate::middleware::ArtifactSurface::new()));
        #[cfg(feature = "atos")]
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
        middleware_registry.register(Arc::new(crate::middleware::DecisionExtractor::new()));
        // `read_only_enforcer` is the red-team alias's gate. For M4
        // it shares the ApprovalGate implementation under a distinct
        // id — M5 splits them if the behavior actually diverges.
        #[cfg(feature = "atos")]
        {
            let read_only = Arc::new(crate::middleware::ApprovalGate::new());
            middleware_registry.register(read_only);
        }

        // Session store is wired up when the daemon has a MeshStore
        // in hand. The handler falls back to legacy routing when
        // this is None. ATOS-only.
        #[cfg(feature = "atos")]
        let session_store = Some(sovereign_atos::session::SessionStore::new(
            (*mesh_store).clone(),
            self_node_id,
        ));
        Self {
            inner: Arc::new(AppStateInner {
                started_at: std::time::Instant::now(),
                rpc_iroh_accept: std::sync::atomic::AtomicBool::new(false),
                self_node_id_swap: ArcSwap::from_pointee(self_node_id),
                mesh: RwLock::new(mesh),
                inference_store,
                knowledge_store,
                model_aliases: ModelAliasTable::default_table(),
                pipeline_aliases:
                    commonwealth_core::pipeline_aliases::PipelineAliasTable::default_table(),
                slot_aliases: ArcSwap::from_pointee(std::collections::HashMap::new()),
                servable_model_files: ArcSwap::from_pointee(Vec::new()),
                self_node_pubkey: std::sync::RwLock::new(None),
                self_iroh_dialinfo: std::sync::RwLock::new(None),
                join_key_expires_at: std::sync::RwLock::new(None),
                self_dial_signer: std::sync::RwLock::new(None),
                client_token: std::sync::RwLock::new(None),
                peer_transport: std::sync::RwLock::new(Arc::new(
                    commonwealth_transport::IpTransport::default(),
                )),
                clock: std::sync::RwLock::new(Arc::new(commonwealth_core::SystemClock)),
                peer_last_contact: std::sync::RwLock::new(std::collections::HashMap::new()),
                middleware_registry: Arc::new(middleware_registry),
                #[cfg(feature = "atos")]
                session_store,
                repo_root: std::env::current_dir().ok(),
                corpus_engine,
                mesh_store,
                app_registry,
                app_port_map: AppPortMap::new(),
                fanout_inflight: std::sync::atomic::AtomicUsize::new(0),
                active_ingests: RwLock::new(HashSet::new()),
                corpus_progress: RwLock::new(HashMap::new()),
                newsworthy_force_tick: RwLock::new(None),
                local_inference_availability: RwLock::new(1.0_f32),
                local_inference_capable: std::sync::atomic::AtomicBool::new(false),
                on_mesh_mutation: None,
                local_inference: None,
                rpc_shard_warmer: None,
                work_queue: Arc::new(WorkQueueManager::new()),
                grant_store: Arc::new(EphemeralGrantStore::new()),
                active_pull_loops: RwLock::new(HashSet::new()),
                verify_reports: RwLock::new(HashMap::new()),
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
                // Peer-admission fair scheduler: global ceiling = the boot
                // value above; queue depth is unused on this shed-only gate
                // (`try_grant` never queues). Reciprocity weights start empty
                // (every node neutral) until the daemon's refresh loop runs.
                peer_sched: Mutex::new(SchedCore::new(DEFAULT_PEER_INFLIGHT_CEILING, 1)),
                reciprocity_weights: ArcSwap::from_pointee(HashMap::new()),
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
    pub fn install_in_flight_publisher(&self, publisher: Arc<std::sync::atomic::AtomicU32>) {
        let _ = self.inner.local_in_flight_publisher.set(publisher);
    }

    /// Borrow the installed in-flight publisher Arc. Hot-reload path
    /// calls this to pass the same Arc into
    /// `MeshInferenceProvider::with_in_flight_publisher`. `None` when
    /// the bootstrap hasn't run an install yet (test harnesses,
    /// storage-only nodes).
    pub fn in_flight_publisher(&self) -> Option<Arc<std::sync::atomic::AtomicU32>> {
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

    /// Install the worker-side RPC shard warmer ([`RpcShardWarmer`]) — the
    /// `POST /internal/rpc-warm` backend. Same contract as `with_local_inference`:
    /// call before cloning AppState into the HTTP servers (uses `Arc::get_mut`).
    pub fn with_rpc_shard_warmer(mut self, warmer: std::sync::Arc<dyn RpcShardWarmer>) -> Self {
        match Arc::get_mut(&mut self.inner) {
            Some(inner) => {
                inner.rpc_shard_warmer = Some(warmer);
            }
            None => {
                tracing::error!(
                    strong_count = Arc::strong_count(&self.inner),
                    "with_rpc_shard_warmer called on shared AppState — auto-warm \
                     orchestration disabled; a distributed primary load will fall \
                     back to local-only. Move the installer above any inner.clone()."
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
        self.inner
            .inference_store
            .set_llama_address(model_id, &address);
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
        tracing::debug!(
            availability,
            "inference_availability updated by sovereign-server"
        );
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
        self.lock_peer_sched().set_slots(max);
    }

    /// Read the configured peer-inflight ceiling (the global slot budget).
    pub fn contribution_max_peer_inflight(&self) -> usize {
        self.lock_peer_sched().slots()
    }

    /// Read the current in-flight peer request count.
    pub fn peer_inflight_count(&self) -> usize {
        self.lock_peer_sched().in_flight()
    }

    /// Read the current count of **outbound** peer fan-out requests in flight
    /// (the `fanout_inflight` gauge). Drives the `BoundedFanOut` glassbox check.
    pub fn fanout_inflight_count(&self) -> usize {
        self.inner
            .fanout_inflight
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Lock the peer scheduler, recovering from poison rather than cascading a
    /// panic into every future admission.
    fn lock_peer_sched(&self) -> std::sync::MutexGuard<'_, SchedCore<NodeId>> {
        self.inner
            .peer_sched
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Reciprocity weight for a peer node (`1.0` = neutral / unknown). A
    /// lock-free `ArcSwap` read — safe on the admission hot path.
    fn peer_reciprocity_weight(&self, node: &NodeId) -> f64 {
        self.inner
            .reciprocity_weights
            .load()
            .get(node)
            .copied()
            .unwrap_or(1.0)
    }

    /// Recompute the cached per-node reciprocity weights from the contribution
    /// ledger. Called off the hot path (a daemon loop, ~30 s cadence); never
    /// per request. `k` is the reciprocity gain (`0` disables it). On error
    /// the previous weights are kept — a transient ledger hiccup must not flap
    /// everyone to neutral mid-contention.
    pub async fn refresh_reciprocity_weights(&self, k: f64) {
        let caps: HashMap<NodeId, commonwealth_core::capabilities::NodeCapabilities> = {
            let view = self.inner.mesh.read().await;
            view.members
                .iter()
                .map(|(id, m)| (*id, m.capabilities.clone()))
                .collect()
        };
        let contributions = match commonwealth_state::current_contributions(
            &self.inner.mesh_store,
            &caps,
            commonwealth_core::contributions::DEFAULT_WINDOW_DAYS,
        ) {
            Ok(map) => map,
            Err(e) => {
                tracing::warn!(error = %e, "reciprocity: aggregate failed; keeping last weights");
                return;
            }
        };
        let max = contributions
            .values()
            .map(|c| c.inference_served.wall_seconds)
            .fold(0.0_f64, f64::max);
        let weights: HashMap<NodeId, f64> = contributions
            .into_iter()
            .filter_map(|(id, c)| {
                let w = reciprocity_weight(c.inference_served.wall_seconds, max, k);
                (w > 1.0).then_some((id, w))
            })
            .collect();
        let n = weights.len();
        self.inner.reciprocity_weights.store(Arc::new(weights));
        tracing::debug!(contributors = n, "reciprocity: peer weights refreshed");
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

    /// Try to admit a peer-served request from `node`. Returns a
    /// `PeerInflightGuard` (RAII: `release`s the node's slot on drop), or an
    /// `AdmissionRejection` (mapped to 503 by the middleware) when the request
    /// shouldn't be served right now.
    ///
    /// Order matters: pause checked first (the most explicit "no"), then yield
    /// (the user is actively using their machine), then the fair scheduler — a
    /// per-node cap (anti-hog) scaled by the node's reciprocity weight, under
    /// the global ceiling. The scheduler is shed-only here (the peer load
    /// balancer routes elsewhere on refusal), so a refusal is immediate.
    /// `retry_after_secs` hints how long to wait before retrying.
    pub fn admit_peer_request(
        &self,
        node: NodeId,
    ) -> Result<crate::admission::PeerInflightGuard, crate::admission::AdmissionRejection> {
        use crate::admission::{AdmissionReason, AdmissionRejection, PeerInflightGuard};

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

        // Fair admission: a per-node cap (reciprocity-scaled) under the global
        // ceiling, enforced by the shared `SchedCore`. `node` is `Copy`, so we
        // reuse it for the guard after the (consuming) `try_grant`.
        let weight = self.peer_reciprocity_weight(&node);
        let mut sched = self.lock_peer_sched();
        let cap = effective_peer_cap(sched.slots(), weight);
        match sched.try_grant(node, weight, cap) {
            TryGrant::Granted => {
                drop(sched);
                Ok(PeerInflightGuard::new(
                    std::sync::Arc::clone(&self.inner),
                    node,
                ))
            }
            // Both outcomes mean "at capacity now" on this shed-only gate.
            TryGrant::WouldQueue { .. } | TryGrant::Shed { .. } => Err(AdmissionRejection {
                error: "peer concurrency ceiling reached".into(),
                reason: AdmissionReason::CeilingExceeded,
                retry_after_secs: 2,
            }),
        }
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
            .filter(|m| {
                m.is_active() && (m.status == NodeStatus::Online || m.status == NodeStatus::Busy)
            })
            .count()
    }

    /// Total member count (active members only — a tombstoned/departed node
    /// still circulates for convergence but is not counted).
    pub async fn total_member_count(&self) -> usize {
        let mesh = self.inner.mesh.read().await;
        mesh.members.values().filter(|m| m.is_active()).count()
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
        require_encryption: false,
        members: HashMap::new(),
        peers: vec![],
    };
    AppState::new(NodeId::from_u128(1), mesh)
}

#[cfg(test)]
mod fair_admission_tests {
    use super::effective_peer_cap;

    #[test]
    fn unlimited_ceiling_means_no_per_node_cap() {
        // Not rationing → share freely (preserves the pre-existing default:
        // the only bound is the global ceiling, which is unbounded here).
        assert_eq!(effective_peer_cap(usize::MAX, 1.0), u32::MAX);
        assert_eq!(effective_peer_cap(usize::MAX, 1.5), u32::MAX);
    }

    #[test]
    fn rationing_caps_a_pure_consumer_at_base() {
        // ceiling 4, neutral weight → base cap of 1 (anti-hog: one consumer
        // can't grab all four slots).
        assert_eq!(effective_peer_cap(4, 1.0), 1);
    }

    #[test]
    fn rationing_lifts_a_top_contributor_to_the_ceiling() {
        // weight 1.0 + k (= 1.5 at k=0.5) → may hold the whole ceiling.
        assert_eq!(effective_peer_cap(4, 1.5), 4);
        // A mid contributor lands between base and ceiling.
        let mid = effective_peer_cap(4, 1.25);
        assert!((2..=3).contains(&mid), "mid contributor: {mid}");
    }

    #[test]
    fn zero_ceiling_caps_at_base_slots_do_the_rejecting() {
        // The cap clamps to ≥ base even at ceiling 0; it's the 0-slot budget
        // in `SchedCore` that actually rejects, not this cap.
        assert_eq!(effective_peer_cap(0, 1.5), 1);
    }
}
