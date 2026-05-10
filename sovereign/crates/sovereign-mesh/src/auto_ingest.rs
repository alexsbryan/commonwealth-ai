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
    // Operator-toggleable kill switch. The env var seeds the
    // runtime atomic on `AppState`; the loop itself reads the
    // atomic on every tick so `POST /internal/mesh/quiesce` can
    // flip participation without a daemon restart. Default off —
    // peer collaboration runs normally.
    let env_quiesce = std::env::var("SOVEREIGN_DISABLE_AUTO_COLLAB")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if env_quiesce {
        state.set_mesh_quiesced(true);
        tracing::warn!(
            "auto_ingest: SOVEREIGN_DISABLE_AUTO_COLLAB set — \
             starting in quiesced mode; flip via POST /internal/mesh/quiesce \
             to rejoin without a restart"
        );
    }
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
        // Quiesce gate. When the operator has flipped the runtime
        // flag (or the SOVEREIGN_DISABLE_AUTO_COLLAB env var seeded
        // it at boot), skip both pull-discovery and dispatch. The
        // tick still runs at CHECK_INTERVAL cadence so re-enabling
        // takes effect within one loop iteration.
        if state.mesh_quiesced() {
            tracing::debug!("auto_ingest: mesh quiesced — skipping tick");
            first_iteration = false;
            tokio::time::sleep(CHECK_INTERVAL).await;
            continue;
        }

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

        // ── Stranded-partition recovery (proactive) ──────────────
        //
        // Detect corpora with `<corpus>-partition-*/` dirs on disk
        // but no canonical, and try to merge them into a canonical
        // ourselves. The deadlock this catches: the queue-mode
        // ingest's handoff blob lives in the in-memory MeshStore,
        // which is wiped on every daemon restart; if no peer in
        // the mesh still gossips the blob when we come back up,
        // the dispatcher's existing recovery path
        // (`find_local_handoff_for_corpus → spawn_queue_merge`) has
        // nothing to fire from. Without a proactive merge, the
        // corpus stays stranded indefinitely even though every
        // shard's chunks are present locally across the partition
        // dirs.
        //
        // This scan fires every CHECK_INTERVAL (30s) but the
        // recovery primitive itself enforces a 5-minute per-corpus
        // cooldown, so a long-running merge isn't relaunched.
        // `try_recover_stranded_partitions` short-circuits cheaply
        // when nothing to do (no partitions / canonical exists).
        let stranded = engine.corpora_with_stranded_partitions();
        for corpus_id in &stranded {
            // Phase 6 canonical-sync: before falling through to a
            // local merge (which may produce an incomplete canonical
            // when this node's partitions don't cover every shard),
            // scan gossip for a peer advertising a canonical with
            // BETTER coverage. If found, pull from that peer instead
            // of merging locally. Avoids the case where two peers
            // each have a partial canonical and both keep merging
            // their partial state forever.
            if let Some(lead) =
                find_best_peer_canonical(&state, corpus_id).await
            {
                tracing::info!(
                    corpus = %corpus_id,
                    candidate_urls = ?lead.candidate_urls,
                    fingerprint = %&lead.fingerprint[..lead.fingerprint.len().min(12)],
                    coverage_ratio = ?lead.coverage_ratio,
                    chunk_count = lead.chunk_count,
                    "auto_ingest: peer has healthier canonical — attempting pull"
                );
                match crate::canonical_pull::pull_canonical_from_peer(
                    &lead.candidate_urls,
                    corpus_id,
                    engine.index_dir(),
                    Some(&lead.fingerprint),
                )
                .await
                {
                    Ok(report) => {
                        tracing::info!(
                            corpus = %corpus_id,
                            peer = %report.peer_url,
                            bytes_uncompressed = report.bytes_uncompressed,
                            "auto_ingest: pulled canonical from peer — local merge skipped"
                        );
                        // Skip the local-merge path; the canonical
                        // is in place. Next tick of the loop will
                        // re-publish gossip naturally.
                        continue;
                    }
                    Err(e) => {
                        // Connection-level failures (all peer
                        // addresses unreachable) are TRANSIENT —
                        // we WILL retry next tick. Falling through
                        // to local merge here is what produced the
                        // 17/38 partial canonical bug RuggedFox
                        // hit: even when our partition meta lacks
                        // total_shards (so the IncompleteCoverage
                        // gate skips), we KNOW from gossip that a
                        // peer has healthier coverage. Producing a
                        // partial canonical ourselves and then re-
                        // advertising it on gossip pollutes the
                        // mesh's canonical-sync convergence — every
                        // peer ends up with a different "complete"
                        // canonical and they fight forever.
                        //
                        // Defer to the next tick; the gossip layer
                        // is publishing reachability and the next
                        // attempt will likely find an address that
                        // works. The operator can manually run
                        // `sovereign corpus merge-partitions <id>`
                        // if they want to force a partial merge.
                        tracing::warn!(
                            corpus = %corpus_id,
                            error = %e,
                            "auto_ingest: peer pull failed — deferring local merge \
                             (peer advertises healthier canonical; will retry next tick). \
                             Override with `sovereign corpus merge-partitions {}`.",
                            corpus_id,
                        );
                        continue;
                    }
                }
            }

            let outcome = commonwealth_api::auto_recover::try_recover_stranded_partitions(
                engine.index_dir(),
                corpus_id,
            )
            .await;
            match outcome {
                commonwealth_api::auto_recover::RecoveryOutcome::Recovered { chunks, shards_covered } => {
                    tracing::info!(
                        corpus = %corpus_id,
                        chunks,
                        shards_covered,
                        "auto_ingest: proactive stranded-partition recovery SUCCEEDED — \
                         canonical now exists; gossip will re-advertise"
                    );
                }
                commonwealth_api::auto_recover::RecoveryOutcome::Failed(err) => {
                    tracing::warn!(
                        corpus = %corpus_id,
                        recovery_error = %err,
                        "auto_ingest: proactive stranded-partition recovery FAILED — \
                         operator can run `sovereign corpus merge-partitions {}` manually",
                        corpus_id,
                    );
                }
                commonwealth_api::auto_recover::RecoveryOutcome::IncompleteCoverage {
                    ..
                } => {
                    // auto_recover already logged the detailed WARN
                    // with covered/total/missing. Stay quiet here so
                    // the 30s tick doesn't spam logs while we wait
                    // for the peer with full coverage to produce
                    // the canonical (or for the missing shards to
                    // land locally via collaborate-pull).
                }
                _ => {
                    // AlreadyHasCanonical / NotEnoughPartitions /
                    // InCooldown — quiet on the happy paths.
                }
            }
        }

        let in_progress_vec: Vec<String> = engine.in_progress_ingestions();
        let in_progress: HashSet<String> = in_progress_vec.iter().cloned().collect();
        let new_ingest_appeared = in_progress
            .iter()
            .any(|id| !last_known_in_progress.contains(id));
        last_known_in_progress = in_progress.clone();

        // Publish this node's per-corpus `processed_shards` into the
        // gossip-replicated MeshStore so the coordinator can union
        // every peer's progress when computing `remaining` in
        // `corpus_collaborate`. Without this, each peer dispatches
        // from its own local view (`engine.corpus_processed_shards`
        // only walks LOCAL partition dirs) and queues redundant
        // work that another peer already did. Observed in the wild:
        // 8 of 33 distinct shards processed twice across a two-peer
        // Wikipedia ingest because neither peer knew the other's
        // progress until the merge step (which had its own bug
        // and never ran).
        //
        // Key shape: `processed_shards:<corpus_id>:<self_node_id_hex>`.
        // The `:<node_id>` suffix is load-bearing — without it, every
        // peer writes the same key and LWW on the gossip layer keeps
        // only the last-writer's entry. With it, each peer has its
        // own gossip slot and the dispatch-side scan unions across
        // them naturally. Publish runs every CHECK_INTERVAL; cheap
        // (a small JSON read of the partition meta + a MeshStore
        // write).
        publish_local_processed_shards(&state, engine, self_id, &in_progress_vec).await;

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

            // Peer-only node: no source data means no collaborate role here.
            // The coordinator will send ingest_partition when it's ready.
            if !has_local_source {
                tracing::debug!(
                    corpus = %corpus_id,
                    "auto_ingest: no local source data — skipping collaborate (peer node, waiting for ingest_partition)"
                );
                continue;
            }

            // ── 1. Dispatch collaboration FIRST ─────────────────────────
            //
            // Register the pull-queue handoff before any decision about
            // spawning a solo local ingest. The previous ordering fired
            // spawn_local_ingest first, registered the handoff a few
            // hundred ms later, then on the next tick the pull_loop
            // discovered the handoff and began self-pulling — racing
            // the already-running solo task on the same partition dir.
            // That collision surfaced as an endless "Table 'chunks'
            // already exists" crash loop on every unit after the first
            // (see `pull_loop: ingest_with_overrides failed` in prod
            // logs from 2026-04-21). Running corpus_collaborate first
            // lets `has_active_queue_handoff` below observe the handoff
            // we just registered and cleanly hand ownership to pull_loops.
            let should_collab = !(active_ingests.contains(corpus_id)
                && !new_peer_appeared
                && !new_ingest_appeared)
                && !(triggered.get(corpus_id).map_or(false, |t| t.elapsed() < COOLDOWN)
                    && !new_peer_appeared
                    && !new_ingest_appeared);

            if should_collab {
                tracing::info!(
                    corpus = %corpus_id,
                    new_peer = new_peer_appeared,
                    is_restart = triggered.get(corpus_id).is_none(),
                    "auto_ingest: triggering collaboration"
                );

                let url = format!(
                    "http://127.0.0.1:{daemon_port}/internal/corpus/collaborate"
                );
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

            // ── 2. Ensure local work is running ─────────────────────────
            //
            // The daemon is the single owner of "resume my in-progress
            // partition-of-self" under the unified ingest primitive —
            // Desktop used to spawn its own engine.ingest() task, but
            // that process dies when the user closes the app. If the
            // daemon survives and sees a partition-of-self dir marked
            // in-progress, it has to pick up the work itself; otherwise
            // Wikipedia never finishes unless the user leaves Desktop
            // open for hours.
            //
            // We skip this path when (a) an ingest task is already
            // tracked in active_ingests — avoids double-spawn racing
            // the LanceDB writer — (b) there is no partition-of-self
            // directory, which means the corpus id came from legacy
            // canonical state that the engine's ingest() will continue
            // writing in place, or (c) a queue-mode handoff is present
            // — pull_loops (local self-pull + remote peers) will own
            // the ingest and a solo writer would deadlock the partition.
            if !active_ingests.contains(corpus_id) {
                let partition_path = engine.partition_path(corpus_id);
                if partition_path.exists() {
                    // When a queue-mode handoff for this corpus is visible in
                    // mesh_store, a pull_loop (spawned either locally or on
                    // peers) owns the ingest. Skipping spawn_local_ingest
                    // prevents two loops from writing to the same partition
                    // dir and fighting over the single embed slot — the
                    // reason LittleMac's pre-fix throughput stayed at CPU
                    // rate even while it held a queue lease.
                    //
                    // Because the collaborate dispatch above ran first, a
                    // freshly-registered handoff from *this* tick is
                    // already visible to this check — closing the
                    // spawn-before-register race window.
                    if has_active_queue_handoff(&state, corpus_id).await {
                        tracing::info!(
                            corpus = %corpus_id,
                            "auto_ingest: queue handoff active — pull_loops own this corpus, skipping spawn_local_ingest"
                        );
                    } else {
                        spawn_local_ingest(state.clone(), corpus_id.clone()).await;
                    }
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
/// Returns `true` when mesh_store carries an Open-phase, queue-mode
/// IngestionHandoff for `corpus_id`. Used by the tick loop to avoid
/// double-ingesting: if the queue owns this corpus, neither coordinator
/// nor peer should run a separate spawn_local_ingest — the pull_loop
/// (self or remote) is the single writer into `<corpus>-partition-<node>/`.
async fn has_active_queue_handoff(state: &AppState, corpus_id: &str) -> bool {
    let entries = match state.inner.mesh_store.scan("corpus-engine", "handoff:") {
        Ok(e) => e,
        Err(_) => return false,
    };
    for entry in entries {
        let handoff: IngestionHandoff = match serde_json::from_slice(&entry.value) {
            Ok(h) => h,
            Err(_) => continue,
        };
        if handoff.corpus_id == corpus_id
            && handoff.is_queue_mode()
            && matches!(handoff.phase, HandoffPhase::Open)
        {
            return true;
        }
    }
    false
}

async fn publish_local_processed_shards(
    state: &AppState,
    engine: &std::sync::Arc<corpus_engine::CorpusEngine>,
    self_id: NodeId,
    in_progress: &[String],
) {
    for corpus_id in in_progress {
        let local: Vec<usize> = engine.corpus_processed_shards(corpus_id);
        if local.is_empty() {
            // Nothing to announce yet — skip rather than publish an
            // empty array. Avoids the `last_writer = empty` LWW
            // hazard if the publisher loses its meta file mid-run.
            continue;
        }
        let key = commonwealth_state::processed_shards_key(corpus_id, self_id);
        let payload = match serde_json::to_vec(&local) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    corpus = %corpus_id,
                    error = %e,
                    "auto_ingest: serialize processed_shards failed"
                );
                continue;
            }
        };
        if let Err(e) = state.inner.mesh_store.set(
            commonwealth_state::PROCESSED_SHARDS_APP_ID,
            &key,
            payload.into(),
            self_id,
        ) {
            tracing::warn!(
                corpus = %corpus_id,
                key = %key,
                error = %e,
                "auto_ingest: publish processed_shards failed"
            );
            continue;
        }
        tracing::debug!(
            corpus = %corpus_id,
            shard_count = local.len(),
            key = %key,
            "auto_ingest: published processed_shards"
        );
    }
}

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

/// How many consecutive `next_unit` failures we tolerate before giving up
/// on a handoff. Counts both 5xx responses (coordinator alive but broken)
/// and connection errors (coordinator unreachable, port changed, stuck
/// reqwest pool). Without counting connection errors, a coordinator
/// restart that leaves the old port unresponsive produces an immortal
/// pull_loop that retries every 15s forever — observed in the wild as
/// 1700+ retries over 7 hours, blocking re-discovery of the same
/// handoff_id from gossip and (because the mesh_store entry survives in
/// gossip and gets re-implanted into the coordinator's store on reconnect)
/// allowing two ingests to run on the same partition once the queue
/// reopens. Breaking out lets the next auto_ingest tick respawn with a
/// fresh `reqwest::Client`, which also recovers from any stuck connection
/// pool state.
const MAX_NEXT_UNIT_FAILURES: u32 = 5;

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
        // Coordinators pull from their own queue too. Without this, a
        // fast Metal-backed coordinator sits idle after registering the
        // queue while a slow CPU peer chews one unit at a time — no
        // work-sharing benefit. The self-pull is a localhost HTTP hop
        // (cheap) and exercises the same code path as a remote pull,
        // so there's no special case downstream.
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

/// Pick the best reachable URL for a peer. Delegates to
/// `peer_addr::rank` so this matches the order used by gossip and
/// inference fallback. The previous local sort had IPv4 CGNAT and
/// IPv6 ULA tied at rank 0, leading to nondeterministic IPv6-first
/// picks that broke on hosts without IPv6 routing.
fn best_peer_url(addrs: &[std::net::SocketAddr]) -> Option<String> {
    let sorted = crate::peer_addr::sorted_addresses(addrs);
    let best = *sorted.first()?;
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
    let mut consecutive_failures = 0u32;

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
                consecutive_failures += 1;
                if consecutive_failures >= MAX_NEXT_UNIT_FAILURES {
                    tracing::warn!(
                        handoff = %handoff_id,
                        consecutive_failures,
                        error = %e,
                        "pull_loop: too many next_unit connection failures — giving up"
                    );
                    break;
                }
                tracing::warn!(
                    handoff = %handoff_id,
                    consecutive_failures,
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
            // Remove the stale handoff blob from the local mesh_store
            // so `discover_and_spawn_pull_loops` stops respawning this
            // loop on every auto_ingest tick. Without this cleanup, a
            // handoff the coordinator deregistered keeps churning at
            // 30-second cadence: spawn → 404 → exit → drop from
            // `active_pull_loops` → next tick rescans the same blob in
            // local gossip state → spawn again. That respawn loop is
            // the source of the multi-MB log noise the operator was
            // staring at. Re-creation by the coordinator (gossip
            // re-propagation if the handoff genuinely re-opens) is
            // automatic — `merge_entry` accepts new versions — so the
            // delete is safe.
            let key = format!("handoff:{}", handoff_id);
            if let Err(e) = state.inner.mesh_store.delete("corpus-engine", &key) {
                tracing::warn!(
                    handoff = %handoff_id,
                    error = %e,
                    "pull_loop: failed to delete stale handoff from mesh_store"
                );
            }
            break;
        }
        if status.is_server_error() {
            consecutive_failures += 1;
            if consecutive_failures >= MAX_NEXT_UNIT_FAILURES {
                tracing::warn!(
                    handoff = %handoff_id,
                    consecutive_failures,
                    "pull_loop: too many 5xx responses — giving up"
                );
                break;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        consecutive_failures = 0;

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
        // Publish per-corpus progress into AppState so the
        // desktop's `/internal/corpus/status` poller sees live
        // chunk/doc counts. Without this, queue-mode runs leave
        // `corpus_progress` empty and the desktop UI falls back
        // to the on-disk inference path, which renders "Starting…"
        // because unit-scoped runs don't update the partition-wide
        // `committed_iter_pos`. The fallback string was technically
        // honest ("we don't know the live state") but operationally
        // misleading (the real ingest had ~1.5M chunks committed).
        let progress_state = state.clone();
        let progress_cid = corpus_id.clone();
        let progress_cb: corpus_engine::ProgressCallback = Box::new(move |p| {
            let progress_state = progress_state.clone();
            let progress_cid = progress_cid.clone();
            tokio::spawn(async move {
                progress_state
                    .inner
                    .corpus_progress
                    .write()
                    .await
                    .insert(progress_cid, p);
            });
        });
        let ingest_result = engine
            .ingest_with_overrides(
                &recipe_id,
                file_indices,
                article_range,
                &output_path,
                Some(progress_cb),
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

// ─── Phase 6 canonical-sync: peer-pull preference helper ────────

/// Information about a peer's canonical for a given corpus_id,
/// extracted from their gossiped `hosted_corpora`. Returned by
/// [`find_best_peer_canonical`] when a peer's canonical is judged
/// healthier than what local merge would produce.
///
/// `candidate_urls` is the full set of base URLs published by the
/// peer (LAN, Tailscale CGNAT, IPv6 ULA — whatever they announced).
/// The pull function tries each in turn until one succeeds, so we
/// remain reachable across mixed network topologies. Empty list
/// means the peer's gossip carried no addresses, which is a
/// degenerate case the caller should skip.
#[derive(Debug, Clone)]
pub(crate) struct PeerCanonicalLead {
    pub candidate_urls: Vec<String>,
    pub fingerprint: String,
    pub coverage_ratio: Option<f64>,
    pub chunk_count: u64,
}

/// Walk gossipped peers and return the most attractive canonical
/// for `corpus_id`, if any.
///
/// "Attractive" = has a `canonical_fingerprint` (so we can validate
/// the pull) AND ranks better than local on whichever heuristic
/// applies:
///   - **Sharded corpora**: highest `coverage_ratio` (processed /
///     total). Robust to legitimate corpus updates that shrink the
///     chunk set. Ties broken by chunk_count.
///   - **Non-sharded**: highest chunk_count. Coarse but fine for
///     the corpora that don't ship a shard manifest.
///
/// Returns `None` when no peer advertises a fingerprint for this
/// corpus, or when no peer beats whatever local could produce.
/// "What local could produce" is computed by `corpora_with_stranded_partitions`'s
/// caller — this function is invoked only for stranded corpora,
/// so any peer canonical is by definition better than the (zero)
/// local canonical. The ranking is among PEERS, picking the best
/// remote source.
async fn find_best_peer_canonical(
    state: &commonwealth_api::state::AppState,
    corpus_id: &str,
) -> Option<PeerCanonicalLead> {
    let mesh = state.inner.mesh.read().await;
    let self_id = state.self_node_id();
    let mut best: Option<PeerCanonicalLead> = None;
    for member in mesh.members.values() {
        // Skip ourselves — gossip echoes our own capability report.
        if member.node_id == self_id {
            continue;
        }
        // Skip offline peers — even if they advertised hosted_corpora
        // recently, the pull will time out. The mesh's status field
        // is updated by gossip-driven liveness probes.
        if !matches!(
            member.status,
            commonwealth_core::mesh::NodeStatus::Online
                | commonwealth_core::mesh::NodeStatus::Busy
        ) {
            continue;
        }
        for shard_info in &member.capabilities.hosted_corpora {
            if shard_info.corpus_id != corpus_id {
                continue;
            }
            let Some(fp) = shard_info.canonical_fingerprint.as_deref() else {
                // Peer hosts the corpus but didn't stamp a
                // fingerprint yet (legacy install pre-Phase-6).
                // Skip: we can't validate the pull without one.
                continue;
            };
            if fp.is_empty() {
                continue;
            }
            // Build the full candidate-URL list from every published
            // address. Whatever ports gossip recorded are ignored —
            // the canonical-stream endpoint lives on the internal
            // mesh port (9742) regardless of how the peer happens
            // to bind its client port. The pull function tries each
            // in turn so a topology change (peer roams off LAN onto
            // Tailscale, etc.) doesn't strand the request on a
            // dead address.
            //
            // IPv6 addresses must be bracketed in URLs.
            let candidate_urls: Vec<String> = member
                .addresses
                .iter()
                .map(|addr| {
                    let ip = addr.ip();
                    if ip.is_ipv6() {
                        format!("http://[{ip}]:9742")
                    } else {
                        format!("http://{ip}:9742")
                    }
                })
                .collect();
            if candidate_urls.is_empty() {
                continue;
            }

            let candidate = PeerCanonicalLead {
                candidate_urls,
                fingerprint: fp.to_string(),
                coverage_ratio: shard_info.coverage_ratio(),
                chunk_count: shard_info.chunk_count,
            };

            // Compare to current best. Coverage ratio wins when
            // both candidates have it; chunk_count is the
            // tiebreaker / fallback.
            best = Some(match best {
                None => candidate,
                Some(prev) => {
                    let prev_better = match (
                        prev.coverage_ratio,
                        candidate.coverage_ratio,
                    ) {
                        (Some(p), Some(c)) => p >= c,
                        (Some(_), None) => true,
                        (None, Some(_)) => false,
                        (None, None) => prev.chunk_count >= candidate.chunk_count,
                    };
                    if prev_better {
                        prev
                    } else {
                        candidate
                    }
                }
            });
        }
    }
    best
}
