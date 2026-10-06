    /// Embed `text` with the model the indexes were built with.
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;
