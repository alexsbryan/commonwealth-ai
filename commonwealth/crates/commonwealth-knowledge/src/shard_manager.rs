// SPDX-License-Identifier: AGPL-3.0-or-later
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use commonwealth_core::contributions::LedgerEventKind;
use commonwealth_core::ids::{HandoffId, NodeId};
use commonwealth_core::knowledge::{IngestionHandoff, KnowledgeShardAssignment, PartitionStatus};
use commonwealth_state::{ContributionEmitter, MeshStore};
use corpus_engine::{ChunkRange, Corpus, CorpusEngine, IndexInfo, ShardInfo};

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
    pub fn with_work_queue(mut self, work_queue: Arc<crate::work_queue::WorkQueueManager>) -> Self {
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
    pub async fn consolidate_shards(&self, corpus_id: &str) -> corpus_engine::Result<IndexInfo> {
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
            // Resolve participating peers. Preferred source: the live
            // `WorkQueueManager` snapshot, which is updated as peers
            // lease/complete units. Fallback when the snapshot is
            // missing (queue-mode handoff that outlived a coordinator
            // restart): the gossiped `processed_shards:<corpus>:<peer>`
            // entries in `MeshStore`. Each peer that has actually done
            // work for this corpus publishes a non-empty list under
            // its own slot, so the union of `entry.origin`s across
            // every non-empty entry is the participating-peers set.
            // Without this fallback, restart-mid-Merging silently
            // strands the corpus in "ingestion done, never merged"
            // and the operator has no recovery path short of
            // re-running ingest from scratch.
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            let (participating, completed_at, source) = match self.work_queue.as_ref() {
                Some(wq) => match wq.snapshot(&handoff_id).await {
                    Some(snap) => (
                        snap.participating_peers.clone(),
                        snap.last_mutation_ms,
                        "work queue",
                    ),
                    None => (
                        participating_peers_from_gossip(&self.mesh_store, &handoff.corpus_id),
                        now_ms,
                        "gossip fallback (live queue missing — coordinator restart?)",
                    ),
                },
                None => (
                    participating_peers_from_gossip(&self.mesh_store, &handoff.corpus_id),
                    now_ms,
                    "gossip fallback (no work queue attached)",
                ),
            };

            if participating.is_empty() {
                tracing::warn!(
                    handoff = %handoff_id,
                    corpus = %handoff.corpus_id,
                    source,
                    "coordinate_merge: queue-mode handoff with no resolvable participants — \
                     skipping merge"
                );
                return Ok(None);
            }

            // The coordinator itself counts as a participant when its
            // own partition dir exists on disk — `participating_peers`
            // tracks only peers that pulled units (the coordinator
            // ingests via its own pull loop, so it should be there
            // too, but belt-and-braces).
            let mut peers = participating;
            peers.insert(local_node_id);
            let synthesized: Vec<commonwealth_core::knowledge::IngestionPartition> = peers
                .into_iter()
                .map(|node_id| commonwealth_core::knowledge::IngestionPartition {
                    node_id,
                    file_indices: Vec::new(),
                    article_range: None,
                    status: PartitionStatus::Complete { completed_at },
                })
                .collect();
            tracing::info!(
                handoff = %handoff_id,
                peers = synthesized.len(),
                source,
                "coordinate_merge: queue-mode — synthesized partitions"
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
            if p.node_id == local_node_id && !matches!(p.status, PartitionStatus::Complete { .. }) {
                p.status = PartitionStatus::Complete {
                    completed_at: now_ms,
                };
                updated = true;
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

        // Ephemeral grant-scoped ingest: once we pull a peer's shard we tell
        // that peer to wipe its own working partition dir (wipe-after-pull).
        // Captured here so the borrow doesn't tangle with the loop below.
        let is_ephemeral = handoff.ephemeral;

        // Local shard. Machine A may have ingested into the original corpus
        // path instead of a partition path (when it had existing partial data).
        {
            let partition_path = self
                .engine
                .index_dir()
                .join(format!("{}-partition-{}", handoff.corpus_id, local_node_id));
            let original_path = self.engine.index_dir().join(&handoff.corpus_id);
            if partition_path.exists() {
                shard_dirs.push(partition_path);
            } else if Corpus::meta_in(&original_path).exists() {
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

            let dest_dir = self.engine.index_dir().join(format!(
                "{}-partition-{}",
                handoff.corpus_id, partition.node_id
            ));

            match self
                .fetch_remote_shard(&handoff.corpus_id, &base_url, &dest_dir, local_node_id)
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

                    // Wipe-after-pull: for an ephemeral grant, the peer must
                    // not retain the user's chunk text + embeddings. Fire-
                    // and-forget — the peer also self-evicts when its pull
                    // loop exits (auto_ingest), so a missed call here is
                    // covered by that belt-and-suspenders path.
                    if is_ephemeral {
                        let evict_url = format!("{base_url}/internal/corpus/partition_evict");
                        let corpus = handoff.corpus_id.clone();
                        let hid = handoff_id;
                        tokio::spawn(async move {
                            let _ = reqwest::Client::new()
                                .post(&evict_url)
                                .json(&serde_json::json!({
                                    "corpus_id": corpus,
                                    "handoff_id": hid,
                                }))
                                .send()
                                .await;
                        });
                    }
                }
                Err(e) => tracing::warn!(
                    node = %partition.node_id,
                    error = %e,
                    "coordinate_merge: failed to fetch remote shard"
                ),
            }
        }

        if shard_dirs.is_empty() {
            return Err(corpus_engine::Error::NoShardsFound(format!(
                "no shard dirs for handoff {handoff_id}"
            )));
        }

        let output_dir = self.engine.index_dir().join(&handoff.corpus_id);
        tracing::info!(
            handoff = %handoff_id,
            shards = shard_dirs.len(),
            output = %output_dir.display(),
            "coordinate_merge: merging partitions"
        );

        let info = self
            .engine
            .merge_partitions(&shard_dirs, &output_dir)
            .await?;

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
            anyhow::bail!("index/serve returned {} from {base_url}", resp.status());
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
        let tar_path = self
            .engine
            .index_dir()
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
            anyhow::bail!("index/transfer returned {status} from {target_base_url}: {body}");
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

/// Recover the set of peers that did work on `corpus_id` by scanning
/// the gossiped `processed_shards:<corpus>:<peer>` blobs. Each peer
/// publishes a non-empty list under its own slot only after doing real
/// ingest work, so the set of `entry.origin`s with non-empty payloads
/// is the participating-peers set.
///
/// Used by [`ShardManager::coordinate_merge`]'s queue-mode fallback
/// when the live `WorkQueueManager` snapshot is gone (typical case:
/// coordinator restart between "last unit completed" and "merge
/// finished"). Empty set means no peer has published — the corpus
/// hasn't actually been ingested anywhere visible to gossip and the
/// caller should bail rather than try to merge nothing.
fn participating_peers_from_gossip(
    mesh_store: &MeshStore,
    corpus_id: &str,
) -> std::collections::HashSet<NodeId> {
    let prefix = format!("processed_shards:{corpus_id}:");
    let entries = match mesh_store.scan(commonwealth_state::PROCESSED_SHARDS_APP_ID, &prefix) {
        Ok(e) => e,
        Err(_) => return std::collections::HashSet::new(),
    };
    let mut peers = std::collections::HashSet::new();
    for entry in entries {
        // Skip empty arrays — a peer that publishes `[]` hasn't
        // actually contributed work; counting it would synthesize a
        // bogus participant whose remote shard fetch will then fail.
        let nonempty = serde_json::from_slice::<Vec<usize>>(&entry.value)
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        if nonempty {
            peers.insert(entry.origin);
        }
    }
    peers
}

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

/// Delete a peer's working partition dir
/// (`<index_dir>/<corpus_id>-partition-<node>/`) and its `.tar` staging temp.
/// Returns `true` when the dir existed. Pure fs and idempotent — safe to call
/// redundantly (the coordinator's wipe-after-pull and the peer's own
/// pull-loop self-evict may both fire).
///
/// The dir name uses the node id's `Display` form, matching how partitions are
/// created (`corpus_queue::ingest_partition` and `coordinate_merge` above).
/// This is the mechanism behind the "no peer retention" guarantee for
/// ephemeral grant-scoped ingests.
pub fn evict_partition_dir(index_dir: &Path, corpus_id: &str, node_id: NodeId) -> bool {
    let dir = index_dir.join(format!("{corpus_id}-partition-{node_id}"));
    let tar = dir.with_extension("tar");
    let existed = dir.exists();
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_file(&tar).ok();
    existed
}

/// Result of the post-merge integrity spot-check.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VerifyReport {
    /// How many chunks were sampled and re-embedded.
    pub sampled: u32,
    /// How many matched within tolerance (cosine ≥ 1 − ε).
    pub passed: u32,
    /// Lowest cosine observed across the sample (1.0 for an empty sample).
    pub min_cosine: f32,
    /// `(sample_index, cosine)` for each chunk that missed the tolerance.
    pub failures: Vec<(u32, f32)>,
}

impl VerifyReport {
    /// True when every sampled chunk matched (or nothing was sampled).
    pub fn all_passed(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Re-embed a sample of the merged corpus's chunks LOCALLY and compare cosine
/// against the stored (peer-produced) vectors. Because both sides run the exact
/// same `EmbedModelInfo` (enforced at collaborate time), cosine ≈ 1 is expected
/// — a miss signals corruption, a wrong model, or tampering by a helper peer.
/// Non-fatal to the merge; the caller surfaces the report (glassbox
/// "re-checked N chunks — all matched"). Mirrors the prebuilt-restore re-embed
/// precedent (`corpus-engine` `try_restore_prebuilt`).
pub async fn verify_merge_sample(
    engine: &CorpusEngine,
    corpus_id: &str,
    sample_n: usize,
    epsilon: f32,
) -> corpus_engine::Result<VerifyReport> {
    let index = engine.open_index_for_corpus(corpus_id).await?;
    let samples = index.sample_chunks_with_embeddings(sample_n).await?;
    let embed = engine.embed_fn();

    let mut passed = 0u32;
    let mut min_cosine = 1.0f32;
    let mut failures = Vec::new();
    for (i, (text, stored)) in samples.iter().enumerate() {
        let local = match (embed)(text.as_str()).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    corpus = %corpus_id,
                    error = %e,
                    "verify_merge_sample: local re-embed failed for a sampled chunk"
                );
                failures.push((i as u32, 0.0));
                continue;
            }
        };
        let cos = cosine_similarity(&local, stored);
        if cos < min_cosine {
            min_cosine = cos;
        }
        if cos >= 1.0 - epsilon {
            passed += 1;
        } else {
            failures.push((i as u32, cos));
        }
    }

    Ok(VerifyReport {
        sampled: samples.len() as u32,
        passed,
        min_cosine: if samples.is_empty() { 1.0 } else { min_cosine },
        failures,
    })
}

/// Cosine similarity of two equal-length vectors. Returns 0.0 for a
/// length mismatch or a zero vector (both are "no match" for our purposes).
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identical_vectors_is_one() {
        let v = vec![0.2, -0.5, 0.9, 0.1];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_scaled_vector_is_still_one() {
        // Cosine is scale-invariant — a peer that returns the same direction
        // at a different magnitude must still verify as a match.
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![2.0, 4.0, 6.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn cosine_guards_mismatch_and_zero_vectors() {
        // Length mismatch, empty, and zero-norm all read as "no match" (0.0)
        // so a corrupt/absent embedding can never masquerade as verified.
        assert_eq!(cosine_similarity(&[1.0, 2.0], &[1.0]), 0.0);
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn verify_report_all_passed_tracks_failures() {
        let clean = VerifyReport {
            sampled: 24,
            passed: 24,
            min_cosine: 0.999,
            failures: vec![],
        };
        assert!(clean.all_passed());

        let dirty = VerifyReport {
            sampled: 24,
            passed: 23,
            min_cosine: 0.3,
            failures: vec![(7, 0.3)],
        };
        assert!(!dirty.all_passed());
    }

    #[test]
    fn evict_partition_dir_reports_absence_and_wipes_presence() {
        // No tempfile dependency: build a unique dir under the OS temp root
        // keyed by pid so parallel test runs don't collide.
        let base = std::env::temp_dir().join(format!("cw-evict-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let node = NodeId::from_u128(0xABCD);
        let corpus = "vault";
        // Nothing there yet → false, no panic.
        assert!(!evict_partition_dir(&base, corpus, node));

        // Create the partition dir the coordinator would wipe post-pull.
        let part = base.join(format!("{corpus}-partition-{node}"));
        std::fs::create_dir_all(&part).unwrap();
        std::fs::write(part.join("chunk.jsonl"), b"peer plaintext").unwrap();
        assert!(part.exists());

        // Evict reports it existed AND removes it — the no-retention guarantee.
        assert!(evict_partition_dir(&base, corpus, node));
        assert!(!part.exists());

        let _ = std::fs::remove_dir_all(&base);
    }
}
