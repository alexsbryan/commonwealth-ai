// SPDX-License-Identifier: AGPL-3.0-or-later
use std::net::SocketAddr;

use axum::Router;
use commonwealth_core::capabilities::*;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::*;

/// A simulated node for integration testing.
///
/// Holds the caller's node state as a type parameter, because this crate meets
/// the runtime at the OICP/contracts seam: it names no host, and the caller
/// binds the concrete node (its `AppState`, its routers) when it builds the
/// node. `start_servers` takes the two routers rather than mounting them
/// itself, for the same reason — the host that builds them is the caller's.
pub struct SimulatedNode<S> {
    pub node_id: NodeId,
    pub name: String,
    pub state: S,
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

    fn hardware(&self) -> HardwareProfile {
        HardwareProfile {
            gpus: self.gpus.clone(),
            system_ram_gb: self.ram_gb,
            cpu_cores: self.cpu_cores,
            total_storage_gb: self.storage_gb,
            free_storage_gb: self.free_storage_gb,
            network_bandwidth_mbps: Some(1000),
        }
    }

    /// Build the node, creating its state with `make_state`.
    pub fn build<S>(
        self,
        mesh: &Mesh,
        make_state: impl FnOnce(NodeId, Mesh) -> S,
    ) -> SimulatedNode<S> {
        let hardware = self.hardware();
        let state = make_state(self.node_id, mesh.clone());

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
    pub fn build_and_register<S>(
        self,
        mesh: &mut Mesh,
        make_state: impl FnOnce(NodeId, Mesh) -> S,
    ) -> SimulatedNode<S> {
        let node_id = self.node_id;
        let name = self.name.clone();

        let hardware = self.hardware();

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
            inference_availability: 1.0,
            inference_capable: true,
            loaded_models: vec![],
            origins: Vec::new(),
            media_allow: Vec::new(),

            embed_model: None,
            benchmark: None,
            current_in_flight: None,
            anchor: None,
        };

        let member = MemberRecord {
            removed_at: None,
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
            dial_info_version: 0,
            dial_info_sig: None,
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

        let state = make_state(node_id, mesh.clone());

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

impl<S> SimulatedNode<S> {
    /// Start the node's API servers on random ports. Returns the client and
    /// internal addresses.
    ///
    /// The caller supplies the two routers — this crate does not name the host
    /// that builds them. The client router carries the `client_auth`
    /// ConnectInfo layer, so it is served with the connect-info factory or
    /// every request 500s (matches the production listener in
    /// `server::serve`).
    pub async fn start_servers(
        &mut self,
        client_app: Router,
        internal_app: Router,
    ) -> (SocketAddr, SocketAddr) {
        let client_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let internal_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();

        let client_addr = client_listener.local_addr().unwrap();
        let internal_addr = internal_listener.local_addr().unwrap();

        self.client_addr = Some(client_addr);
        self.internal_addr = Some(internal_addr);

        let client_app = client_app.into_make_service_with_connect_info::<SocketAddr>();

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
            inference_availability: 1.0,
            inference_capable: true,
            loaded_models: vec![],
            origins: Vec::new(),
            media_allow: Vec::new(),

            embed_model: None,
            benchmark: None,
            current_in_flight: None,
            anchor: None,
        }
    }
}

impl<S> Drop for SimulatedNode<S> {
    fn drop(&mut self) {
        self.shutdown();
    }
}
