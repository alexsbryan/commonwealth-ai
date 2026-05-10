//! MeshStore adapter for inference state.
//!
//! Serializes/deserializes `InferencePlan`, `ModelInfo`, and
//! llama-server addresses into the distributed KV store so that state
//! survives restarts and propagates to peers via gossip.
//!
//! The dimensional contribution ledger lives in
//! `commonwealth_state::ContributionEmitter` and is no longer
//! threaded through this adapter — see
//! `commonwealth_core::contributions` for the new shape.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use commonwealth_core::ids::{ModelId, NodeId};
use commonwealth_state::MeshStore;

use crate::inference_plan::InferencePlan;
use crate::model::ModelInfo;

const APP_ID: &str = "inference";

/// Encode a ModelId as a 32-character lowercase hex string using all 16 bytes.
///
/// `ModelId::Display` only shows the first 8 bytes, which causes key collisions
/// for small test IDs (e.g. from_u128(1) and from_u128(2) both display as
/// "model-0000000000000000"). The full 16-byte hex avoids this.
fn model_id_hex(id: ModelId) -> String {
    id.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

fn node_id_hex(id: NodeId) -> String {
    id.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

/// Thin wrapper over `MeshStore` for inference-domain state.
/// All methods are synchronous (SQLite is sync).
#[derive(Clone)]
pub struct InferenceStateStore {
    store: Arc<MeshStore>,
    node_id: NodeId,
}

impl InferenceStateStore {
    pub fn new(store: Arc<MeshStore>, node_id: NodeId) -> Self {
        Self { store, node_id }
    }

    // ── Inference plan ──────────────────────────────────────────────

    pub fn get_plan(&self) -> Option<InferencePlan> {
        self.store
            .get(APP_ID, "plan")
            .ok()
            .flatten()
            .and_then(|e| serde_json::from_slice(&e.value).ok())
    }

    pub fn set_plan(&self, plan: &InferencePlan) {
        if let Ok(bytes) = serde_json::to_vec(plan) {
            let _ = self.store.set(APP_ID, "plan", Bytes::from(bytes), self.node_id);
        }
    }

    // ── Model info ──────────────────────────────────────────────────

    pub fn get_model_info(&self, model_id: ModelId) -> Option<ModelInfo> {
        let key = format!("model:{}", model_id_hex(model_id));
        self.store
            .get(APP_ID, &key)
            .ok()
            .flatten()
            .and_then(|e| serde_json::from_slice(&e.value).ok())
    }

    pub fn set_model_info(&self, info: &ModelInfo) {
        let key = format!("model:{}", model_id_hex(info.id));
        if let Ok(bytes) = serde_json::to_vec(info) {
            let _ = self.store.set(APP_ID, &key, Bytes::from(bytes), self.node_id);
        }
    }

    /// Drop a previously-registered model from the store. Returns
    /// `true` when an entry was removed, `false` when there was
    /// nothing to remove. Used by the runtime
    /// `/internal/models/unload` handler so unloaded extras stop
    /// appearing in `/v1/models` immediately, instead of lingering
    /// until the next daemon restart.
    pub fn remove_model_info(&self, model_id: ModelId) -> bool {
        let key = format!("model:{}", model_id_hex(model_id));
        self.store.delete(APP_ID, &key).unwrap_or(false)
    }

    pub fn list_models(&self) -> HashMap<ModelId, ModelInfo> {
        self.store
            .scan(APP_ID, "model:")
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| serde_json::from_slice::<ModelInfo>(&e.value).ok())
            .map(|m| (m.id, m))
            .collect()
    }

    /// Same as `list_models` but pairs each entry with the `NodeId` that
    /// last wrote it to the store. The origin is the only liveness signal
    /// available — `ModelInfo::available_on` is currently never populated
    /// — so callers that need to filter by peer reachability can join
    /// this against the mesh's online-member set.
    pub fn list_models_with_origins(&self) -> Vec<(NodeId, ModelInfo)> {
        self.store
            .scan(APP_ID, "model:")
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| {
                serde_json::from_slice::<ModelInfo>(&e.value)
                    .ok()
                    .map(|m| (e.origin, m))
            })
            .collect()
    }

    // ── Mesh plan (adaptive scheduler) ────────────────────────────────

    /// Write the current MeshPlan. All nodes read this to know their role.
    pub fn set_mesh_plan(&self, plan: &crate::plan::MeshPlan) {
        if let Ok(bytes) = serde_json::to_vec(plan) {
            let _ = self.store.set(APP_ID, "mesh_plan", Bytes::from(bytes), self.node_id);
        }
    }

    pub fn get_mesh_plan(&self) -> Option<crate::plan::MeshPlan> {
        self.store
            .get(APP_ID, "mesh_plan")
            .ok()
            .flatten()
            .and_then(|e| serde_json::from_slice(&e.value).ok())
    }

    // ── Tier queue depths ──────────────────────────────────────────

    /// Write per-node queue depths for tier routing decisions.
    pub fn set_queue_depths(&self, depths: &crate::plan::TierQueueDepths) {
        let key = format!("queue_depth:{}", node_id_hex(self.node_id));
        if let Ok(bytes) = serde_json::to_vec(depths) {
            let _ = self.store.set(APP_ID, &key, Bytes::from(bytes), self.node_id);
        }
    }

    pub fn all_queue_depths(&self) -> Vec<crate::plan::TierQueueDepths> {
        self.store
            .scan(APP_ID, "queue_depth:")
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| serde_json::from_slice(&e.value).ok())
            .collect()
    }

    // ── llama-server addresses ───────────────────────────────────────

    pub fn get_llama_address(&self, model_id: ModelId) -> Option<String> {
        let key = format!("llama_addr:{}", model_id_hex(model_id));
        self.store
            .get(APP_ID, &key)
            .ok()
            .flatten()
            .and_then(|e| String::from_utf8(e.value.to_vec()).ok())
    }

    pub fn set_llama_address(&self, model_id: ModelId, addr: &str) {
        let key = format!("llama_addr:{}", model_id_hex(model_id));
        let _ = self.store.set(
            APP_ID,
            &key,
            Bytes::from(addr.to_string()),
            self.node_id,
        );
    }

    // ── Embed model info (for collaborative ingestion) ───────────────

    /// Store the active embedding model's identity and shape.
    /// Called by the Sovereign side when it loads an embed model slot.
    pub fn set_local_embed_model(&self, info: &commonwealth_core::oicp::EmbedModelInfo) {
        if let Ok(bytes) = serde_json::to_vec(info) {
            let _ = self.store.set(APP_ID, "embed_model", Bytes::from(bytes), self.node_id);
        }
    }

    /// Retrieve the stored embed model info, or `None` if not yet set.
    pub fn get_local_embed_model(&self) -> Option<commonwealth_core::oicp::EmbedModelInfo> {
        self.store
            .get(APP_ID, "embed_model")
            .ok()
            .flatten()
            .and_then(|e| serde_json::from_slice(&e.value).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use commonwealth_core::ids::{ModelId, NodeId};
    use crate::model::{ModelArchitecture, ModelInfo};
    use crate::oicp::{Capability, CapabilityProfile};

    fn make_store() -> InferenceStateStore {
        let mesh_store = Arc::new(
            commonwealth_state::MeshStore::in_memory().expect("in-memory store"),
        );
        InferenceStateStore::new(mesh_store, NodeId::from_u128(1))
    }

    fn test_model(id: u128, name: &str, caps: CapabilityProfile) -> ModelInfo {
        ModelInfo {
            id: ModelId::from_u128(id),
            name: name.into(),
            repo: format!("test/{name}"),
            file: format!("{name}.gguf"),
            size_bytes: 1_000_000,
            total_layers: 32,
            architecture: ModelArchitecture::Qwen,
            available_on: HashMap::new(),
            oicp_capabilities: caps,
            quantization: "Q4_K_M".into(),
            min_memory_gb: 0,
            preferred_memory_gb: 0,
            supports_parallel_instances: false,
            supports_pipeline_shard: false,
        }
    }

    #[test]
    fn list_models_returns_all_registered() {
        let store = make_store();

        let mut coder_caps = CapabilityProfile::default();
        coder_caps.insert(Capability::Code, 4);
        coder_caps.insert(Capability::General, 2);

        let mut general_caps = CapabilityProfile::default();
        general_caps.insert(Capability::General, 3);
        general_caps.insert(Capability::Analysis, 3);

        let coder = test_model(1, "coder-30b", coder_caps);
        let general = test_model(2, "general-30b", general_caps);

        store.set_model_info(&coder);
        store.set_model_info(&general);

        let models = store.list_models();
        assert_eq!(models.len(), 2, "both models should be in the store");
        assert!(models.contains_key(&ModelId::from_u128(1)));
        assert!(models.contains_key(&ModelId::from_u128(2)));
    }
}
