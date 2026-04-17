use serde::{Deserialize, Serialize};

use crate::knowledge::CorpusShardInfo;

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

    fn minimal_capabilities_json(inference_availability: Option<f32>) -> String {
        let availability_field = inference_availability
            .map(|v| format!(", \"inference_availability\": {v}"))
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
                "reported_at": 1000{availability_field}
            }}"#
        )
    }

    #[test]
    fn inference_availability_defaults_to_1_when_absent() {
        // Old peers (pre-feature) don't include this field. Must deserialize to 1.0.
        let json = minimal_capabilities_json(None);
        let caps: NodeCapabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(
            caps.inference_availability, 1.0,
            "old peers without inference_availability must default to 1.0 (fully available)"
        );
    }

    #[test]
    fn inference_availability_round_trips_hot_value() {
        let json = minimal_capabilities_json(Some(0.20));
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
}
