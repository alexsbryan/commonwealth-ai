// SPDX-License-Identifier: AGPL-3.0-or-later
//! Collaborative corpus ingestion entry point.
//!
//! `POST /internal/corpus/collaborate` is the operator-facing kickoff:
//! given a `corpus_id`, it plans a partition across compatible mesh
//! peers and either (a) seeds a pull-based work queue (the default)
//! or (b) directly notifies each peer with a static partition (the
//! legacy push path, kept under `SOVEREIGN_USE_LEGACY_PARTITION=1`
//! for rolling-upgrade compatibility).

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use commonwealth_core::ids::NodeId;
use commonwealth_core::knowledge::IngestionHandoff;
use commonwealth_core::mesh::NodeStatus;
use commonwealth_inference::scheduler::knowledge_assignment::{
    build_work_units_hf, build_work_units_jsonl_sharded, build_work_units_jsonl_single,
    plan_collaborative_ingestion, plan_collaborative_ingestion_jsonl,
    plan_collaborative_ingestion_jsonl_sharded, CollaborativeIngestionError,
};

use crate::state::AppState;

use super::{find_local_handoff_for_corpus, spawn_queue_merge, ErrorBody, IngestPartitionRequest};

/// Pull-based work queue is the default. Set `SOVEREIGN_USE_LEGACY_PARTITION=1`
/// to fall back to the static per-peer partitioning path — kept for
/// rolling-upgrade compatibility until the legacy `ingest_partition`
/// route, `PartitionStatus`, and the `partitions: Vec<IngestionPartition>`
/// gossip field are removed.
const LEGACY_PARTITION_ENV: &str = "SOVEREIGN_USE_LEGACY_PARTITION";

pub(super) fn use_pull_queue() -> bool {
    !std::env::var(LEGACY_PARTITION_ENV)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

// ── Collaborative corpus ingestion ────────────────────────

/// POST /internal/corpus/collaborate — kick off collaborative ingestion.
///
/// Reads the source-file manifest for `corpus_id`, plans the partition
/// across compatible mesh peers, notifies each peer via
/// `POST /internal/corpus/ingest_partition`, and returns the full
/// `IngestionHandoff` so the CLI can display the partition table.
pub async fn corpus_collaborate(
    State(state): State<AppState>,
    Json(req): Json<CollaborateRequest>,
) -> Result<Json<IngestionHandoff>, (StatusCode, Json<ErrorBody>)> {
    let engine = state.inner.corpus_engine.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: "no corpus engine available on this node".into(),
            }),
        )
    })?;

    // ── Ephemeral ingest-grant gate ─────────────────────────────────────
    //
    // A shared corpus (`mesh_sharing = true`) collaborates freely, exactly
    // as before. A local-only corpus (`mesh_sharing = false` — e.g. an
    // Obsidian vault or watched folder) may ONLY be shipped to peers under a
    // live, authorizing ephemeral grant: the out-of-band, revocable,
    // user-selected capability that never mutates the corpus's standing
    // local posture. We resolve `mesh_sharing`/`grantable` from the RECIPE
    // (the source of truth, present from registration) rather than the
    // stamped index — the post-create `grantable` stamp may not be written
    // yet during a fresh collaborative ingest.
    let recipe_privacy = engine.load_recipe(req.corpus_id.as_str()).await.map_err(|e| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                error: format!("cannot resolve recipe for corpus '{}': {e}", req.corpus_id),
            }),
        )
    })?;
    if !recipe_privacy.corpus.mesh_sharing {
        // Local-only corpus. Require a live grant that authorizes exactly
        // the requested peer set. `grantable = false` (structural
        // KnowledgeView) can never pass — even a stray grant is refused.
        let now_ms = commonwealth_core::clock::unix_now_millis();
        let requested = req.allowed_peers.clone().unwrap_or_default();
        let authorized = recipe_privacy.corpus.grantable
            && state
                .inner
                .grant_store
                .live(req.corpus_id.as_str(), now_ms)
                .map(|g| g.authorizes(&requested))
                .unwrap_or(false);
        if !authorized {
            return Err((
                StatusCode::FORBIDDEN,
                Json(ErrorBody {
                    error: format!(
                        "corpus '{}' is local-only; a collaborative ingest requires a live \
                         ephemeral grant authorizing the selected peers (none found). Issue \
                         one via POST /internal/corpus/grant first.",
                        req.corpus_id
                    ),
                }),
            ));
        }
    }

    // Build local node view.
    let mesh = state.inner.mesh.read().await;
    let self_id = *state.inner.self_node_id_swap.load_full().as_ref();
    let local_member = mesh.members.get(&self_id).cloned().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                error: "local node not found in mesh".into(),
            }),
        )
    })?;

    let local_embed_model = state
        .inner
        .inference_store
        .get_local_embed_model()
        .ok_or_else(|| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorBody {
                    error: "embed model not configured on this node — cannot plan collaboration"
                        .into(),
                }),
            )
        })?;

    // Include online peers whose *gossiped* embed_model matches ours
    // exactly. Without this upfront filter the coordinator would ship
    // partitions to mismatched peers, they'd reject with 409 (fire-and-
    // forget, so coordinator never learns), and the user would see
    // Machine B do nothing with no diagnostic trail. The match requires:
    //   1. Peer is Online (offline peers can't accept partitions).
    //   2. Peer has gossiped an embed_model (pre-bootstrap peers are
    //      excluded; they'll re-evaluate on the next capability
    //      refresh when they join).
    //   3. Peer's embed_model exactly equals ours (model_id +
    //      dimensions + pooling + normalization — same EmbedModelInfo
    //      equality the peer-side ingest_partition handler already
    //      enforces).
    //
    // We also tally rejections with reasons so the log line below
    // explains *why* each would-be peer is excluded — when the user
    // sees no collaboration, they can look here and see "Machine B
    // has qwen3-embed-0.6b; we have qwen3-embed-4b — mismatch".
    let mut rejected: Vec<(NodeId, &'static str)> = Vec::new();
    let candidates: Vec<_> = mesh
        .members
        .values()
        .filter(|m| m.node_id != self_id)
        .filter_map(|m| {
            if m.status != NodeStatus::Online {
                rejected.push((m.node_id, "offline"));
                return None;
            }
            match m.capabilities.embed_model.as_ref() {
                None => {
                    rejected.push((m.node_id, "no embed_model advertised"));
                    None
                }
                Some(em) if em != &local_embed_model => {
                    rejected.push((m.node_id, "embed_model mismatch"));
                    None
                }
                Some(_) => Some(m.clone()),
            }
        })
        .collect();
    drop(mesh);

    // Per-job peer allowlist (ephemeral grant-scoped ingest). When the
    // caller pins a specific set of helper peers, drop any embed-compatible
    // candidate that isn't on the list — tallying each drop into `rejected`
    // so the glassbox log line below still explains every excluded peer.
    let mut candidates = candidates;
    if let Some(allowed) = req.allowed_peers.as_ref() {
        let allow: std::collections::HashSet<NodeId> = allowed.iter().copied().collect();
        candidates.retain(|m| {
            if allow.contains(&m.node_id) {
                true
            } else {
                rejected.push((m.node_id, "not in per-job allowlist"));
                false
            }
        });
    }

    if !rejected.is_empty() {
        tracing::info!(
            rejected = ?rejected,
            compatible = candidates.len(),
            "corpus_collaborate: candidate filter results"
        );
    }

    let recipe_id = req.recipe_id.as_deref().unwrap_or(req.corpus_id.as_str());

    // Detect whether this is a JSONL corpus (Wikipedia-style BulkDownload) or
    // an HF parquet corpus.  JSONL corpora have no source-file manifest, so we
    // use article-range partitioning instead of file-index partitioning.
    //
    // Detection order:
    //   1. If the engine has a source-file manifest → HF parquet corpus.
    //   2. If `count_jsonl_articles` succeeds → JSONL corpus, source present.
    //   3. If neither → this node has no source data at all. This happens when
    //      a peer (e.g. mac-peer) has an in-progress partition-of-self dir but
    //      the Wikipedia ZIP was not yet downloaded here. In this case the
    //      auto-ingest loop's `spawn_local_ingest` will resume the ingest
    //      (which downloads the ZIP on demand), but this node cannot act as a
    //      collaboration *coordinator* — return 422 with a clear explanation so
    //      the caller doesn't interpret "No source manifest" as a data-loss error
    //      or prompt the user to run `reconstruct-manifest` unnecessarily.
    let has_hf_manifest = engine
        .source_manifest(req.corpus_id.as_str())
        .ok()
        .flatten()
        .is_some();
    let jsonl_article_count = if !has_hf_manifest {
        engine.count_jsonl_articles(req.corpus_id.as_str()).ok()
    } else {
        None
    };
    if !has_hf_manifest && jsonl_article_count.is_none() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ErrorBody {
                error: format!(
                    "corpus '{}' source data not present on this node — \
                     cannot plan collaboration here (this node may be a peer \
                     receiving a partition assignment, not the coordinator). \
                     Run collaborate from the node that initiated the install.",
                    req.corpus_id
                ),
            }),
        ));
    }
    let is_jsonl = !has_hf_manifest;

    // ── Pull-based work queue path (env-gated) ──────────────────────────
    //
    // When SOVEREIGN_USE_WORK_QUEUE=1, build a flat unit list for this
    // corpus shape and register it with `WorkQueueManager`. The returned
    // handoff has `phase: Open` and an empty `partitions` vec — peers
    // discover it via gossip and run a pull loop instead of receiving a
    // one-shot `ingest_partition`. Compute-weighting emerges naturally
    // because fast peers pull more often; fault tolerance comes from the
    // lease reaper. See `commonwealth-knowledge::work_queue`.
    if use_pull_queue() {
        let units = if is_jsonl {
            let shard_count = engine.jsonl_source_shard_count(req.corpus_id.as_str()).unwrap_or(1);
            if shard_count > 1 {
                // Union LOCAL processed_shards (this peer's partition
                // dirs on disk) with PEER processed_shards (every
                // other peer's last-published view, gossiped via
                // MeshStore by `auto_ingest::publish_local_processed_shards`).
                // Without the peer-side union, dispatch queues units
                // for shards that another peer has already finished —
                // observed in the wild: 8 of 33 distinct shards
                // processed twice on a two-peer Wikipedia ingest.
                let mut processed: std::collections::HashSet<usize> = engine
                    .corpus_processed_shards(req.corpus_id.as_str())
                    .into_iter()
                    .collect();
                let peer_processed = commonwealth_state::union_processed_shards(
                    &state.inner.mesh_store,
                    req.corpus_id.as_str(),
                );
                processed.extend(peer_processed);
                let remaining: Vec<usize> = (0..shard_count)
                    .filter(|i| !processed.contains(i))
                    .collect();
                tracing::info!(
                    corpus = %req.corpus_id,
                    shard_count,
                    processed_count = processed.len(),
                    remaining_count = remaining.len(),
                    "corpus_collaborate: queue-mode dispatch (peer-aware processed_shards)"
                );
                build_work_units_jsonl_sharded(remaining)
            } else {
                let total_articles = jsonl_article_count.ok_or_else(|| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(ErrorBody {
                            error: format!("count_jsonl_articles failed for '{}'", req.corpus_id),
                        }),
                    )
                })?;
                let committed_iter_pos = engine.corpus_committed_iter_pos(req.corpus_id.as_str());
                let current_article = engine
                    .estimate_article_pos(req.corpus_id.as_str(), committed_iter_pos, 500)
                    .map_err(|e| {
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(ErrorBody {
                                error: e.to_string(),
                            }),
                        )
                    })?
                    .unwrap_or(0);
                // Slice into ~32 units so a small mesh (2–4 peers) sees
                // enough granularity for load-balancing without being chatty.
                build_work_units_jsonl_single(current_article, total_articles, 32)
            }
        } else {
            let remaining = engine.remaining_source_files(req.corpus_id.as_str()).map_err(|e| {
                let status = if e.to_string().contains("No index found") {
                    StatusCode::NOT_FOUND
                } else if e.to_string().contains("No source manifest") {
                    StatusCode::UNPROCESSABLE_ENTITY
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                };
                (
                    status,
                    Json(ErrorBody {
                        error: e.to_string(),
                    }),
                )
            })?;
            build_work_units_hf(&remaining)
        };

        if units.is_empty() {
            // "No remaining units" splits into two very different
            // states:
            //
            //   (a) the corpus is fully merged — the canonical
            //       `<corpus>/` index exists. Nothing to do; 409.
            //
            //   (b) every shard has been ingested into per-peer
            //       partitions, but the merge step never ran (typical
            //       cause: the coordinator restarted between the last
            //       `complete_unit` and `coordinate_merge` finishing —
            //       `WorkQueueManager` is in-memory, so the
            //       `complete_unit → Merging → spawn_queue_merge`
            //       trigger is gone forever). Without recovery here,
            //       both peers idle indefinitely with `auto_ingest:
            //       corpus already complete — cooling down`. Look up
            //       the existing handoff blob (still in `mesh_store`
            //       via gossip) and re-fire the merge so the corpus
            //       actually finishes.
            let canonical_exists = engine.corpus(req.corpus_id.as_str()).is_installed();

            if !canonical_exists {
                if let Some(existing) =
                    find_local_handoff_for_corpus(&state, req.corpus_id.as_str(), self_id)
                {
                    tracing::info!(
                        corpus = %req.corpus_id,
                        handoff = %existing.handoff_id,
                        "corpus_collaborate: drained queue with no canonical index — \
                         re-firing merge"
                    );
                    spawn_queue_merge(state.clone(), existing.handoff_id);
                    return Ok(Json(existing));
                }
                // No live handoff blob to re-fire from. The MeshStore
                // is in-memory on the daemon (see `daemon::start_daemon`)
                // so any stranded handoff was wiped on restart, and
                // gossip can't help if no peer still holds it. Try
                // local on-disk recovery: if `<corpus>-partition-*/`
                // dirs exist locally, merge them into a canonical
                // ourselves. See `auto_recover` for the cooldown +
                // discovery details. Cheap when nothing to do
                // (deterministic short-circuits before the cooldown).
                let outcome = crate::auto_recover::try_recover_stranded_partitions(
                    engine.index_dir(),
                    req.corpus_id.as_str(),
                )
                .await;
                match outcome {
                    crate::auto_recover::RecoveryOutcome::Recovered {
                        chunks,
                        shards_covered,
                    } => {
                        tracing::info!(
                            corpus = %req.corpus_id,
                            chunks,
                            shards_covered,
                            "corpus_collaborate: stranded-partition recovery merge \
                             SUCCEEDED — canonical now exists; gossip will re-advertise"
                        );
                        return Err((
                            StatusCode::CONFLICT,
                            Json(ErrorBody {
                                error: format!(
                                    "corpus '{}' was stranded across partitions; recovery merged them \
                                     into a canonical with {chunks} chunks ({shards_covered} shards). \
                                     Future requests will pick up the canonical.",
                                    req.corpus_id
                                ),
                            }),
                        ));
                    }
                    crate::auto_recover::RecoveryOutcome::AlreadyHasCanonical => {
                        // Race: another request raced ahead and built
                        // canonical between our `canonical_exists` check
                        // and the recovery call. Fall through to the
                        // 409 — installed_indexes() picks up the
                        // canonical on the next dispatcher tick.
                    }
                    crate::auto_recover::RecoveryOutcome::NotEnoughPartitions => {
                        tracing::warn!(
                            corpus = %req.corpus_id,
                            "corpus_collaborate: queue drained but no canonical index and no \
                             local handoff found, AND no <corpus>-partition-*/ dirs to merge — \
                             peer must re-trigger from a node that holds the handoff blob"
                        );
                    }
                    crate::auto_recover::RecoveryOutcome::InCooldown => {
                        tracing::info!(
                            corpus = %req.corpus_id,
                            "corpus_collaborate: stranded-partition recovery in cooldown — \
                             a recent attempt is still healing or just failed; not retrying yet"
                        );
                    }
                    crate::auto_recover::RecoveryOutcome::Failed(err) => {
                        tracing::warn!(
                            corpus = %req.corpus_id,
                            recovery_error = %err,
                            "corpus_collaborate: queue drained, no handoff found, AND \
                             stranded-partition recovery failed — manual intervention \
                             required (e.g. sovereign corpus merge-partitions {})",
                            req.corpus_id,
                        );
                    }
                    crate::auto_recover::RecoveryOutcome::IncompleteCoverage {
                        covered,
                        total,
                        missing,
                    } => {
                        // auto_recover already logged a detailed WARN
                        // with the missing-shard list; here we just note
                        // that the collaborate dispatcher gave up too.
                        // No retry — the next collaborate request or
                        // auto_ingest tick will re-check, and recovery
                        // fires the moment local coverage becomes
                        // complete.
                        tracing::info!(
                            corpus = %req.corpus_id,
                            covered,
                            total,
                            missing_count = missing.len(),
                            "corpus_collaborate: stranded-partition recovery skipped \
                             (incomplete local coverage); peer with full coverage \
                             will produce canonical"
                        );
                    }
                    crate::auto_recover::RecoveryOutcome::InvalidCorpusId => {
                        tracing::warn!(
                            corpus_id = %req.corpus_id,
                            "corpus_collaborate: empty corpus id, nothing to recover"
                        );
                    }
                    crate::auto_recover::RecoveryOutcome::CanonicalDirectoryReserved => {
                        // The canonical-named directory exists but
                        // doesn't carry our `_corpus_meta.json` —
                        // owned by SCIP for code corpora. Fall
                        // through to the 409 quietly; this isn't a
                        // recoverable state from the chunk-merge
                        // path.
                    }
                }
            }

            return Err((
                StatusCode::CONFLICT,
                Json(ErrorBody {
                    error: format!(
                        "corpus '{}' has no remaining units — nothing to queue",
                        req.corpus_id
                    ),
                }),
            ));
        }

        let unit_count = units.len();
        let mut handoff = IngestionHandoff::new_queue(
            req.corpus_id.clone(),
            recipe_id.to_string(),
            local_embed_model.clone(),
            self_id,
        );
        // Carry the per-job allowlist into the gossiped handoff so peers
        // self-enforce enrollment. A local-only corpus reaching this point
        // passed the ephemeral-grant gate above, so mark the handoff
        // `ephemeral` — peers wipe their partition working dir on teardown
        // instead of retaining it. Ordinary shared corpora stay non-ephemeral.
        handoff.allowed_peers = req.allowed_peers.clone();
        handoff.ephemeral = !recipe_privacy.corpus.mesh_sharing;

        state
            .inner
            .work_queue
            .register(
                handoff.handoff_id,
                handoff.corpus_id.clone(),
                handoff.recipe_id.clone(),
                handoff.embed_model.clone(),
                units,
                self_id,
                handoff
                    .allowed_peers
                    .as_ref()
                    .map(|v| v.iter().copied().collect()),
            )
            .await;

        // Correlate the ephemeral grant with the handoff it authorized, so
        // teardown (revoke / expiry sweep) can retire this exact queue.
        if handoff.ephemeral {
            state
                .inner
                .grant_store
                .bind_handoff(&handoff.corpus_id, handoff.handoff_id);
        }

        // Write the handoff announcement into the local mesh_store so
        // `ShardManager::load_handoff` and `discover_and_spawn_pull_loops`
        // can find it on this node.
        let gossip_key = format!("handoff:{}", handoff.handoff_id);
        let handoff_bytes = match serde_json::to_vec(&handoff) {
            Ok(b) => b,
            Err(e) => {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorBody {
                        error: format!("serialize handoff: {e}"),
                    }),
                ));
            }
        };
        let _ = state.inner.mesh_store.set(
            "corpus-engine",
            &gossip_key,
            bytes::Bytes::from(handoff_bytes.clone()),
            self_id,
        );

        // Close the duplicate-ingest race: on fresh daemon the auto_ingest
        // tick can fire `spawn_local_ingest` microseconds before this
        // handoff becomes visible via `has_active_queue_handoff`. Tripping
        // the engine's per-corpus cancel flag here signals any in-flight
        // `engine.ingest(...)` task to bail at its next cancel check,
        // leaving the pull_loops as the single writer into
        // `<corpus>-partition-<node>/`. No-op when nothing is running.
        if engine.cancel_corpus_ingest(req.corpus_id.as_str()) {
            tracing::info!(
                corpus = %req.corpus_id,
                handoff = %handoff.handoff_id,
                "corpus_collaborate: queue handoff registered — cancelling in-flight spawn_local_ingest"
            );
        }

        // The gossip loop only replicates the `Mesh` member list; it does
        // NOT yet replicate mesh_store entries (the sender half is
        // missing — `all_entries_for_gossip` is defined but unused, and
        // nothing POSTs to `/internal/app/state`). So without an explicit
        // push here, peers never learn of the open queue and their
        // `discover_and_spawn_pull_loops` has nothing to scan. Mirrors
        // the legacy path's direct peer-dispatch, but pushes into the
        // mesh_store namespace the pull-loop already scans.
        let handoff_id_for_log = handoff.handoff_id;
        let corpus_id_for_log = handoff.corpus_id.clone();
        let transport = state.peer_transport();
        for peer in &candidates {
            // The transport owns address ordering (canonical
            // `peer_addr` ranking — this loop used to carry its own
            // inline copy that had drifted to rank Tailscale ULA
            // tied with CGNAT IPv4) and the port policy.
            let endpoints = transport
                .endpoints(
                    &commonwealth_transport::peer_contact(peer),
                    commonwealth_transport::TrafficClass::ControlPlane,
                )
                .await;
            if endpoints.is_empty() {
                tracing::warn!(
                    node = %peer.node_id,
                    handoff = %handoff_id_for_log,
                    "queue broadcast: peer has no address — skipping"
                );
                continue;
            }
            let body = serde_json::json!({
                "entries": [{
                    "app_id": "corpus-engine",
                    "key": gossip_key.clone(),
                    // `/internal/app/state` currently treats value_b64 as
                    // raw UTF-8 (its base64_decode is a stub). JSON is
                    // UTF-8, so round-trips cleanly.
                    "value_b64": String::from_utf8_lossy(&handoff_bytes).into_owned(),
                    "timestamp": std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                    "origin_hex": hex::encode(self_id.as_bytes()),
                }]
            });
            let node_id = peer.node_id;
            let corpus_log = corpus_id_for_log.clone();
            tokio::spawn(async move {
                let client = match reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(10))
                    .build()
                {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(error = %e, "queue broadcast: reqwest build failed");
                        return;
                    }
                };
                for ep in &endpoints {
                    let peer_url = format!("{}/internal/app/state", ep.base_url);
                    match client.post(&peer_url).json(&body).send().await {
                        Ok(resp) if resp.status().is_success() => {
                            tracing::info!(
                                node = %node_id,
                                url = %peer_url,
                                handoff = %handoff_id_for_log,
                                corpus = %corpus_log,
                                "queue broadcast: handoff delivered to peer"
                            );
                            return;
                        }
                        Ok(resp) => {
                            tracing::warn!(
                                node = %node_id,
                                url = %peer_url,
                                status = %resp.status(),
                                "queue broadcast: peer rejected handoff"
                            );
                            return;
                        }
                        Err(e) => {
                            tracing::debug!(
                                node = %node_id,
                                url = %peer_url,
                                error = %e,
                                "queue broadcast: transport error — trying next address"
                            );
                        }
                    }
                }
                tracing::warn!(
                    node = %node_id,
                    handoff = %handoff_id_for_log,
                    "queue broadcast: could not reach peer on any advertised address"
                );
            });
        }

        tracing::info!(
            corpus = %handoff.corpus_id,
            handoff = %handoff.handoff_id,
            units = unit_count,
            peers_notified = candidates.len(),
            "corpus_collaborate: pull-based queue registered"
        );

        return Ok(Json(handoff));
    }

    // ── Legacy static-partition path (default) ──────────────────────────
    let handoff = if is_jsonl {
        // ── JSONL path (Wikipedia) ──────────────────────────────────────────
        //
        // Multi-shard JSONL sources (Wikipedia ships 76 JSONL shards
        // inside one ZIP) partition on the ZIP's table of contents
        // rather than on absolute article index. Shard boundaries
        // come from the archive's own entry list, so two peers with
        // the same ZIP will produce the same articles for a given
        // shard index regardless of whether either has a partial
        // `extracted.jsonl` cache or a snapshot drift. Article-index
        // partitioning — which compared A's local counts to B's —
        // was unsafe (silent corruption when snapshots drifted, a
        // confusing "zero chunks" error when B's extraction was
        // truncated) and is now only used for the single-shard case.
        let shard_count = engine.jsonl_source_shard_count(req.corpus_id.as_str()).unwrap_or(1);
        if shard_count > 1 {
            let processed: std::collections::HashSet<usize> = engine
                .corpus_processed_shards(req.corpus_id.as_str())
                .into_iter()
                .collect();
            let remaining: Vec<usize> = (0..shard_count)
                .filter(|i| !processed.contains(i))
                .collect();

            tracing::info!(
                corpus = %req.corpus_id,
                shard_count,
                processed = processed.len(),
                remaining = remaining.len(),
                "collaborate: planning JSONL shard-index partition"
            );

            plan_collaborative_ingestion_jsonl_sharded(
                req.corpus_id.as_str(),
                recipe_id,
                remaining,
                &local_member,
                &candidates,
                &local_embed_model,
            )
            .map_err(|e| {
                let body = Json(ErrorBody {
                    error: e.to_string(),
                });
                match e {
                    CollaborativeIngestionError::AlreadyComplete(_) => (StatusCode::CONFLICT, body),
                    _ => (StatusCode::UNPROCESSABLE_ENTITY, body),
                }
            })?
        } else {
            // Re-use the count we already computed during corpus-type detection.
            let total_articles = jsonl_article_count.ok_or_else(|| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorBody {
                        error: format!("count_jsonl_articles failed for '{}'", req.corpus_id),
                    }),
                )
            })?;

            // Load committed_iter_pos to estimate how far Machine A has gone.
            let committed_iter_pos = engine.corpus_committed_iter_pos(req.corpus_id.as_str());
            let current_article = engine
                .estimate_article_pos(req.corpus_id.as_str(), committed_iter_pos, 500)
                .map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(ErrorBody {
                            error: e.to_string(),
                        }),
                    )
                })?
                .unwrap_or(0);

            tracing::info!(
                corpus = %req.corpus_id,
                total_articles,
                current_article,
                committed_iter_pos,
                "collaborate: planning JSONL article-range partition"
            );

            plan_collaborative_ingestion_jsonl(
                req.corpus_id.as_str(),
                recipe_id,
                current_article,
                total_articles,
                &local_member,
                &candidates,
                &local_embed_model,
            )
            .map_err(|e| {
                let body = Json(ErrorBody {
                    error: e.to_string(),
                });
                match e {
                    CollaborativeIngestionError::AlreadyComplete(_) => (StatusCode::CONFLICT, body),
                    _ => (StatusCode::UNPROCESSABLE_ENTITY, body),
                }
            })?
        }
    } else {
        // ── HF parquet path (Gutenberg, StackExchange, …) ─────────────────
        let remaining = engine.remaining_source_files(req.corpus_id.as_str()).map_err(|e| {
            let status = if e.to_string().contains("No index found") {
                StatusCode::NOT_FOUND
            } else if e.to_string().contains("No source manifest") {
                StatusCode::UNPROCESSABLE_ENTITY
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (
                status,
                Json(ErrorBody {
                    error: e.to_string(),
                }),
            )
        })?;

        if remaining.is_empty() {
            return Err((
                StatusCode::CONFLICT,
                Json(ErrorBody {
                    error: format!(
                        "corpus '{}' is already complete — no remaining files",
                        req.corpus_id
                    ),
                }),
            ));
        }

        plan_collaborative_ingestion(
            req.corpus_id.as_str(),
            recipe_id,
            &remaining,
            &local_member,
            &candidates,
            &local_embed_model,
        )
        .map_err(|e| {
            let body = Json(ErrorBody {
                error: e.to_string(),
            });
            match e {
                CollaborativeIngestionError::AlreadyComplete(_) => (StatusCode::CONFLICT, body),
                CollaborativeIngestionError::NoManifest(_) => (StatusCode::NOT_FOUND, body),
                _ => (StatusCode::UNPROCESSABLE_ENTITY, body),
            }
        })?
    };

    // Notify remote peers. Fire-and-forget with tracing: failing to
    // reach a peer is logged but doesn't fail the collaborate call —
    // the partition is still in the handoff, and the peer will pick up
    // the assignment via gossip.
    //
    // NOTE: We do NOT spawn the coordinator's own local partition from
    // here. Under the unified ingest primitive, local work for this
    // node is already happening via `CorpusEngine::ingest` writing to
    // `<corpus>-partition-<self>/` — Desktop's install command drives
    // that path, and the auto-collaborate loop only fires when such a
    // partition already exists (see `in_progress_ingestions`). Spawning
    // another task here would race the Desktop-owned task on the same
    // LanceDB writer. Finalisation: once the coordinator-local partition
    // completes, the Desktop-owned spawn itself invokes
    // `ShardManager::coordinate_merge` (via the existing peer-finalise
    // hook), which stitches remote shards in and renames to canonical.
    {
        let transport = state.peer_transport();
        let mesh = state.inner.mesh.read().await;
        if let Some(local_partition) = handoff.partitions.iter().find(|p| p.node_id == self_id) {
            tracing::info!(
                corpus = %handoff.corpus_id,
                files = local_partition.file_indices.len(),
                "collaborate: local share recorded in handoff — Desktop install pipeline owns the local ingest"
            );
        }

        for partition in &handoff.partitions {
            if partition.node_id == self_id {
                continue;
            }
            let Some(peer) = mesh.members.get(&partition.node_id) else {
                tracing::warn!(
                    node = %partition.node_id,
                    "collaborate: peer not found in mesh — skipping notification"
                );
                continue;
            };
            // The transport orders candidates (Tailscale-first
            // ranking — works across Wi-Fi networks where LAN IPs
            // silently fail: AP isolation, different subnets,
            // captive portals on one side. When LAN failed first we
            // used to give up entirely — that's how Machine A ended
            // up unable to dispatch to B even though both machines
            // had routable Tailscale addresses advertised).
            let endpoints = transport
                .endpoints(
                    &commonwealth_transport::peer_contact(peer),
                    commonwealth_transport::TrafficClass::ControlPlane,
                )
                .await;
            if endpoints.is_empty() {
                tracing::warn!(
                    node = %partition.node_id,
                    "collaborate: peer has no address — skipping notification"
                );
                continue;
            }
            let payload = IngestPartitionRequest {
                handoff_id: handoff.handoff_id,
                corpus_id: handoff.corpus_id.clone(),
                recipe_id: handoff.recipe_id.clone(),
                file_indices: partition.file_indices.clone(),
                article_range: partition.article_range,
                embed_model: handoff.embed_model.clone(),
            };
            let node_id = partition.node_id;
            tokio::spawn(async move {
                let client = match reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(10))
                    .build()
                {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "corpus_collaborate: failed to build reqwest client — skipping partition dispatch"
                        );
                        return;
                    }
                };
                let mut attempt_errors: Vec<String> = Vec::new();
                let mut accepted = false;
                for ep in &endpoints {
                    let peer_url = format!("{}/internal/corpus/ingest_partition", ep.base_url);
                    match client.post(&peer_url).json(&payload).send().await {
                        Ok(resp) if resp.status().is_success() => {
                            tracing::info!(
                                node = %node_id,
                                url = %peer_url,
                                "collaborate: peer accepted partition"
                            );
                            accepted = true;
                            break;
                        }
                        Ok(resp) => {
                            // 409/503 from the peer means the peer answered
                            // but refused — don't bother trying other
                            // addresses, they'd all produce the same
                            // rejection from the same peer process.
                            let status = resp.status();
                            let body = resp.text().await.unwrap_or_default();
                            tracing::warn!(
                                node = %node_id,
                                url = %peer_url,
                                status = %status,
                                body = %body,
                                "collaborate: peer rejected partition"
                            );
                            accepted = false;
                            attempt_errors.clear();
                            attempt_errors.push(format!("{peer_url}: {status} {body}"));
                            break;
                        }
                        Err(e) => {
                            // Transport-level failure on this address.
                            // Try the next one (Tailscale → LAN fallback).
                            attempt_errors.push(format!("{peer_url}: {e}"));
                        }
                    }
                }
                if !accepted && !attempt_errors.is_empty() {
                    tracing::warn!(
                        node = %node_id,
                        attempts = attempt_errors.len(),
                        errors = ?attempt_errors,
                        "collaborate: could not reach peer on any advertised address"
                    );
                }
            });
        }
    }

    tracing::info!(
        corpus = %req.corpus_id,
        partitions = handoff.partitions.len(),
        "corpus_collaborate: handoff planned"
    );

    Ok(Json(handoff))
}

#[derive(Debug, Deserialize)]
pub struct EligiblePeersRequest {
    pub corpus_id: kernel_types::CorpusId,
}

#[derive(Debug, Serialize)]
pub struct EligiblePeerDto {
    /// Full-hex node id (matches the mesh member list + contribution ledger).
    pub node_id: String,
    pub name: String,
    pub online: bool,
    pub eligible: bool,
    /// Machine token mirroring the collaborate candidate filter, so the picker
    /// copy stays in lockstep: `ok` | `offline` | `no_embed_model` |
    /// `embed_model_mismatch`.
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct EligiblePeersResponse {
    pub peers: Vec<EligiblePeerDto>,
    /// Whether this corpus may be peer-assisted at all (`[corpus] grantable`).
    /// The desktop hides the whole offer when false.
    pub grantable: bool,
}

/// POST /internal/corpus/collaborate/eligible_peers — which mesh peers can help
/// with a peer-assisted ingest of `corpus_id`. Reuses the same
/// `EmbedModelInfo`-equality candidate logic as `corpus_collaborate`, but
/// returns EVERY peer with an eligibility verdict + reason so the desktop
/// picker can show ineligible peers dimmed with an explanation (glassbox —
/// never a silent omission).
pub async fn corpus_eligible_peers(
    State(state): State<AppState>,
    Json(req): Json<EligiblePeersRequest>,
) -> Result<Json<EligiblePeersResponse>, (StatusCode, Json<ErrorBody>)> {
    let engine = state.inner.corpus_engine.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: "no corpus engine available on this node".into(),
            }),
        )
    })?;

    let grantable = engine
        .load_recipe(req.corpus_id.as_str())
        .await
        .map(|r| r.corpus.grantable)
        .unwrap_or(false);

    let local_embed_model = state.inner.inference_store.get_local_embed_model();

    let mesh = state.inner.mesh.read().await;
    let self_id = *state.inner.self_node_id_swap.load_full().as_ref();
    let mut peers: Vec<EligiblePeerDto> = Vec::new();
    for m in mesh.members.values() {
        if m.node_id == self_id {
            continue;
        }
        let online = m.status == NodeStatus::Online;
        let (eligible, reason) = if !online {
            (false, "offline")
        } else {
            match (
                local_embed_model.as_ref(),
                m.capabilities.embed_model.as_ref(),
            ) {
                (_, None) => (false, "no_embed_model"),
                (Some(local), Some(em)) if em == local => (true, "ok"),
                (Some(_), Some(_)) => (false, "embed_model_mismatch"),
                // We have no local embed model to compare against — cannot
                // certify a match, so treat as ineligible.
                (None, Some(_)) => (false, "no_embed_model"),
            }
        };
        peers.push(EligiblePeerDto {
            node_id: m.node_id.to_hex(),
            name: m.name.clone(),
            online,
            eligible,
            reason: reason.to_string(),
        });
    }
    drop(mesh);
    // Eligible + online first, then by name — the picker checks these by default.
    peers.sort_by(|a, b| b.eligible.cmp(&a.eligible).then(a.name.cmp(&b.name)));

    Ok(Json(EligiblePeersResponse { peers, grantable }))
}

#[derive(Debug, Deserialize)]
pub struct CollaborateRequest {
    pub corpus_id: kernel_types::CorpusId,
    /// Recipe to use. Defaults to `corpus_id` when absent.
    pub recipe_id: Option<String>,
    /// Per-job peer allowlist for an ephemeral grant-scoped ingest.
    /// `None` (default) preserves today's behaviour: dispatch to every
    /// embed-compatible online peer. `Some(set)` restricts the job to the
    /// user-selected node_ids; an empty set means "coordinator self-serve
    /// only" (a local-only run that still exercises the queue path).
    #[serde(default)]
    pub allowed_peers: Option<Vec<NodeId>>,
}
