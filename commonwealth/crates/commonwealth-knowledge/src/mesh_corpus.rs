use std::sync::Arc;

use corpus_engine::progress::ProgressCallback;
use corpus_engine::{CorpusEngine, CorpusSpec, IngestResult};

pub struct MeshCorpusManager {
    engine: Arc<CorpusEngine>,
}

impl MeshCorpusManager {
    pub fn new(engine: Arc<CorpusEngine>) -> Self {
        Self { engine }
    }

    /// Install a corpus, preferring existing local data.
    pub async fn install(
        &self,
        corpus_id: &str,
        progress: Option<ProgressCallback>,
    ) -> corpus_engine::Result<IngestResult> {
        // Already installed locally?
        if self
            .engine
            .installed_indexes()
            .await?
            .iter()
            .any(|i| i.corpus_id == corpus_id && !i.is_shard)
        {
            return Err(corpus_engine::Error::AlreadyInstalled(corpus_id.into()));
        }

        // Ingest from source.
        let spec = CorpusSpec::Builtin(corpus_id.to_string());
        self.engine.ingest(&spec, progress).await
    }

    /// List installed indexes.
    pub async fn installed(&self) -> corpus_engine::Result<Vec<corpus_engine::IndexInfo>> {
        self.engine.installed_indexes().await
    }

    /// Remove an installed corpus.
    pub fn remove(&self, corpus_id: &str) -> corpus_engine::Result<()> {
        self.engine.remove_index(corpus_id)
    }
}
