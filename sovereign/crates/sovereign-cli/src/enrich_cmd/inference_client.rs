//! HTTP client glue for talking to the Commonwealth daemon's
//! OpenAI-compatible chat + embeddings endpoints.
//!
//! This is the only place in `enrich_cmd/` that knows about
//! reqwest and the wire shape — every other subcommand just
//! takes the pair of closures (`EmbedFn` + `ChatCompletionFn`)
//! produced by `build_client_pair`.

use std::sync::Arc;
use std::time::Duration;

use corpus_engine::enrichment::pipeline::{ChatCompletionFn, ChatPrompt};
use corpus_engine::error::{Error, Result};
use corpus_engine::types::EmbedFn;

use crate::util::urls::{v1_models_url, v1_url, DEFAULT_CLIENT_PORT};

/// Default chat request timeout. Long because a primary-slot LLM on
/// an M2 can take 20-40s on a single chapter.
const CHAT_TIMEOUT: Duration = Duration::from_secs(180);

/// Default embed request timeout. Embeddings are fast; we keep this
/// tight so a hung embed surface doesn't freeze a whole run.
const EMBED_TIMEOUT: Duration = Duration::from_secs(15);

/// Reusable OpenAI-compatible chat client pointed at the local daemon.
#[derive(Debug, Clone)]
pub struct DaemonInferenceClient {
    client: reqwest::Client,
    base_url: String,
    chat_model: String,
    embed_model: String,
    /// Per-request output token cap. `None` means "let the daemon
    /// decide" — which on some llama.cpp builds means 256, too small
    /// for thinking models. Callers that load `EnrichConfig` should
    /// thread its `max_output_tokens` through via
    /// `with_max_output_tokens`.
    max_output_tokens: Option<u32>,
}

impl DaemonInferenceClient {
    pub fn new(
        base_url: impl Into<String>,
        chat_model: impl Into<String>,
        embed_model: impl Into<String>,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(CHAT_TIMEOUT)
            .build()?;
        Ok(Self {
            client,
            base_url: base_url.into(),
            chat_model: chat_model.into(),
            embed_model: embed_model.into(),
            max_output_tokens: None,
        })
    }

    /// Set the per-request output cap. Applies to future `complete`
    /// calls; embed calls are unaffected.
    pub fn with_max_output_tokens(mut self, tokens: u32) -> Self {
        self.max_output_tokens = Some(tokens);
        self
    }

    pub fn with_localhost(chat_model: impl Into<String>, embed_model: impl Into<String>) -> Result<Self> {
        Self::new(
            v1_url(DEFAULT_CLIENT_PORT).trim_end_matches("/v1").to_string(),
            chat_model,
            embed_model,
        )
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn chat_model(&self) -> &str {
        &self.chat_model
    }

    pub fn embed_model(&self) -> &str {
        &self.embed_model
    }

    /// Call `/v1/chat/completions` with a single system + user message.
    pub async fn complete(&self, prompt: &ChatPrompt) -> Result<String> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let mut body = serde_json::json!({
            "model": self.chat_model,
            "messages": [
                {"role": "system", "content": prompt.system},
                {"role": "user", "content": prompt.user},
            ],
            "temperature": 0.2,
            "stream": false,
        });
        if let Some(n) = self.max_output_tokens {
            if let Some(obj) = body.as_object_mut() {
                obj.insert("max_tokens".into(), serde_json::json!(n));
            }
        }
        let resp = self.client.post(&url).json(&body).send().await?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| Error::Serialization(format!("chat response read error: {e}")))?;
        if !status.is_success() {
            return Err(Error::Serialization(format!(
                "daemon chat error {status}: {text}"
            )));
        }
        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| Error::Serialization(format!("non-JSON chat response: {e} — body: {text}")))?;
        let content = v
            .pointer("/choices/0/message/content")
            .and_then(|s| s.as_str())
            .ok_or_else(|| {
                Error::Serialization(format!(
                    "chat response missing choices[0].message.content: {text}"
                ))
            })?;
        Ok(content.to_string())
    }

    /// Call `/v1/embeddings` for a single text. Uses a shorter timeout
    /// than chat since embeds are fast.
    pub async fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let url = format!("{}/v1/embeddings", self.base_url);
        let body = serde_json::json!({
            "model": self.embed_model,
            "input": text,
        });
        // Build a one-shot client with the embed timeout so callers
        // don't share the long chat timeout on what should be <1s.
        let short_client = reqwest::Client::builder()
            .timeout(EMBED_TIMEOUT)
            .build()?;
        let resp = short_client.post(&url).json(&body).send().await?;
        let status = resp.status();
        let payload = resp
            .text()
            .await
            .map_err(|e| Error::Embed(format!("embed read: {e}")))?;
        if !status.is_success() {
            let hint = if status.as_u16() == 404 {
                " (the daemon does not expose an embeddings route — upgrade the daemon \
                 binary or verify it was built with sovereign-mesh's HTTP surface)"
            } else {
                ""
            };
            return Err(Error::Embed(format!(
                "daemon embed error {status} at {url}: {}{}",
                if payload.is_empty() { "<empty body>" } else { payload.as_str() },
                hint
            )));
        }
        let v: serde_json::Value = serde_json::from_str(&payload)
            .map_err(|e| Error::Embed(format!("non-JSON embed response: {e}")))?;
        let arr = v
            .pointer("/data/0/embedding")
            .and_then(|x| x.as_array())
            .ok_or_else(|| {
                Error::Embed(format!(
                    "embed response missing data[0].embedding: {payload}"
                ))
            })?;
        Ok(arr
            .iter()
            .map(|x| x.as_f64().unwrap_or(0.0) as f32)
            .collect())
    }

    /// Wrap this client as the `(EmbedFn, ChatCompletionFn)` pair that
    /// `PhaseRunner::new` expects.
    pub fn into_closures(self) -> (EmbedFn, ChatCompletionFn) {
        let arc = Arc::new(self);
        let embed_arc = arc.clone();
        let embed: EmbedFn = Arc::new(move |text: &str| {
            let this = embed_arc.clone();
            let text = text.to_string();
            Box::pin(async move { this.embed_one(&text).await })
        });
        let chat_arc = arc;
        let chat: ChatCompletionFn = Arc::new(move |prompt: &ChatPrompt| {
            let this = chat_arc.clone();
            let prompt = prompt.clone();
            Box::pin(async move { this.complete(&prompt).await })
        });
        (embed, chat)
    }
}

/// Readiness probe — returns `true` iff `GET /v1/models` responds
/// 200 within 500ms. Used by `enrich init` / `extract` to fail early
/// if the daemon isn't running.
pub async fn probe_daemon(base_url: &str) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
    else {
        return false;
    };
    let url = if base_url.ends_with("/v1/models") {
        base_url.to_string()
    } else {
        format!("{base_url}/v1/models")
    };
    client
        .get(&url)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Enumerate the daemon's registered models. Returns `(chat_model, embed_model)`
/// heuristically — the first chat-capable ID and the first embedding ID — or
/// `(None, None)` on any failure.
///
/// The `/v1/models` endpoint doesn't carry capability tags consistently across
/// backends, so we fall back to name-pattern matching: anything containing
/// `"embedding"` or `"-embed"` is classed as embed; everything else is chat.
pub async fn resolve_default_models(base_url: &str) -> (Option<String>, Option<String>) {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return (None, None),
    };
    let url = v1_models_url(DEFAULT_CLIENT_PORT);
    // If caller gave us a non-default base, use their URL.
    let url = if base_url.contains("://") && !base_url.ends_with("/v1/models") {
        format!("{}/v1/models", base_url.trim_end_matches('/'))
    } else {
        url
    };
    let Ok(resp) = client.get(&url).send().await else {
        return (None, None);
    };
    let Ok(v) = resp.json::<serde_json::Value>().await else {
        return (None, None);
    };
    let Some(arr) = v.get("data").and_then(|d| d.as_array()) else {
        return (None, None);
    };

    let mut chat = None;
    let mut embed = None;
    for m in arr {
        let Some(id) = m.get("id").and_then(|s| s.as_str()) else {
            continue;
        };
        let lower = id.to_lowercase();
        let is_embed = lower.contains("embedding") || lower.contains("-embed");
        if is_embed {
            if embed.is_none() {
                embed = Some(id.to_string());
            }
        } else if chat.is_none() {
            chat = Some(id.to_string());
        }
    }
    (chat, embed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_builder_stores_fields() {
        let c = DaemonInferenceClient::new(
            "http://localhost:9741",
            "qwen3-8b",
            "qwen3-embedding-0.6b",
        )
        .unwrap();
        assert_eq!(c.base_url(), "http://localhost:9741");
        assert_eq!(c.chat_model(), "qwen3-8b");
        assert_eq!(c.embed_model(), "qwen3-embedding-0.6b");
    }

    #[test]
    fn with_localhost_strips_v1_suffix_from_base() {
        let c = DaemonInferenceClient::with_localhost("x", "y").unwrap();
        assert!(c.base_url().ends_with(":9741"));
        assert!(!c.base_url().ends_with("/v1"));
    }

    #[tokio::test]
    async fn probe_daemon_returns_false_for_unreachable_host() {
        // Port 1 is reserved and never listening.
        assert!(!probe_daemon("http://127.0.0.1:1").await);
    }

    #[tokio::test]
    async fn resolve_default_models_returns_none_on_unreachable() {
        let (chat, embed) = resolve_default_models("http://127.0.0.1:1").await;
        assert!(chat.is_none());
        assert!(embed.is_none());
    }
}
