use std::collections::HashSet;
use std::sync::Arc;

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
use commonwealth_knowledge::store_adapter::KnowledgeStateStore;
use commonwealth_state::MeshStore;
use corpus_engine::CorpusEngine;
use futures::Stream;

use crate::openai_types::{ChatCompletionRequest, ChatCompletionResponse};

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

    /// Streaming chat completion. Each yielded item is a partial
    /// text delta (as the LLM emits tokens). The handler turns the
    /// stream into SSE frames for the wire.
    async fn chat_completion_stream(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<String, String>> + Send>>,
        String,
    >;

    /// Provider manifest for `/oicp/v1/capabilities`. Peers fetch
    /// this to know what capabilities this node advertises — the
    /// MeshAwareSelector on the client side uses it to pick a
    /// backend. Returning `None` falls through to the scheduler-
    /// based manifest path.
    fn provider_manifest(&self) -> Option<ProviderManifest>;
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
    pub self_node_id: NodeId,
    pub mesh: RwLock<Mesh>,
    /// Inference plan, model info, ledger, and llama addresses — all via MeshStore.
    pub inference_store: InferenceStateStore,
    /// Knowledge shard plan — via MeshStore.
    pub knowledge_store: KnowledgeStateStore,
    pub model_aliases: ModelAliasTable,
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
}

impl AppState {
    pub fn new(self_node_id: NodeId, mesh: Mesh) -> Self {
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
        Self {
            inner: Arc::new(AppStateInner {
                self_node_id,
                mesh: RwLock::new(mesh),
                inference_store,
                knowledge_store,
                model_aliases: ModelAliasTable::default_table(),
                corpus_engine,
                mesh_store,
                app_registry,
                app_port_map: AppPortMap::new(),
                active_ingests: RwLock::new(HashSet::new()),
                local_inference_availability: RwLock::new(1.0_f32),
                local_inference_capable: std::sync::atomic::AtomicBool::new(false),
                on_mesh_mutation: None,
                local_inference: None,
            }),
        }
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
                tracing::warn!(
                    "with_local_inference called on shared AppState; \
                     local inference service not installed — \
                     /v1/chat/completions will fall through to \
                     orchestrator routing"
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
                tracing::warn!(
                    "with_mesh_mutation_hook called on shared AppState; \
                     persistence hook not installed — handlers will still \
                     mutate correctly, but on-join persistence falls back \
                     to the 10s gossip-loop cadence"
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
