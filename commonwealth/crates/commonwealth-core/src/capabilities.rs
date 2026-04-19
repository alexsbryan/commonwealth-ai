use serde::{Deserialize, Serialize};

use crate::knowledge::CorpusShardInfo;
use crate::oicp::EmbedModelInfo;

/// A node's full capability report — hardware profile plus current availability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeCapabilities {
    pub hardware: HardwareProfile,
    pub available: AvailableResources,
    pub active_processes: Vec<ProcessInfo>,
    pub hosted_corpora: Vec<CorpusShardInfo>,
    pub reported_at: u64,
    /// Fractional availability for inference workloads (0.0–1.0).
    /// Driven by sovereign-server's ActivityReporter; 1.0 means fully idle.
    /// Nodes without sovereign-server always report 1.0 (the serde default).
    #[serde(default = "default_inference_availability")]
    pub inference_availability: f32,

    /// Hard gate: false means never route inference requests to this node.
    /// Set at daemon startup after model probe; gossiped so remote schedulers
    /// can filter this node immediately.
    /// Defaults to false so old gossip payloads (without this field) are
    /// conservatively excluded until the peer re-joins with an updated daemon.
    /// Asymmetry with inference_availability (defaults 1.0) is intentional:
    /// availability is a soft preference signal; capability is a hard claim.
    #[serde(default)]
    pub inference_capable: bool,

    /// Model names confirmed loadable by the daemon's startup probe.
    /// Empty for storage-only nodes.
    #[serde(default)]
    pub loaded_models: Vec<String>,

    /// Advertised embedding model for this node. Populated when the
    /// daemon has an embed slot loaded; `None` before bootstrap
    /// completes or on nodes that run no embed model at all.
    ///
    /// **Why this must be gossiped.** The collaborative-ingestion
    /// planner on Machine A needs to know whether Machine B's embed
    /// space matches its own BEFORE dispatching a partition. Cosine
    /// similarity across different embedding spaces is meaningless,
    /// so a mismatch means the partition can't be merged later.
    ///
    /// Before this field existed, the planner filtered candidates on
    /// `free_storage_gb > 0` only, dispatched to everyone, and relied
    /// on the peer-side `ingest_partition` handler to reject with 409
    /// on mismatch. That rejection was fire-and-forget, so the
    /// coordinator never learned about it and logs looked like the
    /// peer just didn't participate. With this field the planner
    /// filters upfront and logs the mismatch loudly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_model: Option<EmbedModelInfo>,
}

fn default_inference_availability() -> f32 {
    1.0
}

/// Static hardware profile of a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareProfile {
    pub gpus: Vec<GpuInfo>,
    pub system_ram_gb: u32,
    pub cpu_cores: u32,
    pub total_storage_gb: u32,
    pub free_storage_gb: u32,
    pub network_bandwidth_mbps: Option<u32>,
}

/// Information about a single GPU.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuInfo {
    pub name: String,
    pub vram_gb: u32,
    pub compute_type: ComputeType,
    pub estimated_tflops: f32,
}

/// GPU compute backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeType {
    Cuda,
    Rocm,
    Metal,
    Vulkan,
}

/// Currently available resources on a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailableResources {
    pub free_vram_gb: f32,
    pub free_ram_gb: f32,
    pub free_storage_gb: f32,
    pub gpu_utilization: f32,
    pub cpu_utilization: f32,
    /// User can pause contribution to the mesh.
    pub available_for_mesh: bool,
}

/// Information about a managed process running on a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub process_id: crate::ids::ProcessId,
    pub kind: ProcessKind,
    pub pid: u32,
}

/// Kind of managed process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessKind {
    LlamaServer,
    RpcServer,
}

impl Default for AvailableResources {
    fn default() -> Self {
        Self {
            free_vram_gb: 0.0,
            free_ram_gb: 0.0,
            free_storage_gb: 0.0,
            gpu_utilization: 0.0,
            cpu_utilization: 0.0,
            available_for_mesh: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_type_serde_roundtrip() {
        for ct in [
            ComputeType::Cuda,
            ComputeType::Rocm,
            ComputeType::Metal,
            ComputeType::Vulkan,
        ] {
            let json = serde_json::to_string(&ct).unwrap();
            let back: ComputeType = serde_json::from_str(&json).unwrap();
            assert_eq!(ct, back);
        }
    }

    #[test]
    fn hardware_profile_serde_roundtrip() {
        let profile = HardwareProfile {
            gpus: vec![GpuInfo {
                name: "RTX 4090".into(),
                vram_gb: 24,
                compute_type: ComputeType::Cuda,
                estimated_tflops: 82.6,
            }],
            system_ram_gb: 64,
            cpu_cores: 16,
            total_storage_gb: 1000,
            free_storage_gb: 500,
            network_bandwidth_mbps: Some(1000),
        };
        let json = serde_json::to_string(&profile).unwrap();
        let back: HardwareProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.gpus.len(), 1);
        assert_eq!(back.gpus[0].vram_gb, 24);
    }

    #[test]
    fn available_resources_default() {
        let r = AvailableResources::default();
        assert!(r.available_for_mesh);
        assert_eq!(r.free_vram_gb, 0.0);
    }

    // ─── inference_availability backward-compat ───────────────

    fn minimal_capabilities_json(
        inference_availability: Option<f32>,
        inference_capable: Option<bool>,
    ) -> String {
        let availability_field = inference_availability
            .map(|v| format!(", \"inference_availability\": {v}"))
            .unwrap_or_default();
        let capable_field = inference_capable
            .map(|v| format!(", \"inference_capable\": {v}"))
            .unwrap_or_default();
        format!(
            r#"{{
                "hardware": {{
                    "gpus": [],
                    "system_ram_gb": 16,
                    "cpu_cores": 8,
                    "total_storage_gb": 500,
                    "free_storage_gb": 200,
                    "network_bandwidth_mbps": null
                }},
                "available": {{
                    "free_vram_gb": 0.0,
                    "free_ram_gb": 4.0,
                    "free_storage_gb": 200.0,
                    "gpu_utilization": 0.0,
                    "cpu_utilization": 0.1,
                    "available_for_mesh": true
                }},
                "active_processes": [],
                "hosted_corpora": [],
                "reported_at": 1000{availability_field}{capable_field}
            }}"#
        )
    }

    #[test]
    fn inference_availability_defaults_to_1_when_absent() {
        // Old peers (pre-feature) don't include this field. Must deserialize to 1.0.
        let json = minimal_capabilities_json(None, None);
        let caps: NodeCapabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(
            caps.inference_availability, 1.0,
            "old peers without inference_availability must default to 1.0 (fully available)"
        );
    }

    #[test]
    fn inference_availability_round_trips_hot_value() {
        let json = minimal_capabilities_json(Some(0.20), None);
        let caps: NodeCapabilities = serde_json::from_str(&json).unwrap();
        assert!(
            (caps.inference_availability - 0.20).abs() < 1e-6,
            "hot availability 0.20 must survive a JSON round-trip"
        );
        // Re-serialize and back.
        let re_json = serde_json::to_string(&caps).unwrap();
        let back: NodeCapabilities = serde_json::from_str(&re_json).unwrap();
        assert!((back.inference_availability - 0.20).abs() < 1e-6);
    }

    #[test]
    fn inference_capable_defaults_to_false_when_absent() {
        // Old peers without inference_capable must default to false (conservative exclusion).
        let json = minimal_capabilities_json(None, None);
        let caps: NodeCapabilities = serde_json::from_str(&json).unwrap();
        assert!(
            !caps.inference_capable,
            "old peers without inference_capable must default to false (excluded from routing)"
        );
        assert!(
            caps.loaded_models.is_empty(),
            "old peers without loaded_models must default to empty vec"
        );
    }

    #[test]
    fn embed_model_defaults_to_none_when_absent() {
        // Old peers (pre-field) don't include embed_model. Must
        // deserialize to None — the planner treats None as
        // "exclude from distribution", which is the conservative
        // answer for a peer that never advertised compatibility.
        let json = minimal_capabilities_json(None, None);
        let caps: NodeCapabilities = serde_json::from_str(&json).unwrap();
        assert!(
            caps.embed_model.is_none(),
            "old peers without embed_model must default to None"
        );
    }

    #[test]
    fn embed_model_round_trips() {
        use crate::oicp::{EmbedModelInfo, NormalizationStrategy, PoolingStrategy};
        let caps = NodeCapabilities {
            hardware: HardwareProfile {
                gpus: vec![],
                system_ram_gb: 16,
                cpu_cores: 8,
                total_storage_gb: 500,
                free_storage_gb: 200,
                network_bandwidth_mbps: None,
            },
            available: AvailableResources::default(),
            active_processes: vec![],
            hosted_corpora: vec![],
            reported_at: 1000,
            inference_availability: 1.0,
            inference_capable: false,
            loaded_models: vec![],
            embed_model: Some(EmbedModelInfo {
                model_id: "qwen3-embedding-0.6b".into(),
                dimensions: 1024,
                pooling: PoolingStrategy::Mean,
                normalization: NormalizationStrategy::Application,
            }),
        };
        let json = serde_json::to_string(&caps).unwrap();
        let back: NodeCapabilities = serde_json::from_str(&json).unwrap();
        let em = back.embed_model.expect("embed_model survives");
        assert_eq!(em.model_id, "qwen3-embedding-0.6b");
        assert_eq!(em.dimensions, 1024);
    }

    #[test]
    fn inference_capable_round_trips() {
        let json = minimal_capabilities_json(None, Some(true));
        let caps: NodeCapabilities = serde_json::from_str(&json).unwrap();
        assert!(caps.inference_capable, "inference_capable: true must survive JSON round-trip");
        let re_json = serde_json::to_string(&caps).unwrap();
        let back: NodeCapabilities = serde_json::from_str(&re_json).unwrap();
        assert!(back.inference_capable);
    }
}
