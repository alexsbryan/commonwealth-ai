// SPDX-License-Identifier: AGPL-3.0-or-later
use commonwealth_core::oicp::{EmbedModelInfo, NormalizationStrategy, PoolingStrategy};

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
        // v0.3 reconstruction from /v1/models cannot discover the query
        // prefix; the v0.4 daemon path (A:P2) threads the real value in.
        query_instruction_prefix: String::new(),
    })
}

/// Create an EmbedFn that calls an OpenAI-compatible embeddings API.
/// `POST /v1/embeddings` as an `EmbedFn`. Moved DOWN to
/// `corpus_engine::embed_http` on 2026-09-03 (it builds a corpus-engine type
/// from corpus-engine's own `reqwest`) and gained a `model` parameter — the
/// id was hardcoded to `qwen3-embedding-0.6b` here, which no other frontend
/// serves. Re-exported so this path keeps resolving.
pub use corpus_engine::embed_http::http_embed_fn;
