//! Member-list gossip — the thing that keeps two peers' views of
//! the mesh converged after the initial join.
//!
//! Model: anti-entropy push-pull over plain HTTP on port 9742. Every
//! `interval` (default 10s) we pick up to `FANOUT` random members
//! and POST our current `Mesh` to their `/internal/gossip`; they
//! merge it into theirs and reply with their (now-updated) snapshot
//! which we then merge in. Convergence in one round per pair.
//!
//! Two side effects every round:
//! 1. Our own `last_seen` is bumped to `now()` so peers learn we're
//!    still here and don't decay us to Offline.
//! 2. Members whose `last_seen` is older than `offline_threshold`
//!    are marked `NodeStatus::Offline` locally — the mechanism that
//!    turns "the founder closed their laptop" from a silent stale
//!    member list into a visible offline indicator.
//!
//! Reuses `Mesh::merge_from` for the actual last-writer-wins
//! reconciliation. This module is just the network plumbing on top.
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use commonwealth_api::state::AppState;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::{Mesh, MemberRecord, MeshPeering, NodeStatus};
use commonwealth_core::ids::MeshId;
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::capabilities::build_local_capabilities;

/// Default: send to at most this many peers per round. Small mesh
/// sizes make higher fan-out pointless; bandwidth is negligible at
/// 2 even with full-snapshot gossip.
const FANOUT: usize = 2;

/// Hard per-peer HTTP timeout. Mirrors `sovereign-mesh::join` so
/// slow/unreachable peers don't drag out a gossip round.
const PEER_TIMEOUT: Duration = Duration::from_secs(3);

/// After this long without a successful gossip contact, a peer is
/// marked Offline. Needs to be >> `interval` so a single missed
/// round doesn't flap peers offline — roughly 6× the interval is
/// a reasonable default.
pub const DEFAULT_OFFLINE_THRESHOLD: Duration = Duration::from_secs(60);

/// Default gossip cadence. Chosen to match the UI's 5s poll
/// comfortably (UI sees converged state within ~2× the cadence).
pub const DEFAULT_GOSSIP_INTERVAL: Duration = Duration::from_secs(10);

/// Handle to the spawned gossip task. Aborts the task when dropped
/// (mirrors `commonwealth_discovery::mdns::BrowseHandle`). The
/// `DaemonState::Running` variant holds one of these so stopping
/// the daemon cleanly tears down the gossip loop along with mDNS.
pub struct GossipHandle {
    _task: tokio::task::JoinHandle<()>,
}

impl Drop for GossipHandle {
    fn drop(&mut self) {
        self._task.abort();
    }
}

/// Spawn the periodic gossip task. Call once per daemon start.
///
/// `persist_dir` is the directory containing `mesh.json`. When
/// provided, every round re-persists the current mesh snapshot so
/// that mutations from any source — the `/internal/join` handler,
/// `merge_from` via gossip, `last_seen` bumps, status decays —
/// survive a daemon restart without needing a per-handler persist
/// callback. Costs one JSON file write per 10s (trivial). `None`
/// (test harnesses, CLI without persistence) skips persistence.
pub fn spawn_gossip_loop(
    app_state: AppState,
    interval: Duration,
    offline_threshold: Duration,
    persist_dir: Option<std::path::PathBuf>,
) -> GossipHandle {
    let task = tokio::spawn(async move {
        info!(
            interval_secs = interval.as_secs(),
            offline_threshold_secs = offline_threshold.as_secs(),
            persistence = persist_dir.is_some(),
            "gossip: loop started"
        );
        loop {
            tokio::time::sleep(interval).await;
            if let Err(e) = run_one_round(&app_state, offline_threshold).await {
                warn!(error = %e, "gossip: round errored");
            }
            if let Some(dir) = persist_dir.as_deref() {
                let mesh = app_state.inner.mesh.read().await.clone();
                let self_id = app_state.inner.self_node_id;
                if let Err(e) = crate::persist::save(dir, &mesh, self_id) {
                    // Don't spam — persistence failure is rarely
                    // fatal to the running session, but the operator
                    // should know their mesh won't survive restart.
                    warn!(
                        error = %e,
                        "gossip: mesh.json re-persist failed"
                    );
                }
            }
        }
    });
    GossipHandle { _task: task }
}

/// Fire a single gossip round immediately — used as a "fast initial
/// sync" trigger right after the daemon starts so a restart doesn't
/// wait a full interval before reconciling with peers. Bounded by
/// `max_duration` so daemon startup stays prompt even when all
/// peers are unreachable.
pub async fn initial_sync(
    app_state: &AppState,
    offline_threshold: Duration,
    max_duration: Duration,
) {
    match tokio::time::timeout(max_duration, run_one_round(app_state, offline_threshold)).await {
        Ok(Ok(())) => {
            debug!("gossip: initial_sync completed");
        }
        Ok(Err(e)) => warn!(error = %e, "gossip: initial_sync errored"),
        Err(_) => {
            debug!(
                max_ms = max_duration.as_millis() as u64,
                "gossip: initial_sync timed out — continuing startup"
            );
        }
    }
}

/// One full gossip round. Touches own `last_seen`, decays stale
/// peers, then pair-gossips with up to `FANOUT` random members.
pub async fn run_one_round(
    app_state: &AppState,
    offline_threshold: Duration,
) -> Result<(), GossipError> {
    let self_id = app_state.inner.self_node_id;
    let now = now_secs();
    let threshold = offline_threshold.as_secs();

    // Build a fresh snapshot of our own capabilities BEFORE we take
    // the mesh write lock — `installed_indexes()` awaits a directory
    // read, and we don't want to pin the lock across that. The
    // engine is optional: test daemons and the CLI run without one.
    let fresh_caps = build_local_capabilities(
        app_state.inner.corpus_engine.as_ref(),
        now,
    )
    .await;
    // Step 1: touch self + decay stale peers. One write-lock window.
    // Compare current vs. fresh hosted_corpora so we can log at
    // info only when the advertised set changed (new corpus
    // installed, one removed) — the every-10s heartbeat otherwise
    // logs at debug. Same gating policy as `mesh_state: rebuilt`.
    let candidates: Vec<(NodeId, Vec<std::net::SocketAddr>)> = {
        let mut mesh = app_state.inner.mesh.write().await;
        let prior_corpora: std::collections::BTreeSet<String> = mesh
            .members
            .get(&self_id)
            .map(|m| {
                m.capabilities
                    .hosted_corpora
                    .iter()
                    .map(|c| c.corpus_id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let fresh_corpora: std::collections::BTreeSet<String> = fresh_caps
            .hosted_corpora
            .iter()
            .map(|c| c.corpus_id.clone())
            .collect();
        if fresh_corpora != prior_corpora {
            tracing::info!(
                hosted_corpora = ?fresh_corpora,
                system_ram_gb = fresh_caps.hardware.system_ram_gb,
                "gossip: hosted_corpora set changed — re-publishing"
            );
        } else {
            tracing::debug!(
                hosted_corpora = ?fresh_corpora,
                "gossip: publishing (unchanged)"
            );
        }
        if let Some(me) = mesh.members.get_mut(&self_id) {
            me.last_seen = now;
            me.status = NodeStatus::Online;
            // Replace capabilities with the freshly-sampled version
            // every round. This is the mechanism by which a newly-
            // installed SEP corpus becomes visible to peers within
            // one gossip interval — without it, `hosted_corpora`
            // stays frozen at whatever it was when the daemon
            // started (typically empty, since the user hasn't yet
            // run the install).
            me.capabilities = fresh_caps;
        }
        for (id, m) in mesh.members.iter_mut() {
            if *id == self_id {
                continue;
            }
            // Only decay if the record is actually stale AND not
            // already Offline (avoid unnecessary writes).
            if now.saturating_sub(m.last_seen) > threshold
                && m.status != NodeStatus::Offline
            {
                m.status = NodeStatus::Offline;
                debug!(
                    peer = %m.node_id,
                    staleness_secs = now.saturating_sub(m.last_seen),
                    "gossip: marked peer Offline (stale last_seen)"
                );
            }
        }
        mesh.members
            .values()
            .filter(|m| m.node_id != self_id)
            .map(|m| (m.node_id, m.addresses.clone()))
            .collect()
    };

    if candidates.is_empty() {
        // Solo mesh — nothing to do. Still valuable to have fired
        // the round so self's `last_seen` stays current for the
        // moment a peer does arrive.
        return Ok(());
    }

    // Step 2: pick up to FANOUT peers at random. Scope the RNG so
    // the non-Send `ThreadRng` doesn't cross an `.await` below —
    // spawned futures must be `Send` and `rand::rng()` isn't.
    let selection = {
        let mut rng = rand::rng();
        let mut tmp = candidates;
        tmp.shuffle(&mut rng);
        tmp.truncate(FANOUT);
        tmp
    };

    // Step 3: snapshot our mesh once and POST it to each picked
    // peer. Using the same snapshot across the fan-out keeps rounds
    // cheap and means every peer sees the same view of us.
    let my_snapshot = { app_state.inner.mesh.read().await.clone() };
    let http = reqwest::Client::builder()
        .timeout(PEER_TIMEOUT)
        .build()
        .map_err(|e| GossipError::ClientBuild(e.to_string()))?;

    for (peer_id, addrs) in selection {
        if addrs.is_empty() {
            debug!(peer = %peer_id, "gossip: no addresses on record, skipping");
            continue;
        }
        for addr in &addrs {
            match gossip_with_peer(&http, *addr, &my_snapshot).await {
                Ok(their_view) => {
                    let mut mesh = app_state.inner.mesh.write().await;
                    let report = mesh.merge_from(self_id, &their_view);
                    if report.added > 0 {
                        info!(
                            peer = %peer_id,
                            peer_addr = %addr,
                            added = report.added,
                            updated = report.updated,
                            "gossip: member added from peer's view"
                        );
                    } else if report.updated > 0 {
                        tracing::debug!(
                            peer = %peer_id,
                            peer_addr = %addr,
                            updated = report.updated,
                            "gossip: merged peer's view (last_seen refresh)"
                        );
                    }
                    // Also bump THIS peer's last_seen in case their
                    // view of themselves lagged — we successfully
                    // reached them just now, so they're Online.
                    if let Some(peer) = mesh.members.get_mut(&peer_id) {
                        peer.last_seen = now_secs();
                        peer.status = NodeStatus::Online;
                    }
                    break; // one working address is enough
                }
                Err(e) => {
                    debug!(
                        peer = %peer_id,
                        peer_addr = %addr,
                        error = %e,
                        "gossip: reach failed, trying next address"
                    );
                    continue;
                }
            }
        }
    }

    Ok(())
}

async fn gossip_with_peer(
    http: &reqwest::Client,
    addr: std::net::SocketAddr,
    my_view: &Mesh,
) -> Result<Mesh, GossipError> {
    let body = GossipRequestWire {
        mesh: MeshWire::from(my_view),
    };
    let url = format!("http://{addr}/internal/gossip");
    let response = http
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| GossipError::Transport(e.to_string()))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(GossipError::Rejected);
    }
    if !response.status().is_success() {
        return Err(GossipError::Transport(format!(
            "unexpected status {}",
            response.status()
        )));
    }

    let parsed: GossipResponseWire = response
        .json()
        .await
        .map_err(|e| GossipError::BadResponse(e.to_string()))?;
    Ok(parsed.mesh.into_mesh())
}

#[derive(Debug, thiserror::Error)]
pub enum GossipError {
    #[error("failed to build HTTP client: {0}")]
    ClientBuild(String),
    #[error("peer rejected gossip (wrong mesh or key)")]
    Rejected,
    #[error("transport error: {0}")]
    Transport(String),
    #[error("malformed peer response: {0}")]
    BadResponse(String),
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── Wire types ───────────────────────────────────────────────
//
// Mirror of `commonwealth_api::routes_internal::{GossipRequest,
// GossipResponse, MeshWire}`. Duplicated here (like `join::MeshWire`)
// because the server-side type isn't re-exported and projecting
// HashMap<NodeId, MemberRecord> → Vec<MemberRecord> for serde is
// the whole reason MeshWire exists.

#[derive(Debug, Serialize)]
struct GossipRequestWire {
    mesh: MeshWire,
}

#[derive(Debug, Deserialize)]
struct GossipResponseWire {
    mesh: MeshWire,
}

#[derive(Debug, Serialize, Deserialize)]
struct MeshWire {
    id: MeshId,
    name: String,
    join_key_hash: [u8; 32],
    members: Vec<MemberRecord>,
    peers: Vec<MeshPeering>,
}

impl From<&Mesh> for MeshWire {
    fn from(m: &Mesh) -> Self {
        Self {
            id: m.id,
            name: m.name.clone(),
            join_key_hash: m.join_key_hash,
            members: m.members.values().cloned().collect(),
            peers: m.peers.clone(),
        }
    }
}

impl MeshWire {
    fn into_mesh(self) -> Mesh {
        use std::collections::HashMap;
        let members = self
            .members
            .into_iter()
            .map(|m| (m.node_id, m))
            .collect::<HashMap<_, _>>();
        Mesh {
            id: self.id,
            name: self.name,
            join_key_hash: self.join_key_hash,
            members,
            peers: self.peers,
        }
    }
}
