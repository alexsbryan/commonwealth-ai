use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use commonwealth_api::state::AppState;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::NodeStatus;

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
