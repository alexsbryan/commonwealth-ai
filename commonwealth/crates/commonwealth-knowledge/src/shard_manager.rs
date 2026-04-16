use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use corpus_engine::{ChunkRange, CorpusEngine, IndexInfo, ShardInfo};
use commonwealth_core::ids::{HandoffId, NodeId};
use commonwealth_core::knowledge::{IngestionHandoff, KnowledgeShardAssignment, PartitionStatus};
use commonwealth_state::MeshStore;

pub struct ShardManager {
    engine: Arc<CorpusEngine>,
    shard_dir: PathBuf,
    mesh_store: Arc<MeshStore>,
}

impl ShardManager {
    pub fn new(engine: Arc<CorpusEngine>, shard_dir: PathBuf, mesh_store: Arc<MeshStore>) -> Self {
        Self { engine, shard_dir, mesh_store }
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

    /// Run the merge coordination state machine for a completed partition.
    ///
    /// Called by the `ingest_partition` handler when local ingestion finishes.
    /// If this node is not the designated merge leader it returns early.
    /// If it is the leader it waits for all partitions to complete (with
    /// timeout), collects peer shards via HTTP transfer, and merges.
    ///
    /// Merge leader: the node with the lowest NodeId among all partitions.
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

        // Mark this node's partition as Complete.
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
        if handoff.merge_assigned_to != Some(local_node_id) {
            tracing::info!(
                handoff = %handoff_id,
                leader = ?handoff.merge_assigned_to,
                "coordinate_merge: not the merge leader, returning"
            );
            return Ok(None);
        }

        tracing::info!(
            handoff = %handoff_id,
            "coordinate_merge: we are the merge leader, waiting for all partitions"
        );

        // Poll until all partitions are complete or timeout.
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

        // Collect shard directories.
        let mut shard_dirs: Vec<PathBuf> = Vec::new();

        // Local shard.
        let local_shard = self.engine.index_dir()
            .join(format!("{}-partition-{}", handoff.corpus_id, local_node_id));
        if local_shard.exists() {
            shard_dirs.push(local_shard);
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

            match self.fetch_remote_shard(&handoff.corpus_id, &base_url, &dest_dir).await {
                Ok(()) => shard_dirs.push(dest_dir),
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
    async fn fetch_remote_shard(
        &self,
        corpus_id: &str,
        base_url: &str,
        dest_dir: &Path,
    ) -> anyhow::Result<()> {
        let transfer_url = format!("{base_url}/internal/index/transfer");
        let client = reqwest::Client::new();
        let resp = client
            .post(&transfer_url)
            .header("X-Corpus-Id", corpus_id)
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!(
                "index/transfer returned {} from {base_url}",
                resp.status()
            );
        }

        let bytes = resp.bytes().await?;
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

        Ok(())
    }

    // ---- T7: Index transfer sender ----

    /// Stream a local corpus index to a remote node as a tar archive.
    ///
    /// Tars `<index_dir>/<corpus_id>/`, POSTs it to
    /// `POST {target_base_url}/internal/index/transfer`, and returns a
    /// receipt on success.
    pub async fn stream_index(
        &self,
        corpus_id: &str,
        target_base_url: &str,
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
