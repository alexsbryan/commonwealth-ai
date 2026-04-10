use std::sync::Arc;
use corpus_engine::EmbedFn;

/// Create an EmbedFn that calls an OpenAI-compatible embeddings API.
pub fn http_embed_fn(embeddings_url: String) -> EmbedFn {
    Arc::new(move |text: &str| {
        let url = embeddings_url.clone();
        let text = text.to_string();
        Box::pin(async move {
            let client = reqwest::Client::new();
            let resp = client
                .post(&url)
                .json(&serde_json::json!({
                    "input": text,
                    "model": "nomic-embed-text-v2"
                }))
                .send()
                .await
                .map_err(|e| corpus_engine::Error::Embed(e.to_string()))?
                .json::<serde_json::Value>()
                .await
                .map_err(|e| corpus_engine::Error::Embed(e.to_string()))?;

            let embedding = resp["data"][0]["embedding"]
                .as_array()
                .ok_or_else(|| corpus_engine::Error::Embed("bad embedding response format".into()))?
                .iter()
                .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                .collect();

            Ok(embedding)
        })
    })
}
