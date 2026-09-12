// SPDX-License-Identifier: AGPL-3.0-or-later
//! GET /internal/newsworthy/status — operator surface for the
//! `wikipedia-newsworthy` freshness daemon.
//!
//! Without this route the watcher is fully invisible from the
//! desktop. Users see "Add" on the Newsworthy chip and have no way
//! to verify whether the daemon is running, whether they're leader
//! or follower, when the last tick fired, or whether anything is
//! tracked. The route reads the snapshot the watcher publishes to
//! `MeshStore` at the end of every tick (key
//! `wikipedia-newsworthy:status/last_tick`) and overlays the live
//! mesh-membership view (current leader, online peer count) so the
//! UI can answer the three questions the user actually asks:
//!
//!   1. Did the watcher tick recently? (`last_tick_at`)
//!   2. Who is doing the ingest work? (`leader_node_id`)
//!   3. Is this node contributing? (`role_leader`, `corpus_installed`)
//!
//! Pure read of mesh state — no engine work, no MediaWiki calls.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use commonwealth_core::contributions::{LedgerEvent, LedgerEventKind};
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::NodeStatus;
use commonwealth_core::partition;
use corpus_engine::update::newsworthy_watcher::{
    TickStatusSnapshot, APP_ID_STATUS, STATUS_KEY_LAST_TICK,
};

use crate::state::AppState;

const NEWSWORTHY_CORPUS_ID: &str = "wikipedia-newsworthy";

#[derive(Debug, Serialize, Deserialize)]
pub struct NewsworthyStatusResponse {
    /// Snapshot of this node's most recent tick. `None` when the
    /// watcher hasn't completed a tick yet (fresh daemon boot before
    /// first jittered tick lands).
    pub last_tick: Option<TickStatusSnapshot>,
    /// Live answer to "is `wikipedia-newsworthy` installed on this
    /// node right now?" — derived from the engine's
    /// `installed_indexes()` at request time, NOT read from the
    /// snapshot. The snapshot's same-named field is the install
    /// state observed during the last tick; if an operator just
    /// flipped `ingestion_in_progress` (or completed an install) the
    /// snapshot stays stale until the next tick, which can be up to
    /// 24h. The chip uses this field for "show the warning band or
    /// not"; the snapshot field stays for diagnostic completeness.
    pub local_corpus_installed: bool,
    /// Display id of the node currently elected leader for the
    /// `wikipedia-newsworthy` ingest, computed over peers that have
    /// advertised the corpus in a recent `StorageSnapshot`. `None`
    /// when no online peer has installed the corpus — the watcher
    /// pool is empty and ingest is paused everywhere.
    pub leader_node_id: Option<String>,
    /// Online peers that have advertised `wikipedia-newsworthy` in
    /// their latest gossiped snapshot. Self is included whenever this
    /// node has the corpus installed.
    pub installed_peer_count: usize,
    /// True when self is part of the leader-election pool. Always
    /// equal to `local_corpus_installed` today, but kept as a
    /// separate field because a future "advertised but not yet
    /// active" state would split these.
    pub self_in_pool: bool,
}

pub async fn newsworthy_status(State(state): State<AppState>) -> Json<NewsworthyStatusResponse> {
    let last_tick = read_last_tick(&state);
    let local_corpus_installed = local_corpus_installed(&state).await;
    let (leader_node_id, installed_peer_count) = compute_leader(&state).await;
    Json(NewsworthyStatusResponse {
        last_tick,
        local_corpus_installed,
        leader_node_id,
        installed_peer_count,
        self_in_pool: local_corpus_installed,
    })
}

/// `POST /internal/newsworthy/tick` — fire one watcher tick on
/// demand, bypassing the 24h interval. Returns 202 on successful
/// queue, 503 when the watcher isn't running (no corpus engine on
/// this daemon), 503 when the watcher's force-tick channel is full
/// (one already queued — coalescing is intentional, see comment in
/// daemon.rs). The route does NOT await the tick body; the watcher
/// runs it asynchronously and republishes the snapshot when done.
/// Callers poll `GET /internal/newsworthy/status` and watch
/// `last_tick.observed_at` to confirm the tick landed.
#[derive(Debug, Serialize)]
pub struct NewsworthyTickResponse {
    pub queued: bool,
    pub reason: Option<String>,
}

pub async fn newsworthy_tick(
    State(state): State<AppState>,
) -> (StatusCode, Json<NewsworthyTickResponse>) {
    let sender = {
        let guard = state.inner.newsworthy_force_tick.read().await;
        guard.clone()
    };
    let Some(sender) = sender else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(NewsworthyTickResponse {
                queued: false,
                reason: Some(
                    "watcher not running on this daemon — either \
                     [daemon].freshness_watchers_enabled = false in config.toml, \
                     or no corpus engine is wired"
                        .into(),
                ),
            }),
        );
    };
    match sender.try_send(()) {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(NewsworthyTickResponse {
                queued: true,
                reason: None,
            }),
        ),
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => (
            StatusCode::ACCEPTED,
            Json(NewsworthyTickResponse {
                queued: false,
                reason: Some("a tick is already queued — coalescing per design".into()),
            }),
        ),
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(NewsworthyTickResponse {
                queued: false,
                reason: Some("watcher tick channel closed (watcher shut down)".into()),
            }),
        ),
    }
}

async fn local_corpus_installed(state: &AppState) -> bool {
    let Some(engine) = state.inner.corpus_engine.clone() else {
        return false;
    };
    match engine.installed_indexes().await {
        Ok(list) => list
            .iter()
            .any(|i| i.corpus_id == NEWSWORTHY_CORPUS_ID && !i.is_shard),
        Err(_) => false,
    }
}

fn read_last_tick(state: &AppState) -> Option<TickStatusSnapshot> {
    let entry = state
        .inner
        .mesh_store
        .get(APP_ID_STATUS, STATUS_KEY_LAST_TICK)
        .ok()
        .flatten()?;
    serde_json::from_slice(entry.value.as_ref()).ok()
}

/// Mirror of `MeshNewsworthyHost::online_members_holding_target` —
/// kept here as a pure read so the route can answer "who is leader?"
/// without taking a dependency on `sovereign-mesh`. The two
/// implementations must stay aligned; the watcher writes leadership
/// decisions, this route reports them.
async fn compute_leader(state: &AppState) -> (Option<String>, usize) {
    let mesh = state.inner.mesh.read().await;
    let online: Vec<NodeId> = mesh
        .members
        .iter()
        .filter(|(_, m)| m.status != NodeStatus::Offline)
        .map(|(id, _)| *id)
        .collect();
    drop(mesh);

    if online.is_empty() {
        return (None, 0);
    }

    let self_id = state.self_node_id();
    let mut holders: Vec<NodeId> = Vec::new();

    if let Some(engine) = state.inner.corpus_engine.clone() {
        if let Ok(list) = engine.installed_indexes().await {
            if list
                .iter()
                .any(|i| i.corpus_id == NEWSWORTHY_CORPUS_ID && !i.is_shard)
            {
                holders.push(self_id);
            }
        }
    }

    let events: Vec<LedgerEvent> = state
        .inner
        .contribution_emitter
        .events()
        .unwrap_or_default();
    let mut latest_per_node: std::collections::HashMap<NodeId, (u64, &Vec<(String, f64)>)> =
        std::collections::HashMap::new();
    for ev in &events {
        if ev.node_id == self_id {
            continue;
        }
        if let LedgerEventKind::StorageSnapshot { corpora } = &ev.kind {
            let entry = latest_per_node.entry(ev.node_id);
            match entry {
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert((ev.timestamp, corpora));
                }
                std::collections::hash_map::Entry::Occupied(mut o) => {
                    if ev.timestamp > o.get().0 {
                        o.insert((ev.timestamp, corpora));
                    }
                }
            }
        }
    }
    for (node_id, (_, corpora)) in latest_per_node {
        if !online.contains(&node_id) {
            continue;
        }
        if corpora.iter().any(|(id, _)| id == NEWSWORTHY_CORPUS_ID) {
            holders.push(node_id);
        }
    }

    let leader = partition::elect_leader(&holders).map(|id| id.to_string());
    (leader, holders.len())
}
