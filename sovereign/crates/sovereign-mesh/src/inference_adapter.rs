//! Adapter: `sovereign_core::traits::InferenceProvider` →
//! `commonwealth_api::state::LocalInferenceService`.
//!
//! Why this exists: Commonwealth's HTTP handlers speak OpenAI-style
//! `ChatCompletionRequest`/`Response`; Sovereign's runtime speaks
//! its own `CompletionRequest`/`Response`. The two are similar but
//! not identical — messages-vs-prompt, sampling knob names, OICP
//! shape. This adapter owns the translation in one place so neither
//! side has to know about the other.
//!
//! Scope for v1: non-streaming + streaming chat completions, no
//! tool calls, no function calling. The Sovereign runtime doesn't
//! use those on its request path today; when it does, extend here.
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use commonwealth_api::openai_types::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, Usage,
};
use commonwealth_api::state::LocalInferenceService;
use commonwealth_inference::oicp::{
    Capability, ModelStatus, ProviderInfo, ProviderManifest, ProviderModel, ProviderType,
    OICP_VERSION,
};
use futures::{Stream, StreamExt};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, Speed};

/// Wraps a sovereign-core `InferenceProvider` so it can answer
/// Commonwealth-flavoured `/v1/chat/completions` requests from
/// peers on the mesh. Held inside an `Arc` — cheap to clone,
/// thread-safe.
pub struct SovereignInferenceAdapter {
    provider: Arc<dyn InferenceProvider>,
}

impl SovereignInferenceAdapter {
    pub fn new(provider: Arc<dyn InferenceProvider>) -> Self {
        Self { provider }
    }

    /// Flatten an OpenAI-style message list into a single prompt.
    /// Sovereign's `CompletionRequest` takes a flat prompt plus an
    /// optional system message, so we concat user/assistant turns
    /// into the prompt and pull the first system message out
    /// separately. Assistant replies in the history become labelled
    /// `Assistant:` lines — gives the model conversational context
    /// without needing a proper chat template layer here.
    fn flatten(request: &ChatCompletionRequest) -> (String, Option<String>) {
        let mut system: Option<String> = None;
        let mut convo = String::new();
        for msg in &request.messages {
            match msg.role.as_str() {
                "system" => {
                    if system.is_none() {
                        system = Some(msg.content.clone());
                    } else if let Some(existing) = system.as_mut() {
                        // Multiple system messages — keep them all,
                        // separated. Rare but allowed by OpenAI.
                        existing.push_str("\n\n");
                        existing.push_str(&msg.content);
                    }
                }
                "user" => {
                    convo.push_str("User: ");
                    convo.push_str(&msg.content);
                    convo.push_str("\n\n");
                }
                "assistant" => {
                    convo.push_str("Assistant: ");
                    convo.push_str(&msg.content);
                    convo.push_str("\n\n");
                }
                other => {
                    // Tool / function roles go through verbatim as
                    // labelled lines — harmless for models that
                    // don't understand them.
                    convo.push_str(other);
                    convo.push_str(": ");
                    convo.push_str(&msg.content);
                    convo.push_str("\n\n");
                }
            }
        }
        convo.push_str("Assistant:");
        (convo, system)
    }

    fn build_completion_request(
        &self,
        request: &ChatCompletionRequest,
    ) -> CompletionRequest {
        let (prompt, system) = Self::flatten(request);
        let mut req = CompletionRequest::new(&prompt).with_speed(Speed::Slow);
        if let Some(s) = system {
            req = req.with_system(&s);
        }
        req.max_tokens = request.max_tokens.map(|n| n as usize);
        req.temperature = request.temperature;
        req.top_p = request.top_p;
        if let Some(oicp) = &request.oicp {
            req = req.with_oicp(oicp.clone());
        }
        req
    }
}

#[async_trait]
impl LocalInferenceService for SovereignInferenceAdapter {
    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, String> {
        let req = self.build_completion_request(&request);
        let started = std::time::Instant::now();
        let resp = self
            .provider
            .complete(&req)
            .await
            .map_err(|e| format!("{e}"))?;
        tracing::info!(
            latency_ms = started.elapsed().as_millis() as u64,
            model = %resp.model_id,
            tokens = resp.tokens_used,
            "sovereign inference adapter: complete served"
        );
        Ok(ChatCompletionResponse {
            id: format!("chatcmpl-{}", started.elapsed().as_micros()),
            object: "chat.completion".into(),
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            model: resp.model_id,
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".into(),
                    content: resp.text,
                },
                finish_reason: Some("stop".into()),
            }],
            usage: Some(Usage {
                // Sovereign's `CompletionResponse.tokens_used`
                // conflates prompt + completion in a single number.
                // Report it as `total_tokens` and leave the split
                // unknown rather than lie — OpenAI clients tolerate
                // either prompt+completion set or total_tokens
                // alone.
                prompt_tokens: 0,
                completion_tokens: resp.tokens_used as u32,
                total_tokens: resp.tokens_used as u32,
            }),
        })
    }

    async fn chat_completion_stream(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<String, String>> + Send>>,
        String,
    > {
        let req = self.build_completion_request(&request);
        let inner = self
            .provider
            .complete_stream(&req)
            .await
            .map_err(|e| format!("{e}"))?;
        tracing::info!(
            "sovereign inference adapter: streaming started"
        );
        // Adapt sovereign_core's Result<String, Error> to our
        // Result<String, String> by stringifying the error.
        let mapped = inner.map(|item| item.map_err(|e| format!("{e}")));
        Ok(Box::pin(mapped))
    }

    fn provider_manifest(&self) -> Option<ProviderManifest> {
        Some(build_self_manifest(self.provider.as_ref()))
    }
}

/// Resolve a single human-readable model name from a provider,
/// preferring the Slow (synthesis) slot. Used by both the adapter
/// (so peer-side manifest and response `model` fields agree) and
/// by `MeshInferenceProvider` (so local-side scoring uses the same
/// identity the peer would see).
pub fn resolve_primary_model_name(provider: &dyn InferenceProvider) -> String {
    let slow = provider.model_id_for(Speed::Slow);
    if !slow.is_empty() && slow != "unknown" {
        return slow;
    }
    let fast = provider.model_id_for(Speed::Fast);
    if !fast.is_empty() && fast != "unknown" {
        return fast;
    }
    "sovereign-local".to_string()
}

/// Build this node's OICP `ProviderManifest` — one model with the
/// capability profile declared for it in `lcol-llm/models.toml`.
/// Shared between the server adapter (what peers fetch at
/// `/oicp/v1/capabilities`) and the client-side
/// `MeshInferenceProvider` (what local scores itself against), so
/// the two never disagree about our own declared capabilities.
///
/// Capability resolution:
///   1. Match the loaded model's filename against every slot in
///      the bundled `ModelsManifest`. A user on the `default`
///      profile with Qwen3.5-9B picks up `general=3, analysis=3,
///      ...` from the manifest automatically.
///   2. Fall back to `General=2, Analysis=2` (Moderate) when the
///      loaded model isn't in the manifest. That's the BYOM path
///      — the user pointed Sovereign at their own GGUF we don't
///      have profiling data for. Conservative but honest.
pub fn build_self_manifest(provider: &dyn InferenceProvider) -> ProviderManifest {
    let model_name = resolve_primary_model_name(provider);
    let capabilities = sovereign_core::models_manifest::DEFAULT_MANIFEST
        .capabilities_for_file(&model_name)
        .unwrap_or_else(|| {
            tracing::debug!(
                model = %model_name,
                "build_self_manifest: no OICP entry in models.toml — using defaults"
            );
            let mut caps = std::collections::HashMap::new();
            caps.insert(Capability::General, 2u8);
            caps.insert(Capability::Analysis, 2u8);
            caps
        });
    tracing::info!(
        model = %model_name,
        caps = ?capabilities,
        "build_self_manifest: advertised capabilities"
    );
    ProviderManifest {
        oicp_version: OICP_VERSION.to_string(),
        provider: Some(ProviderInfo {
            name: Some(model_name.clone()),
            provider_type: Some(ProviderType::Mesh),
        }),
        models: vec![ProviderModel {
            id: model_name,
            base_model: None,
            quantization: None,
            capabilities,
            context_tokens: 32_768,
            status: ModelStatus {
                available: true,
                loaded: true,
                estimated_tokens_per_sec: None,
                estimated_ttft_ms: None,
                estimated_load_time_sec: None,
            },
        }],
        knowledge: None,
        federation: None,
    }
}
