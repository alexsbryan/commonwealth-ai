// SPDX-License-Identifier: AGPL-3.0-or-later
//! Contribution controls and activity surfaces for `mesh_admin`: the
//! peer-inflight ceiling, the runtime pause, the recent-contributions feed
//! and the per-node activity summary.
//!
//! Split out of `mesh_admin.rs` at REVIEW-audit-9: the AppState repoint's
//! longer `state.inner.<part>` paths pushed the parent over arch-gate's
//! 800-line approach-band line, so the self-contained contribution routes
//! move here. Re-exported at `mesh_admin::*`, so every importer is unchanged.

use super::*;

// ─── Contribution controls ─────────────────────────────────────
//
// The Settings UI (and W3's tray menu) reads this status, sets the
// peer-inflight ceiling, and toggles the runtime pause.
//
// Routes are mounted on the INTERNAL port (`:9742`) — verified in
// `server::internal_router`, and matched by their callers, which
// build URLs from `AppState::internal_base_url()` (the desktop tray
// and `commands/budget.rs`). This comment previously claimed the
// client port "behind the same loopback guard as
// `/internal/inference/warmup`"; both halves were wrong, and warmup
// has since moved to `:9741` for its own reasons.
//
// Beware the general trap: the `/internal/` path prefix says NOTHING
// about which port a route is on. `/internal/corpus/watch/*` is on
// the client port, `/internal/rpc-warm` is on this one. Read the
// router, not the path.

#[derive(Debug, Serialize)]
pub struct ContributionStatusResponse {
    /// Configured max concurrent peer requests. `usize::MAX`
    /// serialises as a large number — the UI displays "unlimited"
    /// when comparing to a sentinel.
    pub ceiling: usize,
    /// Live in-flight peer request count (approximate under
    /// contention — see field docs on `peer_inflight_count`).
    pub in_flight: usize,
    /// Unix-seconds expiry of the active pause, or `null` when not
    /// paused. The UI computes "Resumes at <time>" from this.
    pub paused_until: Option<i64>,
    /// Seconds until the active pause expires (null when not paused).
    pub pause_remaining_secs: Option<u64>,
    /// Whether peer requests honour the foreground-yield window.
    pub yield_peers_to_foreground: bool,
    /// Currently-yielding-to-local-user marker; `null` when not in
    /// the yield window. Lets the UI badge "yielding to chat".
    pub yielding_secs_remaining: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct SetContributionCeilingRequest {
    /// Max concurrent peer requests. `0` rejects all; `null` /
    /// missing means "unlimited" (`usize::MAX`).
    pub max: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct PauseContributionsRequest {
    /// How long the pause should last. `0` is a no-op (use
    /// `/internal/contribution/resume` to clear an active pause).
    pub duration_secs: u64,
}

/// `GET /internal/contribution/status` — snapshot for the Settings
/// panel + tray status chip. Cheap (atomic loads only).
pub async fn contribution_status(
    State(state): State<AppState>,
) -> Json<ContributionStatusResponse> {
    let paused_until_raw = state.contribution_paused_until();
    let paused_until = if paused_until_raw > 0 {
        Some(paused_until_raw)
    } else {
        None
    };
    Json(ContributionStatusResponse {
        ceiling: state.contribution_max_peer_inflight(),
        in_flight: state.peer_inflight_count(),
        paused_until,
        pause_remaining_secs: state.seconds_until_unpaused(),
        yield_peers_to_foreground: state.yield_peers_to_foreground(),
        yielding_secs_remaining: state.seconds_until_foreground_idle(),
    })
}

/// `POST /internal/contribution/ceiling` — set the peer-inflight
/// cap. `null` / missing `max` maps to unlimited.
pub async fn contribution_ceiling_set(
    State(state): State<AppState>,
    Json(req): Json<SetContributionCeilingRequest>,
) -> Json<ContributionStatusResponse> {
    let new_cap = req.max.unwrap_or(usize::MAX);
    let prev = state.contribution_max_peer_inflight();
    state.set_contribution_max_peer_inflight(new_cap);
    if prev != new_cap {
        tracing::info!(
            previous = prev,
            new = new_cap,
            "contribution: peer-inflight ceiling updated"
        );
    }
    contribution_status(State(state)).await
}

/// `POST /internal/contribution/pause` — pause for N seconds.
/// Idempotent; subsequent calls reset the expiry. The tray's "Pause
/// for 15min" / "1hr" submenu items call this.
pub async fn contribution_pause(
    State(state): State<AppState>,
    Json(req): Json<PauseContributionsRequest>,
) -> Json<ContributionStatusResponse> {
    if req.duration_secs == 0 {
        // No-op; surface current status. Avoids a confused state
        // where `paused_until` gets set to now and immediately
        // expires.
        return contribution_status(State(state)).await;
    }
    let now = sovereign_time::unix_now();
    let expiry = now.saturating_add(req.duration_secs as i64);
    state.set_contribution_paused_until(expiry);
    tracing::info!(
        duration_secs = req.duration_secs,
        expiry_unix = expiry,
        "contribution: paused via /internal/contribution/pause"
    );
    contribution_status(State(state)).await
}

/// `POST /internal/contribution/resume` — clear an active pause.
/// No body required.
pub async fn contribution_resume(
    State(state): State<AppState>,
) -> Json<ContributionStatusResponse> {
    if state.contribution_paused_until() != 0 {
        state.set_contribution_paused_until(0);
        tracing::info!("contribution: resumed via /internal/contribution/resume");
    }
    contribution_status(State(state)).await
}

#[derive(Debug, Deserialize)]
pub struct RecentContributionsParams {
    /// Max events to return. Defaults to 20; capped at 200 to bound
    /// the response size (the full ledger can have many thousands of
    /// events on a long-running node).
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct RecentContributionsResponse {
    /// Most recent ledger events, sorted by timestamp DESC. Each
    /// entry is a full `LedgerEvent` (origin node + timestamp + kind);
    /// the UI is responsible for friendly-name resolution and any
    /// formatting beyond the raw fact.
    pub events: Vec<commonwealth_core::contributions::LedgerEvent>,
}

/// `GET /internal/contribution/recent` — recent ledger events, newest
/// first. Powers the W3 contribution-panel "served feed" without
/// forcing the UI to aggregate across the full per-node window. Cheap:
/// reads the MeshStore once and sorts in-memory.
pub async fn contribution_recent(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<RecentContributionsParams>,
) -> Result<Json<RecentContributionsResponse>, (StatusCode, String)> {
    let limit = params.limit.unwrap_or(20).min(200);
    let entries = state
        .inner
        .fabric
        .mesh_store
        .scan(commonwealth_state::CONTRIBUTIONS_APP_ID, "")
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("contribution_recent: scan failed: {e}"),
            )
        })?;
    let mut events: Vec<commonwealth_core::contributions::LedgerEvent> = entries
        .into_iter()
        .filter_map(|e| serde_json::from_slice(e.value.as_ref()).ok())
        .collect();
    // Newest first. `LedgerEvent.timestamp` is unix-seconds; ties
    // are broken by stable_sort order, which is fine for UI display.
    events.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    events.truncate(limit);
    Ok(Json(RecentContributionsResponse { events }))
}

// ── Dimensional per-node contributions (Mesh Health Members panel) ──
//
// Mirror of the Tauri-side `NodeContributionsDto` the desktop's
// `mesh_get_contributions` returns in Local mode. Field names are
// frozen — the desktop deserializes against this exact shape in
// Attach mode, so renaming a field here without updating the
// desktop side blanks the Members ledger.

#[derive(Debug, Clone, Serialize)]
pub struct CorpusHostingView {
    pub corpus_id: String,
    pub corpus_name: String,
    pub size_gb: f64,
    pub queries_served: u64,
    pub is_sole_host: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeContributionsView {
    pub node_id: String,
    pub window_days: u32,
    pub inference_served_requests: u64,
    pub inference_served_tokens: u64,
    pub inference_served_wall_seconds: f64,
    pub inference_consumed_requests: u64,
    pub inference_consumed_tokens: u64,
    pub corpora_hosted: Vec<CorpusHostingView>,
    pub bytes_served: u64,
    pub bytes_received: u64,
}

/// `GET /internal/contribution/view` — aggregated per-node contributions
/// over the default 30-day window, one entry per peer the local
/// MeshStore has ledger events about. Powers the Mesh → Members
/// section of the desktop Settings panel in Attach mode, where the
/// Tauri shell can't reach the daemon's in-process `AppState`
/// directly.
pub async fn contribution_view(
    State(state): State<AppState>,
) -> Result<Json<Vec<NodeContributionsView>>, (StatusCode, String)> {
    let caps_map: std::collections::HashMap<
        NodeId,
        commonwealth_core::capabilities::NodeCapabilities,
    > = {
        let mesh_view = state.inner.fabric.mesh.read().await;
        mesh_view
            .members
            .iter()
            .map(|(id, member)| (*id, member.capabilities.clone()))
            .collect()
    };
    let map = commonwealth_state::current_contributions(
        &state.inner.fabric.mesh_store,
        &caps_map,
        commonwealth_core::contributions::DEFAULT_WINDOW_DAYS,
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("contribution_view: aggregate failed: {e}"),
        )
    })?;
    let mut out: Vec<NodeContributionsView> = map
        .into_iter()
        .map(|(node_id, c)| NodeContributionsView {
            node_id: node_id
                .as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            window_days: c.window_days,
            inference_served_requests: c.inference_served.requests,
            inference_served_tokens: c.inference_served.total_tokens_generated,
            inference_served_wall_seconds: c.inference_served.wall_seconds,
            inference_consumed_requests: c.inference_consumed.requests,
            inference_consumed_tokens: c.inference_consumed.total_tokens_generated,
            corpora_hosted: c
                .corpora_hosted
                .into_iter()
                .map(|h| CorpusHostingView {
                    corpus_id: h.corpus_id,
                    corpus_name: h.corpus_name,
                    size_gb: h.size_gb,
                    queries_served: h.queries_served,
                    is_sole_host: h.is_sole_host,
                })
                .collect(),
            bytes_served: c.bytes_served,
            bytes_received: c.bytes_received,
        })
        .collect();
    out.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    Ok(Json(out))
}

// ── Local Activity ledger (Activity & Sharing surface) ──────────
//
// The activity counterpart to the contribution handlers above. Reads
// the gossip-excluded `activity-private` namespace and folds in this
// node's *own* mesh-contribution totals, so one response answers
// "what has my daemon been doing — for me, and for the mesh?" — the
// glassbox view that works even for a mesh of one.

#[derive(Debug, Deserialize)]
pub struct ActivitySummaryParams {
    /// Lookback window in days. Defaults to the activity ledger's
    /// 7-day window; capped at 365.
    pub window_days: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct ActivitySummaryResponse {
    #[serde(flatten)]
    pub activity: commonwealth_core::activity::ActivitySummary,
    // Folded-in mesh contribution: what THIS node provided to peers
    // (the gossiped contribution ledger, over its own 30-day window).
    pub peer_inference_served_requests: u64,
    pub peer_inference_served_tokens: u64,
    pub peer_knowledge_queries_served: u64,
    pub peer_bytes_served: u64,
    pub peer_bytes_received: u64,
}

/// `GET /internal/activity/summary` — the local Activity rollup plus
/// this node's mesh-contribution totals. Powers the "all on this
/// machine" totals card in Settings → Activity & Sharing.
pub async fn activity_summary(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<ActivitySummaryParams>,
) -> Result<Json<ActivitySummaryResponse>, (StatusCode, String)> {
    let window_days = params
        .window_days
        .unwrap_or(commonwealth_core::activity::DEFAULT_ACTIVITY_WINDOW_DAYS)
        .min(365);
    let activity =
        commonwealth_state::current_activity(&state.inner.fabric.mesh_store, window_days).map_err(
            |e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("activity_summary: aggregate failed: {e}"),
                )
            },
        )?;

    // Fold in this node's own contribution totals. Self-origin
    // contribution events land on the self node's `NodeContributions`,
    // so the self entry is exactly "what I served to / received from
    // the mesh."
    let self_id = state.inner.fabric.contribution_emitter.self_node_id();
    let caps_map: std::collections::HashMap<
        NodeId,
        commonwealth_core::capabilities::NodeCapabilities,
    > = {
        let mesh_view = state.inner.fabric.mesh.read().await;
        mesh_view
            .members
            .iter()
            .map(|(id, member)| (*id, member.capabilities.clone()))
            .collect()
    };
    let contrib = commonwealth_state::current_contributions(
        &state.inner.fabric.mesh_store,
        &caps_map,
        commonwealth_core::contributions::DEFAULT_WINDOW_DAYS,
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("activity_summary: contribution aggregate failed: {e}"),
        )
    })?;
    let self_c = contrib.get(&self_id);

    Ok(Json(ActivitySummaryResponse {
        activity,
        peer_inference_served_requests: self_c.map(|c| c.inference_served.requests).unwrap_or(0),
        peer_inference_served_tokens: self_c
            .map(|c| c.inference_served.total_tokens_generated)
            .unwrap_or(0),
        peer_knowledge_queries_served: self_c
            .map(|c| {
                c.corpora_hosted
                    .iter()
                    .map(|h| h.queries_served)
                    .sum::<u64>()
            })
            .unwrap_or(0),
        peer_bytes_served: self_c.map(|c| c.bytes_served).unwrap_or(0),
        peer_bytes_received: self_c.map(|c| c.bytes_received).unwrap_or(0),
    }))
}

#[derive(Debug, Serialize)]
pub struct ActivityRecentResponse {
    /// Most recent local activity events, newest first. Each entry is
    /// a full `ActivityEvent`; the UI formats the friendly summary.
    pub events: Vec<commonwealth_core::activity::ActivityEvent>,
}

/// `GET /internal/activity/recent` — recent local activity events,
/// newest first. Powers the unified feed in Activity & Sharing
/// (interleaved client-side with the contribution feed).
pub async fn activity_recent(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<RecentContributionsParams>,
) -> Result<Json<ActivityRecentResponse>, (StatusCode, String)> {
    let limit = params.limit.unwrap_or(20).min(200);
    let entries = state
        .inner
        .fabric
        .mesh_store
        .scan(commonwealth_state::ACTIVITY_APP_ID, "")
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("activity_recent: scan failed: {e}"),
            )
        })?;
    let mut events: Vec<commonwealth_core::activity::ActivityEvent> = entries
        .into_iter()
        .filter_map(|e| serde_json::from_slice(e.value.as_ref()).ok())
        .collect();
    events.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    events.truncate(limit);
    Ok(Json(ActivityRecentResponse { events }))
}
