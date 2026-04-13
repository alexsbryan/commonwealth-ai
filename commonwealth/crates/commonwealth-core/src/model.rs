use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::ids::{ModelId, NodeId};
use crate::oicp::CapabilityProfile;

/// Information about a model known to the mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: ModelId,
    pub name: String,
    pub repo: String,
    pub file: String,
    pub size_bytes: u64,
    pub total_layers: u32,
    pub architecture: ModelArchitecture,
    pub available_on: HashMap<NodeId, ModelAvailability>,
    pub oicp_capabilities: CapabilityProfile,
    pub quantization: String,

    // ── Deployment constraints (used by the adaptive mesh scheduler) ──

    /// Minimum unified/VRAM memory required to load this model at all.
    /// Nodes below this threshold are never assigned this model.
    #[serde(default)]
    pub min_memory_gb: u32,

    /// Preferred memory for comfortable operation (model + KV cache headroom).
    #[serde(default)]
    pub preferred_memory_gb: u32,

    /// Whether this model can run as multiple independent instances.
    /// Dense models: true. MoE with expert routing that requires shared
    /// memory: false.
    #[serde(default)]
    pub supports_parallel_instances: bool,

    /// Whether this model can be sharded across nodes via pipeline parallelism.
    #[serde(default)]
    pub supports_pipeline_shard: bool,
}

/// Model architecture family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelArchitecture {
    Llama,
    Qwen,
    Mistral,
    Phi,
    Gemma,
    Other,
}

/// Whether a model is available on a specific node and in what state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelAvailability {
    /// Model file is on disk, ready to load.
    Available,
    /// Model is currently loaded in memory and serving.
    Loaded,
    /// Model is being downloaded.
    Downloading,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ModelId;

    #[test]
    fn model_info_serde_roundtrip() {
        let model = ModelInfo {
            id: ModelId::from_u128(1),
            name: "Qwen3-Coder-30B".into(),
            repo: "Qwen/Qwen3-Coder-30B-GGUF".into(),
            file: "qwen3-coder-30b-q4_k_m.gguf".into(),
            size_bytes: 17_000_000_000,
            total_layers: 64,
            architecture: ModelArchitecture::Qwen,
            available_on: HashMap::new(),
            oicp_capabilities: CapabilityProfile::default(),
            quantization: "Q4_K_M".into(),
            min_memory_gb: 32,
            preferred_memory_gb: 48,
            supports_parallel_instances: true,
            supports_pipeline_shard: true,
        };
        let json = serde_json::to_string(&model).unwrap();
        let back: ModelInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "Qwen3-Coder-30B");
        assert_eq!(back.total_layers, 64);
        assert_eq!(back.architecture, ModelArchitecture::Qwen);
    }

    #[test]
    fn model_availability_serde_roundtrip() {
        for avail in [
            ModelAvailability::Available,
            ModelAvailability::Loaded,
            ModelAvailability::Downloading,
        ] {
            let json = serde_json::to_string(&avail).unwrap();
            let back: ModelAvailability = serde_json::from_str(&json).unwrap();
            assert_eq!(avail, back);
        }
    }
}
