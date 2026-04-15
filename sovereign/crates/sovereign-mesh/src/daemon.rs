//! Embedded Commonwealth daemon lifecycle management.
//!
//! The daemon runs in-process within Sovereign — no separate binary needed.
//! It starts when the user creates or joins a mesh, and stops when they leave.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{info, warn};

use commonwealth_api::state::AppState;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::Mesh;
use commonwealth_discovery::mdns::{BrowseHandle, DiscoveredPeer, MdnsDiscovery};
use commonwealth_discovery::membership;

use crate::deep_link::DeepLink;
use crate::gossip::{self, GossipHandle};
use crate::persist;
use crate::state::MeshState;

/// The embedded Commonwealth daemon, managed by Sovereign's UI.
pub struct EmbeddedDaemon {
    state: Arc<RwLock<DaemonState>>,
    /// Where to persist `mesh.json` so the daemon can auto-resume on
    /// app restart. Set once at construction.
    data_dir: PathBuf,
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
        /// Aborts the gossip heartbeat loop on Drop. Same pattern
        /// as `_browse_handle` — tying the task's lifetime to the
        /// Running variant means stopping the daemon also stops
        /// gossip; no explicit teardown.
        _gossip_handle: GossipHandle,
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
    /// Construct a daemon that persists its running-mesh state to
    /// `data_dir/mesh.json`. Call [`try_resume`](Self::try_resume)
    /// once at app start to re-attach to a previously-created mesh.
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            state: Arc::new(RwLock::new(DaemonState::Stopped)),
            data_dir,
        }
    }

    /// Legacy constructor that doesn't persist — use only in tests
    /// where a tempdir isn't worth setting up. Production code must
    /// prefer `new(data_dir)`.
    pub fn new_in_memory() -> Self {
        Self {
            state: Arc::new(RwLock::new(DaemonState::Stopped)),
            data_dir: PathBuf::new(),
        }
    }

    fn persistence_enabled(&self) -> bool {
        !self.data_dir.as_os_str().is_empty()
    }

    /// If a mesh has been persisted from a previous session, start
    /// the daemon with that mesh so mDNS advertises immediately and
    /// existing members can reconnect without the user recreating.
    /// No-op if no persisted file exists or if persistence is
    /// disabled (the `new_in_memory` constructor).
    pub async fn try_resume(&self) -> Result<bool, MeshError> {
        if !self.persistence_enabled() {
            return Ok(false);
        }
        if self.is_running().await {
            return Ok(false);
        }
        let loaded = match persist::load(&self.data_dir) {
            Ok(Some(p)) => p,
            Ok(None) => return Ok(false),
            Err(e) => {
                warn!(
                    error = %e,
                    "mesh.json failed to load — ignoring, starting clean"
                );
                return Ok(false);
            }
        };
        let (mesh, self_node_id) = loaded.into_live();
        let mesh_name = mesh.name.clone();
        self.start_daemon(mesh, self_node_id).await?;
        info!(mesh_name, "resumed mesh from persisted state");
        // A resumed mesh may have peers cached from a prior session.
        // Kick off an immediate gossip sweep so their `last_seen`
        // gets refreshed (or decayed) within ~2s of the app opening,
        // rather than showing the user a stale roster for the first
        // DEFAULT_GOSSIP_INTERVAL.
        self.trigger_initial_sync().await;
        Ok(true)
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

        // Persist *after* start_daemon succeeds so we never leave a
        // mesh.json that points at a daemon that never bound.
        if self.persistence_enabled() {
            if let DaemonState::Running { app_state, .. } = &*self.state.read().await {
                let live = app_state.inner.mesh.read().await.clone();
                if let Err(e) = persist::save(&self.data_dir, &live, node_id) {
                    warn!(error = %e, "mesh.json write failed — mesh is in-memory only");
                }
            }
        }

        info!(mesh_name, "mesh created, daemon started");
        // On create there are no peers yet, but fire initial_sync
        // anyway — it touches our own last_seen to "now" so the
        // very first gossip exchange we later receive has a fresh
        // self record to merge against.
        self.trigger_initial_sync().await;

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

        let (join_key, url_mesh_name, relay_hint) = match link {
            DeepLink::Join {
                join_key,
                mesh_name,
                relay_hint,
            } => (join_key.clone(), mesh_name.clone(), relay_hint.clone()),
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
            relay_hint.as_deref(),
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

        // Persist the adopted mesh so the next app start resumes
        // automatically. Without this, joiners would have to paste
        // the link again every launch.
        if self.persistence_enabled() {
            if let DaemonState::Running { app_state, .. } = &*self.state.read().await {
                let live = app_state.inner.mesh.read().await.clone();
                if let Err(e) = persist::save(&self.data_dir, &live, adopted_node_id) {
                    warn!(error = %e, "mesh.json write failed — joined mesh is in-memory only");
                }
            }
        }

        info!(mesh_name, node_id = %adopted_node_id, "joined mesh, daemon started");
        // Fire a gossip round immediately so the founder (and any
        // other existing members in the adopted snapshot) learn
        // about us right away — the handshake registered us on
        // the founder, but other peers still need to find out.
        self.trigger_initial_sync().await;

        Ok(JoinMeshResult {
            mesh_name,
            node_id: adopted_node_id.to_string(),
        })
    }

    /// Leave the mesh: stop the daemon AND delete the persisted
    /// state so the next app start doesn't auto-resume. This is
    /// what the UI's "Leave" button calls.
    pub async fn stop(&self) -> Result<(), MeshError> {
        let mut state = self.state.write().await;
        match std::mem::replace(&mut *state, DaemonState::Stopped) {
            DaemonState::Running { _shutdown_tx, .. } => {
                // Dropping the sender signals the daemon to shut down.
                drop(_shutdown_tx);
                // Drop the write guard before touching the filesystem
                // — persistence shouldn't gate the in-memory stop.
                drop(state);
                if self.persistence_enabled() {
                    if let Err(e) = persist::clear(&self.data_dir) {
                        warn!(
                            error = %e,
                            "mesh.json could not be deleted on leave; \
                             it may auto-resume on next launch"
                        );
                    }
                }
                info!("mesh daemon stopped");
                Ok(())
            }
            DaemonState::Stopped => Err(MeshError::NotRunning),
        }
    }

    /// Get the current mesh state for UI display.
    ///
    /// Rebuilds the snapshot from the live `AppState` on every call
    /// rather than returning a cached value. The `/internal/join`
    /// handler on the founder side mutates `app_state.inner.mesh`
    /// directly — if this returned a stale snapshot (the original
    /// implementation did) the UI's poll never saw new members land
    /// until the daemon restarted, which looked exactly like the
    /// handshake silently failing. Rebuilding is cheap (a walk over
    /// `mesh.members` + derived aggregations) relative to the poll
    /// cadence (5s from MeshSettings, 3s from diagnostics).
    pub async fn mesh_state(&self) -> Option<MeshState> {
        let state = self.state.read().await;
        match &*state {
            DaemonState::Running { app_state, mesh_state, .. } => {
                let fresh = MeshState::from_app_state(app_state).await;
                // Observable signal for "the UI just polled and got a
                // fresh snapshot" — lets the user verify both that the
                // poll is live AND that any new members are visible on
                // this side. Info-level so it shows under the default
                // `sovereign_mesh=info` filter without RUST_LOG
                // overrides. If this log spam ever becomes annoying,
                // gate it on a changed-count predicate.
                tracing::info!(
                    members = fresh.status.members_total,
                    online = fresh.status.members_online,
                    "mesh_state: rebuilt snapshot from live AppState"
                );
                // Keep the cached snapshot in sync too, so anything
                // still reading it directly stays current.
                *mesh_state.write().await = fresh.clone();
                Some(fresh)
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
        let mesh_name = mesh.name.clone();
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
        let mdns = MdnsDiscovery::new(
            node_id,
            &mesh_id_hex,
            &mesh_name,
            &node_name,
            9742,
        )
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

            // Enumerate local non-loopback IPs and log them so the
            // founder can copy one into a `?relay=<IP>` query param
            // if mDNS doesn't reach the joiner (e.g. WiFi AP
            // isolation, router multicast filtering, different
            // subnets). Matches the exact workaround documented in
            // the Tailscale section of the crate README.
            for iface in local_ip_candidates() {
                info!(
                    ip = %iface,
                    "mesh: reachable at this address — share as \
                     `?relay={iface}:9742` if mDNS fails"
                );
            }

            tokio::select! {
                _ = axum::serve(client_listener, client_router) => {}
                _ = axum::serve(internal_listener, internal_router) => {}
                _ = shutdown_rx => {
                    info!("Commonwealth daemon shutting down");
                }
            }
        });

        // Spawn the gossip heartbeat task. It uses `app_state` via a
        // clone (cheap Arc bump) so it stays live independently of
        // the Running variant's ownership. Aborted on daemon stop by
        // the `_gossip_handle: Drop` → `JoinHandle::abort()`.
        //
        // Log at spawn site (synchronous to `start_daemon`) — the
        // matching "gossip: loop started" info inside the task fires
        // when the runtime first polls the future, which can be
        // later. Seeing "spawning gossip loop" but NOT "loop
        // started" means the task is queued but starved; seeing
        // NEITHER means the binary predates this code and a rebuild
        // is required.
        info!("spawning gossip loop");
        let gossip_handle = gossip::spawn_gossip_loop(
            app_state.clone(),
            gossip::DEFAULT_GOSSIP_INTERVAL,
            gossip::DEFAULT_OFFLINE_THRESHOLD,
        );

        let mut state = self.state.write().await;
        *state = DaemonState::Running {
            app_state,
            mesh_state,
            client_addr,
            mdns,
            _browse_handle: browse_handle,
            _gossip_handle: gossip_handle,
            _shutdown_tx: shutdown_tx,
        };

        Ok(())
    }

    /// Fire a bounded initial gossip round so a freshly-resumed or
    /// freshly-joined daemon reconciles with peers within ~2s
    /// instead of waiting a full `DEFAULT_GOSSIP_INTERVAL`. Callers
    /// invoke this after each of `create_mesh` / `join_mesh` /
    /// `try_resume` returns.
    async fn trigger_initial_sync(&self) {
        let state = self.state.read().await;
        if let DaemonState::Running { app_state, .. } = &*state {
            gossip::initial_sync(
                app_state,
                gossip::DEFAULT_OFFLINE_THRESHOLD,
                std::time::Duration::from_secs(2),
            )
            .await;
        }
    }
}

/// Best-effort list of the host's externally-reachable IPs, so the
/// founder can copy one into `?relay=<ip>:9742` when mDNS is blocked
/// (WiFi AP isolation, multicast filtering, cross-subnet LANs).
///
/// Uses the portable "UDP-connect to a public IP without sending"
/// trick: kernel updates `local_addr` on the socket to reflect the
/// preferred outbound source address. No packets are actually sent.
/// Returns the IPv4 default-route source and, if dual-stack, the
/// IPv6 one. Skips loopback. Not exhaustive (won't enumerate VPN
/// interfaces that aren't the default route) but covers the common
/// home-WiFi and Tailscale cases.
fn local_ip_candidates() -> Vec<std::net::IpAddr> {
    let mut ips = Vec::new();
    if let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if sock.connect("1.1.1.1:80").is_ok() {
            if let Ok(addr) = sock.local_addr() {
                if !addr.ip().is_loopback() {
                    ips.push(addr.ip());
                }
            }
        }
    }
    if let Ok(sock) = std::net::UdpSocket::bind("[::]:0") {
        if sock.connect("[2606:4700:4700::1111]:80").is_ok() {
            if let Ok(addr) = sock.local_addr() {
                let ip = addr.ip();
                if !ip.is_loopback() && !ips.contains(&ip) {
                    ips.push(ip);
                }
            }
        }
    }
    ips
}

impl Default for EmbeddedDaemon {
    /// In-memory default — useful for tests and quick scripts, but
    /// never used from the desktop app which calls
    /// `EmbeddedDaemon::new(data_dir)` to get persistence.
    fn default() -> Self {
        Self::new_in_memory()
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
