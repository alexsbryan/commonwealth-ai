// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-cluster RAPTOR checkpoint — durable resume across daemon
//! restarts.
//!
//! ## Why
//!
//! `build_raptor_atlas` makes ~N/20 + N/100 + N/400 LLM calls (leaves
//! + mid + root). At 1 req/s and 5-15s per Slow-slot call, a 700-chunk
//! corpus is 15-30 minutes. Before this module, a daemon restart
//! mid-build threw away every LLM result accumulated so far; the
//! desktop chip stayed on `RaptorLeaves` forever and the user had no
//! way to know whether retrying would replay 0% or 90% of the work.
//!
//! ## Shape
//!
//! ```text
//! <index_dir>/_raptor_checkpoint/
//!   manifest.json                  — schema_version, input_hash,
//!                                    started_at, last_progress_at,
//!                                    completed_at (when done)
//!   level-0/
//!     clustering.json              — { k, assignments } persisted
//!                                    BEFORE the LLM fan-out so retry
//!                                    sees identical cluster identity
//!     cluster-000.json             — serialized RaptorNode
//!     cluster-001.json
//!     ...
//!   level-1/
//!     ...
//! ```
//!
//! ## Invariants
//!
//! - `input_hash` is over the sorted `(chunk_id, embedding-byte-count)`
//!   pairs handed to `build_raptor_atlas`. Mismatch on resume →
//!   checkpoint discarded + fresh build (chunks changed under us).
//! - `clustering.json` is written atomically before any LLM call at
//!   that level. Once on disk, the cluster→member mapping is frozen
//!   for the lifetime of the build.
//! - `cluster-NNN.json` is written immediately after a successful
//!   summarization. Failures don't write — retry re-summarizes.
//! - The completed manifest carries `completed_at: Some(ts)`. Callers
//!   load it directly and skip the entire LLM fan-out.

use std::path::{Path, PathBuf};

use blake3;
use serde::{Deserialize, Serialize};
use sovereign_core::error::{Error, Result};
use sovereign_core::types::RaptorNode;

const CHECKPOINT_SUBDIR: &str = "_raptor_checkpoint";
const MANIFEST_NAME: &str = "manifest.json";
const CLUSTERING_NAME: &str = "clustering.json";
const SCHEMA_VERSION: u32 = 1;

/// On-disk manifest. Lives at `<index>/_raptor_checkpoint/manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointManifest {
    pub schema_version: u32,
    /// Hash of the input set the checkpoint corresponds to. Used to
    /// detect "user changed chunks under us" between attempts — if
    /// the live input doesn't match, we abandon the checkpoint and
    /// start fresh. Without this guard a stale checkpoint would
    /// silently pollute the new build with summaries of vanished
    /// chunks.
    pub input_hash: String,
    pub started_at: i64,
    pub last_progress_at: i64,
    /// `Some(ts)` exactly when the build finished and `all_nodes` is
    /// fully serialized to disk. `None` while in-progress.
    pub completed_at: Option<i64>,
}

/// On-disk clustering record for a single level. The level dir holds
/// one clustering.json and N cluster-NNN.json files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LevelClustering {
    pub k: u32,
    pub assignments: Vec<u32>,
}

/// Handle the LLM-fan-out path threads through to short-circuit on
/// cached results. Construct once per build; pass by reference to
/// `build_raptor_atlas`.
///
/// `progress` is optional — when present, per-cluster completion
/// reports through the generic enrichment progress sink so the
/// desktop chip shows `Summarising leaves (17 / 45)` instead of the
/// indistinguishable `RaptorLeaves`. Sink calls are async-fire-and-
/// forget so a slow consumer never throttles the LLM fan-out.
pub struct RaptorCheckpointHandle {
    pub dir: PathBuf,
    pub input_hash: String,
}

impl RaptorCheckpointHandle {
    /// Resolve the checkpoint directory for `index_dir`. Does NOT
    /// create directories on disk — write paths are created lazily by
    /// `record_*`. The handle is cheap to construct so callers may
    /// build one before deciding whether they need it.
    pub fn at(index_dir: &Path, input_hash: impl Into<String>) -> Self {
        Self {
            dir: index_dir.join(CHECKPOINT_SUBDIR),
            input_hash: input_hash.into(),
        }
    }

    /// Compute the deterministic input hash for a build. Sorting by
    /// `chunk_id` makes the order-independent — kmeans is order-
    /// sensitive but we persist its output, so the input set's
    /// identity (not order) is what gates checkpoint validity.
    pub fn compute_input_hash(chunks: &[u32], embedding_dim: usize) -> String {
        let mut sorted = chunks.to_vec();
        sorted.sort_unstable();
        let mut hasher = blake3::Hasher::new();
        for id in &sorted {
            hasher.update(&id.to_le_bytes());
        }
        hasher.update(&(embedding_dim as u32).to_le_bytes());
        hasher.finalize().to_hex().to_string()
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.dir.join(MANIFEST_NAME)
    }
    pub fn level_dir(&self, level: u8) -> PathBuf {
        self.dir.join(format!("level-{level}"))
    }
    pub fn clustering_path(&self, level: u8) -> PathBuf {
        self.level_dir(level).join(CLUSTERING_NAME)
    }
    pub fn cluster_node_path(&self, level: u8, cluster_idx: usize) -> PathBuf {
        self.level_dir(level)
            .join(format!("cluster-{cluster_idx:03}.json"))
    }

    /// Read the manifest if present. Returns `None` if the checkpoint
    /// dir doesn't exist or the manifest is missing/malformed.
    /// Manifest deserialization failures are treated as "no usable
    /// checkpoint" rather than propagated — a corrupted state file
    /// from an older schema shouldn't block a fresh build.
    pub fn read_manifest(&self) -> Option<CheckpointManifest> {
        let path = self.manifest_path();
        if !path.exists() {
            return None;
        }
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).ok(),
            Err(_) => None,
        }
    }

    /// Decision point for `build_raptor_atlas` on entry:
    ///   - `Resume(manifest)` — input_hash matches; load whatever's
    ///     cached and continue from there.
    ///   - `StaleAndReset` — manifest exists but input_hash differs;
    ///     the caller should wipe the checkpoint dir before starting.
    ///   - `Fresh` — no manifest. Start from scratch; write the
    ///     manifest after clustering completes.
    pub fn decide(&self) -> CheckpointDecision {
        let Some(manifest) = self.read_manifest() else {
            return CheckpointDecision::Fresh;
        };
        if manifest.input_hash != self.input_hash {
            return CheckpointDecision::StaleAndReset;
        }
        CheckpointDecision::Resume(manifest)
    }

    /// Wipe the checkpoint dir. Called when `decide() == StaleAndReset`
    /// — the inputs changed under us, so the old per-cluster results
    /// are no longer aligned with the live chunk set. Cheap I/O;
    /// errors are logged but not propagated.
    pub fn reset(&self) {
        if self.dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&self.dir) {
                tracing::warn!(
                    path = %self.dir.display(),
                    error = %e,
                    "raptor_checkpoint: reset failed; next attempt may pollute build"
                );
            }
        }
    }

    /// Idempotent: create the manifest if absent.
    pub fn ensure_manifest(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| Error::Storage(format!("raptor_checkpoint io: {e}")))?;
        if self.manifest_path().exists() {
            return Ok(());
        }
        let now = now_secs();
        let manifest = CheckpointManifest {
            schema_version: SCHEMA_VERSION,
            input_hash: self.input_hash.clone(),
            started_at: now,
            last_progress_at: now,
            completed_at: None,
        };
        write_json_atomic(&self.manifest_path(), &manifest)
    }

    /// Bump `last_progress_at`. Called after every successful cluster
    /// summarization so the stall-sweeper sees fresh activity.
    pub fn touch(&self) -> Result<()> {
        let mut manifest = match self.read_manifest() {
            Some(m) => m,
            None => return Ok(()),
        };
        manifest.last_progress_at = now_secs();
        write_json_atomic(&self.manifest_path(), &manifest)
    }

    pub fn mark_complete(&self) -> Result<()> {
        let mut manifest = match self.read_manifest() {
            Some(m) => m,
            None => return Ok(()),
        };
        let now = now_secs();
        manifest.last_progress_at = now;
        manifest.completed_at = Some(now);
        write_json_atomic(&self.manifest_path(), &manifest)
    }

    pub fn write_clustering(&self, level: u8, clustering: &LevelClustering) -> Result<()> {
        std::fs::create_dir_all(self.level_dir(level))
            .map_err(|e| Error::Storage(format!("raptor_checkpoint io: {e}")))?;
        write_json_atomic(&self.clustering_path(level), clustering)
    }
    pub fn read_clustering(&self, level: u8) -> Result<Option<LevelClustering>> {
        let path = self.clustering_path(level);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path)
            .map_err(|e| Error::Storage(format!("raptor_checkpoint io: {e}")))?;
        let parsed = serde_json::from_slice(&bytes).map_err(|e| {
            Error::Storage(format!("raptor_checkpoint: parse {}: {e}", path.display()))
        })?;
        Ok(Some(parsed))
    }

    pub fn write_cluster_node(
        &self,
        level: u8,
        cluster_idx: usize,
        node: &RaptorNode,
    ) -> Result<()> {
        std::fs::create_dir_all(self.level_dir(level))
            .map_err(|e| Error::Storage(format!("raptor_checkpoint io: {e}")))?;
        write_json_atomic(&self.cluster_node_path(level, cluster_idx), node)
    }
    pub fn read_cluster_node(&self, level: u8, cluster_idx: usize) -> Result<Option<RaptorNode>> {
        let path = self.cluster_node_path(level, cluster_idx);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path)
            .map_err(|e| Error::Storage(format!("raptor_checkpoint io: {e}")))?;
        let parsed: RaptorNode = serde_json::from_slice(&bytes).map_err(|e| {
            Error::Storage(format!("raptor_checkpoint: parse {}: {e}", path.display()))
        })?;
        Ok(Some(parsed))
    }

    /// Walk the checkpoint dir and return every persisted `RaptorNode`
    /// across all levels, in `(level asc, cluster_idx asc)` order.
    /// Used when the manifest is `completed_at: Some(_)` so the
    /// caller can return without re-running the LLM.
    pub fn load_all_nodes(&self) -> Result<Vec<RaptorNode>> {
        if !self.dir.exists() {
            return Ok(Vec::new());
        }
        let mut by_level: std::collections::BTreeMap<u8, Vec<(usize, RaptorNode)>> =
            std::collections::BTreeMap::new();
        for entry in std::fs::read_dir(&self.dir)
            .map_err(|e| Error::Storage(format!("raptor_checkpoint io: {e}")))?
        {
            let entry = entry.map_err(|e| Error::Storage(format!("raptor_checkpoint io: {e}")))?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            let Some(level_suffix) = name.strip_prefix("level-") else {
                continue;
            };
            let level: u8 = match level_suffix.parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            for file in std::fs::read_dir(&path)
                .map_err(|e| Error::Storage(format!("raptor_checkpoint io: {e}")))?
            {
                let file =
                    file.map_err(|e| Error::Storage(format!("raptor_checkpoint io: {e}")))?;
                let fpath = file.path();
                let Some(fname) = fpath.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let Some(idx_str) = fname
                    .strip_prefix("cluster-")
                    .and_then(|s| s.strip_suffix(".json"))
                else {
                    continue;
                };
                let idx: usize = match idx_str.parse() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let bytes = std::fs::read(&fpath)
                    .map_err(|e| Error::Storage(format!("raptor_checkpoint io: {e}")))?;
                let node: RaptorNode = serde_json::from_slice(&bytes).map_err(|e| {
                    Error::Storage(format!("raptor_checkpoint: parse {}: {e}", fpath.display()))
                })?;
                by_level.entry(level).or_default().push((idx, node));
            }
        }
        let mut out = Vec::new();
        for (_level, mut rows) in by_level {
            rows.sort_by_key(|(idx, _)| *idx);
            for (_idx, node) in rows {
                out.push(node);
            }
        }
        Ok(out)
    }
}

#[derive(Debug)]
pub enum CheckpointDecision {
    Fresh,
    Resume(CheckpointManifest),
    StaleAndReset,
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Storage(format!("raptor_checkpoint io: {e}")))?;
    }
    let json = serde_json::to_vec_pretty(value).map_err(|e| {
        Error::Storage(format!(
            "raptor_checkpoint: serialize {}: {e}",
            path.display()
        ))
    })?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json)
        .map_err(|e| Error::Storage(format!("raptor_checkpoint io: {e}")))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| Error::Storage(format!("raptor_checkpoint io: {e}")))?;
    Ok(())
}

use sovereign_core::time::unix_now as now_secs;

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_node(level: u8, idx: usize) -> RaptorNode {
        RaptorNode {
            node_id: format!("L{level}-N{idx}"),
            level,
            summary: format!("summary for cluster {idx} at level {level}"),
            summary_embedding: vec![0.1, 0.2, 0.3],
            centroid_embedding: vec![0.4, 0.5, 0.6],
            children_node_ids: Vec::new(),
            direct_member_chunk_ids: vec![idx as u32 * 10, idx as u32 * 10 + 1],
            evidence_chunk_ids: vec![idx as u32 * 10, idx as u32 * 10 + 1],
            quote_spans: Vec::new(),
            primary_entities: Vec::new(),
            cluster_coherence: 0.8,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn input_hash_is_order_independent() {
        let h1 = RaptorCheckpointHandle::compute_input_hash(&[3, 1, 2], 1024);
        let h2 = RaptorCheckpointHandle::compute_input_hash(&[1, 2, 3], 1024);
        assert_eq!(h1, h2);
    }

    #[test]
    fn input_hash_changes_on_member_change() {
        let h1 = RaptorCheckpointHandle::compute_input_hash(&[1, 2, 3], 1024);
        let h2 = RaptorCheckpointHandle::compute_input_hash(&[1, 2, 4], 1024);
        assert_ne!(h1, h2);
    }

    #[test]
    fn decide_returns_fresh_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let h = RaptorCheckpointHandle::at(tmp.path(), "hash-a");
        assert!(matches!(h.decide(), CheckpointDecision::Fresh));
    }

    #[test]
    fn decide_returns_resume_when_hash_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let h = RaptorCheckpointHandle::at(tmp.path(), "hash-a");
        h.ensure_manifest().unwrap();
        assert!(matches!(h.decide(), CheckpointDecision::Resume(_)));
    }

    #[test]
    fn decide_returns_stale_when_hash_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let h_old = RaptorCheckpointHandle::at(tmp.path(), "hash-a");
        h_old.ensure_manifest().unwrap();
        let h_new = RaptorCheckpointHandle::at(tmp.path(), "hash-b");
        assert!(matches!(h_new.decide(), CheckpointDecision::StaleAndReset));
    }

    #[test]
    fn round_trips_cluster_node() {
        let tmp = tempfile::tempdir().unwrap();
        let h = RaptorCheckpointHandle::at(tmp.path(), "hash-a");
        h.ensure_manifest().unwrap();
        let node = dummy_node(0, 7);
        h.write_cluster_node(0, 7, &node).unwrap();
        let read = h.read_cluster_node(0, 7).unwrap().unwrap();
        assert_eq!(read.node_id, node.node_id);
        assert_eq!(read.summary, node.summary);
    }

    #[test]
    fn load_all_nodes_sorts_by_level_then_idx() {
        let tmp = tempfile::tempdir().unwrap();
        let h = RaptorCheckpointHandle::at(tmp.path(), "hash-a");
        h.ensure_manifest().unwrap();
        h.write_cluster_node(0, 2, &dummy_node(0, 2)).unwrap();
        h.write_cluster_node(0, 0, &dummy_node(0, 0)).unwrap();
        h.write_cluster_node(1, 0, &dummy_node(1, 0)).unwrap();
        let all = h.load_all_nodes().unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].node_id, "L0-N0");
        assert_eq!(all[1].node_id, "L0-N2");
        assert_eq!(all[2].node_id, "L1-N0");
    }

    #[test]
    fn reset_wipes_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let h = RaptorCheckpointHandle::at(tmp.path(), "hash-a");
        h.ensure_manifest().unwrap();
        h.write_cluster_node(0, 0, &dummy_node(0, 0)).unwrap();
        assert!(h.dir.exists());
        h.reset();
        assert!(!h.dir.exists());
    }
}
