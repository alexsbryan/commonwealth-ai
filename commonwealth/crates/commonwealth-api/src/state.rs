use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use commonwealth_core::ids::{ModelId, NodeId};
use commonwealth_core::knowledge::KnowledgeShardPlan;
use commonwealth_core::mesh::{Mesh, NodeStatus};
use commonwealth_core::model::ModelInfo;
use commonwealth_core::model_aliases::ModelAliasTable;
use commonwealth_core::scheduler::InferencePlan;

/// Shared application state for all API handlers.
#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<AppStateInner>,
}

pub struct AppStateInner {
    pub self_node_id: NodeId,
    pub mesh: RwLock<Mesh>,
    pub models: RwLock<HashMap<ModelId, ModelInfo>>,
    pub inference_plan: RwLock<InferencePlan>,
    /// Maps model_id → llama-server address (host:port) on this node.
    pub llama_server_addresses: RwLock<HashMap<ModelId, String>>,
    pub knowledge_plan: RwLock<KnowledgeShardPlan>,
    pub model_aliases: ModelAliasTable,
}

impl AppState {
    pub fn new(self_node_id: NodeId, mesh: Mesh) -> Self {
        Self {
            inner: Arc::new(AppStateInner {
                self_node_id,
                mesh: RwLock::new(mesh),
                models: RwLock::new(HashMap::new()),
                inference_plan: RwLock::new(InferencePlan {
                    model_plans: vec![],
                }),
                llama_server_addresses: RwLock::new(HashMap::new()),
                knowledge_plan: RwLock::new(KnowledgeShardPlan {
                    assignments: vec![],
                    redundancy_achieved: HashMap::new(),
                }),
                model_aliases: ModelAliasTable::default_table(),
            }),
        }
    }

    /// Register a model as available on the mesh.
    pub async fn register_model(&self, model: ModelInfo) {
        let id = model.id;
        self.inner.models.write().await.insert(id, model);
    }

    /// Set the address of a llama-server for a model (after orchestrator spawns it).
    pub async fn set_llama_server_address(&self, model_id: ModelId, address: String) {
        self.inner
            .llama_server_addresses
            .write()
            .await
            .insert(model_id, address);
    }

    /// Get the llama-server address for a model.
    pub async fn get_llama_server_address(&self, model_id: ModelId) -> Option<String> {
        self.inner
            .llama_server_addresses
            .read()
            .await
            .get(&model_id)
            .cloned()
    }

    /// Get the default model (first in the inference plan).
    pub async fn default_model_id(&self) -> Option<ModelId> {
        let plan = self.inner.inference_plan.read().await;
        plan.model_plans.first().map(|p| p.model)
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
    let mesh = Mesh {
        id: MeshId::from_u128(1),
        name: "Test Mesh".into(),
        join_key_hash: [0u8; 32],
        members: HashMap::new(),
        peers: vec![],
    };
    AppState::new(NodeId::from_u128(1), mesh)
}
