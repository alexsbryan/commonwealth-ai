use std::collections::HashMap;

use tracing::info;

use commonwealth_core::capabilities::NodeCapabilities;
use commonwealth_core::config::DaemonConfig;
use commonwealth_core::ids::NodeId;
use commonwealth_core::latency::LatencyMatrix;
use crate::model::ModelInfo;
use crate::inference_plan::{InferencePlan, ShardPlan};

use super::layer_assignment::{assign_layers, AssignmentError, EligibleNode};

/// Build a complete inference plan for a single model.
///
/// Takes the model info, node capabilities, latency matrix, and optional
/// preferred entry node (for privacy). Returns a `ShardPlan` if the model
/// can be hosted on the available nodes.
pub fn build_shard_plan(
    model: &ModelInfo,
    node_capabilities: &HashMap<NodeId, NodeCapabilities>,
    node_configs: &HashMap<NodeId, NodeConfig>,
    latency_matrix: &LatencyMatrix,
    preferred_entry: Option<NodeId>,
) -> Result<ShardPlan, AssignmentError> {
    // Build eligible node list: nodes that are online, available, and have
    // enough resources after reserves.
    let eligible_nodes: Vec<EligibleNode> = node_capabilities
        .iter()
        .filter_map(|(&node_id, caps)| {
            if !caps.available.available_for_mesh {
                return None;
            }

            let config = node_configs.get(&node_id);
            let reserve_vram = config.map(|c| c.reserve_vram_gb).unwrap_or(0) as f32;
            let available_vram = caps.available.free_vram_gb - reserve_vram;

            if available_vram <= 0.0 {
                return None;
            }

            // Pick the first GPU, or use default index 0.
            let gpu_index = 0;

            // RPC address: internal port on the node's first address.
            let internal_port = config.map(|c| c.internal_port).unwrap_or(9742);
            let rpc_address = format!("127.0.0.1:{}", internal_port + 100)
                .parse()
                .unwrap();

            Some(EligibleNode {
                node_id,
                available_vram_gb: available_vram,
                gpu_index,
                rpc_address,
            })
        })
        .collect();

    let result = assign_layers(
        model.total_layers,
        model.size_bytes,
        &eligible_nodes,
        latency_matrix,
        preferred_entry,
    )?;

    // Estimate performance.
    let (tps, ttft) = estimate_performance(model, &result.assignments.len());

    let plan = ShardPlan {
        model: model.id,
        entry_node: result.entry_node,
        assignments: result.assignments,
        estimated_tokens_per_sec: tps,
        estimated_ttft_ms: ttft,
    };

    info!(
        model = %model.name,
        entry_node = %plan.entry_node,
        num_nodes = plan.assignments.len(),
        tps = format!("{:.1}", tps),
        ttft_ms = ttft,
        "shard plan built"
    );

    Ok(plan)
}

/// Build a complete inference plan for multiple models (portfolio).
pub fn build_inference_plan(
    models: &[&ModelInfo],
    node_capabilities: &HashMap<NodeId, NodeCapabilities>,
    node_configs: &HashMap<NodeId, NodeConfig>,
    latency_matrix: &LatencyMatrix,
) -> InferencePlan {
    let mut model_plans = Vec::new();

    for model in models {
        match build_shard_plan(model, node_capabilities, node_configs, latency_matrix, None) {
            Ok(plan) => model_plans.push(plan),
            Err(e) => {
                tracing::warn!(model = %model.name, error = %e, "could not build shard plan");
            }
        }
    }

    InferencePlan { model_plans }
}

/// Simplified node config for plan building. Extracted from DaemonConfig.
#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub reserve_vram_gb: u32,
    pub reserve_ram_gb: u32,
    pub internal_port: u16,
}

impl From<&DaemonConfig> for NodeConfig {
    fn from(config: &DaemonConfig) -> Self {
        Self {
            reserve_vram_gb: config.contribution.reserve_vram_gb,
            reserve_ram_gb: config.contribution.reserve_ram_gb,
            internal_port: config.node.internal_port,
        }
    }
}

/// Rough performance estimation based on model and shard count.
fn estimate_performance(model: &ModelInfo, num_nodes: &usize) -> (f32, u32) {
    let num_nodes = *num_nodes as f32;

    // Base TPS estimate from model size.
    let base_tps = match model.total_layers {
        0..=32 => 60.0,
        33..=64 => 40.0,
        65..=96 => 25.0,
        _ => 15.0,
    };

    // Multi-node overhead: ~5% per additional node.
    let overhead = 1.0 - (num_nodes - 1.0) * 0.05;
    let tps = (base_tps * overhead.max(0.5)).max(1.0);

    // TTFT estimate: base latency + per-node communication overhead.
    let base_ttft = match model.total_layers {
        0..=32 => 500,
        33..=64 => 1000,
        65..=96 => 1500,
        _ => 2000,
    };
    let ttft = base_ttft + (num_nodes as u32 - 1) * 100;

    (tps, ttft)
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_core::capabilities::*;
    use commonwealth_core::ids::ModelId;
    use crate::model::*;
    use crate::oicp::CapabilityProfile;

    fn test_model(id: u128, layers: u32, size_gb: u64) -> ModelInfo {
        ModelInfo {
            id: ModelId::from_u128(id),
            name: format!("test-model-{id}"),
            repo: "test/model".into(),
            file: "model.gguf".into(),
            size_bytes: size_gb * 1_073_741_824,
            total_layers: layers,
            architecture: ModelArchitecture::Qwen,
            available_on: HashMap::new(),
            oicp_capabilities: CapabilityProfile::default(),
            quantization: "Q4_K_M".into(),
            min_memory_gb: 0,
            preferred_memory_gb: 0,
            supports_parallel_instances: false,
            supports_pipeline_shard: false,
        }
    }

    fn test_capabilities(vram: f32) -> NodeCapabilities {
        NodeCapabilities {
            hardware: HardwareProfile {
                gpus: vec![GpuInfo {
                    name: "Test GPU".into(),
                    vram_gb: vram as u32,
                    compute_type: ComputeType::Cuda,
                    estimated_tflops: 40.0,
                }],
                system_ram_gb: 64,
                cpu_cores: 16,
                total_storage_gb: 1000,
                free_storage_gb: 500,
                network_bandwidth_mbps: Some(1000),
            },
            available: AvailableResources {
                free_vram_gb: vram,
                free_ram_gb: 32.0,
                free_storage_gb: 500.0,
                gpu_utilization: 0.0,
                cpu_utilization: 0.2,
                available_for_mesh: true,
            },
            active_processes: vec![],
            hosted_corpora: vec![],
            reported_at: 0,
        }
    }

    fn test_node_config() -> NodeConfig {
        NodeConfig {
            reserve_vram_gb: 4,
            reserve_ram_gb: 8,
            internal_port: 9742,
        }
    }

    #[test]
    fn build_shard_plan_single_node() {
        let model = test_model(1, 64, 17);
        let mut caps = HashMap::new();
        caps.insert(NodeId::from_u128(1), test_capabilities(24.0));
        let mut configs = HashMap::new();
        configs.insert(NodeId::from_u128(1), test_node_config());
        let matrix = LatencyMatrix::new();

        let plan = build_shard_plan(&model, &caps, &configs, &matrix, None).unwrap();
        assert_eq!(plan.model, model.id);
        assert_eq!(plan.assignments.len(), 1);
        assert_eq!(plan.entry_node, NodeId::from_u128(1));
    }

    #[test]
    fn build_shard_plan_multi_node() {
        let model = test_model(1, 64, 17);
        let mut caps = HashMap::new();
        let mut configs = HashMap::new();
        for i in 1..=3u128 {
            caps.insert(NodeId::from_u128(i), test_capabilities(12.0));
            configs.insert(NodeId::from_u128(i), test_node_config());
        }
        let matrix = LatencyMatrix::new();

        let plan = build_shard_plan(&model, &caps, &configs, &matrix, None).unwrap();
        assert_eq!(plan.assignments.len(), 3);

        let total: u32 = plan.assignments.iter().map(|a| a.layers.count()).sum();
        assert_eq!(total, 64);
    }

    #[test]
    fn build_shard_plan_respects_reserves() {
        let model = test_model(1, 64, 17);
        let mut caps = HashMap::new();
        // Node has 5 GB VRAM, 4 GB reserve → only 1 GB usable.
        caps.insert(NodeId::from_u128(1), test_capabilities(5.0));
        let mut configs = HashMap::new();
        configs.insert(NodeId::from_u128(1), test_node_config());
        let matrix = LatencyMatrix::new();

        // 17 GB model / 64 layers ≈ 0.27 GB per layer. 1 GB usable → should work.
        let plan = build_shard_plan(&model, &caps, &configs, &matrix, None).unwrap();
        assert!(!plan.assignments.is_empty());
    }

    #[test]
    fn build_shard_plan_excludes_unavailable_nodes() {
        let model = test_model(1, 32, 8);
        let mut caps = HashMap::new();

        let mut available = test_capabilities(24.0);
        caps.insert(NodeId::from_u128(1), available.clone());

        available.available.available_for_mesh = false;
        caps.insert(NodeId::from_u128(2), available);

        let mut configs = HashMap::new();
        configs.insert(NodeId::from_u128(1), test_node_config());
        configs.insert(NodeId::from_u128(2), test_node_config());
        let matrix = LatencyMatrix::new();

        let plan = build_shard_plan(&model, &caps, &configs, &matrix, None).unwrap();
        // Only node 1 should be used.
        assert_eq!(plan.assignments.len(), 1);
        assert_eq!(plan.assignments[0].node_id, NodeId::from_u128(1));
    }

    #[test]
    fn build_inference_plan_multi_model() {
        let model_a = test_model(1, 32, 8);
        let model_b = test_model(2, 32, 8);
        let mut caps = HashMap::new();
        let mut configs = HashMap::new();

        // Enough VRAM for both models.
        for i in 1..=3u128 {
            caps.insert(NodeId::from_u128(i), test_capabilities(24.0));
            configs.insert(NodeId::from_u128(i), test_node_config());
        }
        let matrix = LatencyMatrix::new();

        let plan = build_inference_plan(&[&model_a, &model_b], &caps, &configs, &matrix);
        assert_eq!(plan.model_plans.len(), 2);
    }

    #[test]
    fn performance_estimation_reasonable() {
        let model = test_model(1, 64, 17);
        let (tps, ttft) = estimate_performance(&model, &3);
        assert!(tps > 0.0);
        assert!(ttft > 0);
        assert!(tps < 100.0, "TPS unreasonably high: {tps}");
        assert!(ttft < 10000, "TTFT unreasonably high: {ttft}");
    }
}
