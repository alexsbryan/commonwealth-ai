// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /v1/embeddings` as an [`EmbedFn`] — the baseline any OpenAI-compatible
//! inference frontend satisfies (`docs/CODE_TOOLING_BOUNDARY.md` §5.1's "no
//! required host" rule, applied to knowledge).
//!
//! Lived in `commonwealth-knowledge` (`embed_http.rs`) until 2026-09-03 with
//! the model id hardcoded; it builds a corpus-engine type from corpus-engine's
//! own `reqwest`, so it belongs here where a host that links nothing above the
//! knowledge layer can reach it. `commonwealth-knowledge` re-exports it.

use std::sync::Arc;

use crate::error::Error;
use crate::types::EmbedFn;

/// An `EmbedFn` that posts each text to `embeddings_url` with the given model
/// id and returns `data[0].embedding`.
///
/// A non-2xx status is an error naming the status and the body — until this
/// move a 4xx JSON body fell through to "bad embedding response format", which
/// hid the actual refusal (§18.3).
pub fn http_embed_fn(embeddings_url: String, model: String) -> EmbedFn {
    let client = reqwest::Client::new();
    Arc::new(move |text: &str| {
        let client = client.clone();
        let url = embeddings_url.clone();
        let model = model.clone();
        let text = text.to_string();
        Box::pin(async move {
            let resp = client
                .post(&url)
                .json(&serde_json::json!({ "input": text, "model": model }))
                .send()
                .await
                .map_err(|e| Error::Embed(format!("POST {url}: {e}")))?;
            let status = resp.status();
            let body = resp
                .json::<serde_json::Value>()
                .await
                .map_err(|e| Error::Embed(format!("POST {url}: {status}, unreadable body: {e}")))?;
            if !status.is_success() {
                return Err(Error::Embed(format!(
                    "POST {url} returned {status}: {}",
                    truncate(&body.to_string(), 300)
                )));
            }
            let embedding = body["data"][0]["embedding"]
                .as_array()
                .ok_or_else(|| {
                    Error::Embed(format!(
                        "POST {url}: no data[0].embedding in response: {}",
                        truncate(&body.to_string(), 300)
                    ))
                })?
                .iter()
                .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                .collect();
            Ok(embedding)
        })
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}
