//! Embedded Commonwealth daemon lifecycle management.
//!
//! The daemon runs in-process within Sovereign — no separate binary needed.
//! It starts when the user creates or joins a mesh, and stops when they leave.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{info, warn};

use commonwealth_api::state::{AppState, LocalInferenceService};
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::Mesh;
use commonwealth_discovery::mdns::{BrowseHandle, DiscoveredPeer, MdnsDiscovery};
use commonwealth_discovery::membership;
use corpus_engine::CorpusEngine;
use sovereign_core::traits::InferenceProvider;

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
    /// The CorpusEngine this daemon consults when peers gossip-query
    /// our knowledge over `/internal/knowledge/search`, and when we
    /// publish our own `hosted_corpora` on gossip rounds.
    ///
    /// Held in an RwLock<Option<_>> because Sovereign's bootstrap
    /// constructs the daemon *before* it builds the engine (the
    /// engine needs an `EmbedFn` that isn't ready until the fast
    /// model has loaded). The desktop calls `set_corpus_engine`
    /// during bootstrap just before `try_resume`, so by the time
    /// the daemon is Running the engine is always present. Tests
    /// and the CLI's mesh subcommands keep it `None`.
    corpus_engine: RwLock<Option<Arc<CorpusEngine>>>,
    /// The Sovereign `InferenceProvider` that answers peer chat
    /// completions hitting our `/v1/chat/completions`. Same
    /// injection timing as `corpus_engine`: set during desktop
    /// bootstrap before the daemon is started. When this is
    /// absent, the daemon's handler falls through to the
    /// scheduler/llama-server path (which is empty in the
    /// Sovereign+mesh embed, so peer inference just 503s).
    inference_provider: RwLock<Option<Arc<dyn InferenceProvider>>>,
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
            corpus_engine: RwLock::new(None),
            inference_provider: RwLock::new(None),
        }
    }

    /// Legacy constructor that doesn't persist — use only in tests
    /// where a tempdir isn't worth setting up. Production code must
    /// prefer `new(data_dir)`.
    pub fn new_in_memory() -> Self {
        Self {
            state: Arc::new(RwLock::new(DaemonState::Stopped)),
            data_dir: PathBuf::new(),
            corpus_engine: RwLock::new(None),
            inference_provider: RwLock::new(None),
        }
    }

    /// Install a `CorpusEngine` so that when the daemon starts, its
    /// `AppState` has something to search — without this, the
    /// handlers on `/v1/knowledge/search` and
    /// `/internal/knowledge/search` return 503 and peers asking us
    /// for philosophy passages see an empty mesh. Call once, during
    /// Sovereign's bootstrap, *before* `try_resume` / `create_mesh`
    /// / `join_mesh` so the first gossip round that runs after
    /// startup already advertises our real `hosted_corpora`.
    ///
    /// If called while the daemon is already running, the engine is
    /// swapped in — useful when bootstrap rebuilds the engine mid-
    /// session (e.g. the user changes the embed model). Existing
    /// Arc<AppState> instances captured by running HTTP tasks keep
    /// the old engine; the next created `AppState` (after a
    /// `stop` + restart) will pick up the new one.
    pub async fn set_corpus_engine(&self, engine: Arc<CorpusEngine>) {
        *self.corpus_engine.write().await = Some(engine);
    }

    /// Install the `InferenceProvider` that answers peer chat
    /// completions. Same injection timing as `set_corpus_engine`:
    /// call during desktop bootstrap, before any mesh start. The
    /// same provider Sovereign uses for the local user's chats —
    /// a peer asking us for synthesis gets the same quality a
    /// local user would.
    pub async fn set_inference_provider(
        &self,
        provider: Arc<dyn InferenceProvider>,
    ) {
        *self.inference_provider.write().await = Some(provider);
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
        // Use routable local IPs rather than `0.0.0.0:port`. The wildcard
        // bind is correct for the listener, but storing it on our
        // `MemberRecord.addresses` means peers receiving our gossip would
        // try to dial `0.0.0.0`, which on macOS resolves to 127.0.0.1 —
        // they'd hit themselves instead of us. See `reachable_addresses`.
        let addrs = reachable_addresses(internal_port);

        let (mesh, join_key) = membership::init_mesh(mesh_name, node_name, addrs);
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
        // Same rationale as create_mesh: we must advertise routable IPs
        // in our MemberRecord, not a wildcard, so the founder can reach
        // us back during gossip rounds after the initial handshake.
        let addrs = reachable_addresses(internal_port);

        // Step 2 — placeholder mesh so mDNS has something to advertise.
        let (placeholder_mesh, _throwaway_key) =
            membership::init_mesh(&mesh_name, node_name, addrs.clone());
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
            addrs,
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

        // Build an AppState that already knows about our CorpusEngine
        // (if one was installed via `set_corpus_engine`). Without
        // this, Commonwealth's knowledge handlers can only return
        // stubs — the whole reason Peer A couldn't see Peer B's SEP
        // corpus. The MeshStore is in-memory here; the desktop's
        // long-term persistence story for mesh state still goes
        // through `mesh.json` on disk, not through MeshStore.
        let corpus_engine = self.corpus_engine.read().await.clone();
        let mesh_store = Arc::new(
            commonwealth_state::MeshStore::in_memory()
                .expect("in-memory MeshStore failed"),
        );
        let app_registry = Arc::new(commonwealth_app::registry::AppRegistry::new());
        let app_state = AppState::new_with_platform_and_engine(
            node_id,
            mesh,
            mesh_store,
            app_registry,
            corpus_engine,
        );

        // If Sovereign installed an InferenceProvider, wrap it in
        // the OpenAI-flavour adapter so this node's
        // `/v1/chat/completions` serves peer requests directly
        // from the same local model the user would use. Without
        // this, peer inference requests 503 because the daemon's
        // scheduler/llama-server path is empty in the embedded
        // topology.
        let app_state = if let Some(provider) =
            self.inference_provider.read().await.as_ref()
        {
            let adapter: Arc<dyn LocalInferenceService> = Arc::new(
                crate::inference_adapter::SovereignInferenceAdapter::new(
                    provider.clone(),
                ),
            );
            info!("inference adapter: wired into /v1/chat/completions");
            app_state.with_local_inference(adapter)
        } else {
            app_state
        };

        // Install a persistence hook that fires on every Mesh
        // mutation from a route handler (`/internal/join`,
        // `/internal/gossip`). This closes the race window where
        // the founder accepts a new member but crashes before the
        // next 10s gossip-loop re-persist fires, forgetting the
        // joiner on restart. We do this BEFORE the state is
        // .clone()'d into the HTTP servers so Arc::get_mut in the
        // builder succeeds.
        let app_state = if self.persistence_enabled() {
            let data_dir = self.data_dir.clone();
            let hook: commonwealth_api::state::MeshMutationHook = Arc::new(
                move |mesh: &commonwealth_core::mesh::Mesh, self_id: NodeId| {
                    if let Err(e) = persist::save(&data_dir, mesh, self_id) {
                        tracing::warn!(
                            error = %e,
                            "mesh_mutation_hook: persist failed"
                        );
                    }
                },
            );
            app_state.with_mesh_mutation_hook(hook)
        } else {
            app_state
        };

        // Client API on 0.0.0.0:9741 — this is the OpenAI-compatible
        // public surface documented in SYSTEM_OVERVIEW.md §5.5.
        // Peers fetch `/oicp/v1/capabilities` here, the Joiner's
        // HybridProvider POSTs `/v1/chat/completions` here for
        // federated inference, and mesh apps can federate via
        // `/v1/apps/*`. Was 127.0.0.1 for earlier dev builds where
        // only the in-process Tauri commands called it — that broke
        // mesh inference federation because peers couldn't reach us.
        //
        // Trust boundary: this port has no authentication today.
        // The Commonwealth security model (per glossary) is "a
        // closed trust ring" — the join_key_hash gates membership,
        // and deployment environments (Tailscale ACLs, LAN
        // firewalls) are expected to bound reachability to mesh
        // members. A future revision should add per-request auth
        // against `Mesh.join_key_hash` so a reachable-but-
        // non-member attacker can't burn our inference budget.
        let client_addr: SocketAddr = "0.0.0.0:9741".parse().unwrap();
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
        // Hand `data_dir` to the gossip loop so it can re-persist
        // mesh.json after every round — catching the Founder's
        // /internal/join mutation (which mutates in-memory but used
        // to leave the on-disk snapshot stale, so a Founder restart
        // forgot every Joiner and Joiners had to rejoin each time).
        let persist_dir = if self.persistence_enabled() {
            Some(self.data_dir.clone())
        } else {
            None
        };
        let gossip_handle = gossip::spawn_gossip_loop(
            app_state.clone(),
            gossip::DEFAULT_GOSSIP_INTERVAL,
            gossip::DEFAULT_OFFLINE_THRESHOLD,
            persist_dir,
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
/// Build the `Vec<SocketAddr>` we'll store in our own `MemberRecord`.
/// Each local non-loopback IP becomes `ip:port`. If no interface can
/// be discovered (e.g. no network at all), fall back to the wildcard
/// `0.0.0.0:port` — worse than useless for cross-machine gossip, but
/// at least lets a solo-on-localhost founder start up. Peers that
/// receive a wildcard address will see self-loopback behavior; the
/// warning log below makes that case visible.
fn reachable_addresses(port: u16) -> Vec<SocketAddr> {
    let ips = local_ip_candidates();
    if ips.is_empty() {
        warn!(
            port,
            "no routable local IPs discovered — falling back to \
             0.0.0.0:{port} in MemberRecord. Cross-machine gossip \
             will not work until a network interface is available."
        );
        return vec![SocketAddr::new(
            std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
            port,
        )];
    }
    ips.into_iter().map(|ip| SocketAddr::new(ip, port)).collect()
}

fn local_ip_candidates() -> Vec<std::net::IpAddr> {
    // Two-tier strategy:
    //
    //   Tier 1: enumerate EVERY local non-loopback interface via
    //   `if-addrs`. This is what we actually want — on a machine
    //   with both WiFi (192.168.x) and Tailscale (100.x) up, both
    //   addresses need to be published so peers can reach us via
    //   whichever one they can route to. The old default-route
    //   trick missed Tailscale entirely on dual-homed machines,
    //   which is EXACTLY the Commonwealth LAN-+-VPN topology.
    //
    //   Tier 2 (fallback): the "UDP-connect to a public IP without
    //   sending" trick. Kept for cases where `if-addrs` errors out
    //   (should never happen on darwin/linux but the contract is
    //   best-effort). Never used in practice.
    //
    // Ordering: preferred routable IPs first — link-local addresses
    // (169.254.x, fe80::) and private-ranges come after globals.
    // Rationale: the peer tries addresses in list order, so putting
    // the most reliable ones first shortens the mean fan-out path.
    let mut ips: Vec<std::net::IpAddr> = Vec::new();

    match if_addrs::get_if_addrs() {
        Ok(addrs) => {
            for iface in addrs {
                let ip = iface.ip();
                if ip.is_loopback() {
                    continue;
                }
                // Link-local addresses are useless cross-machine:
                // 169.254.x is unconfigured DHCP fallback, fe80::
                // is IPv6 link-local which can't route off the
                // local segment. Macs have lots of these from
                // Thunderbolt / virtual interfaces / utun0,1,2...
                // Including them just spams the startup log and
                // wastes fan-out attempts (reqwest dials them and
                // gets EHOSTUNREACH). Drop outright.
                let is_link_local = match ip {
                    std::net::IpAddr::V4(v4) => {
                        v4.octets()[0] == 169 && v4.octets()[1] == 254
                    }
                    std::net::IpAddr::V6(v6) => {
                        v6.segments()[0] & 0xffc0 == 0xfe80
                    }
                };
                if is_link_local {
                    continue;
                }
                ips.push(ip);
            }
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "if_addrs::get_if_addrs failed — falling back to \
                 UDP-connect default-route detection"
            );
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
