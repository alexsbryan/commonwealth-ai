use commonwealth_core::oicp::{EmbedModelInfo, NormalizationStrategy, PoolingStrategy};
use corpus_engine::EmbedFn;
use std::sync::Arc;

/// Query a running llama-server (or compatible) to discover its active
/// embedding model's identity and output shape.
///
/// Calls `GET {base_url}/v1/models` — the standard OpenAI models endpoint.
/// Returns `None` on any failure (network error, unexpected shape, missing
/// metadata) so callers can gracefully degrade rather than fail.
///
/// The `embeddings_url` is the full endpoint path, e.g.
/// `http://localhost:8080/v1/embeddings`.  The function strips the path
/// component to arrive at the base URL.
pub async fn embed_model_info(embeddings_url: &str) -> Option<EmbedModelInfo> {
    // Derive base URL by stripping the path component.
    // e.g. "http://localhost:8080/v1/embeddings" → "http://localhost:8080"
    let base = {
        // Find the authority section: everything up to the third '/'.
        let after_scheme = embeddings_url.find("://").map(|i| i + 3)?;
        let path_start = embeddings_url[after_scheme..]
            .find('/')
            .map(|i| after_scheme + i)
            .unwrap_or(embeddings_url.len());
        embeddings_url[..path_start].to_string()
    };

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/v1/models"))
        .send()
        .await
        .ok()?
        .json::<serde_json::Value>()
        .await
        .ok()?;

    // OpenAI /v1/models response: { "data": [ { "id": "<model_id>", ... } ] }
    let model_id = resp["data"]
        .as_array()?
        .first()?
        .get("id")?
        .as_str()?
        .to_string();

    // Query /v1/models/{model_id} for extended metadata if available.
    // llama-server exposes embedding dimensions in the model info object.
    let model_detail = client
        .get(format!("{base}/v1/models/{model_id}"))
        .send()
        .await
        .ok()?
        .json::<serde_json::Value>()
        .await
        .ok()?;

    let dimensions = model_detail
        .get("embedding_length")
        .or_else(|| model_detail.get("dimensions"))
        .and_then(|v| v.as_u64())
        .map(|d| d as usize)?;

    // llama-server exposes pooling type as a string in extended model info.
    let pooling = match model_detail
        .get("pooling_type")
        .and_then(|v| v.as_str())
        .unwrap_or("mean")
    {
        "last" => PoolingStrategy::Last,
        "cls" => PoolingStrategy::Cls,
        _ => PoolingStrategy::Mean,
    };

    Some(EmbedModelInfo {
        model_id,
        dimensions,
        pooling,
        // llama-server normalises server-side by default via --embd-normalize 2.
        normalization: NormalizationStrategy::Server,
    })
}

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
                    "model": "qwen3-embedding-0.6b"
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
