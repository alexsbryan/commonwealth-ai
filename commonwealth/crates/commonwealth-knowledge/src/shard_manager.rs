use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use corpus_engine::{ChunkRange, CorpusEngine, IndexInfo, ShardInfo};
use commonwealth_core::contributions::LedgerEventKind;
use commonwealth_core::ids::{HandoffId, NodeId};
use commonwealth_core::knowledge::{IngestionHandoff, KnowledgeShardAssignment, PartitionStatus};
use commonwealth_state::{ContributionEmitter, MeshStore};

pub struct ShardManager {
    engine: Arc<CorpusEngine>,
    shard_dir: PathBuf,
    mesh_store: Arc<MeshStore>,
    /// Optional emitter for `ShardTransferred` events on the merge
    /// leader's pull path. Optional so existing call sites that
    /// don't have a daemon-scoped emitter (legacy tests, ad-hoc
    /// tooling) keep working unchanged. Missing emitter ⇒ no
    /// ledger event ⇒ ledger silently underreports the transfer,
    /// which is honest about the gap.
    emitter: Option<ContributionEmitter>,
    /// Optional handle on the daemon's `WorkQueueManager`. Required
    /// for queue-mode `coordinate_merge` (legacy static-partition
    /// handoffs carry the participating peer list inline; queue-mode
    /// handoffs have an empty `partitions` and we have to read it
    /// from `HandoffQueue.participating_peers`). Missing queue ⇒
    /// queue-mode merges fall through with a clear log line and
    /// nothing else, which is the safest behaviour for tests and
    /// ad-hoc tooling that constructs a ShardManager without a
    /// daemon attached.
    work_queue: Option<Arc<crate::work_queue::WorkQueueManager>>,
}

impl ShardManager {
    pub fn new(engine: Arc<CorpusEngine>, shard_dir: PathBuf, mesh_store: Arc<MeshStore>) -> Self {
        Self {
            engine,
            shard_dir,
            mesh_store,
            emitter: None,
            work_queue: None,
        }
    }

    /// Attach a `ContributionEmitter` so successful peer-shard
    /// pulls during `coordinate_merge` write `ShardTransferred`
    /// events into the dimensional ledger. The emit is on behalf
    /// of the peer (`from_node = peer.node_id`), so the aggregator
    /// credits the actual sender's `bytes_served` rather than the
    /// puller's. See
    /// `commonwealth_core::contributions::aggregate` for the
    /// pull-emission special case.
    pub fn with_emitter(mut self, emitter: ContributionEmitter) -> Self {
        self.emitter = Some(emitter);
        self
    }

    /// Attach the daemon's `WorkQueueManager` so queue-mode
    /// `coordinate_merge` can enumerate participating peers from
    /// `HandoffQueue.participating_peers`. Without it, queue-mode
    /// merges log a warning and become a no-op rather than
    /// crashing — the legacy partition-list path is unaffected.
    pub fn with_work_queue(
        mut self,
        work_queue: Arc<crate::work_queue::WorkQueueManager>,
    ) -> Self {
        self.work_queue = Some(work_queue);
        self
    }

    // ---- Shard preparation and installation ----

    /// Prepare shard directories for distribution to assigned nodes.
    pub async fn prepare_shards(
        &self,
        corpus_id: &str,
        assignments: &[KnowledgeShardAssignment],
    ) -> corpus_engine::Result<Vec<PreparedShard>> {
        let mut shards = Vec::new();
        for assignment in assignments {
            if assignment.corpus_id != corpus_id {
                continue;
            }
            if let Some(ref range) = assignment.chunk_range {
                let output = self.shard_dir.join(format!(
                    "{}-shard-{}-{}",
                    corpus_id, range.start_id, range.end_id
                ));
                let chunk_range = ChunkRange::new(range.start_id, range.end_id);
                let info = self
                    .engine
                    .extract_shard(corpus_id, chunk_range, &output)
                    .await?;
                shards.push(PreparedShard {
                    target_node: assignment.node_id,
                    info,
                });
            }
        }
        Ok(shards)
    }

    /// Install a received shard directory into the shared index directory.
    pub fn install_received_shard(
        &self,
        corpus_id: &str,
        chunk_range: &ChunkRange,
        received_dir: &Path,
    ) -> corpus_engine::Result<PathBuf> {
        let dest = self.engine.index_dir().join(format!(
            "{}-shard-{}-{}",
            corpus_id, chunk_range.start_id, chunk_range.end_id
        ));
        std::fs::rename(received_dir, &dest).map_err(corpus_engine::Error::Io)?;
        Ok(dest)
    }

    /// Merge all local shard directories for a corpus into a complete index.
    pub async fn consolidate_shards(
        &self,
        corpus_id: &str,
    ) -> corpus_engine::Result<IndexInfo> {
        let shard_dirs: Vec<PathBuf> = self
            .engine
            .installed_indexes()
            .await?
            .iter()
            .filter(|i| i.corpus_id == corpus_id && i.is_shard)
            .map(|i| i.path.clone())
            .collect();

        if shard_dirs.is_empty() {
            return Err(corpus_engine::Error::NoShardsFound(corpus_id.into()));
        }

        let output = self.engine.index_dir().join(corpus_id);
        let info = self.engine.merge_shards(&shard_dirs, &output).await?;

        // Clean up shard directories after successful merge.
        for path in &shard_dirs {
            std::fs::remove_dir_all(path).ok();
        }

        Ok(info)
    }

    // ---- Collaborative merge coordinator ----

    /// Load the `IngestionHandoff` for a given handoff ID from gossip state.
    fn load_handoff(&self, handoff_id: HandoffId) -> Option<IngestionHandoff> {
        let key = format!("handoff:{handoff_id}");
        self.mesh_store
            .get("corpus-engine", &key)
            .ok()
            .flatten()
            .and_then(|e| serde_json::from_slice(&e.value).ok())
    }

    /// Persist an updated `IngestionHandoff` to gossip state.
    fn save_handoff(&self, handoff: &IngestionHandoff, local_node_id: NodeId) {
        let key = format!("handoff:{}", handoff.handoff_id);
        if let Ok(bytes) = serde_json::to_vec(handoff) {
            let _ = self.mesh_store.set(
                "corpus-engine",
                &key,
                bytes::Bytes::from(bytes),
                local_node_id,
            );
        }
    }

    /// Run the merge coordination state machine for a completed handoff.
    ///
    /// Two modes, distinguished by whether `handoff.partitions` is
    /// populated:
    ///
    /// * **Legacy static-partition** (non-empty `partitions`):
    ///   marks this node's partition as Complete, polls peers' partition
    ///   statuses, then fetches their shards. Merge leader is the lowest
    ///   `NodeId` among all partitions.
    /// * **Queue-mode** (empty `partitions`, `phase != legacy_open`):
    ///   the coordinator's `WorkQueueManager` already sequenced the work
    ///   and waited for every unit to terminate before triggering this
    ///   call (see `corpus_complete_unit`'s phase-transition wiring).
    ///   Participating peers come from `HandoffQueue.participating_peers`
    ///   rather than the empty `partitions` list. Merge leader is set
    ///   to the coordinator at handoff creation.
    pub async fn coordinate_merge(
        &self,
        handoff_id: HandoffId,
        local_node_id: NodeId,
        peer_shard_base_urls: &[(NodeId, String)],
    ) -> corpus_engine::Result<Option<IndexInfo>> {
        const PARTITION_POLL_INTERVAL: Duration = Duration::from_secs(30);
        const MAX_WAIT_SECS: u64 = 3600; // 1 hour

        let mut handoff = match self.load_handoff(handoff_id) {
            Some(h) => h,
            None => {
                tracing::warn!(handoff = %handoff_id, "coordinate_merge: handoff not found");
                return Ok(None);
            }
        };

        let queue_mode = handoff.is_queue_mode();

        // In queue mode, synthesize partition entries from
        // `HandoffQueue.participating_peers` so the rest of the
        // function (which is partition-list-driven) just works. Mark
        // every entry Complete since `corpus_complete_unit` only
        // triggers this path when the queue's phase is Merging,
        // which means every unit terminated.
        if queue_mode {
            let Some(work_queue) = &self.work_queue else {
                tracing::warn!(
                    handoff = %handoff_id,
                    "coordinate_merge: queue-mode handoff but no WorkQueueManager attached — \
                     skipping merge. Wire ShardManager::with_work_queue() at construction."
                );
                return Ok(None);
            };
            let Some(snapshot) = work_queue.snapshot(&handoff_id).await else {
                tracing::warn!(
                    handoff = %handoff_id,
                    "coordinate_merge: queue-mode handoff but coordinator has no live queue \
                     for it — was the daemon restarted mid-ingest?"
                );
                return Ok(None);
            };
            // The coordinator itself counts as a participant when its
            // own partition dir exists on disk — `participating_peers`
            // tracks only peers that pulled units (the coordinator
            // ingests via its own pull loop, so it should be there
            // too, but belt-and-braces).
            let mut peers: std::collections::HashSet<NodeId> =
                snapshot.participating_peers.clone();
            peers.insert(local_node_id);
            let synthesized: Vec<commonwealth_core::knowledge::IngestionPartition> = peers
                .into_iter()
                .map(|node_id| commonwealth_core::knowledge::IngestionPartition {
                    node_id,
                    file_indices: Vec::new(),
                    article_range: None,
                    status: PartitionStatus::Complete {
                        completed_at: snapshot.last_mutation_ms,
                    },
                })
                .collect();
            tracing::info!(
                handoff = %handoff_id,
                peers = synthesized.len(),
                "coordinate_merge: queue-mode — synthesized partitions from work queue"
            );
            handoff.partitions = synthesized;
        }

        // Mark this node's partition as Complete (legacy path; no-op
        // for queue mode where every entry is already Complete).
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let mut updated = false;
        for p in &mut handoff.partitions {
            if p.node_id == local_node_id {
                if !matches!(p.status, PartitionStatus::Complete { .. }) {
                    p.status = PartitionStatus::Complete { completed_at: now_ms };
                    updated = true;
                }
            }
        }
        if updated {
            handoff.updated_at = now_ms;
            self.save_handoff(&handoff, local_node_id);
            tracing::info!(
                handoff = %handoff_id,
                node = %local_node_id,
                "coordinate_merge: marked local partition complete"
            );
        }

        // Check if we are the merge leader.
        if handoff.merge_leader != Some(local_node_id) {
            tracing::info!(
                handoff = %handoff_id,
                leader = ?handoff.merge_leader,
                "coordinate_merge: not the merge leader, returning"
            );
            return Ok(None);
        }

        tracing::info!(
            handoff = %handoff_id,
            queue_mode,
            "coordinate_merge: we are the merge leader"
        );

        // Poll until all partitions are complete or timeout. In
        // queue mode this is a no-op — we already synthesized every
        // entry as Complete.
        if !queue_mode {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(MAX_WAIT_SECS);
            loop {
                let all_done = handoff.partitions.iter().all(|p| {
                    matches!(
                        p.status,
                        PartitionStatus::Complete { .. } | PartitionStatus::Failed { .. }
                    )
                });
                if all_done {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    tracing::warn!(
                        handoff = %handoff_id,
                        "coordinate_merge: timed out, merging with available shards"
                    );
                    break;
                }
                tokio::time::sleep(PARTITION_POLL_INTERVAL).await;
                if let Some(h) = self.load_handoff(handoff_id) {
                    handoff = h;
                }
            }
        }

        // Collect shard directories.
        let mut shard_dirs: Vec<PathBuf> = Vec::new();

        // Local shard. Machine A may have ingested into the original corpus
        // path instead of a partition path (when it had existing partial data).
        {
            let partition_path = self.engine.index_dir()
                .join(format!("{}-partition-{}", handoff.corpus_id, local_node_id));
            let original_path = self.engine.index_dir().join(&handoff.corpus_id);
            if partition_path.exists() {
                shard_dirs.push(partition_path);
            } else if original_path.join("_corpus_meta.json").exists() {
                tracing::info!(
                    corpus = %handoff.corpus_id,
                    "coordinate_merge: using original index path as local shard"
                );
                shard_dirs.push(original_path);
            }
        }

        // Remote shards.
        for partition in &handoff.partitions {
            if partition.node_id == local_node_id {
                continue;
            }
            if !matches!(partition.status, PartitionStatus::Complete { .. }) {
                tracing::warn!(
                    node = %partition.node_id,
                    "coordinate_merge: skipping incomplete/failed partition"
                );
                continue;
            }

            let peer_url = peer_shard_base_urls
                .iter()
                .find(|(nid, _)| *nid == partition.node_id)
                .map(|(_, u)| u.clone());

            let Some(base_url) = peer_url else {
                tracing::warn!(
                    node = %partition.node_id,
                    "coordinate_merge: no address for peer, skipping"
                );
                continue;
            };

            let dest_dir = self.engine.index_dir()
                .join(format!("{}-partition-{}", handoff.corpus_id, partition.node_id));

            match self
                .fetch_remote_shard(
                    &handoff.corpus_id,
                    &base_url,
                    &dest_dir,
                    local_node_id,
                )
                .await
            {
                Ok(bytes_received) => {
                    // Successful transfer: emit `ShardTransferred`
                    // on behalf of the peer that shipped the bytes.
                    // Without this, the dimensional ledger would
                    // never reflect any peer-to-peer activity during
                    // collaborative ingest — only the merge leader
                    // observes the transfer completing, so only the
                    // merge leader can emit. See `aggregate` for
                    // the pull-emission special case.
                    if let Some(em) = &self.emitter {
                        em.record(LedgerEventKind::ShardTransferred {
                            from_node: partition.node_id,
                            to_node: local_node_id,
                            corpus_id: handoff.corpus_id.clone(),
                            bytes: bytes_received,
                        });
                    }
                    shard_dirs.push(dest_dir);
                }
                Err(e) => tracing::warn!(
                    node = %partition.node_id,
                    error = %e,
                    "coordinate_merge: failed to fetch remote shard"
                ),
            }
        }

        if shard_dirs.is_empty() {
            return Err(corpus_engine::Error::NoShardsFound(
                format!("no shard dirs for handoff {handoff_id}")
            ));
        }

        let output_dir = self.engine.index_dir().join(&handoff.corpus_id);
        tracing::info!(
            handoff = %handoff_id,
            shards = shard_dirs.len(),
            output = %output_dir.display(),
            "coordinate_merge: merging partitions"
        );

        let info = self.engine.merge_partitions(&shard_dirs, &output_dir).await?;

        for shard_dir in &shard_dirs {
            std::fs::remove_dir_all(shard_dir).ok();
        }

        tracing::info!(
            handoff = %handoff_id,
            chunks = info.chunk_count,
            "coordinate_merge: complete"
        );

        Ok(Some(info))
    }

    /// Fetch a remote corpus partition shard via HTTP transfer.
    /// Returns the number of bytes received on the wire (used as the
    /// `bytes` field of the `ShardTransferred` ledger event the
    /// caller emits on success).
    ///
    /// `local_node_id` is the merge-leader's id. It rides along on
    /// the request as `X-Node-Id` so the serving daemon can attribute
    /// the request to a specific recipient — useful for the peer's
    /// side of mesh-health observability when we eventually serve
    /// shards out of this route too.
    async fn fetch_remote_shard(
        &self,
        corpus_id: &str,
        base_url: &str,
        dest_dir: &Path,
        local_node_id: NodeId,
    ) -> anyhow::Result<u64> {
        // GET /internal/index/serve, NOT POST /internal/index/transfer.
        // The transfer endpoint reads the request body (upload
        // semantics); the serve endpoint writes the response body
        // (download semantics). Earlier versions of this function
        // called POST /transfer with no body, which silently
        // returned a JSON error and broke every queue-mode merge —
        // see `routes_internal::index_serve` for the full story.
        let serve_url = format!("{base_url}/internal/index/serve");
        let client = reqwest::Client::new();
        let resp = client
            .get(&serve_url)
            .header("X-Corpus-Id", corpus_id)
            .header("X-Node-Id", local_node_id.to_string())
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!(
                "index/serve returned {} from {base_url}",
                resp.status()
            );
        }

        let bytes = resp.bytes().await?;
        let bytes_received = bytes.len() as u64;
        std::fs::create_dir_all(dest_dir)?;
        let tarball_path = dest_dir.with_extension("tar");
        std::fs::write(&tarball_path, &bytes)?;

        let tar_status = std::process::Command::new("tar")
            .args([
                "xf",
                &tarball_path.to_string_lossy(),
                "-C",
                &dest_dir.to_string_lossy(),
            ])
            .status()?;
        std::fs::remove_file(&tarball_path).ok();

        if !tar_status.success() {
            anyhow::bail!("tar extraction failed for shard from {base_url}");
        }

        Ok(bytes_received)
    }

    // ---- T7: Index transfer sender ----

    /// Stream a local corpus index to a remote node as a tar archive.
    ///
    /// Tars `<index_dir>/<corpus_id>/`, POSTs it to
    /// `POST {target_base_url}/internal/index/transfer`, and returns a
    /// receipt on success.
    ///
    /// `to_node` and `emitter` are optional so existing callers
    /// without ledger context can keep calling the function. When
    /// supplied, a `ShardTransferred` event is emitted on success
    /// per the dimensional ledger spec — the sender owns the byte
    /// count, and the recipient's `bytes_received` is inferred from
    /// the same event during aggregation
    /// (`commonwealth_core::contributions::aggregate`).
    pub async fn stream_index(
        &self,
        corpus_id: &str,
        target_base_url: &str,
        to_node: Option<NodeId>,
        emitter: Option<&ContributionEmitter>,
    ) -> anyhow::Result<TransferReceipt> {
        let index_path = self.engine.index_dir().join(corpus_id);
        if !index_path.exists() {
            anyhow::bail!(
                "corpus '{}' not found at {}",
                corpus_id,
                index_path.display()
            );
        }

        // Create a temporary tar archive.
        let tar_path = self.engine.index_dir()
            .join(format!(".{corpus_id}.transfer.tar"));

        let tar_status = std::process::Command::new("tar")
            .args([
                "cf",
                &tar_path.to_string_lossy(),
                "-C",
                &self.engine.index_dir().to_string_lossy(),
                corpus_id,
            ])
            .status()?;

        if !tar_status.success() {
            anyhow::bail!("tar creation failed for corpus '{corpus_id}'");
        }

        let tar_bytes = std::fs::read(&tar_path)?;
        std::fs::remove_file(&tar_path).ok();
        let bytes_transferred = tar_bytes.len() as u64;

        let transfer_url = format!("{target_base_url}/internal/index/transfer");
        let client = reqwest::Client::new();
        let resp = client
            .post(&transfer_url)
            .header("X-Corpus-Id", corpus_id)
            .header("Content-Type", "application/octet-stream")
            .body(tar_bytes)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "index/transfer returned {status} from {target_base_url}: {body}"
            );
        }

        tracing::info!(
            corpus = %corpus_id,
            bytes = bytes_transferred,
            target = %target_base_url,
            "stream_index: shard transferred successfully"
        );

        // Emit `ShardTransferred` on success when both the
        // recipient id and an emitter are supplied. The aggregator
        // also lands `bytes_received` on `to_node` from the same
        // event, so a single emission produces both halves of the
        // byte ledger (per `aggregate` in commonwealth-core).
        if let (Some(to), Some(em)) = (to_node, emitter) {
            em.record(LedgerEventKind::ShardTransferred {
                from_node: em.self_node_id(),
                to_node: to,
                corpus_id: corpus_id.to_string(),
                bytes: bytes_transferred,
            });
        }

        Ok(TransferReceipt {
            corpus_id: corpus_id.to_string(),
            bytes_transferred,
        })
    }
} // end impl ShardManager

pub struct PreparedShard {
    pub target_node: NodeId,
    pub info: ShardInfo,
}

/// Result of a successful corpus index transfer.
#[derive(Debug)]
pub struct TransferReceipt {
    pub corpus_id: String,
    pub bytes_transferred: u64,
}
