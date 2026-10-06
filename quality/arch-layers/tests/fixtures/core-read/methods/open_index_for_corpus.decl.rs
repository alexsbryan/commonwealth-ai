    /// Open the index for `corpus_id` under [`Self::index_dir`] (cached, as `open_index`).
    async fn open_index_for_corpus(&self, corpus_id: &str) -> Result<CorpusIndex>;
