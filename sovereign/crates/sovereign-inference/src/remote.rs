use std::pin::Pin;
use std::time::Instant;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use serde::Deserialize;

use sovereign_core::error::{Error, Result};
use sovereign_core::oicp::{OicpResponseMeta, ProviderManifest};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::*;

/// OpenAI-compatible API client.
///
/// Works with any endpoint implementing the OpenAI chat/completions API:
/// vLLM, Ollama, llama.cpp server, text-generation-inference, etc.
pub struct RemoteApiProvider {
    client: reqwest::Client,
    endpoint: String,
    api_key: Option<String>,
    model_id: String,
    context_size: u32,
}

impl RemoteApiProvider {
    pub fn new(
        endpoint: &str,
        api_key: Option<String>,
        model_id: &str,
        context_size: u32,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_default();

        Self {
            client,
            endpoint: endpoint.trim_end_matches('/').to_string(),
            api_key,
            model_id: model_id.to_string(),
            context_size,
        }
    }

    fn build_request(&self, request: &CompletionRequest) -> serde_json::Value {
        let mut messages = Vec::new();

        if let Some(ref system) = request.system_message {
            messages.push(serde_json::json!({
                "role": "system",
                "content": system,
            }));
        }

        messages.push(serde_json::json!({
            "role": "user",
            "content": &request.prompt,
        }));

        let mut body = serde_json::json!({
            "model": &self.model_id,
            "messages": messages,
        });

        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        if let Some(temperature) = request.temperature {
            body["temperature"] = serde_json::json!(temperature);
        }

        // Attach OICP requirements if present.
        if let Some(ref oicp) = request.oicp {
            if let Ok(oicp_val) = serde_json::to_value(oicp) {
                body["oicp"] = oicp_val;
            }
        }

        body
    }

    fn auth_header(&self) -> Option<String> {
        self.api_key.as_ref().map(|k| format!("Bearer {k}"))
    }

    /// Fetch the OICP capabilities manifest from a provider.
    /// Returns None if the provider doesn't support OICP (404 or parse failure).
    pub async fn fetch_oicp_manifest(&self) -> Option<ProviderManifest> {
        let url = format!("{}/oicp/v1/capabilities", self.endpoint);

        let mut req = self.client.get(&url);
        if let Some(ref auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }

        let response = req.send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }

        response.json::<ProviderManifest>().await.ok()
    }
}

// ─── OpenAI Response Types ───────────────────────────────────

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<UsageInfo>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    oicp: Option<OicpResponseMeta>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct UsageInfo {
    #[serde(default)]
    total_tokens: usize,
}

// ─── SSE Streaming Types ─────────────────────────────────────

#[derive(Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
}

#[derive(Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Deserialize)]
struct StreamDelta {
    content: Option<String>,
}

// ─── InferenceProvider Implementation ────────────────────────

#[async_trait]
impl InferenceProvider for RemoteApiProvider {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        let start = Instant::now();
        let url = format!("{}/chat/completions", self.endpoint);
        let body = self.build_request(request);

        let mut req = self.client.post(&url).json(&body);
        if let Some(ref auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }

        let response = req
            .send()
            .await
            .map_err(|e| Error::Inference(format!("Remote API request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Inference(format!(
                "Remote API returned {status}: {}",
                &body[..body.len().min(500)]
            )));
        }

        let chat_response: ChatCompletionResponse = response
            .json()
            .await
            .map_err(|e| Error::Inference(format!("Failed to parse API response: {e}")))?;

        let text = chat_response
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default();

        let tokens_used = chat_response
            .usage
            .map(|u| u.total_tokens)
            .unwrap_or(0);

        let model_id = chat_response
            .model
            .unwrap_or_else(|| self.model_id.clone());

        Ok(CompletionResponse {
            text,
            tokens_used,
            model_id,
            latency_ms: start.elapsed().as_millis() as u64,
            oicp_meta: chat_response.oicp,
        })
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        let url = format!("{}/chat/completions", self.endpoint);
        let mut body = self.build_request(request);
        body["stream"] = serde_json::json!(true);

        let mut req = self.client.post(&url).json(&body);
        if let Some(ref auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }

        let response = req
            .send()
            .await
            .map_err(|e| Error::Inference(format!("Remote stream request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            return Err(Error::Inference(format!(
                "Remote stream API returned {status}"
            )));
        }

        let byte_stream = response.bytes_stream();

        let token_stream = byte_stream
            .filter_map(|chunk| async move {
                let bytes = chunk.ok()?;
                let text = String::from_utf8_lossy(&bytes);

                let mut tokens = Vec::new();
                for line in text.lines() {
                    let line = line.trim();
                    if line == "data: [DONE]" {
                        break;
                    }
                    if let Some(data) = line.strip_prefix("data: ") {
                        if let Ok(chunk) = serde_json::from_str::<StreamChunk>(data) {
                            if let Some(content) = chunk
                                .choices
                                .first()
                                .and_then(|c| c.delta.content.clone())
                            {
                                tokens.push(content);
                            }
                        }
                    }
                }

                if tokens.is_empty() {
                    None
                } else {
                    Some(Ok(tokens.join("")))
                }
            });

        Ok(Box::pin(token_stream))
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let url = format!("{}/embeddings", self.endpoint);
        let body = serde_json::json!({
            "model": &self.model_id,
            "input": text,
        });

        let mut req = self.client.post(&url).json(&body);
        if let Some(ref auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }

        let response = req
            .send()
            .await
            .map_err(|e| Error::Inference(format!("Embedding request failed: {e}")))?;

        if !response.status().is_success() {
            return Err(Error::NotImplemented("Embedding not supported by this endpoint".to_string()));
        }

        #[derive(Deserialize)]
        struct EmbedResponse {
            data: Vec<EmbedData>,
        }
        #[derive(Deserialize)]
        struct EmbedData {
            embedding: Vec<f32>,
        }

        let embed_response: EmbedResponse = response
            .json()
            .await
            .map_err(|e| Error::Inference(format!("Failed to parse embedding response: {e}")))?;

        embed_response
            .data
            .into_iter()
            .next()
            .map(|d| d.embedding)
            .ok_or(Error::Inference("No embedding data in response".to_string()))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: self.context_size as usize,
            supports_structured_output: false,
            relative_speed: Speed::Medium,
            relative_reasoning: Depth::Deep,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_request_basic() {
        let provider = RemoteApiProvider::new(
            "http://localhost:8000/v1",
            None,
            "test-model",
            4096,
        );

        let request = CompletionRequest::new("Hello, world!");
        let body = provider.build_request(&request);

        assert_eq!(body["model"], "test-model");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Hello, world!");
    }

    #[test]
    fn build_request_with_system() {
        let provider = RemoteApiProvider::new(
            "http://localhost:8000/v1",
            None,
            "test-model",
            4096,
        );

        let request = CompletionRequest {
            prompt: "Hi".to_string(),
            system_message: Some("You are helpful.".to_string()),
            preferred_speed: Speed::Fast,
            max_tokens: Some(100),
            temperature: Some(0.5),
            structured_output: None,
            oicp: None,
        };

        let body = provider.build_request(&request);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(body["max_tokens"], 100);
        assert_eq!(body["temperature"], 0.5);
    }

    #[test]
    fn build_request_with_oicp() {
        use sovereign_core::oicp::{Capability, InferenceRequirements};

        let provider = RemoteApiProvider::new(
            "http://localhost:8000/v1",
            None,
            "test-model",
            4096,
        );

        let mut required = std::collections::HashMap::new();
        required.insert(Capability::Code, 3);

        let request = CompletionRequest {
            prompt: "Review this code".to_string(),
            system_message: None,
            preferred_speed: Speed::Slow,
            max_tokens: None,
            temperature: None,
            structured_output: None,
            oicp: Some(InferenceRequirements {
                required,
                preferred: Default::default(),
                min_context_tokens: Some(8192),
                latency: Default::default(),
                privacy: Default::default(),
                grounding: None,
            }),
        };

        let body = provider.build_request(&request);
        assert!(body.get("oicp").is_some());
        assert!(body["oicp"]["required"]["code"].as_u64().is_some());
    }

    #[test]
    fn auth_header_present() {
        let provider = RemoteApiProvider::new(
            "http://localhost:8000/v1",
            Some("sk-test-key".to_string()),
            "model",
            4096,
        );
        assert_eq!(provider.auth_header(), Some("Bearer sk-test-key".to_string()));
    }

    #[test]
    fn auth_header_absent() {
        let provider = RemoteApiProvider::new(
            "http://localhost:8000/v1",
            None,
            "model",
            4096,
        );
        assert_eq!(provider.auth_header(), None);
    }
}
