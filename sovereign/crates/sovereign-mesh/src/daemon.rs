//! Embedded Commonwealth daemon lifecycle management.
//!
//! The daemon runs in-process within Sovereign — no separate binary needed.
//! It starts when the user creates or joins a mesh, and stops when they leave.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{info, warn};

use commonwealth_api::state::AppState;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::Mesh;
use commonwealth_discovery::membership;

use crate::deep_link::DeepLink;
use crate::state::MeshState;

/// The embedded Commonwealth daemon, managed by Sovereign's UI.
pub struct EmbeddedDaemon {
    state: Arc<RwLock<DaemonState>>,
}

enum DaemonState {
    Stopped,
    Running {
        #[allow(dead_code)]
        app_state: AppState,
        mesh_state: Arc<RwLock<MeshState>>,
        client_addr: SocketAddr,
        _shutdown_tx: tokio::sync::oneshot::Sender<()>,
    },
}

/// Result of creating a new mesh.
pub struct CreateMeshResult {
    pub mesh_name: String,
    pub join_key: String,
    pub join_link: String,
}

/// Result of joining an existing mesh.
pub struct JoinMeshResult {
    pub mesh_name: String,
    pub node_id: String,
}

impl EmbeddedDaemon {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(DaemonState::Stopped)),
        }
    }

    /// Whether the daemon is currently running.
    pub async fn is_running(&self) -> bool {
        matches!(*self.state.read().await, DaemonState::Running { .. })
    }

    /// Create a new mesh and start the daemon.
    pub async fn create_mesh(
        &self,
        mesh_name: &str,
        node_name: &str,
    ) -> Result<CreateMeshResult, MeshError> {
        if self.is_running().await {
            return Err(MeshError::AlreadyRunning);
        }

        let internal_port = 9742u16;
        let addr: SocketAddr = format!("0.0.0.0:{internal_port}")
            .parse()
            .map_err(|e| MeshError::Config(format!("bad address: {e}")))?;

        let (mesh, join_key) = membership::init_mesh(mesh_name, node_name, vec![addr]);
        let node_id = mesh
            .members
            .keys()
            .next()
            .copied()
            .ok_or_else(|| MeshError::Config("no node in mesh".into()))?;

        let join_link = crate::deep_link::build_join_link(
            &join_key,
            None, // relay_hint — local network for now
            Some(mesh_name),
        );

        self.start_daemon(mesh, node_id).await?;

        info!(mesh_name, "mesh created, daemon started");

        Ok(CreateMeshResult {
            mesh_name: mesh_name.to_string(),
            join_key,
            join_link,
        })
    }

    /// Join an existing mesh from a deep link and start the daemon.
    pub async fn join_mesh(
        &self,
        link: &DeepLink,
        node_name: &str,
    ) -> Result<JoinMeshResult, MeshError> {
        if self.is_running().await {
            return Err(MeshError::AlreadyRunning);
        }

        let (join_key, mesh_name) = match link {
            DeepLink::Join {
                join_key,
                mesh_name,
                ..
            } => (join_key.clone(), mesh_name.clone()),
        };

        // Validate the join key format.
        membership::validate_join_key_format(&join_key)
            .map_err(|e| MeshError::InvalidJoinKey(e.to_string()))?;

        // In a full implementation, this would contact a mesh member to
        // complete the handshake. For now, create a placeholder mesh entry
        // that will be populated when gossip syncs.
        let internal_port = 9742u16;
        let addr: SocketAddr = format!("0.0.0.0:{internal_port}")
            .parse()
            .map_err(|e| MeshError::Config(format!("bad address: {e}")))?;

        let (mesh, _) = membership::init_mesh(
            &mesh_name.clone().unwrap_or_else(|| "Joined Mesh".to_string()),
            node_name,
            vec![addr],
        );

        let node_id = mesh.members.keys().next().copied().unwrap();

        self.start_daemon(mesh, node_id).await?;

        info!(mesh_name = mesh_name.as_deref().unwrap_or("unknown"), "joined mesh, daemon started");

        Ok(JoinMeshResult {
            mesh_name: mesh_name.unwrap_or_else(|| "Joined Mesh".to_string()),
            node_id: node_id.to_string(),
        })
    }

    /// Stop the daemon and leave the mesh.
    pub async fn stop(&self) -> Result<(), MeshError> {
        let mut state = self.state.write().await;
        match std::mem::replace(&mut *state, DaemonState::Stopped) {
            DaemonState::Running { _shutdown_tx, .. } => {
                // Dropping the sender signals the daemon to shut down.
                drop(_shutdown_tx);
                info!("mesh daemon stopped");
                Ok(())
            }
            DaemonState::Stopped => Err(MeshError::NotRunning),
        }
    }

    /// Get the current mesh state for UI display.
    pub async fn mesh_state(&self) -> Option<MeshState> {
        let state = self.state.read().await;
        match &*state {
            DaemonState::Running { mesh_state, .. } => {
                Some(mesh_state.read().await.clone())
            }
            DaemonState::Stopped => None,
        }
    }

    /// Get the Commonwealth API address (for internal use).
    pub async fn api_address(&self) -> Option<SocketAddr> {
        let state = self.state.read().await;
        match &*state {
            DaemonState::Running { client_addr, .. } => Some(*client_addr),
            DaemonState::Stopped => None,
        }
    }

    // ── Private ─────────────────────────────────────────

    async fn start_daemon(
        &self,
        mesh: Mesh,
        node_id: NodeId,
    ) -> Result<(), MeshError> {
        let app_state = AppState::new(node_id, mesh);

        let client_addr: SocketAddr = "127.0.0.1:9741".parse().unwrap();
        let internal_addr: SocketAddr = "127.0.0.1:9742".parse().unwrap();

        let mesh_state = Arc::new(RwLock::new(MeshState::from_app_state(&app_state).await));

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        // Spawn the API servers in the background.
        let app_state_clone = app_state.clone();
        tokio::spawn(async move {
            let client_router = commonwealth_api::server::client_router(app_state_clone.clone());
            let internal_router =
                commonwealth_api::server::internal_router(app_state_clone);

            let client_listener = match tokio::net::TcpListener::bind(client_addr).await {
                Ok(l) => l,
                Err(e) => {
                    warn!("Failed to bind client API on {client_addr}: {e}");
                    return;
                }
            };
            let internal_listener = match tokio::net::TcpListener::bind(internal_addr).await {
                Ok(l) => l,
                Err(e) => {
                    warn!("Failed to bind internal API on {internal_addr}: {e}");
                    return;
                }
            };

            info!("Commonwealth daemon started (client: {client_addr}, internal: {internal_addr})");

            tokio::select! {
                _ = axum::serve(client_listener, client_router) => {}
                _ = axum::serve(internal_listener, internal_router) => {}
                _ = shutdown_rx => {
                    info!("Commonwealth daemon shutting down");
                }
            }
        });

        let mut state = self.state.write().await;
        *state = DaemonState::Running {
            app_state,
            mesh_state,
            client_addr,
            _shutdown_tx: shutdown_tx,
        };

        Ok(())
    }
}

impl Default for EmbeddedDaemon {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MeshError {
    #[error("Mesh daemon is already running")]
    AlreadyRunning,

    #[error("Mesh daemon is not running")]
    NotRunning,

    #[error("Invalid join key: {0}")]
    InvalidJoinKey(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Network error: {0}")]
    Network(String),
}
