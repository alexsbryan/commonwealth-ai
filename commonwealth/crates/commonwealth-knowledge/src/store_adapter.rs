//! MeshStore adapter for knowledge shard state.

use std::sync::Arc;

use bytes::Bytes;
use commonwealth_core::ids::NodeId;
use commonwealth_core::knowledge::KnowledgeShardPlan;
use commonwealth_state::MeshStore;

const APP_ID: &str = "knowledge";

/// Thin wrapper over `MeshStore` for knowledge-domain state.
#[derive(Clone)]
pub struct KnowledgeStateStore {
    store: Arc<MeshStore>,
    node_id: NodeId,
}

impl KnowledgeStateStore {
    pub fn new(store: Arc<MeshStore>, node_id: NodeId) -> Self {
        Self { store, node_id }
    }

    pub fn get_shard_plan(&self) -> Option<KnowledgeShardPlan> {
        self.store
            .get(APP_ID, "knowledge_plan")
            .ok()
            .flatten()
            .and_then(|e| serde_json::from_slice(&e.value).ok())
    }

    pub fn set_shard_plan(&self, plan: &KnowledgeShardPlan) {
        if let Ok(bytes) = serde_json::to_vec(plan) {
            let _ = self
                .store
                .set(APP_ID, "knowledge_plan", Bytes::from(bytes), self.node_id);
        }
    }
}
