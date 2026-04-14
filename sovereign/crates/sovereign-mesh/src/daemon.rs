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
use commonwealth_discovery::mdns::{BrowseHandle, DiscoveredPeer, MdnsDiscovery};
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
        /// Live mDNS advertiser + discovery — kept to drive
        /// `discovered_peers()` and (in Phase B) the join handshake.
        mdns: Arc<MdnsDiscovery>,
        /// Dropping this handle stops the background browse task.
        /// Underscore-prefixed because it's held purely for its Drop
        /// impl.
        _browse_handle: BrowseHandle,
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
    ///
    /// Flow:
    ///   1. Validate the join-key format.
    ///   2. Start the daemon with a *placeholder* mesh so mDNS
    ///      advertises us and the browse task populates the peers
    ///      table. Chicken-and-egg: mDNS needs a `mesh_id` to
    ///      advertise a service, but we don't know the founder's
    ///      mesh_id until the handshake completes.
    ///   3. Call `perform_join` — scans mDNS for peers whose TXT
    ///      `name` matches the URL, POSTs `/internal/join` with
    ///      the raw key to each, returns the founder's authoritative
    ///      mesh on the first 200.
    ///   4. Swap the placeholder mesh in `AppState` for the adopted
    ///      one. Gossip takes over from here.
    ///
    /// The mDNS TXT record keeps advertising the placeholder
    /// `mesh_id` until the next daemon restart — cosmetic: peers
    /// match on `name`, not mesh_id, so nothing breaks.
    pub async fn join_mesh(
        &self,
        link: &DeepLink,
        node_name: &str,
    ) -> Result<JoinMeshResult, MeshError> {
        if self.is_running().await {
            return Err(MeshError::AlreadyRunning);
        }

        let (join_key, url_mesh_name) = match link {
            DeepLink::Join {
                join_key,
                mesh_name,
                ..
            } => (join_key.clone(), mesh_name.clone()),
        };
        let mesh_name = url_mesh_name
            .clone()
            .unwrap_or_else(|| "Joined Mesh".to_string());

        membership::validate_join_key_format(&join_key)
            .map_err(|e| MeshError::InvalidJoinKey(e.to_string()))?;

        let internal_port = 9742u16;
        let addr: SocketAddr = format!("0.0.0.0:{internal_port}")
            .parse()
            .map_err(|e| MeshError::Config(format!("bad address: {e}")))?;

        // Step 2 — placeholder mesh so mDNS has something to advertise.
        let (placeholder_mesh, _throwaway_key) =
            membership::init_mesh(&mesh_name, node_name, vec![addr]);
        let placeholder_node_id = placeholder_mesh
            .members
            .keys()
            .next()
            .copied()
            .ok_or_else(|| MeshError::Config("placeholder mesh has no node".into()))?;

        self.start_daemon(placeholder_mesh, placeholder_node_id).await?;

        // Step 3 — handshake. Clone the Arc<MdnsDiscovery> so we don't
        // hold the DaemonState lock for the ~5s the handshake may take.
        let mdns = {
            let state = self.state.read().await;
            match &*state {
                DaemonState::Running { mdns, .. } => Arc::clone(mdns),
                DaemonState::Stopped => unreachable!("just started above"),
            }
        };

        let handshake = crate::join::perform_join(
            &mesh_name,
            &join_key,
            node_name,
            vec![addr],
            mdns.as_ref(),
            std::time::Duration::from_secs(5),
        )
        .await;

        let handshake = match handshake {
            Ok(h) => h,
            Err(e) => {
                // Tear down the placeholder daemon so the next attempt
                // from the UI doesn't hit AlreadyRunning.
                let _ = self.stop().await;
                return Err(MeshError::Network(e.to_string()));
            }
        };

        // Step 4 — adopt the founder's authoritative mesh.
        let adopted_node_id = handshake.assigned_node_id;
        {
            let state = self.state.read().await;
            if let DaemonState::Running {
                app_state,
                mesh_state,
                ..
            } = &*state
            {
                *app_state.inner.mesh.write().await = handshake.mesh;
                *mesh_state.write().await =
                    MeshState::from_app_state(app_state).await;
            }
        }

        info!(mesh_name, node_id = %adopted_node_id, "joined mesh, daemon started");

        Ok(JoinMeshResult {
            mesh_name,
            node_id: adopted_node_id.to_string(),
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

    /// Snapshot of peers discovered via mDNS on the local network.
    /// Empty when the daemon is stopped or no peers have advertised
    /// on `_commonwealth._tcp.local.` yet.
    pub async fn discovered_peers(&self) -> Vec<DiscoveredPeer> {
        let state = self.state.read().await;
        match &*state {
            DaemonState::Running { mdns, .. } => mdns.discovered_peers(),
            DaemonState::Stopped => Vec::new(),
        }
    }

    // ── Private ─────────────────────────────────────────

    async fn start_daemon(
        &self,
        mesh: Mesh,
        node_id: NodeId,
    ) -> Result<(), MeshError> {
        // mesh_id as hex — broadcast in mDNS TXT records so peers on
        // the LAN can tell which mesh this node belongs to. Public by
        // design (knowing the mesh_id isn't sufficient to join;
        // accessing members still requires the join_key).
        let mesh_id_hex = hex::encode(mesh.id.as_bytes());
        let node_name = mesh
            .members
            .get(&node_id)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| node_id.to_string());

        let app_state = AppState::new(node_id, mesh);

        // Client API stays on localhost — only the in-process Tauri
        // commands call it. Internal API binds to 0.0.0.0 so peers on
        // the LAN can reach it for the join handshake + gossip.
        // Previously was 127.0.0.1 which made cross-machine join
        // physically impossible regardless of other logic.
        let client_addr: SocketAddr = "127.0.0.1:9741".parse().unwrap();
        let internal_addr: SocketAddr = "0.0.0.0:9742".parse().unwrap();

        let mesh_state = Arc::new(RwLock::new(MeshState::from_app_state(&app_state).await));

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        // Register on mDNS and start browsing. Both are load-bearing:
        // advertise lets remote peers find us; browse populates the
        // discovered-peers table that `perform_join` (Phase B) uses
        // to locate handshake targets.
        let mdns = MdnsDiscovery::new(node_id, &mesh_id_hex, &node_name, 9742)
            .map_err(|e| MeshError::Network(format!("mDNS register failed: {e}")))?;
        let mdns = Arc::new(mdns);
        // A 32-slot channel is plenty — the browse loop pushes on
        // ServiceResolved and we don't actively consume. If the buffer
        // fills (many peers on a busy LAN), the background task drops
        // extras; the discovered-peers hash map is still authoritative.
        let (peer_tx, _peer_rx) = tokio::sync::mpsc::channel::<DiscoveredPeer>(32);
        let browse_handle = mdns
            .browse(peer_tx)
            .map_err(|e| MeshError::Network(format!("mDNS browse failed: {e}")))?;

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
            mdns,
            _browse_handle: browse_handle,
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
