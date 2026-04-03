use std::net::SocketAddr;

use commonwealth_core::capabilities::*;
use commonwealth_core::ids::{ModelId, NodeId};
use commonwealth_core::mesh::*;
use commonwealth_core::model::*;
use commonwealth_core::scheduler::InferencePlan;

use commonwealth_api::server::{client_router, internal_router};
use commonwealth_api::state::AppState;

/// A simulated node for integration testing.
/// Runs an in-process API server and holds mesh/model state.
pub struct SimulatedNode {
    pub node_id: NodeId,
    pub name: String,
    pub state: AppState,
    pub hardware: HardwareProfile,
    pub client_addr: Option<SocketAddr>,
    pub internal_addr: Option<SocketAddr>,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

/// Builder for configuring a simulated node.
pub struct SimulatedNodeBuilder {
    node_id: NodeId,
    name: String,
    gpus: Vec<GpuInfo>,
    ram_gb: u32,
    cpu_cores: u32,
    storage_gb: u32,
    free_storage_gb: u32,
}

impl SimulatedNodeBuilder {
    pub fn new(id: u128, name: &str) -> Self {
        Self {
            node_id: NodeId::from_u128(id),
            name: name.into(),
            gpus: vec![],
            ram_gb: 64,
            cpu_cores: 16,
            storage_gb: 1000,
            free_storage_gb: 500,
        }
    }

    pub fn gpu(mut self, name: &str, vram_gb: u32, compute_type: ComputeType) -> Self {
        self.gpus.push(GpuInfo {
            name: name.into(),
            vram_gb,
            compute_type,
            estimated_tflops: vram_gb as f32 * 2.0,
        });
        self
    }

    pub fn ram_gb(mut self, gb: u32) -> Self {
        self.ram_gb = gb;
        self
    }

    pub fn storage_gb(mut self, total: u32, free: u32) -> Self {
        self.storage_gb = total;
        self.free_storage_gb = free;
        self
    }

    pub fn build(self, mesh: &Mesh) -> SimulatedNode {
        let hardware = HardwareProfile {
            gpus: self.gpus,
            system_ram_gb: self.ram_gb,
            cpu_cores: self.cpu_cores,
            total_storage_gb: self.storage_gb,
            free_storage_gb: self.free_storage_gb,
            network_bandwidth_mbps: Some(1000),
        };

        let state = AppState::new(self.node_id, mesh.clone());

        SimulatedNode {
            node_id: self.node_id,
            name: self.name,
            state,
            hardware,
            client_addr: None,
            internal_addr: None,
            shutdown_tx: None,
        }
    }

    /// Build and register this node as a member in the given mesh.
    pub fn build_and_register(self, mesh: &mut Mesh) -> SimulatedNode {
        let node_id = self.node_id;
        let name = self.name.clone();

        let hardware = HardwareProfile {
            gpus: self.gpus,
            system_ram_gb: self.ram_gb,
            cpu_cores: self.cpu_cores,
            total_storage_gb: self.storage_gb,
            free_storage_gb: self.free_storage_gb,
            network_bandwidth_mbps: Some(1000),
        };

        let total_vram: f32 = hardware.gpus.iter().map(|g| g.vram_gb as f32).sum();

        let caps = NodeCapabilities {
            hardware: hardware.clone(),
            available: AvailableResources {
                free_vram_gb: total_vram,
                free_ram_gb: self.ram_gb as f32,
                free_storage_gb: self.free_storage_gb as f32,
                gpu_utilization: 0.0,
                cpu_utilization: 0.1,
                available_for_mesh: true,
            },
            active_processes: vec![],
            hosted_corpora: vec![],
            reported_at: 0,
        };

        let member = MemberRecord {
            node_id,
            name: name.clone(),
            invited_by: node_id,
            joined_at: 0,
            last_seen: 0,
            status: NodeStatus::Online,
            capabilities: caps,
            addresses: vec![],
        };
        mesh.members.insert(node_id, member);

        let state = AppState::new(node_id, mesh.clone());

        SimulatedNode {
            node_id,
            name,
            state,
            hardware,
            client_addr: None,
            internal_addr: None,
            shutdown_tx: None,
        }
    }
}

impl SimulatedNode {
    /// Start the node's API servers on random ports.
    /// Returns the client and internal addresses.
    pub async fn start_servers(&mut self) -> (SocketAddr, SocketAddr) {
        let client_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let internal_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();

        let client_addr = client_listener.local_addr().unwrap();
        let internal_addr = internal_listener.local_addr().unwrap();

        self.client_addr = Some(client_addr);
        self.internal_addr = Some(internal_addr);

        let client_app = client_router(self.state.clone());
        let internal_app = internal_router(self.state.clone());

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        self.shutdown_tx = Some(shutdown_tx);

        tokio::spawn(async move {
            tokio::select! {
                _ = axum::serve(client_listener, client_app) => {}
                _ = axum::serve(internal_listener, internal_app) => {}
                _ = shutdown_rx => {}
            }
        });

        (client_addr, internal_addr)
    }

    /// Register a model on this node.
    pub async fn register_model(&self, model: ModelInfo) {
        self.state.register_model(model).await;
    }

    /// Set the inference plan for this node.
    pub async fn set_inference_plan(&self, plan: InferencePlan) {
        *self.state.inner.inference_plan.write().await = plan;
    }

    /// Set the llama-server address for a model.
    pub async fn set_llama_server_address(&self, model_id: ModelId, address: String) {
        self.state.set_llama_server_address(model_id, address).await;
    }

    /// Set the knowledge shard plan for this node.
    pub async fn set_knowledge_plan(&self, plan: commonwealth_core::knowledge::KnowledgeShardPlan) {
        *self.state.inner.knowledge_plan.write().await = plan;
    }

    /// Shutdown the node's servers.
    pub fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }

    /// Get the node's capabilities as they'd appear in gossip.
    pub fn capabilities(&self) -> NodeCapabilities {
        let total_vram: f32 = self.hardware.gpus.iter().map(|g| g.vram_gb as f32).sum();
        NodeCapabilities {
            hardware: self.hardware.clone(),
            available: AvailableResources {
                free_vram_gb: total_vram,
                free_ram_gb: self.hardware.system_ram_gb as f32,
                free_storage_gb: self.hardware.free_storage_gb as f32,
                gpu_utilization: 0.0,
                cpu_utilization: 0.1,
                available_for_mesh: true,
            },
            active_processes: vec![],
            hosted_corpora: vec![],
            reported_at: 0,
        }
    }
}

impl Drop for SimulatedNode {
    fn drop(&mut self) {
        self.shutdown();
    }
}
