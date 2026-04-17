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
    plan_collaborative_ingestion, plan_collaborative_ingestion_jsonl, CollaborativeIngestionError,
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
    let self_id = state.inner.self_node_id;
    let local_member = mesh.members.get(&self_id).cloned().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody { error: "local node not found in mesh".into() }),
        )
    })?;

    // Only include online peers — offline members can't accept partitions,
    // and freshly-joined peers may have NodeStatus::Online before their
    // capability gossip fully propagates (free_storage_gb may still be 0).
    let candidates: Vec<_> = mesh
        .members
        .values()
        .filter(|m| m.node_id != self_id && m.status == NodeStatus::Online)
        .cloned()
        .collect();
    drop(mesh);

    let local_embed_model = state.inner.inference_store.get_local_embed_model()
        .ok_or_else(|| (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: "embed model not configured on this node — cannot plan collaboration".into(),
            }),
        ))?;

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

    // Notify remote peers.  Fire-and-forget with tracing: failing to
    // reach a peer is logged but doesn't fail the collaborate call —
    // the partition is still in the handoff, and the peer will pick up
    // the assignment via gossip.
    {
        let mesh = state.inner.mesh.read().await;
        // Start our own partition in the background.
        {
            let local_partition = handoff.partitions.iter()
                .find(|p| p.node_id == self_id)
                .cloned();
            if let Some(partition) = local_partition {
                if let Some(engine) = state.inner.corpus_engine.clone() {
                    let corpus_id = handoff.corpus_id.clone();
                    let recipe_id = handoff.recipe_id.clone();
                    let handoff_id = handoff.handoff_id;
                    let mesh_store = Arc::clone(&state.inner.mesh_store);
                    let state_clone = state.clone();
                    let peer_urls: Vec<(NodeId, String)> = mesh
                        .members
                        .values()
                        .filter(|m| m.node_id != self_id)
                        .filter_map(|m| {
                            m.addresses.first().map(|a| {
                                (m.node_id, format!("http://{}:9742", a.ip()))
                            })
                        })
                        .collect();
                    tokio::spawn(async move {
                        // Resume into the existing partial index if present — preserves
                        // accumulated chunks instead of writing to a new partition dir.
                        let original_path = engine.index_dir().join(&corpus_id);
                        let output_path = if original_path.join("_corpus_meta.json").exists() {
                            original_path
                        } else {
                            engine.index_dir().join(format!("{corpus_id}-partition-{self_id}"))
                        };
                        tracing::info!(
                            corpus = %corpus_id,
                            files = partition.file_indices.len(),
                            output = %output_path.display(),
                            "collaborate: starting local partition"
                        );
                        state_clone.inner.active_ingests.write().await
                            .insert(corpus_id.clone());
                        let ingest_result = engine.ingest_with_overrides(
                            &recipe_id,
                            Some(partition.file_indices),
                            partition.article_range,
                            &output_path,
                            None,
                        ).await;
                        state_clone.inner.active_ingests.write().await
                            .remove(&corpus_id);
                        match ingest_result {
                            Ok(result) => {
                                tracing::info!(
                                    corpus = %corpus_id,
                                    chunks = result.chunks_created,
                                    "collaborate: local partition complete — triggering merge check"
                                );
                                let shard_mgr = ShardManager::new(
                                    Arc::clone(&engine),
                                    engine.index_dir().to_path_buf(),
                                    mesh_store,
                                );
                                match shard_mgr.coordinate_merge(handoff_id, self_id, &peer_urls).await {
                                    Ok(Some(info)) => tracing::info!(
                                        corpus = %corpus_id,
                                        chunks = info.chunk_count,
                                        "collaborate: merge complete"
                                    ),
                                    Ok(None) => tracing::info!(
                                        corpus = %corpus_id,
                                        "collaborate: not merge leader — leader will complete merge"
                                    ),
                                    Err(e) => tracing::error!(
                                        corpus = %corpus_id,
                                        error = %e,
                                        "collaborate: merge failed"
                                    ),
                                }
                            }
                            Err(e) => tracing::error!(
                                corpus = %corpus_id,
                                error = %e,
                                "collaborate: local partition failed"
                            ),
                        }
                    });
                }
            }
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
            let Some(addr) = peer.addresses.first() else {
                tracing::warn!(
                    node = %partition.node_id,
                    "collaborate: peer has no address — skipping notification"
                );
                continue;
            };
            let peer_url = format!("http://{}:{}/internal/corpus/ingest_partition", addr.ip(), 9742);
            let payload = IngestPartitionRequest {
                handoff_id: handoff.handoff_id,
                corpus_id: handoff.corpus_id.clone(),
                recipe_id: handoff.recipe_id.clone(),
                file_indices: partition.file_indices.clone(),
                article_range: partition.article_range,
                embed_model: handoff.embed_model.clone(),
            };
            let peer_url_clone = peer_url.clone();
            let node_id = partition.node_id;
            tokio::spawn(async move {
                let client = reqwest::Client::new();
                match client.post(&peer_url_clone).json(&payload).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        tracing::info!(node = %node_id, url = %peer_url_clone, "collaborate: peer accepted partition");
                    }
                    Ok(resp) => {
                        tracing::warn!(
                            node = %node_id,
                            url = %peer_url_clone,
                            status = %resp.status(),
                            "collaborate: peer rejected partition"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            node = %node_id,
                            url = %peer_url_clone,
                            error = %e,
                            "collaborate: could not reach peer"
                        );
                    }
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
    let local_node_id = state.inner.self_node_id;
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
    let self_node_id = state.inner.self_node_id;
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
    let self_node_id = state.inner.self_node_id;
    let mut mesh = state.inner.mesh.write().await;

    match membership::accept_join(
        &mut mesh,
        &req.join_key,
        &req.joining_node_name,
        req.joining_node_addresses,
        self_node_id,
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
