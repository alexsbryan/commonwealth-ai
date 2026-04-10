use std::sync::Arc;

use tokio::sync::RwLock;

use commonwealth_app::registry::AppRegistry;
use commonwealth_app::proxy::AppPortMap;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::{Mesh, NodeStatus};
use commonwealth_inference::model_aliases::ModelAliasTable;
use commonwealth_inference::store_adapter::InferenceStateStore;
use commonwealth_knowledge::store_adapter::KnowledgeStateStore;
use commonwealth_state::MeshStore;
use corpus_engine::CorpusEngine;

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
        let inference_store = InferenceStateStore::new(Arc::clone(&mesh_store), self_node_id);
        let knowledge_store = KnowledgeStateStore::new(Arc::clone(&mesh_store), self_node_id);
        Self {
            inner: Arc::new(AppStateInner {
                self_node_id,
                mesh: RwLock::new(mesh),
                inference_store,
                knowledge_store,
                model_aliases: ModelAliasTable::default_table(),
                corpus_engine: None,
                mesh_store,
                app_registry,
                app_port_map: AppPortMap::new(),
            }),
        }
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
