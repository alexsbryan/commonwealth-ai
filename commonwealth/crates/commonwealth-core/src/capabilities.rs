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
}
