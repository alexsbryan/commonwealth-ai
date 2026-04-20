use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use commonwealth_api::state::AppState;
use commonwealth_core::ids::{HandoffId, NodeId};
use commonwealth_core::knowledge::{
    CompleteOutcome, HandoffPhase, IngestionHandoff, UnitId, WorkUnit,
};
use commonwealth_core::mesh::NodeStatus;
use corpus_engine::CancellationFlag;

const CHECK_INTERVAL: Duration = Duration::from_secs(30);
const COOLDOWN: Duration = Duration::from_secs(30 * 60);

/// Spawn a background task that automatically triggers `POST
/// /internal/corpus/collaborate` when in-progress corpora are
/// detected and at least one compatible peer is available.
///
/// **Trigger conditions** (any one of these fires the trigger):
/// - Daemon startup (first iteration) — handles the restart scenario
///   where Machine A had been ingesting before it was stopped.
/// - New peer appears in the mesh — handles Machine A mid-ingest,
///   Machine B comes online.
/// - **New in-progress corpus appears** — handles Machine A starts
///   downloading Wikipedia *after* daemon startup with peers already
///   known. This is the common desktop case (open app → mesh peers
///   already discovered → *then* click Install). Without this trigger
///   the loop's `should_check` gate stays false forever and Machine B
///   never receives a partition.
///
/// **Guard**: if an ingest task is actively running (corpus_id is in
/// `AppStateInner::active_ingests`) the trigger is skipped — UNLESS a
/// new peer just appeared, in which case `corpus_collaborate` is still
/// called. That handler checks `active_ingests` itself and skips the
/// local partition spawn while still dispatching work to the new peer.
pub fn spawn_auto_collaborate_loop(state: AppState, daemon_port: u16) {
    tokio::spawn(async move {
        auto_collaborate_loop(state, daemon_port).await;
    });
}

async fn auto_collaborate_loop(state: AppState, daemon_port: u16) {
    let mut triggered: HashMap<String, Instant> = HashMap::new();
    let mut last_known_peers: HashSet<NodeId> = HashSet::new();
    // Snapshot of `in_progress_ingestions()` from the previous tick.
    // Comparing tick-over-tick lets us detect "user just started a
    // new corpus install" — the scenario where `first_iteration`
    // already burned and `new_peer_appeared` is false.
    let mut last_known_in_progress: HashSet<String> = HashSet::new();
    let mut first_iteration = true;
    let client = reqwest::Client::new();

    // Brief startup delay so the HTTP server is accepting connections.
    tokio::time::sleep(Duration::from_secs(10)).await;

    tracing::info!(
        check_interval_secs = CHECK_INTERVAL.as_secs(),
        cooldown_secs = COOLDOWN.as_secs(),
        "auto_ingest: loop started"
    );

    loop {
        let self_id = state.inner.self_node_id_swap.load_full().as_ref().clone();

        let current_peers: HashSet<NodeId> = {
            let mesh = state.inner.mesh.read().await;
            mesh.members
                .values()
                .filter(|m| m.node_id != self_id && m.status == NodeStatus::Online)
                .map(|m| m.node_id)
                .collect()
        };
        let new_peer_appeared = current_peers.iter().any(|id| !last_known_peers.contains(id));
        last_known_peers = current_peers.clone();

        // We can't call `in_progress_ingestions()` until we have an
        // engine, so query it right after the peer-set refresh.
        let Some(engine) = state.inner.corpus_engine.as_ref() else {
            tracing::debug!("auto_ingest: no corpus engine yet — waiting");
            first_iteration = false;
            tokio::time::sleep(CHECK_INTERVAL).await;
            continue;
        };

        // Under the unified-ingest primitive (Layer 1),
        // `in_progress_ingestions()` already maps both on-disk shapes
        // to the user-visible corpus id:
        //   - Canonical `<corpus>/` (legacy resume) → `<corpus>`.
        //   - Partition-of-self `<corpus>-partition-<self>/` → `<corpus>`.
        //   - Peer partitions `<corpus>-partition-<other>/` → skipped
        //     (the coordinator's `coordinate_merge` owns those, not
        //     this loop).
        //
        // So no additional filtering is needed here — every id the
        // engine returns is one we can legitimately dispatch to
        // `corpus_collaborate`. The previous `-partition-node-` filter
        // was a belt-and-suspenders for a shape `in_progress_ingestions`
        // never produces under the new model.
        // ── Pull-based queue discovery ──────────────────────────────
        //
        // Scan gossip for open pull-based handoffs and spawn a `pull_loop`
        // per eligible one. Eligible = phase::Open, embed_model matches
        // ours, and we aren't already pulling for this handoff. Runs
        // every tick so handoffs that appear mid-cycle get picked up
        // within CHECK_INTERVAL.
        discover_and_spawn_pull_loops(state.clone(), self_id, daemon_port).await;

        let in_progress_vec: Vec<String> = engine.in_progress_ingestions();
        let in_progress: HashSet<String> = in_progress_vec.iter().cloned().collect();
        let new_ingest_appeared = in_progress
            .iter()
            .any(|id| !last_known_in_progress.contains(id));
        last_known_in_progress = in_progress.clone();

        let should_check =
            first_iteration || new_peer_appeared || new_ingest_appeared;
        first_iteration = false;

        if current_peers.is_empty() {
            // No peers means nothing to dispatch to — log at debug to
            // keep the happy path quiet but still visible with
            // RUST_LOG=sovereign_mesh=debug.
            tracing::debug!(
                in_progress_count = in_progress.len(),
                "auto_ingest: no peers online — skipping"
            );
            tokio::time::sleep(CHECK_INTERVAL).await;
            continue;
        }

        if !should_check {
            tracing::debug!(
                peers = current_peers.len(),
                in_progress_count = in_progress.len(),
                "auto_ingest: no trigger (peers and ingests unchanged)"
            );
            tokio::time::sleep(CHECK_INTERVAL).await;
            continue;
        }

        tracing::info!(
            peers = current_peers.len(),
            in_progress_count = in_progress.len(),
            new_peer = new_peer_appeared,
            new_ingest = new_ingest_appeared,
            "auto_ingest: trigger fired — evaluating corpora"
        );

        // Retire cooldown entries for corpora that have since completed.
        triggered.retain(|id, _| in_progress.contains(id));

        let active_ingests: HashSet<String> = {
            state.inner.active_ingests.read().await.clone()
        };

        // Use the ordered Vec form to keep log output stable across
        // ticks — iterating a HashSet shuffles per-run and makes
        // diagnostic reading harder.
        let in_progress = in_progress_vec;

        for corpus_id in &in_progress {
            // Determine whether this node has local source data for this
            // corpus (HF manifest or extracted JSONL cache).  Pure peer
            // nodes that only receive ingest_partition assignments from a
            // coordinator have neither — they must not attempt a local
            // install, and there is nothing to coordinate from here.
            let has_local_source = engine
                .source_manifest(corpus_id)
                .ok()
                .flatten()
                .is_some()
                || engine.count_jsonl_articles(corpus_id).is_ok();

            // Ensure local work is running. The daemon is the single
            // owner of "resume my in-progress partition-of-self" under
            // the unified ingest primitive — Desktop used to spawn its
            // own engine.ingest() task, but that process dies when the
            // user closes the app. If the daemon survives and sees a
            // partition-of-self dir marked in-progress, it has to pick
            // up the work itself; otherwise Wikipedia never finishes
            // unless the user leaves Desktop open for hours.
            //
            // We skip this path when (a) an ingest task is already
            // tracked in active_ingests — avoids double-spawn racing
            // the LanceDB writer — (b) there is no partition-of-self
            // directory, which means the corpus id came from legacy
            // canonical state that the engine's ingest() will continue
            // writing in place, or (c) this node has no local source
            // data — calling spawn_corpus_install on a peer that was
            // assigned a partition via ingest_partition would fail with
            // "zero chunks" AND insert the corpus into active_ingests,
            // causing the coordinator's next ingest_partition to bounce
            // with a 409 and stall the whole pipeline.
            if !active_ingests.contains(corpus_id) && has_local_source {
                let partition_path = engine.partition_path(corpus_id);
                if partition_path.exists() {
                    spawn_local_ingest(state.clone(), corpus_id.clone()).await;
                }
            }

            // Peer-only node: no source data means no collaborate role here.
            // The coordinator will send ingest_partition when it's ready.
            if !has_local_source {
                tracing::debug!(
                    corpus = %corpus_id,
                    "auto_ingest: no local source data — skipping collaborate (peer node, waiting for ingest_partition)"
                );
                continue;
            }

            // Skip the peer-dispatch step when we're already ingesting
            // AND neither a new peer nor a new in-progress corpus just
            // appeared. (When either appears we still call
            // corpus_collaborate so the freshly-arrived peer gets its
            // share of work.)
            if active_ingests.contains(corpus_id)
                && !new_peer_appeared
                && !new_ingest_appeared
            {
                tracing::debug!(
                    corpus = %corpus_id,
                    "auto_ingest: ingest task is active and no new trigger — skipping collaborate dispatch"
                );
                continue;
            }

            if triggered.get(corpus_id).map_or(false, |t| t.elapsed() < COOLDOWN)
                && !new_peer_appeared
                && !new_ingest_appeared
            {
                continue;
            }

            tracing::info!(
                corpus = %corpus_id,
                new_peer = new_peer_appeared,
                is_restart = triggered.get(corpus_id).is_none(),
                "auto_ingest: triggering collaboration"
            );

            let url = format!("http://127.0.0.1:{daemon_port}/internal/corpus/collaborate");
            let body = serde_json::json!({ "corpus_id": corpus_id });
            match client.post(&url).json(&body).send().await {
                Ok(resp) if resp.status().is_success() => {
                    tracing::info!(corpus = %corpus_id, "auto_ingest: collaboration started");
                    triggered.insert(corpus_id.clone(), Instant::now());
                }
                Ok(resp) if resp.status().as_u16() == 409 => {
                    tracing::info!(
                        corpus = %corpus_id,
                        "auto_ingest: corpus already complete — cooling down"
                    );
                    triggered.insert(corpus_id.clone(), Instant::now());
                }
                Ok(resp) if resp.status().as_u16() == 422 => {
                    // No compatible peers — don't cooldown, retry on
                    // next peer join. INFO (not debug) so the user
                    // sees *why* Machine B isn't getting work when
                    // they check the log: mismatched embed model,
                    // offline peers, etc. The coordinator's own
                    // log line carries the specific reason.
                    let body = resp.text().await.unwrap_or_default();
                    tracing::info!(
                        corpus = %corpus_id,
                        reason = %body,
                        "auto_ingest: no compatible peers yet — will retry on peer/ingest change"
                    );
                }
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let body = resp.text().await.unwrap_or_default();
                    tracing::warn!(
                        corpus = %corpus_id,
                        status,
                        body = %body,
                        "auto_ingest: unexpected response from collaborate handler"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        corpus = %corpus_id,
                        error = %e,
                        "auto_ingest: request to local collaborate endpoint failed"
                    );
                }
            }
        }

        tokio::time::sleep(CHECK_INTERVAL).await;
    }
}

/// Spawn a local ingest for `corpus_id` via the unified
/// [`commonwealth_api::routes_internal::spawn_corpus_install`]
/// helper.
///
/// Used by the auto-collaborate loop to pick up partition-of-self
/// dirs that no other process is currently ingesting — typically
/// after a daemon restart while the Desktop app is closed, or in a
/// headless CLI daemon that never had a Desktop tab to drive the
/// install. Sharing the helper with Desktop's `/internal/corpus/install`
/// route means there is exactly one spawn path, so the cancel route,
/// the progress map, and `active_ingests` bookkeeping all stay
/// consistent regardless of who initiated the install.
async fn spawn_local_ingest(state: AppState, corpus_id: String) {
    use commonwealth_api::routes_internal::spawn_corpus_install;
    let spawned = spawn_corpus_install(state, corpus_id.clone()).await;
    if spawned {
        tracing::info!(
            corpus = %corpus_id,
            "auto_ingest: kicked off local ingest via unified install helper"
        );
    } else {
        tracing::debug!(
            corpus = %corpus_id,
            "auto_ingest: unified install helper reported already-active — leaving alone"
        );
    }
}

// ── Pull-based work queue peer side ─────────────────────────────────
//
// The coordinator gossips a pull-based handoff via its MeshStore under
// `corpus-engine / handoff:{handoff_id}` with `phase: Open` and empty
// `partitions`. Every auto-ingest tick, each peer scans that namespace,
// filters to handoffs it's compatible with (embed model match, not yet
// pulling), and spawns one `pull_loop` task per newly-discovered handoff.
// The loop repeatedly POSTs `/internal/corpus/next_unit` until the queue
// drains, running `ingest_with_overrides` on each leased unit and
// heartbeating while work is in flight.

/// Heartbeat cadence: one third of LEASE_MS. Matches what's documented
/// in commonwealth_core::knowledge::LEASE_MS (5 minutes → 100s heartbeat).
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(100);

/// How many consecutive 5xx responses to `next_unit` we tolerate before
/// giving up on a handoff. Protects against the case where the coordinator
/// crashed and can't serve the queue.
const MAX_NEXT_UNIT_5XX: u32 = 5;

/// Scan gossip for open pull-based handoffs and spawn pull loops for any
/// the local node is eligible to pull from. Called every auto_ingest tick.
async fn discover_and_spawn_pull_loops(state: AppState, self_id: NodeId, daemon_port: u16) {
    // Read the local embed model. If missing, we can't match any
    // handoff — skip silently (peer is still bootstrapping).
    let Some(local_embed) = state.inner.inference_store.get_local_embed_model() else {
        return;
    };

    let entries = match state.inner.mesh_store.scan("corpus-engine", "handoff:") {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(error = %e, "pull_loops: mesh_store scan failed");
            return;
        }
    };

    let active = state.inner.active_pull_loops.read().await;
    let already_running: HashSet<HandoffId> = active.clone();
    drop(active);

    for entry in entries {
        let handoff: IngestionHandoff = match serde_json::from_slice(&entry.value) {
            Ok(h) => h,
            Err(e) => {
                tracing::debug!(key = %entry.key, error = %e, "pull_loops: bad handoff blob");
                continue;
            }
        };

        // Only pull-based handoffs (new_queue sets partitions empty and
        // phase to one of the queue states). Skip legacy static handoffs.
        if !handoff.is_queue_mode() {
            continue;
        }
        // Only Open handoffs accept new pulls. Draining/Merging means the
        // coordinator is waiting for in-flight leases to settle; we don't
        // need to enroll.
        if !matches!(handoff.phase, HandoffPhase::Open) {
            continue;
        }
        // Skip handoffs we're the coordinator for — we pull via direct
        // in-process calls (not HTTP) elsewhere; a self-loop would work
        // but adds no value and pollutes the logs.
        if handoff.merge_leader == Some(self_id) {
            continue;
        }
        // Must match embed model exactly. EmbedModelInfo equality covers
        // model_id, dimensions, pooling, normalization.
        if handoff.embed_model != local_embed {
            tracing::debug!(
                handoff = %handoff.handoff_id,
                peer_model = %handoff.embed_model.model_id,
                local_model = %local_embed.model_id,
                "pull_loops: skipping handoff — embed model mismatch"
            );
            continue;
        }
        if already_running.contains(&handoff.handoff_id) {
            continue;
        }

        // Look up coordinator URL from the mesh member record.
        let Some(coordinator_id) = handoff.merge_leader else {
            tracing::debug!(handoff = %handoff.handoff_id, "pull_loops: handoff has no merge_leader");
            continue;
        };
        let coordinator_url = {
            let mesh = state.inner.mesh.read().await;
            mesh.members
                .get(&coordinator_id)
                .and_then(|m| best_peer_url(&m.addresses.iter().copied().collect::<Vec<_>>()))
        };
        let Some(coordinator_url) = coordinator_url else {
            tracing::warn!(
                handoff = %handoff.handoff_id,
                coordinator = %coordinator_id,
                "pull_loops: coordinator has no reachable address — skipping"
            );
            continue;
        };

        // Mark as running before spawn so concurrent ticks don't double-spawn.
        state
            .inner
            .active_pull_loops
            .write()
            .await
            .insert(handoff.handoff_id);

        let state_clone = state.clone();
        let handoff_clone = handoff.clone();
        let coord_url = coordinator_url.clone();
        tokio::spawn(async move {
            pull_loop(state_clone, handoff_clone, self_id, coord_url, daemon_port).await;
        });

        tracing::info!(
            handoff = %handoff.handoff_id,
            corpus = %handoff.corpus_id,
            coordinator = %coordinator_id,
            url = %coordinator_url,
            "pull_loops: spawned pull loop"
        );
    }
}

/// Pick the best reachable URL for a peer. Prefers Tailscale addresses
/// (CGNAT 100.64.0.0/10 or ULA fd7a:115c:a1e0::/48) which work across
/// Wi-Fi networks where LAN IPs silently fail. Mirrors the logic in
/// `corpus_collaborate`'s peer-dispatch path.
fn best_peer_url(addrs: &[std::net::SocketAddr]) -> Option<String> {
    if addrs.is_empty() {
        return None;
    }
    let mut sorted: Vec<std::net::SocketAddr> = addrs.iter().copied().collect();
    sorted.sort_by_key(|addr| match addr.ip() {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            if o[0] == 100 && (o[1] & 0xc0) == 64 { 0 } else { 1 }
        }
        std::net::IpAddr::V6(v6) => {
            let s = v6.segments();
            if s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0 { 0 } else { 2 }
        }
    });
    let best = sorted[0];
    let host = match best.ip() {
        std::net::IpAddr::V4(_) => best.ip().to_string(),
        std::net::IpAddr::V6(v6) => format!("[{v6}]"),
    };
    Some(format!("http://{host}:{}", best.port()))
}

/// Inner pull loop. Runs until the queue drains (204 from `next_unit`)
/// or we hit an irrecoverable error. Drops this node's entry from
/// `active_pull_loops` on exit so the next tick can re-enroll if the
/// coordinator reopens the same handoff.
async fn pull_loop(
    state: AppState,
    handoff: IngestionHandoff,
    self_id: NodeId,
    coordinator_url: String,
    _daemon_port: u16,
) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("build reqwest client");
    let handoff_id = handoff.handoff_id;
    let corpus_id = handoff.corpus_id.clone();
    let recipe_id = handoff.recipe_id.clone();
    let mut consecutive_5xx = 0u32;

    tracing::info!(
        handoff = %handoff_id,
        corpus = %corpus_id,
        coordinator = %coordinator_url,
        "pull_loop: started"
    );

    loop {
        // Pull next unit.
        let next_req = serde_json::json!({
            "handoff_id": handoff_id,
            "peer_id": self_id,
        });
        let resp = match client
            .post(format!("{coordinator_url}/internal/corpus/next_unit"))
            .json(&next_req)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    handoff = %handoff_id,
                    error = %e,
                    "pull_loop: next_unit request failed — retrying"
                );
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let status = resp.status();
        if status == reqwest::StatusCode::NO_CONTENT {
            tracing::info!(
                handoff = %handoff_id,
                "pull_loop: queue drained — exiting"
            );
            break;
        }
        if status == reqwest::StatusCode::NOT_FOUND {
            tracing::info!(
                handoff = %handoff_id,
                "pull_loop: handoff no longer registered on coordinator — exiting"
            );
            break;
        }
        if status.is_server_error() {
            consecutive_5xx += 1;
            if consecutive_5xx >= MAX_NEXT_UNIT_5XX {
                tracing::warn!(
                    handoff = %handoff_id,
                    consecutive_5xx,
                    "pull_loop: too many 5xx responses — giving up"
                );
                break;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        consecutive_5xx = 0;

        // Parse the leased payload. We use a minimal inline struct here
        // rather than importing the handler's response enum — deserialize
        // from untagged variants can be flaky when field sets overlap.
        #[derive(serde::Deserialize)]
        struct LeasedPayload {
            unit_id: UnitId,
            unit: WorkUnit,
            #[allow(dead_code)]
            lease_expires_at_ms: u64,
        }
        let payload: LeasedPayload = match resp.json().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    handoff = %handoff_id,
                    error = %e,
                    "pull_loop: could not parse next_unit response"
                );
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let unit_id = payload.unit_id;
        let unit = payload.unit;

        // Spawn heartbeat task; it reads a shared CancellationFlag to
        // trigger abort if the coordinator reclaims our lease (410 Gone).
        let cancel_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hb_task = spawn_heartbeat(
            client.clone(),
            coordinator_url.clone(),
            handoff_id,
            self_id,
            unit_id,
            cancel_flag.clone(),
        );

        // Run the ingest under a corpus-engine CancellationFlag so the
        // ingest loop (which polls it at document/batch boundaries) can
        // stop cleanly when the heartbeat tells us the lease is gone.
        let engine = state.inner.corpus_engine.as_ref().expect(
            "pull_loop: corpus_engine must be present — auto_ingest already checks this"
        );
        let engine_cancel: CancellationFlag = engine.cancel_registry().register(&corpus_id);

        // Bridge: spawn a watcher that flips the engine's cancel flag
        // when the HTTP cancel_flag fires.
        let engine_cancel_clone = engine_cancel.clone();
        let bridge_cancel = cancel_flag.clone();
        let bridge = tokio::spawn(async move {
            loop {
                if bridge_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    engine_cancel_clone.cancel();
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        });

        let (file_indices, article_range) = unit.to_ingest_args();
        let output_path = engine.partition_path(&corpus_id);
        let ingest_result = engine
            .ingest_with_overrides(
                &recipe_id,
                file_indices,
                article_range,
                &output_path,
                None,
                Some(unit_id),
            )
            .await;

        hb_task.abort();
        bridge.abort();
        engine.cancel_registry().unregister(&corpus_id);

        // Report outcome. A 409 here means the reaper already requeued
        // our unit — the other peer's output will either overlap (merge
        // dedupes by content_hash + unit_id) or be the authoritative copy.
        let (outcome, reason) = match ingest_result {
            Ok(_) => (CompleteOutcome::Complete, None),
            Err(e) => {
                tracing::warn!(
                    handoff = %handoff_id,
                    unit_id,
                    error = %e,
                    "pull_loop: ingest_with_overrides failed"
                );
                (CompleteOutcome::Failed, Some(e.to_string()))
            }
        };
        let complete_req = serde_json::json!({
            "handoff_id": handoff_id,
            "peer_id": self_id,
            "unit_id": unit_id,
            "outcome": outcome,
            "reason": reason,
        });
        match client
            .post(format!("{coordinator_url}/internal/corpus/complete_unit"))
            .json(&complete_req)
            .send()
            .await
        {
            Ok(r) if r.status() == reqwest::StatusCode::CONFLICT => {
                tracing::info!(
                    handoff = %handoff_id,
                    unit_id,
                    "pull_loop: complete_unit returned 409 — lease was already reclaimed"
                );
            }
            Ok(r) if r.status().is_success() => {
                tracing::debug!(
                    handoff = %handoff_id,
                    unit_id,
                    ?outcome,
                    "pull_loop: unit completed"
                );
            }
            Ok(r) => {
                tracing::warn!(
                    handoff = %handoff_id,
                    unit_id,
                    status = %r.status(),
                    "pull_loop: unexpected complete_unit response"
                );
            }
            Err(e) => {
                tracing::warn!(
                    handoff = %handoff_id,
                    unit_id,
                    error = %e,
                    "pull_loop: complete_unit request failed"
                );
            }
        }
    }

    // Drop from active set so the next tick can re-enroll if the handoff
    // reopens (e.g. coordinator restart).
    state
        .inner
        .active_pull_loops
        .write()
        .await
        .remove(&handoff_id);
}

/// Spawn a heartbeat task for the duration of a single unit's ingest.
/// Fires `POST /internal/corpus/heartbeat` every `HEARTBEAT_INTERVAL`.
/// On 410 Gone, sets the cancellation flag so the ingest loop aborts.
fn spawn_heartbeat(
    client: reqwest::Client,
    coordinator_url: String,
    handoff_id: HandoffId,
    peer_id: NodeId,
    unit_id: UnitId,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(HEARTBEAT_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Skip the immediate first tick — the lease starts fresh at
        // next_unit return, no need to heartbeat until ~100s in.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let body = serde_json::json!({
                "handoff_id": handoff_id,
                "peer_id": peer_id,
                "unit_id": unit_id,
            });
            match client
                .post(format!("{coordinator_url}/internal/corpus/heartbeat"))
                .json(&body)
                .send()
                .await
            {
                Ok(r) if r.status() == reqwest::StatusCode::GONE => {
                    tracing::warn!(
                        handoff = %handoff_id,
                        unit_id,
                        "heartbeat: 410 Gone — lease reclaimed, aborting unit"
                    );
                    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    return;
                }
                Ok(r) if r.status().is_success() => {
                    // Lease renewed; nothing to do.
                }
                Ok(r) => {
                    tracing::debug!(
                        handoff = %handoff_id,
                        unit_id,
                        status = %r.status(),
                        "heartbeat: non-success response"
                    );
                }
                Err(e) => {
                    tracing::debug!(
                        handoff = %handoff_id,
                        unit_id,
                        error = %e,
                        "heartbeat: request failed"
                    );
                }
            }
        }
    })
}
