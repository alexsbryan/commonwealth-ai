use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_engine::{ChunkRange, CorpusEngine, IndexInfo, ShardInfo};
use commonwealth_core::ids::NodeId;
use commonwealth_core::knowledge::KnowledgeShardAssignment;

pub struct ShardManager {
    engine: Arc<CorpusEngine>,
    shard_dir: PathBuf,
}

impl ShardManager {
    pub fn new(engine: Arc<CorpusEngine>, shard_dir: PathBuf) -> Self {
        Self { engine, shard_dir }
    }

    /// Prepare shard files for distribution to assigned nodes.
    pub fn prepare_shards(
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
                    "{}-shard-{}-{}.db",
                    corpus_id, range.start_id, range.end_id
                ));
                let chunk_range = ChunkRange::new(range.start_id, range.end_id);
                let info = self.engine.extract_shard(corpus_id, chunk_range, &output)?;
                shards.push(PreparedShard {
                    target_node: assignment.node_id,
                    info,
                });
            }
        }
        Ok(shards)
    }

    /// Install a received shard file into the shared index directory.
    pub fn install_received_shard(
        &self,
        corpus_id: &str,
        chunk_range: &ChunkRange,
        received_file: &Path,
    ) -> corpus_engine::Result<PathBuf> {
        let dest = self.engine.index_dir().join(format!(
            "{}-shard-{}-{}.db",
            corpus_id, chunk_range.start_id, chunk_range.end_id
        ));
        std::fs::rename(received_file, &dest)
            .map_err(corpus_engine::Error::Io)?;
        Ok(dest)
    }

    /// Merge all local shard files for a corpus into a complete index.
    pub fn consolidate_shards(
        &self,
        corpus_id: &str,
    ) -> corpus_engine::Result<IndexInfo> {
        let shard_files: Vec<PathBuf> = self
            .engine
            .installed_indexes()?
            .iter()
            .filter(|i| i.corpus_id == corpus_id && i.is_shard)
            .map(|i| i.path.clone())
            .collect();

        if shard_files.is_empty() {
            return Err(corpus_engine::Error::NoShardsFound(corpus_id.into()));
        }

        let output = self.engine.index_dir().join(format!("{corpus_id}.db"));
        let info = self.engine.merge_shards(&shard_files, &output)?;

        // Clean up shard files after successful merge.
        for path in &shard_files {
            std::fs::remove_file(path).ok();
        }

        Ok(info)
    }
}

pub struct PreparedShard {
    pub target_node: NodeId,
    pub info: ShardInfo,
}
