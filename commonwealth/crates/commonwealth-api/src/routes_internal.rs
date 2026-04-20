use std::net::SocketAddr;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use std::sync::Arc;

use commonwealth_core::ids::NodeId;
use commonwealth_core::knowledge::IngestionHandoff;
use commonwealth_core::mesh::{Mesh, NodeStatus};
use commonwealth_discovery::membership;
use commonwealth_inference::inference_plan::InferencePlan;
use commonwealth_inference::oicp::{EmbedModelInfo, KnowledgeResult, KnowledgeSearchRequest, KnowledgeSearchResponse};
use commonwealth_inference::scheduler::knowledge_assignment::{
    plan_collaborative_ingestion, plan_collaborative_ingestion_jsonl,
    plan_collaborative_ingestion_jsonl_sharded, CollaborativeIngestionError,
};
use commonwealth_knowledge::shard_manager::ShardManager;

use crate::state::AppState;

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
            Json(ErrorBody { error: "no corpus engine available on this node".into() }),
        )
    })?;

    // Build local node view.
    let mesh = state.inner.mesh.read().await;
    let self_id = state.inner.self_node_id_swap.load_full().as_ref().clone();
    let local_member = mesh.members.get(&self_id).cloned().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody { error: "local node not found in mesh".into() }),
        )
    })?;

    let local_embed_model = state.inner.inference_store.get_local_embed_model()
        .ok_or_else(|| (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: "embed model not configured on this node — cannot plan collaboration".into(),
            }),
        ))?;

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

    if !rejected.is_empty() {
        tracing::info!(
            rejected = ?rejected,
            compatible = candidates.len(),
            "corpus_collaborate: candidate filter results"
        );
    }

    let recipe_id = req.recipe_id.as_deref().unwrap_or(&req.corpus_id);

    // Detect whether this is a JSONL corpus (Wikipedia-style BulkDownload) or
    // an HF parquet corpus.  JSONL corpora have no source-file manifest, so we
    // use article-range partitioning instead of file-index partitioning.
    let is_jsonl = engine
        .source_manifest(&req.corpus_id)
        .ok()
        .flatten()
        .is_none()
        && engine
            .count_jsonl_articles(&req.corpus_id)
            .is_ok();

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
        let shard_count = engine
            .jsonl_source_shard_count(&req.corpus_id)
            .unwrap_or(1);
        if shard_count > 1 {
            let processed: std::collections::HashSet<usize> = engine
                .corpus_processed_shards(&req.corpus_id)
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
                &req.corpus_id,
                recipe_id,
                remaining,
                &local_member,
                &candidates,
                &local_embed_model,
            )
            .map_err(|e| {
                let body = Json(ErrorBody { error: e.to_string() });
                match e {
                    CollaborativeIngestionError::AlreadyComplete(_) => (StatusCode::CONFLICT, body),
                    _ => (StatusCode::UNPROCESSABLE_ENTITY, body),
                }
            })?
        } else {

        let total_articles = engine.count_jsonl_articles(&req.corpus_id)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorBody { error: e.to_string() })))?;

        // Load committed_iter_pos to estimate how far Machine A has gone.
        let committed_iter_pos = engine.corpus_committed_iter_pos(&req.corpus_id);
        let current_article = engine
            .estimate_article_pos(&req.corpus_id, committed_iter_pos, 500)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorBody { error: e.to_string() })))?
            .unwrap_or(0);

        tracing::info!(
            corpus = %req.corpus_id,
            total_articles,
            current_article,
            committed_iter_pos,
            "collaborate: planning JSONL article-range partition"
        );

        plan_collaborative_ingestion_jsonl(
            &req.corpus_id,
            recipe_id,
            current_article,
            total_articles,
            &local_member,
            &candidates,
            &local_embed_model,
        )
        .map_err(|e| {
            let body = Json(ErrorBody { error: e.to_string() });
            match e {
                CollaborativeIngestionError::AlreadyComplete(_) => (StatusCode::CONFLICT, body),
                _ => (StatusCode::UNPROCESSABLE_ENTITY, body),
            }
        })?

        }
    } else {
        // ── HF parquet path (Gutenberg, StackExchange, …) ─────────────────
        let remaining = engine
            .remaining_source_files(&req.corpus_id)
            .map_err(|e| {
                let status = if e.to_string().contains("No index found") {
                    StatusCode::NOT_FOUND
                } else if e.to_string().contains("No source manifest") {
                    StatusCode::UNPROCESSABLE_ENTITY
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                };
                (status, Json(ErrorBody { error: e.to_string() }))
            })?;

        if remaining.is_empty() {
            return Err((
                StatusCode::CONFLICT,
                Json(ErrorBody {
                    error: format!("corpus '{}' is already complete — no remaining files", req.corpus_id),
                }),
            ));
        }

        plan_collaborative_ingestion(
            &req.corpus_id,
            recipe_id,
            &remaining,
            &local_member,
            &candidates,
            &local_embed_model,
        )
        .map_err(|e| {
            let body = Json(ErrorBody { error: e.to_string() });
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
            if peer.addresses.is_empty() {
                tracing::warn!(
                    node = %partition.node_id,
                    "collaborate: peer has no address — skipping notification"
                );
                continue;
            }
            // Try addresses in preference order: Tailscale (100.64.0.0/10 CGNAT +
            // fd7a:115c:a1e0::/48 ULA) first, then IPv4 LAN, then other v6.
            // Tailscale works across Wi-Fi networks where LAN IPs silently
            // fail (AP isolation, different subnets, captive portals on
            // one side). When LAN fails first, we used to give up entirely
            // — that's how Machine A ended up unable to dispatch to B
            // even though both machines had routable Tailscale addresses
            // advertised in `MemberRecord.addresses`.
            let mut ordered_addrs: Vec<std::net::SocketAddr> =
                peer.addresses.iter().copied().collect();
            ordered_addrs.sort_by_key(|addr| match addr.ip() {
                std::net::IpAddr::V4(v4) => {
                    let o = v4.octets();
                    // CGNAT 100.64.0.0/10 → Tailscale IPv4.
                    if o[0] == 100 && (o[1] & 0xc0) == 64 { 0 } else { 1 }
                }
                std::net::IpAddr::V6(v6) => {
                    let s = v6.segments();
                    // Tailscale ULA fd7a:115c:a1e0::/48.
                    if s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0 { 0 } else { 2 }
                }
            });
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
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(10))
                    .build()
                    .expect("build reqwest client");
                let mut attempt_errors: Vec<String> = Vec::new();
                let mut accepted = false;
                for addr in &ordered_addrs {
                    // Format with bracket rule for IPv6 (matches
                    // what sovereign_mesh::daemon::format_relay_fragment
                    // does for the share link).
                    let host = match addr.ip() {
                        std::net::IpAddr::V4(_) => addr.ip().to_string(),
                        std::net::IpAddr::V6(v6) => format!("[{v6}]"),
                    };
                    let peer_url = format!(
                        "http://{host}:{}/internal/corpus/ingest_partition",
                        9742
                    );
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
                            attempt_errors.push(format!(
                                "{peer_url}: {status} {body}"
                            ));
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
pub struct CollaborateRequest {
    pub corpus_id: String,
    /// Recipe to use. Defaults to `corpus_id` when absent.
    pub recipe_id: Option<String>,
}

/// POST /internal/corpus/ingest_partition — start ingesting an assigned partition.
///
/// Called by the collaborate coordinator on each peer after planning.
/// Validates embed model compatibility, then spawns an asynchronous
/// ingestion task for the specified file indices.
pub async fn corpus_ingest_partition(
    State(state): State<AppState>,
    Json(req): Json<IngestPartitionRequest>,
) -> (StatusCode, Json<IngestPartitionResponse>) {
    let _engine = match &state.inner.corpus_engine {
        Some(e) => e.clone(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(IngestPartitionResponse {
                    accepted: false,
                    reason: Some("no corpus engine available on this node".into()),
                }),
            );
        }
    };

    // Validate embed model compatibility. If no embed model info is stored,
    // this node hasn't completed bootstrap (or has no embed model configured)
    // and cannot safely accept a partition — return 503 so the coordinator
    // skips us rather than assigning work we can't do.
    let Some(local_embed_model) = state.inner.inference_store.get_local_embed_model() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(IngestPartitionResponse {
                accepted: false,
                reason: Some("embed model not configured on this node — cannot accept partition".into()),
            }),
        );
    };

    if local_embed_model != req.embed_model {
        tracing::warn!(
            local = ?local_embed_model,
            requested = ?req.embed_model,
            "ingest_partition: embed model mismatch — refusing"
        );
        return (
            StatusCode::CONFLICT,
            Json(IngestPartitionResponse {
                accepted: false,
                reason: Some(format!(
                    "embed model mismatch: local={} requested={}",
                    local_embed_model.model_id, req.embed_model.model_id
                )),
            }),
        );
    }

    let corpus_id = req.corpus_id.clone();
    let recipe_id = req.recipe_id.clone();
    let file_indices = req.file_indices.clone();
    let article_range = req.article_range;
    let handoff_id = req.handoff_id;
    let local_node_id = state.inner.self_node_id_swap.load_full().as_ref().clone();
    let engine = _engine.clone();
    let mesh_store = Arc::clone(&state.inner.mesh_store);
    let state_clone = state.clone();
    // Snapshot peer base URLs now — we can't hold the mesh lock across an async task.
    let peer_urls: Vec<(NodeId, String)> = {
        let mesh = state.inner.mesh.read().await;
        mesh.members.values()
            .filter(|m| m.node_id != local_node_id)
            .filter_map(|m| m.addresses.first().map(|a| {
                (m.node_id, format!("http://{}:9742", a.ip()))
            }))
            .collect()
    };

    // Spawn ingestion asynchronously — 202 Accepted returns immediately.
    tokio::spawn(async move {
        let output_path = engine.index_dir()
            .join(format!("{corpus_id}-partition-{local_node_id}"));

        tracing::info!(
            corpus = %corpus_id,
            recipe = %recipe_id,
            handoff = %handoff_id,
            files = file_indices.len(),
            output = %output_path.display(),
            "ingest_partition: starting ingestion for assigned files"
        );

        state_clone.inner.active_ingests.write().await
            .insert(corpus_id.clone());
        let ingest_result = engine.ingest_with_overrides(
            &recipe_id,
            Some(file_indices),
            article_range,
            &output_path,
            None,
        ).await;
        state_clone.inner.active_ingests.write().await
            .remove(&corpus_id);

        match ingest_result {
            Ok(result) => {
                tracing::info!(
                    corpus = %corpus_id,
                    handoff = %handoff_id,
                    chunks = result.chunks_created,
                    "ingest_partition: complete — triggering merge check"
                );
                let shard_mgr = ShardManager::new(
                    Arc::clone(&engine),
                    engine.index_dir().to_path_buf(),
                    mesh_store,
                );
                match shard_mgr.coordinate_merge(handoff_id, local_node_id, &peer_urls).await {
                    Ok(Some(info)) => tracing::info!(
                        corpus = %corpus_id,
                        chunks = info.chunk_count,
                        "ingest_partition: merge complete"
                    ),
                    Ok(None) => tracing::info!(
                        corpus = %corpus_id,
                        "ingest_partition: not merge leader — leader will complete merge"
                    ),
                    Err(e) => tracing::error!(
                        corpus = %corpus_id,
                        error = %e,
                        "ingest_partition: merge failed"
                    ),
                }
            }
            Err(e) => tracing::error!(
                corpus = %corpus_id,
                handoff = %handoff_id,
                error = %e,
                "ingest_partition: failed"
            ),
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(IngestPartitionResponse {
            accepted: true,
            reason: None,
        }),
    )
}

/// POST /internal/corpus/install — start (or resume) a corpus ingest.
///
/// Thin entry point to [`CorpusEngine::ingest`]. Desktop's Tauri
/// `install_corpus` command and the daemon's auto-collaborate loop
/// both call this so there is exactly one place where an ingest gets
/// spawned on this node: the shared helper
/// [`spawn_corpus_install`]. That helper owns `active_ingests`
/// bookkeeping and the `corpus_progress` map, so the
/// `/internal/corpus/progress` route and the `/internal/corpus/cancel`
/// route have consistent views of what is running.
///
/// Idempotent: a second call while the same corpus is already in
/// `active_ingests` returns `spawned: false` without starting a new
/// task. That's the "dual-path guard" — clicking Install in Desktop
/// while the daemon is already working on this corpus just no-ops.
pub async fn corpus_install(
    State(state): State<AppState>,
    Json(req): Json<InstallRequest>,
) -> Result<Json<InstallResponse>, (StatusCode, Json<ErrorBody>)> {
    if state.inner.corpus_engine.is_none() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: "no corpus engine available on this node".into(),
            }),
        ));
    }
    let spawned = spawn_corpus_install(state, req.corpus_id.clone()).await;
    Ok(Json(InstallResponse {
        corpus_id: req.corpus_id,
        spawned,
    }))
}

/// GET /internal/corpus/progress — snapshot of the latest progress
/// event observed for every corpus currently in
/// `active_ingests`, plus any corpus whose terminal `Complete` event
/// has not yet been evicted by a subsequent install.
///
/// Clients poll this (the Desktop UI polls every ~500 ms while an
/// install is in-flight). The response is a map keyed by corpus id
/// for direct lookup; an empty object means nothing is currently
/// ingesting on this node.
pub async fn corpus_progress(
    State(state): State<AppState>,
) -> Json<ProgressSnapshotResponse> {
    let snapshot = state.inner.corpus_progress.read().await.clone();
    Json(ProgressSnapshotResponse { progress: snapshot })
}

/// GET /internal/corpus/status — richer per-corpus snapshot that
/// combines every signal the Desktop UI needs to render the
/// "Installing…" row without needing to have initiated the install
/// itself.
///
/// Reports an entry for every corpus where any of:
///   - an ingest task is currently in `active_ingests`;
///   - a canonical or partition-of-self directory is present with
///     `ingestion_in_progress=true` (daemon-owned resume after a
///     Desktop close / crash);
///   - a recent progress event is cached but the task has already
///     exited (so terminal phases still propagate to a late
///     subscriber).
///
/// Each entry fuses the latest `IngestProgress` with on-disk state
/// (shard counts, committed_iter_pos, partition/canonical presence)
/// plus a best-effort `estimated_fraction`. The Desktop poller reads
/// this and emits `corpus-progress` events so the UI state stays in
/// sync whether or not this particular Desktop session kicked off
/// the install.
pub async fn corpus_status(
    State(state): State<AppState>,
) -> Json<CorpusStatusResponse> {
    let engine = match state.inner.corpus_engine.as_ref() {
        Some(e) => e.clone(),
        None => {
            return Json(CorpusStatusResponse { entries: Vec::new() });
        }
    };

    // Union of every corpus id worth reporting. Using a BTreeSet so
    // the response is deterministically ordered — makes debugging
    // and the integration test's snapshot comparisons less flaky.
    let mut candidates: std::collections::BTreeSet<String> =
        Default::default();
    for id in state.inner.active_ingests.read().await.iter() {
        candidates.insert(id.clone());
    }
    for id in state.inner.corpus_progress.read().await.keys() {
        candidates.insert(id.clone());
    }
    candidates.extend(engine.in_progress_ingestions());

    let active_snapshot = state.inner.active_ingests.read().await.clone();
    let progress_snapshot = state.inner.corpus_progress.read().await.clone();

    // Gather per-corpus data, then spawn sample jobs for any corpus
    // that needs a fresh article-stats sidecar. We do this OFF the
    // async runtime (`spawn_blocking`) because the first sample for
    // a ~74 GB Wikipedia JSONL burns 1–2 s of synchronous I/O;
    // doing it inline would block other handlers on this axum worker.
    let mut entries: Vec<CorpusStatusEntry> = Vec::new();
    for corpus_id in candidates {
        let disk = engine.corpus_disk_status(&corpus_id);
        let active = active_snapshot.contains(&corpus_id);
        let progress = progress_snapshot.get(&corpus_id).cloned();
        // Cheap sidecar read — no I/O beyond a small file if it
        // exists. Sidecar is absent on the first daemon-session
        // observation of a corpus; we kick off the sampler below and
        // the next `/status` poll will pick up the fresh value.
        let cached_stats = engine.cached_article_stats(&corpus_id);

        if cached_stats.is_none() && disk.committed_iter_pos > 0 {
            // Spawn the sampler in the background. It writes the
            // sidecar on completion; the next poll reads it.
            let engine_for_task = engine.clone();
            let corpus_id_for_task = corpus_id.clone();
            tokio::task::spawn_blocking(move || {
                let _ = engine_for_task.compute_article_stats(&corpus_id_for_task);
            });
        }

        let estimated_fraction = disk
            .estimated_fraction()
            .or_else(|| {
                // Sample-derived fraction for the legacy / resume
                // path: committed sections vs estimated total.
                let stats = cached_stats.as_ref()?;
                if stats.total_sections_estimate == 0 {
                    return None;
                }
                Some(
                    (disk.committed_iter_pos as f32
                        / stats.total_sections_estimate as f32)
                        .clamp(0.0, 1.0),
                )
            })
            .or_else(|| progress.as_ref().and_then(progress_fraction));

        entries.push(CorpusStatusEntry {
            corpus_id: corpus_id.clone(),
            active,
            progress,
            shards_completed: disk.shards_completed.len(),
            shards_total: disk.shards_total,
            committed_iter_pos: disk.committed_iter_pos,
            canonical_present: disk.canonical_present,
            partition_present: disk.partition_present,
            canonical_in_progress: disk.canonical_in_progress,
            partition_in_progress: disk.partition_in_progress,
            estimated_fraction,
            estimated_total_sections: cached_stats
                .as_ref()
                .map(|s| s.total_sections_estimate),
            estimated_total_articles: cached_stats.as_ref().map(|s| s.total_articles),
        });
    }

    Json(CorpusStatusResponse { entries })
}

fn progress_fraction(progress: &corpus_engine::IngestProgress) -> Option<f32> {
    use corpus_engine::IngestProgress as P;
    match progress {
        P::Downloading { percent, .. } => Some((*percent / 100.0).clamp(0.0, 1.0)),
        P::Embedding { chunks_embedded, total, .. } if *total > 0 => {
            Some(((*chunks_embedded as f32) / (*total as f32)).clamp(0.0, 1.0))
        }
        P::Indexing { chunks_indexed, total } if *total > 0 => {
            Some(((*chunks_indexed as f32) / (*total as f32)).clamp(0.0, 1.0))
        }
        P::Complete { .. } => Some(1.0),
        _ => None,
    }
}

#[derive(Debug, Serialize)]
pub struct CorpusStatusResponse {
    pub entries: Vec<CorpusStatusEntry>,
}

#[derive(Debug, Serialize)]
pub struct CorpusStatusEntry {
    pub corpus_id: String,
    /// A task is currently tracked in `active_ingests` for this
    /// corpus. False means either no ingest is running, or an
    /// ingest exited without clearing its entry (daemon crash).
    pub active: bool,
    /// Latest `IngestProgress` observed for this corpus, if any.
    pub progress: Option<corpus_engine::IngestProgress>,
    pub shards_completed: usize,
    pub shards_total: usize,
    pub committed_iter_pos: u64,
    pub canonical_present: bool,
    pub partition_present: bool,
    pub canonical_in_progress: bool,
    pub partition_in_progress: bool,
    /// Best-effort completion fraction in `[0.0, 1.0]`. `None` when
    /// we genuinely can't estimate (e.g. pre-first-embed-batch in
    /// a legacy canonical resume where shards aren't tracked).
    pub estimated_fraction: Option<f32>,
    /// Cached sample estimate of total sections (extractor-emitted
    /// documents) in the source JSONL. Drives the resume-path
    /// percent via `committed_iter_pos / total`. `None` until the
    /// sampler has written a sidecar for this corpus.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_total_sections: Option<u64>,
    /// Cached sample estimate of total JSONL lines (articles) in
    /// the source. Exposed mainly for diagnostic display.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_total_articles: Option<u64>,
}

/// Spawn an `engine.ingest` task for `corpus_id`, unifying the
/// lifecycle bookkeeping across every entry point (install route,
/// auto-collaborate loop, future CLI).
///
/// Responsibilities kept in this one place:
///   - Idempotency guard: skip spawn when `corpus_id` is already in
///     `active_ingests`. Returns `false` so the caller can surface
///     "already ingesting" to the user.
///   - `active_ingests` insert / remove around the spawn.
///   - `corpus_progress` map updates via a progress callback that
///     writes on every `IngestProgress` event.
///   - Result logging with `Error::Cancelled` treated as a clean
///     outcome (the `/internal/corpus/cancel` route has already
///     wiped the partition when this returns).
///
/// Returns `true` when a new task was spawned, `false` when a task
/// was already live for this corpus.
pub async fn spawn_corpus_install(state: AppState, corpus_id: String) -> bool {
    let Some(engine) = state.inner.corpus_engine.clone() else {
        tracing::warn!(
            corpus = %corpus_id,
            "spawn_corpus_install: no corpus engine — ignoring"
        );
        return false;
    };

    {
        let mut active = state.inner.active_ingests.write().await;
        if active.contains(&corpus_id) {
            tracing::info!(
                corpus = %corpus_id,
                "spawn_corpus_install: already active — not spawning a second task"
            );
            return false;
        }
        active.insert(corpus_id.clone());
    }

    let state_for_task = state.clone();
    let corpus_id_for_task = corpus_id.clone();
    tokio::spawn(async move {
        // Progress callback: latest-wins per corpus. We hold the
        // write lock only for the duration of an insert, so readers
        // (the GET /internal/corpus/progress route) see an update
        // on the next tick without blocking the ingest task.
        let progress_state = state_for_task.clone();
        let progress_cid = corpus_id_for_task.clone();
        let progress_cb: corpus_engine::ProgressCallback =
            Box::new(move |p| {
                let progress_state = progress_state.clone();
                let progress_cid = progress_cid.clone();
                // The callback is synchronous but we need an async
                // lock. Spawn a short-lived task to perform the
                // insert; it finishes essentially instantly.
                tokio::spawn(async move {
                    progress_state
                        .inner
                        .corpus_progress
                        .write()
                        .await
                        .insert(progress_cid, p);
                });
            });

        let spec = corpus_engine::CorpusSpec::Builtin(corpus_id_for_task.clone());
        let result = engine.ingest(&spec, Some(progress_cb)).await;

        state_for_task
            .inner
            .active_ingests
            .write()
            .await
            .remove(&corpus_id_for_task);

        match result {
            Ok(info) => tracing::info!(
                corpus = %corpus_id_for_task,
                chunks = info.chunks_created,
                duration_secs = info.duration_secs,
                "spawn_corpus_install: ingest complete"
            ),
            Err(corpus_engine::Error::Cancelled(_)) => {
                // Cancel route handles the wipe; we only clean up
                // the progress map so the UI returns to
                // "not_installed" on the next poll.
                state_for_task
                    .inner
                    .corpus_progress
                    .write()
                    .await
                    .remove(&corpus_id_for_task);
                tracing::info!(
                    corpus = %corpus_id_for_task,
                    "spawn_corpus_install: ingest cancelled"
                );
            }
            Err(e) => tracing::warn!(
                corpus = %corpus_id_for_task,
                error = %e,
                "spawn_corpus_install: ingest failed"
            ),
        }
    });
    true
}

#[derive(Debug, Deserialize)]
pub struct InstallRequest {
    pub corpus_id: String,
}

#[derive(Debug, Serialize)]
pub struct InstallResponse {
    pub corpus_id: String,
    /// True when a new task was spawned, false when an ingest for
    /// this corpus was already running on this node.
    pub spawned: bool,
}

#[derive(Debug, Serialize)]
pub struct ProgressSnapshotResponse {
    pub progress: std::collections::HashMap<String, corpus_engine::IngestProgress>,
}

/// POST /internal/corpus/cancel — user-initiated cancel + wipe.
///
/// Flow:
///   1. Fire the corpus's cancellation flag via the engine's registry.
///      The ingest loop polls this flag at every document + flush
///      boundary and exits with `Error::Cancelled` at the next safe
///      point, without corrupting LanceDB.
///   2. Wait (bounded, ~5 s) for the spawn to clear out of
///      `active_ingests` so that no concurrent writer is left behind
///      when we wipe the directories.
///   3. Wipe canonical `<corpus>/` and every `<corpus>-partition-*/`
///      sibling via `engine.remove_corpus_everything`. Peers' own
///      partition dirs on other machines are not affected (per the
///      "cancel is local" decision in the unified-ingest plan).
///
/// Returns 200 even when no ingest was active for this corpus — the
/// wipe still runs, so a stale partition dir left over from a crashed
/// earlier session gets cleaned up too. The response carries whether a
/// cancel signal was actually delivered so callers can distinguish
/// "cancelled a live ingest" from "idempotent cleanup".
pub async fn corpus_cancel(
    State(state): State<AppState>,
    Json(req): Json<CancelRequest>,
) -> Result<Json<CancelResponse>, (StatusCode, Json<ErrorBody>)> {
    let engine = state.inner.corpus_engine.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: "no corpus engine available on this node".into(),
            }),
        )
    })?;

    let cancelled = engine.cancel_corpus_ingest(&req.corpus_id);

    // Bounded poll until the spawn clears from active_ingests. We do
    // this via polling rather than a notify because active_ingests is
    // mutated from multiple task sites (collaborate spawn, peer
    // partition spawn, future install command) — a single Notify would
    // need to be fired from every one of them and we'd miss races.
    // 5 s is generous: the ingest loop polls cancel between each doc
    // and between every tier-2 flush (~60 s of work max), but each
    // individual doc takes milliseconds, so the loop exits promptly
    // in practice. The wait only hits the ceiling when cancel is
    // fired during a slow embed call that can't be interrupted.
    if cancelled {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let still_active = state
                .inner
                .active_ingests
                .read()
                .await
                .contains(&req.corpus_id);
            if !still_active {
                break;
            }
            if std::time::Instant::now() >= deadline {
                tracing::warn!(
                    corpus = %req.corpus_id,
                    "corpus_cancel: ingest task did not exit within 5s — wiping anyway"
                );
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    // Drop the progress entry so polling clients see "not_installed"
    // on their next tick instead of a stale final-embedding frame.
    state
        .inner
        .corpus_progress
        .write()
        .await
        .remove(&req.corpus_id);

    // Wipe canonical + every partition-* sibling for this corpus.
    if let Err(e) = engine.remove_corpus_everything(&req.corpus_id) {
        tracing::warn!(
            corpus = %req.corpus_id,
            error = %e,
            "corpus_cancel: wipe reported an error; returning failure to caller"
        );
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                error: format!("failed to wipe corpus '{}': {e}", req.corpus_id),
            }),
        ));
    }

    tracing::info!(
        corpus = %req.corpus_id,
        cancel_signalled = cancelled,
        "corpus_cancel: cleanup complete"
    );

    Ok(Json(CancelResponse {
        cancel_signalled: cancelled,
        wiped: true,
    }))
}

#[derive(Debug, Deserialize)]
pub struct CancelRequest {
    pub corpus_id: String,
}

#[derive(Debug, Serialize)]
pub struct CancelResponse {
    /// True when a live ingest task for this corpus existed and was
    /// signalled to stop. False for an idempotent cleanup call (no
    /// task was running).
    pub cancel_signalled: bool,
    /// True when the on-disk wipe completed without error.
    pub wiped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestPartitionRequest {
    pub handoff_id: commonwealth_core::ids::HandoffId,
    pub corpus_id: String,
    pub recipe_id: String,
    pub file_indices: Vec<usize>,
    /// Article range for JSONL corpora (e.g. Wikipedia). Mutually exclusive
    /// with `file_indices` — exactly one should be non-empty/non-None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub article_range: Option<(u64, u64)>,
    pub embed_model: EmbedModelInfo,
}

#[derive(Debug, Serialize)]
pub struct IngestPartitionResponse {
    pub accepted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: String,
}

/// POST /internal/gossip — member state exchange.
///
/// Push-pull in a single request: the caller ships us their current
/// `Mesh` view, we merge it into ours via `Mesh::merge_from`
/// (per-member `last_seen` last-writer-wins), and reply with our
/// now-updated snapshot so the caller can merge it in turn. After
/// one round both sides have converged on the pairwise union.
///
/// Rejects with 401 when the incoming `Mesh` has a different
/// `mesh_id` or `join_key_hash` — the auth boundary. Any member
/// with the join key can gossip freely; outsiders can't inject.
pub async fn gossip(
    State(state): State<AppState>,
    Json(req): Json<GossipRequest>,
) -> Result<Json<GossipResponse>, (StatusCode, Json<GossipRejection>)> {
    let incoming = req.mesh.into_mesh();
    let self_node_id = state.inner.self_node_id_swap.load_full().as_ref().clone();
    let mut mesh = state.inner.mesh.write().await;
    let report = mesh.merge_from(self_node_id, &incoming);

    if report.rejected {
        tracing::warn!("gossip: rejected — mesh_id or join_key_hash mismatch");
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(GossipRejection {
                reason: "mesh_id or join_key_hash does not match".into(),
            }),
        ));
    }

    if report.added > 0 || report.updated > 0 {
        // Info only when a NEW member was added. `updated > 0`
        // alone is the routine last_seen refresh that fires every
        // 10s — noisy heartbeat, not an event. Debug-level so
        // operators can still see it with a tighter filter.
        if report.added > 0 {
            tracing::info!(
                added = report.added,
                updated = report.updated,
                members = mesh.members.len(),
                "gossip: member added via incoming delta"
            );
        } else {
            tracing::debug!(
                updated = report.updated,
                members = mesh.members.len(),
                "gossip: merged incoming delta (last_seen refresh)"
            );
        }
        // Persist immediately on any added/updated member. The
        // gossip loop re-persists on its own cadence too, but that
        // leaves a 10s window where the founder could restart and
        // forget a newly-admitted joiner. Only fire on actual
        // deltas — no point re-writing mesh.json for a last_seen
        // bump that changed nothing structural.
        if let Some(hook) = state.inner.on_mesh_mutation.as_ref() {
            hook(&*mesh, self_node_id);
        }
    }

    Ok(Json(GossipResponse {
        mesh: MeshWire::from(&*mesh),
    }))
}

#[derive(Debug, Deserialize)]
pub struct GossipRequest {
    pub mesh: MeshWire,
}

#[derive(Debug, Serialize)]
pub struct GossipResponse {
    pub mesh: MeshWire,
}

#[derive(Debug, Serialize)]
pub struct GossipRejection {
    pub reason: String,
}

/// POST /internal/scheduling/intent — scheduling lock acquisition.
pub async fn scheduling_intent(
    State(_state): State<AppState>,
    Json(_payload): Json<SchedulingIntent>,
) -> (StatusCode, Json<SchedulingIntentResponse>) {
    (
        StatusCode::OK,
        Json(SchedulingIntentResponse {
            granted: true,
            leader: String::new(),
        }),
    )
}

/// POST /internal/scheduling/plan — shard plan broadcast.
///
/// Peer nodes call this when they compute a new inference plan.
/// The plan is stored in MeshStore and propagated via gossip.
pub async fn scheduling_plan(
    State(state): State<AppState>,
    Json(plan): Json<InferencePlan>,
) -> StatusCode {
    state.inner.inference_store.set_plan(&plan);
    StatusCode::OK
}

/// POST /internal/model/transfer — peer-to-peer model file transfer.
pub async fn model_transfer(
    State(_state): State<AppState>,
    Json(_payload): Json<serde_json::Value>,
) -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}

/// POST /internal/index/transfer — peer-to-peer corpus index transfer.
///
/// Receives a tar stream of a corpus shard directory.  The body is the
/// raw tar bytes; the corpus ID is in the `X-Corpus-Id` request header.
///
/// Protocol:
/// 1. Stream body to `<index_dir>/.incoming/<corpus_id>.tar`
/// 2. Untar to `<index_dir>/.incoming/<corpus_id>/`
/// 3. Verify `_corpus_meta.json` exists in the unpacked directory
/// 4. Atomic rename from `.incoming/<corpus_id>` to `indexes/<corpus_id>`
///
/// On crash during steps 1-3 the `.incoming/` dir is left dirty — the
/// daemon cleans it on next startup.  Step 4 is atomic on POSIX systems
/// so a completed merge can never see a partially-written index.
pub async fn index_transfer(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> (StatusCode, Json<serde_json::Value>) {
    let corpus_id = match headers.get("X-Corpus-Id").and_then(|v| v.to_str().ok()) {
        Some(id) => id.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "missing X-Corpus-Id header"})),
            );
        }
    };

    let engine = match &state.inner.corpus_engine {
        Some(e) => e.clone(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "no corpus engine on this node"})),
            );
        }
    };

    let index_dir = engine.index_dir().to_path_buf();
    let incoming_dir = index_dir.join(".incoming");
    let tarball_path = incoming_dir.join(format!("{corpus_id}.tar"));
    let unpack_path = incoming_dir.join(&corpus_id);

    if let Err(e) = std::fs::create_dir_all(&incoming_dir) {
        tracing::error!(error = %e, "index_transfer: failed to create .incoming dir");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }

    // Write tarball to disk.
    if let Err(e) = std::fs::write(&tarball_path, &body) {
        tracing::error!(error = %e, "index_transfer: failed to write tarball");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }

    // Untar.
    if let Err(e) = std::fs::create_dir_all(&unpack_path) {
        tracing::error!(error = %e, "index_transfer: failed to create unpack dir");
        let _ = std::fs::remove_file(&tarball_path);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }
    let tar_status = std::process::Command::new("tar")
        .args(["xf", &tarball_path.to_string_lossy(), "-C", &unpack_path.to_string_lossy()])
        .status();
    let _ = std::fs::remove_file(&tarball_path);
    match tar_status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            let _ = std::fs::remove_dir_all(&unpack_path);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("tar exited with {s}")})),
            );
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&unpack_path);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    }

    // Verify the unpacked directory has _corpus_meta.json.
    if !unpack_path.join("_corpus_meta.json").exists() {
        tracing::error!(
            corpus = %corpus_id,
            path = %unpack_path.display(),
            "index_transfer: unpacked shard is missing _corpus_meta.json"
        );
        let _ = std::fs::remove_dir_all(&unpack_path);
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "shard missing _corpus_meta.json"})),
        );
    }

    // Atomic rename to final location.
    let final_path = index_dir.join(&corpus_id);
    if let Err(e) = std::fs::rename(&unpack_path, &final_path) {
        tracing::error!(
            error = %e,
            from = %unpack_path.display(),
            to = %final_path.display(),
            "index_transfer: failed to rename to final path"
        );
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }

    let bytes = body.len() as u64;
    tracing::info!(
        corpus = %corpus_id,
        bytes,
        path = %final_path.display(),
        "index_transfer: shard installed successfully"
    );

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "corpus_id": corpus_id,
            "bytes_received": bytes,
            "path": final_path.to_string_lossy()
        })),
    )
}

/// POST /internal/knowledge/search — inter-node shard query (fan-out target).
///
/// Peer nodes call this to search corpus shards hosted on this node.
/// Returns the typed `KnowledgeSearchResponse` from `oicp-types`, the
/// same shape `/v1/knowledge/search` returns — so when the client-
/// side handler fans out to multiple peers it can deserialize all of
/// their replies into one container and merge-rank without a custom
/// wire format per peer.
pub async fn knowledge_search(
    State(state): State<AppState>,
    Json(request): Json<KnowledgeSearchRequest>,
) -> (StatusCode, Json<KnowledgeSearchResponse>) {
    let engine = match &state.inner.corpus_engine {
        Some(e) => e.clone(),
        None => {
            // Peers may have gossiped `hosted_corpora` that's since
            // been removed, or reach us during a brief pre-bootstrap
            // window; 503 + empty body tells them "not me, try
            // someone else" without poisoning their merge.
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(KnowledgeSearchResponse::default()),
            );
        }
    };

    let corpora = request.corpora.as_deref().unwrap_or(&[]);
    let limit = request.effective_limit() as usize;

    // Resolve the target corpora: either the caller's explicit list
    // (which MAY include corpora we don't host — we just skip those)
    // or all locally-installed corpora when the caller sent no
    // filter. Either way, we filter against what `installed_indexes`
    // actually reports so we never try to open an index we don't
    // have.
    let installed: std::collections::HashSet<String> = engine
        .installed_indexes()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|i| i.corpus_id)
        .collect();
    let search_corpora: Vec<String> = if corpora.is_empty() {
        installed.iter().cloned().collect()
    } else {
        corpora
            .iter()
            .filter(|c| installed.contains(*c))
            .cloned()
            .collect()
    };
    let corpora_unavailable: Vec<String> = corpora
        .iter()
        .filter(|c| !installed.contains(*c))
        .cloned()
        .collect();

    let mut all_results: Vec<KnowledgeResult> = Vec::new();
    for corpus_id in &search_corpora {
        match engine.open_index_for_corpus(corpus_id).await {
            Ok(index) => {
                match index
                    .search(&request.query_embedding, &request.query_text, limit)
                    .await
                {
                    Ok(results) => {
                        all_results.extend(results.into_iter().map(|r| KnowledgeResult {
                            content: r.content,
                            title: r.title,
                            corpus_id: corpus_id.clone(),
                            url: r.url,
                            score: r.score,
                            metadata: Default::default(),
                        }));
                    }
                    Err(e) => {
                        tracing::warn!(
                            corpus = corpus_id,
                            error = %e,
                            "internal knowledge_search: search failed"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    corpus = corpus_id,
                    error = %e,
                    "internal knowledge_search: open_index failed"
                );
            }
        }
    }

    all_results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all_results.truncate(limit);

    let hit_count = all_results.len();
    tracing::info!(
        corpora = ?search_corpora,
        hits = hit_count,
        "internal knowledge_search: served"
    );

    (
        StatusCode::OK,
        Json(KnowledgeSearchResponse {
            results: all_results,
            corpora_searched: search_corpora,
            corpora_unavailable,
            total_chunks_searched: None,
        }),
    )
}

/// GET /internal/latency/probe — RTT measurement endpoint.
pub async fn latency_probe() -> StatusCode {
    StatusCode::OK
}

// ── Node activity reporting ─────────────────────────────────

/// POST /internal/node/activity — sovereign-server reports coding activity level.
///
/// sovereign-server's ActivityReporter calls this after each level transition.
/// The level maps to an inference_availability weight that gossip carries to
/// peers so the scheduler routes work away from busy nodes.
///
/// Levels: "hot" (0.20) | "warm" (0.65) | "cool" (0.85) | "idle" (1.00)
pub async fn node_activity(
    State(state): State<AppState>,
    Json(payload): Json<NodeActivityPayload>,
) -> StatusCode {
    let availability = match payload.level.as_str() {
        "hot"  => 0.20_f32,
        "warm" => 0.65_f32,
        "cool" => 0.85_f32,
        _      => 1.00_f32,
    };
    tracing::info!(
        level = %payload.level,
        reason = %payload.reason,
        availability,
        "node_activity: inference_availability updated"
    );
    state.update_local_availability(availability).await;
    StatusCode::NO_CONTENT
}

#[derive(Debug, Deserialize)]
pub struct NodeActivityPayload {
    pub level: String,
    pub reason: String,
}

// ── Mesh join handshake ─────────────────────────────────────
//
// The founder (or any existing member) receives a POST from a
// would-be joiner carrying the raw `join_key`. We BLAKE3-hash it and
// compare against `mesh.join_key_hash`; on match we append the new
// member and return the full mesh snapshot so the joiner can adopt
// it locally. On mismatch we return 401 — the joiner treats this as
// "wrong mesh, try the next mDNS candidate" and moves on.
//
// Security posture (v1):
//   - Plain HTTP on the LAN. The join_key is exposed in transit to
//     anyone sniffing the local network; acceptable under the same
//     trust model as "I shared this link in a trusted chat".
//   - mesh_id in mDNS TXT is public (not secret); knowing it does
//     not grant membership. Only the raw key does, and it's hashed
//     at rest via `Mesh::join_key_hash`.
//   - Timing-attack-resistant equality lives in `membership::verify_join_key`.

#[derive(Debug, Deserialize)]
pub struct JoinRequest {
    pub join_key: String,
    pub joining_node_name: String,
    pub joining_node_addresses: Vec<SocketAddr>,
    /// Stable `NodeId` the joiner persists at
    /// `<data_dir>/node_id`. When present and not already claimed
    /// under a different name, the founder admits the joiner under
    /// this exact ID so rejoins don't leave zombies.
    ///
    /// Backward-compatible: older joiners don't send this field;
    /// `#[serde(default)]` makes the founder accept those requests
    /// unchanged.
    #[serde(default)]
    pub proposed_node_id: Option<NodeId>,
}

/// Wire shape for the full mesh snapshot. The Rust `Mesh` stores
/// members as `HashMap<NodeId, MemberRecord>`; JSON requires object
/// keys be strings, and `NodeId` serialises as a byte-array by
/// default — which crashes `serde_json` with "key must be a string".
/// We flatten to a Vec at the transport boundary, then reassemble
/// on the joiner side in `sovereign-mesh::join`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshWire {
    pub id: commonwealth_core::ids::MeshId,
    pub name: String,
    pub join_key_hash: [u8; 32],
    pub members: Vec<commonwealth_core::mesh::MemberRecord>,
    pub peers: Vec<commonwealth_core::mesh::MeshPeering>,
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
    /// Reassemble into a `Mesh`. Callers use this on the joiner side
    /// to adopt the founder's state.
    pub fn into_mesh(self) -> Mesh {
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

#[derive(Debug, Serialize)]
pub struct JoinResponse {
    /// Freshly-assigned id for the joining node.
    pub assigned_node_id: NodeId,
    /// Full authoritative mesh snapshot. Joiner replaces its local
    /// placeholder with this so member lists, peers, and the canonical
    /// mesh_id all match the founder's view.
    pub mesh: MeshWire,
}

#[derive(Debug, Serialize)]
pub struct JoinRejection {
    pub reason: String,
}

/// POST /internal/join — verify a join_key and (on match) admit the caller.
pub async fn join(
    State(state): State<AppState>,
    Json(req): Json<JoinRequest>,
) -> Result<Json<JoinResponse>, (StatusCode, Json<JoinRejection>)> {
    let self_node_id = state.inner.self_node_id_swap.load_full().as_ref().clone();
    let mut mesh = state.inner.mesh.write().await;

    match membership::accept_join_with_proposed_id(
        &mut mesh,
        &req.join_key,
        &req.joining_node_name,
        req.joining_node_addresses,
        self_node_id,
        req.proposed_node_id,
    ) {
        Ok(new_id) => {
            tracing::info!(
                new_node = %new_id,
                joining_name = %req.joining_node_name,
                "handshake_accepted: admitted new mesh member"
            );
            // Persist IMMEDIATELY on join accept so the founder
            // doesn't forget this member if it restarts within the
            // 10s gossip-loop re-persist window. Hook is `None` in
            // tests and the standalone daemon, so this is a no-op
            // where persistence is managed elsewhere.
            if let Some(hook) = state.inner.on_mesh_mutation.as_ref() {
                hook(&*mesh, self_node_id);
            }
            Ok(Json(JoinResponse {
                assigned_node_id: new_id,
                mesh: MeshWire::from(&*mesh),
            }))
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                joining_name = %req.joining_node_name,
                "handshake_rejected: join request denied"
            );
            Err((
                StatusCode::UNAUTHORIZED,
                Json(JoinRejection {
                    reason: e.to_string(),
                }),
            ))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SchedulingIntent {
    pub node_id: String,
    pub intent: String,
}

#[derive(Debug, Serialize)]
pub struct SchedulingIntentResponse {
    pub granted: bool,
    pub leader: String,
}

// ─── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode as HttpStatus};
    use axum::routing::post;
    use axum::Router;
    use tower::ServiceExt;

    use crate::state::test_app_state;

    fn activity_router() -> (AppState, Router) {
        let state = test_app_state();
        let app = Router::new()
            .route("/internal/node/activity", post(node_activity))
            .with_state(state.clone());
        (state, app)
    }

    async fn post_activity(app: Router, level: &str, reason: &str) -> HttpStatus {
        let body = serde_json::json!({ "level": level, "reason": reason }).to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/internal/node/activity")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        response.status()
    }

    #[tokio::test]
    async fn hot_level_returns_204_no_content() {
        let (_, app) = activity_router();
        let status = post_activity(app, "hot", "tests_running").await;
        assert_eq!(status, HttpStatus::NO_CONTENT);
    }

    #[tokio::test]
    async fn hot_level_sets_availability_to_020() {
        let (state, app) = activity_router();
        post_activity(app, "hot", "tests_running").await;
        let val = *state.inner.local_inference_availability.read().await;
        assert!(
            (val - 0.20).abs() < 1e-6,
            "hot must set availability to 0.20, got {val}"
        );
    }

    #[tokio::test]
    async fn warm_level_sets_availability_to_065() {
        let (state, app) = activity_router();
        post_activity(app, "warm", "recent_edits").await;
        let val = *state.inner.local_inference_availability.read().await;
        assert!((val - 0.65).abs() < 1e-6, "warm must set availability to 0.65, got {val}");
    }

    #[tokio::test]
    async fn cool_level_sets_availability_to_085() {
        let (state, app) = activity_router();
        post_activity(app, "cool", "settling").await;
        let val = *state.inner.local_inference_availability.read().await;
        assert!((val - 0.85).abs() < 1e-6, "cool must set availability to 0.85, got {val}");
    }

    #[tokio::test]
    async fn idle_level_sets_availability_to_100() {
        // Start hot, then go idle to verify full round-trip.
        let (state, app) = activity_router();
        post_activity(app.clone(), "hot", "start").await;
        post_activity(app, "idle", "long_pause").await;
        let val = *state.inner.local_inference_availability.read().await;
        assert!((val - 1.00).abs() < 1e-6, "idle must set availability to 1.00, got {val}");
    }

    #[tokio::test]
    async fn unknown_level_defaults_to_idle() {
        let (state, app) = activity_router();
        post_activity(app, "turbo", "unknown_level").await;
        let val = *state.inner.local_inference_availability.read().await;
        assert!((val - 1.00).abs() < 1e-6, "unknown level must default to 1.00, got {val}");
    }
}
