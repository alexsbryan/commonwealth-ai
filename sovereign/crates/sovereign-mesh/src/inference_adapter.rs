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

/// Build this node's OICP `ProviderManifest` — one `ProviderModel`
/// entry per loaded chat slot (Fast + Slow), each with the
/// capability profile + size_gb declared for it in
/// `lcol-llm/models.toml`. Shared between the server adapter (what
/// peers fetch at `/oicp/v1/capabilities`) and the client-side
/// `MeshInferenceProvider` (what local scores itself against) so
/// the two never disagree about our own declared capabilities.
///
/// Why both slots: on a Founder running the `high` profile, Fast
/// is Qwen3-1.7B (routing only) and Slow is Qwen3.5-27B (flagship
/// synthesis). A Joiner whose preferred profile is `{Analysis:3,
/// General:3}` should consider the 9B-class `default.thoughtful`
/// on its own side vs. the 27B on the Founder's — but if the
/// Founder also has a 9B loaded in the Fast slot and it already
/// satisfies `preferred`, the smaller/faster 9B should win over
/// the 27B. That tie-break only works if both are visible in the
/// manifest. Previously we advertised only the Slow-slot model,
/// so a Founder with a perfectly-adequate 9B available looked
/// like it was only offering the 27B, and the selector routed
/// requests to the bigger model unnecessarily.
///
/// Capability resolution (per slot):
///   1. Match the loaded model's filename against every slot in
///      the bundled `ModelsManifest`. A user on the `default`
///      profile with Qwen3.5-9B picks up `general=3, analysis=3,
///      ...` from the manifest automatically.
///   2. Fall back to `General=2, Analysis=2` (Moderate) when the
///      loaded model isn't in the manifest. That's the BYOM path
///      — the user pointed Sovereign at their own GGUF we don't
///      have profiling data for. Conservative but honest.
///
/// The Embed slot is deliberately excluded from the manifest: it
/// is not a chat-completion candidate and peer selection never
/// consults it. Duplicate entries (same model id in Fast + Slow
/// — happens when the provider collapses slots) are coalesced to
/// one, since the OICP scorer treats identical models identically.
pub fn build_self_manifest(provider: &dyn InferenceProvider) -> ProviderManifest {
    let mut seen_ids = std::collections::HashSet::new();
    let mut models: Vec<ProviderModel> = Vec::new();
    // Speed::Fast first, then Speed::Slow. Iterating in this order
    // means that if Fast and Slow resolve to the same underlying
    // model name (e.g. a stripped-down test harness that only
    // loads one model), we keep the Fast-labelled one — arbitrary
    // tie-break; they're the same model either way.
    for speed in [Speed::Fast, Speed::Slow] {
        let model_name = provider.model_id_for(speed);
        if model_name.is_empty() || model_name == "unknown" {
            continue;
        }
        if !seen_ids.insert(model_name.clone()) {
            continue;
        }
        let info = sovereign_core::models_manifest::DEFAULT_MANIFEST
            .info_for_file(&model_name);
        let (capabilities, size_gb) = match info {
            Some(slot) => (slot.capabilities, slot.size_gb),
            None => {
                tracing::debug!(
                    model = %model_name,
                    ?speed,
                    "build_self_manifest: no OICP entry in models.toml — using defaults"
                );
                let mut caps = std::collections::HashMap::new();
                caps.insert(Capability::General, 2u8);
                caps.insert(Capability::Analysis, 2u8);
                (caps, None)
            }
        };
        tracing::info!(
            model = %model_name,
            ?speed,
            caps = ?capabilities,
            size_gb = ?size_gb,
            "build_self_manifest: advertised slot"
        );
        models.push(ProviderModel {
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
            size_gb,
        });
    }
    // Provider name still reflects the primary — that's the name
    // response attribution uses ("qwen3.5-27b @ peer BeefyMac").
    // Fall back to whatever single model we have, or a sentinel.
    let provider_name = models
        .iter()
        .max_by(|a, b| {
            // Pick the "biggest" advertised model as the primary
            // display name. If sizes are unknown, the one that
            // landed later (Slow) wins via stable ordering.
            a.size_gb
                .unwrap_or(0.0)
                .partial_cmp(&b.size_gb.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|m| m.id.clone())
        .unwrap_or_else(|| "sovereign-local".to_string());
    ProviderManifest {
        oicp_version: OICP_VERSION.to_string(),
        provider: Some(ProviderInfo {
            name: Some(provider_name),
            provider_type: Some(ProviderType::Mesh),
        }),
        models,
        knowledge: None,
        federation: None,
    }
}
