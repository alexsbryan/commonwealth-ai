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
//! Scope for v2: non-streaming tool-call round-trip against the Slow
//! slot. `request.tools` is injected into the model's chat template
//! by `sovereign-inference::embedded::format_prompt`; the raw model
//! output is parsed by `parse_tool_calls_with_errors` and the
//! resulting structured `tool_calls` populated on the response.
//! Streaming with tools is deferred (non-streaming is forced when
//! `tools.is_some()`).
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use commonwealth_api::openai_types::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, FunctionCall,
    ToolCall, Usage,
};
use commonwealth_api::state::LocalInferenceService;
use commonwealth_inference::oicp::{
    Capability, CapabilityClaim, CapabilityHint, CapabilityProfile, LatencyClass,
    ModelStatus, ProviderInfo, ProviderManifest, ProviderModel, ProviderType,
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
                    // Replay prior tool-call requests back into the
                    // prompt in the same `<tool_call>{json}</tool_call>`
                    // format the model emits. This keeps the agent loop
                    // coherent when a client resends an entire
                    // conversation history that includes earlier tool
                    // invocations — the model sees its own prior calls
                    // instead of a silent gap.
                    if let Some(calls) = msg.tool_calls.as_ref() {
                        for tc in calls {
                            let entry = serde_json::json!({
                                "name": tc.function.name,
                                "arguments": tc.function.arguments,
                            });
                            convo.push_str("<tool_call>");
                            convo.push_str(&entry.to_string());
                            convo.push_str("</tool_call>\n");
                        }
                    }
                    convo.push_str("\n");
                }
                "tool" => {
                    // Tool-result turn. Tag the content with the
                    // call id so a downstream template layer (or the
                    // model, for models that parse it) can correlate
                    // the result with the originating `<tool_call>`.
                    // Keeping this stringified rather than dropping
                    // the id means a malformed replay is visible —
                    // silent drops would masquerade as "tool
                    // returned nothing".
                    if let Some(id) = msg.tool_call_id.as_deref() {
                        convo.push_str(&format!("Tool[{id}]: "));
                    } else {
                        convo.push_str("Tool: ");
                    }
                    convo.push_str(&msg.content);
                    convo.push_str("\n\n");
                }
                other => {
                    // Other roles (function, developer, …) go through
                    // verbatim as labelled lines — harmless for models
                    // that don't understand them.
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

    /// Translate OpenAI tool defs into Sovereign core `ToolSchema`.
    /// Kept as a separate helper so the adapter stays testable without
    /// needing a live inference provider.
    fn forward_tools(request: &ChatCompletionRequest) -> Option<Vec<sovereign_core::types::ToolSchema>> {
        let tools = request.tools.as_ref()?;
        if tools.is_empty() {
            return None;
        }
        Some(
            tools
                .iter()
                .map(|t| sovereign_core::types::ToolSchema {
                    name: t.function.name.clone(),
                    description: t.function.description.clone(),
                    parameters: t.function.parameters.clone(),
                })
                .collect(),
        )
    }

    /// Build the internal `CompletionRequest` a peer-served chat
    /// completion should carry. The slot (Fast vs Slow) is chosen
    /// by re-applying OICP scoring against our local manifest —
    /// see `oicp_select::pick_slot_for_oicp`. Without this, the
    /// Joiner's OICP selection work stops at our front door: we
    /// default to Slow and fire up the primary slot regardless of
    /// whether a smaller, faster slot would already satisfy the
    /// request's capability requirements.
    fn build_completion_request(
        &self,
        request: &ChatCompletionRequest,
    ) -> CompletionRequest {
        let (prompt, system) = Self::flatten(request);
        // Build a skeleton request with the OICP envelope attached
        // BEFORE choosing a slot — the slot picker reads the
        // envelope to decide.
        let mut req = CompletionRequest::new(&prompt);
        if let Some(s) = system {
            req = req.with_system(&s);
        }
        // Forward the OpenAI `model` field as `model_id` so the
        // local provider's slot picker can route to a named slot
        // when one is loaded. Empty/whitespace strings stay None so
        // the slot picker falls through to its default policy. The
        // OpenAI `model` field is `Option<String>` because some
        // clients omit it for legacy completions endpoints.
        if let Some(model) = request.model.as_ref() {
            let trimmed = model.trim();
            if !trimmed.is_empty() {
                req = req.with_model_id(trimmed);
            }
        }
        req.max_tokens = request.max_tokens.map(|n| n as usize);
        req.temperature = request.temperature;
        req.top_p = request.top_p;
        if let Some(oicp) = &request.oicp {
            req = req.with_oicp(oicp.clone());
        }
        // Tool-use: carry tool schemas + tool_choice through so the
        // inference backend can inject them into the chat template
        // (`sovereign-inference::embedded::format_prompt`). When tools
        // are present we also bias the slot picker toward Slow — the
        // policy guard (`guard_tools_on_fast`) rejects Fast+tools
        // later in the pipeline, so biasing here avoids racing into a
        // guaranteed-reject state.
        req.tools = Self::forward_tools(request);
        req.tool_choice = request.tool_choice.clone();
        // OpenAI `response_format: {"type":"json_schema", json_schema:
        // {"name":..., "schema":..., "strict":...}}` → core
        // `structured_output: <schema>`. The sampler builder
        // (`build_sampler` in sovereign-inference::embedded) consumes
        // this as the JSON Schema for `LlamaSampler::llguidance`. We
        // also accept `{"type":"json_object"}` (any-JSON) by mapping
        // to the trivial `{"type":"object"}` schema.
        if let Some(rf) = request.response_format.as_ref() {
            req.structured_output = extract_response_format_schema(rf);
        }
        let speed = if req.tools.is_some() {
            sovereign_core::types::Speed::Slow
        } else {
            crate::oicp_select::pick_slot_for_oicp(self.provider.as_ref(), &req)
        };
        req.with_speed(speed)
    }
}

/// Pull a JSON Schema out of the OpenAI `response_format` envelope.
/// Returns `None` for unrecognised or missing shapes so we don't
/// propagate junk into the sampler.
pub(crate) fn extract_response_format_schema(
    rf: &serde_json::Value,
) -> Option<serde_json::Value> {
    let kind = rf.get("type").and_then(|v| v.as_str())?;
    match kind {
        "json_schema" => rf
            .get("json_schema")
            .and_then(|js| js.get("schema"))
            .cloned(),
        "json_object" => Some(serde_json::json!({"type": "object"})),
        _ => None,
    }
}

/// Remove every `<tool_call>...</tool_call>` block from the response
/// text. Used when the adapter has already extracted tool calls into
/// the structured `tool_calls` field — keeping the raw markup in
/// `content` causes clients to double-render.
pub(crate) fn strip_tool_call_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    while cursor < text.len() {
        if let Some(rel) = text[cursor..].find("<tool_call>") {
            let start = cursor + rel;
            out.push_str(&text[cursor..start]);
            match text[start..].find("</tool_call>") {
                Some(end_rel) => {
                    cursor = start + end_rel + "</tool_call>".len();
                }
                None => {
                    // Unterminated — preserve the rest as-is so the
                    // model's unfinished output remains visible.
                    out.push_str(&text[start..]);
                    return out;
                }
            }
        } else {
            out.push_str(&text[cursor..]);
            break;
        }
    }
    // Tidy up double-newlines left behind by stripped blocks.
    out.trim().to_string()
}

/// Guard rejecting tool-enabled requests that would land on the Fast
/// slot. The Fast slot (Qwen3-1.7B in the current stack) has no
/// tools-aware chat template, so executing a tool request against it
/// would silently drop every tool the caller listed. Per M2 scoping
/// we fail loudly with a structured error instead of silently
/// escalating — masking the latency/capacity implication hides
/// capacity issues from operators.
///
/// Returns `Err(message)` only when **both** conditions hold:
/// - `request.tools.is_some()` AND non-empty;
/// - the slot picker landed on `Speed::Fast`.
///
/// Pure function; no I/O. Unit-tested without spinning up the mesh.
pub(crate) fn guard_tools_on_fast(
    tools_present: bool,
    picked_speed: sovereign_core::types::Speed,
) -> Result<(), String> {
    if tools_present && picked_speed == sovereign_core::types::Speed::Fast {
        return Err(
            "fast slot does not support tool_calls; re-send with preferred_speed=Slow \
             (see sovereign atos probe-driver for a capability probe)"
                .to_string(),
        );
    }
    Ok(())
}

#[async_trait]
impl LocalInferenceService for SovereignInferenceAdapter {
    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, String> {
        let req = self.build_completion_request(&request);

        // Slot-policy guard: fail loud when a tool-enabled request
        // lands on the Fast slot. Emit a structured warn log so
        // operators see the attempt even if the client buries the
        // error. Intentionally checked BEFORE inference kicks off —
        // a rejection costs a few microseconds, not a full inference
        // call.
        let tools_present = request.tools.as_ref().is_some_and(|t| !t.is_empty());
        if let Err(msg) = guard_tools_on_fast(tools_present, req.preferred_speed) {
            tracing::warn!(
                tools_count = request.tools.as_ref().map(|t| t.len()).unwrap_or(0),
                preferred_speed = ?req.preferred_speed,
                "inference adapter: rejecting tool request on fast slot"
            );
            return Err(msg);
        }

        let started = std::time::Instant::now();
        let resp = self
            .provider
            .complete(&req)
            .await
            .map_err(|e| format!("{e}"))?;

        // Tool-call extraction. Only parse when the caller supplied
        // tools; otherwise any stray `<tool_call>` text the model
        // produced stays in `content` untouched. `with_errors` variant
        // so parse failures are visible in telemetry instead of
        // silently collapsing to zero calls.
        let (parsed_calls, parse_errors) = if tools_present {
            sovereign_inference::embedded::parse_tool_calls_with_errors(&resp.text)
        } else {
            (Vec::new(), Vec::new())
        };
        for raw in &parse_errors {
            tracing::warn!(
                payload = %raw,
                "inference adapter: tool_call parse failed"
            );
        }

        let tool_calls_out: Option<Vec<ToolCall>> = if parsed_calls.is_empty() {
            None
        } else {
            Some(
                parsed_calls
                    .into_iter()
                    .enumerate()
                    .map(|(i, c)| ToolCall {
                        id: format!(
                            "call_{}_{}",
                            started.elapsed().as_micros(),
                            i
                        ),
                        kind: "function".into(),
                        function: FunctionCall {
                            name: c.name,
                            arguments: c.arguments,
                        },
                    })
                    .collect(),
            )
        };

        // When the model issued tool calls, strip the raw tool-call
        // markup out of the returned content so downstream clients
        // don't re-render it. The structured `tool_calls` field is
        // the authoritative signal.
        let clean_content = if tool_calls_out.is_some() {
            strip_tool_call_blocks(&resp.text)
        } else {
            resp.text
        };

        tracing::info!(
            latency_ms = started.elapsed().as_millis() as u64,
            model = %resp.model_id,
            tokens = resp.tokens_used,
            tool_calls = tool_calls_out.as_ref().map(|c| c.len()).unwrap_or(0),
            parse_errors = parse_errors.len(),
            "sovereign inference adapter: complete served"
        );

        let finish_reason = if tool_calls_out.is_some() {
            "tool_calls".to_string()
        } else {
            "stop".to_string()
        };
        let assistant_msg = ChatMessage {
            role: "assistant".into(),
            content: clean_content,
            tool_call_id: None,
            tool_calls: tool_calls_out,
        };
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
                message: assistant_msg,
                finish_reason: Some(finish_reason),
            }],
            usage: Some(Usage {
                prompt_tokens: resp.prompt_tokens as u32,
                completion_tokens: resp
                    .tokens_used
                    .saturating_sub(resp.prompt_tokens) as u32,
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
        // Streaming + tool_calls is not supported in M2. OpenAI's
        // streaming tool-call frames (delta.tool_calls[i].function.
        // arguments partial JSON) add enough complexity that we'd
        // rather land the non-streaming tool path first and revisit.
        // Fail loud so clients can downgrade rather than silently
        // getting tool-less text.
        if request.tools.as_ref().is_some_and(|t| !t.is_empty()) {
            tracing::warn!(
                tools_count = request.tools.as_ref().map(|t| t.len()).unwrap_or(0),
                "inference adapter: streaming+tools is unsupported in M2"
            );
            return Err(
                "streaming with tools is not supported yet; re-send with stream=false \
                 (non-streaming tool round-trip is available)"
                    .to_string(),
            );
        }

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

    async fn embed(&self, input: &str) -> Result<Vec<f32>, String> {
        // Delegate to the underlying provider's EmbedSlot. The
        // commonwealth-api handler wraps the returned vector in an
        // OpenAI-shape `EmbeddingResponse`.
        self.provider
            .embed(input)
            .await
            .map_err(|e| format!("{e}"))
    }

    // ── Runtime slot management ─────────────────────────────────
    //
    // These delegate to the InferenceProvider trait, which has
    // default `Err(...)` implementations for non-embedded providers
    // (remote API, mesh peer). Only `EmbeddedLlamaCpp` overrides
    // them. The HTTP handler returns 501/400 when the underlying
    // provider can't service the request.

    async fn load_extra_slot(
        &self,
        slot_name: String,
        path: std::path::PathBuf,
        context_size: u32,
    ) -> Result<String, String> {
        self.provider
            .load_extra_slot(slot_name, path, context_size)
            .map_err(|e| format!("{e}"))
    }

    async fn unload_extra_slot(&self, slot_name: &str) -> Result<Option<String>, String> {
        self.provider
            .unload_extra_slot(slot_name)
            .map_err(|e| format!("{e}"))
    }

    async fn extras_inventory(&self) -> Vec<(String, String)> {
        self.provider.extras_inventory()
    }

    async fn warmup_primary(&self) -> Result<(), String> {
        self.provider
            .warmup_primary()
            .await
            .map_err(|e| format!("{e}"))
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
/// `sovereign/models.toml`. Shared between the server adapter (what
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
        let claims = synthesize_slot_claims(speed, &model_name, &capabilities);
        models.push(ProviderModel {
            id: model_name,
            base_model: None,
            quantization: None,
            context_tokens: 32_768,
            status: ModelStatus {
                available: true,
                loaded: true,
                estimated_tokens_per_sec: None,
                estimated_ttft_ms: None,
                estimated_load_time_sec: None,
            },
            size_gb,
            claims,
        });
    }

    // PR-E2: Code specialist. Separate ProviderModel entry so peer
    // schedulers can see the `code` hint claim without having to
    // first elicit a hot-swap. Only emitted when the provider
    // actually has a Code specialist configured — the default
    // `InferenceProvider::code_model_id()` returns `None`, so
    // single-model / remote providers skip this branch.
    //
    // We mark the code slot `loaded: false` because it shares the
    // lazy chat mutex with the primary — one of them can be
    // resident at a time. That lets the selector treat code and
    // primary as equally "warm-ish" rather than double-counting
    // either one.
    if let Some(code_name) = provider.code_model_id() {
        if !code_name.is_empty()
            && code_name != "unknown"
            && seen_ids.insert(code_name.clone())
        {
            let info = sovereign_core::models_manifest::DEFAULT_MANIFEST
                .info_for_file(&code_name);
            let (capabilities, size_gb) = match info {
                Some(slot) => (slot.capabilities, slot.size_gb),
                None => {
                    let mut caps = std::collections::HashMap::new();
                    caps.insert(Capability::Code, 3u8);
                    caps.insert(Capability::General, 2u8);
                    (caps, None)
                }
            };
            tracing::info!(
                model = %code_name,
                caps = ?capabilities,
                size_gb = ?size_gb,
                "build_self_manifest: advertised code specialist"
            );
            // Code slot always advertises as Slow-tier — it carries
            // the full context window and long output budget, and a
            // code-hinted request is never a routing/classification
            // Fast call. The hint is forced to `code` regardless of
            // the filename heuristic, because this slot's entire
            // reason for existing is the `code` claim.
            let code_hint_claims = synthesize_code_slot_claims(&code_name, &capabilities);
            models.push(ProviderModel {
                id: code_name,
                base_model: None,
                quantization: None,
                context_tokens: 32_768,
                status: ModelStatus {
                    available: true,
                    loaded: false,
                    estimated_tokens_per_sec: None,
                    estimated_ttft_ms: None,
                    estimated_load_time_sec: None,
                },
                size_gb,
                claims: code_hint_claims,
            });
        }
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

/// Synthesize v0.3 capability claims for a loaded slot.
///
/// Two-slot model: Fast carries a `LatencyClass::Fast` claim with a
/// reduced context window (routing / classification is always short)
/// and a small output budget; Slow carries a `LatencyClass::Normal`
/// claim with the full advertised context. The hint tracks the
/// model's code specialization by name ("coder" / "code-llama"
/// variants → `code`), falling back to `general`.
///
/// Affinity is derived from the v0.2 proficiency for the relevant
/// capability (Code proficiency for code-hint claims; max of
/// General/Analysis/Instruction proficiencies for general). This
/// keeps the two advertising surfaces in agreement until PR-E
/// replaces the heuristic with structured per-model config.
fn synthesize_slot_claims(
    speed: Speed,
    model_name: &str,
    profile: &CapabilityProfile,
) -> Vec<CapabilityClaim> {
    let lower = model_name.to_lowercase();
    let is_code_specialist = lower.contains("coder")
        || lower.contains("code-llama")
        || lower.contains("codellama")
        || lower.contains("deepseek-coder");

    let (hint, affinity) = if is_code_specialist {
        let code = profile.get(&Capability::Code).copied().unwrap_or(0);
        (
            CapabilityHint::code(),
            (code as f32 / 4.0).clamp(0.0, 1.0),
        )
    } else {
        let best = [
            Capability::General,
            Capability::Analysis,
            Capability::Instruction,
        ]
        .into_iter()
        .map(|c| profile.get(&c).copied().unwrap_or(0))
        .max()
        .unwrap_or(0);
        (
            CapabilityHint::general(),
            (best as f32 / 4.0).clamp(0.0, 1.0),
        )
    };

    // Per-slot context/output envelopes: Fast is tuned for short
    // requests (routing + classification), Slow carries the full
    // 32K context advertised at the model level.
    match speed {
        Speed::Fast => vec![CapabilityClaim::new(
            hint,
            LatencyClass::Fast,
            8_000,
            1_000,
            affinity,
        )],
        Speed::Medium | Speed::Slow => vec![CapabilityClaim::new(
            hint,
            LatencyClass::Normal,
            32_768,
            4_000,
            affinity,
        )],
    }
}

/// PR-E2: synthesize claims for a dedicated Code specialist model.
///
/// Unlike `synthesize_slot_claims`, the hint is pinned to `code`
/// regardless of filename — this model is the code slot by
/// configuration, not by heuristic. Affinity is read from the v0.2
/// Code proficiency (same source of truth the filename heuristic
/// consults) so peer scoring lines up with what the local provider
/// would self-report for the same model.
///
/// Latency is `LatencyClass::Normal` — the slot hot-swaps with the
/// primary in `EmbeddedLlamaCpp`, so first-request TTFT is dominated
/// by a 5–30s reload. Advertising it as Fast would produce incorrect
/// routing for single-turn classification calls. Output budget is
/// generous (4000 tokens) so long refactors and documentation
/// generation aren't clipped mid-diff.
fn synthesize_code_slot_claims(
    model_name: &str,
    profile: &CapabilityProfile,
) -> Vec<CapabilityClaim> {
    let code = profile.get(&Capability::Code).copied().unwrap_or(0);
    // Floor affinity at 0.5 when the filename smells like a code
    // model even if the manifest doesn't report Code proficiency
    // — a BYOM code GGUF not yet in `models.toml` should still be
    // discoverable as code-capable by peers.
    let lower = model_name.to_lowercase();
    let filename_signals_code = lower.contains("coder")
        || lower.contains("code-llama")
        || lower.contains("codellama")
        || lower.contains("deepseek-coder");
    let affinity_floor = if filename_signals_code { 0.5 } else { 0.0 };
    let affinity = ((code as f32 / 4.0).clamp(0.0, 1.0)).max(affinity_floor);

    vec![CapabilityClaim::new(
        CapabilityHint::code(),
        LatencyClass::Normal,
        32_768,
        4_000,
        affinity,
    )]
}

#[cfg(test)]
mod guard_tests {
    use super::guard_tools_on_fast;
    use sovereign_core::types::Speed;

    #[test]
    fn tool_request_on_slow_slot_passes() {
        assert!(guard_tools_on_fast(true, Speed::Slow).is_ok());
        assert!(guard_tools_on_fast(true, Speed::Medium).is_ok());
    }

    #[test]
    fn tool_request_on_fast_slot_rejected() {
        let err = guard_tools_on_fast(true, Speed::Fast).unwrap_err();
        assert!(err.contains("fast slot"));
        assert!(err.contains("tool_calls"));
        assert!(err.contains("preferred_speed=Slow"));
    }

    #[test]
    fn toolless_request_on_fast_slot_passes() {
        assert!(guard_tools_on_fast(false, Speed::Fast).is_ok());
    }

    #[test]
    fn toolless_request_on_slow_slot_passes() {
        assert!(guard_tools_on_fast(false, Speed::Slow).is_ok());
    }
}

#[cfg(test)]
mod adapter_translation_tests {
    use super::{strip_tool_call_blocks, SovereignInferenceAdapter};
    use commonwealth_api::openai_types::{
        ChatCompletionRequest, ChatMessage, FunctionCall, ToolCall, ToolDefinition, ToolFunction,
    };

    fn tool_def(name: &str) -> ToolDefinition {
        ToolDefinition {
            kind: "function".into(),
            function: ToolFunction {
                name: name.into(),
                description: Some(format!("description of {name}")),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"x": {"type": "string"}},
                    "required": ["x"],
                }),
            },
        }
    }

    fn user(content: &str) -> ChatMessage {
        ChatMessage::new("user", content)
    }

    #[test]
    fn flatten_preserves_tool_message_id() {
        let req = ChatCompletionRequest {
            model: None,
            messages: vec![
                user("what's the weather?"),
                ChatMessage {
                    role: "assistant".into(),
                    content: String::new(),
                    tool_call_id: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "call_123".into(),
                        kind: "function".into(),
                        function: FunctionCall {
                            name: "get_weather".into(),
                            arguments: r#"{"city":"SF"}"#.into(),
                        },
                    }]),
                },
                ChatMessage {
                    role: "tool".into(),
                    content: r#"{"temperature":62}"#.into(),
                    tool_call_id: Some("call_123".into()),
                    tool_calls: None,
                },
            ],
            temperature: None,
            max_tokens: None,
            stream: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            tools: Some(vec![tool_def("get_weather")]),
            tool_choice: Some(serde_json::json!("auto")),
            oicp: None,
                    response_format: None,
        };
        let (prompt, _system) = SovereignInferenceAdapter::flatten(&req);
        // The prior tool call is replayed as a <tool_call> block so
        // Qwen3.5's template sees the model's own previous turn in a
        // shape it recognizes.
        assert!(prompt.contains("<tool_call>"));
        assert!(prompt.contains("get_weather"));
        // The tool result carries its call id so the model can
        // correlate it with the originating call.
        assert!(prompt.contains("Tool[call_123]:"));
        assert!(prompt.contains(r#"{"temperature":62}"#));
    }

    #[test]
    fn forward_tools_translates_schema() {
        let req = ChatCompletionRequest {
            model: None,
            messages: vec![user("hi")],
            temperature: None,
            max_tokens: None,
            stream: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            tools: Some(vec![tool_def("a"), tool_def("b")]),
            tool_choice: None,
            oicp: None,
                    response_format: None,
        };
        let forwarded = SovereignInferenceAdapter::forward_tools(&req).unwrap();
        assert_eq!(forwarded.len(), 2);
        assert_eq!(forwarded[0].name, "a");
        assert_eq!(forwarded[1].name, "b");
        assert_eq!(forwarded[0].description.as_deref(), Some("description of a"));
    }

    #[test]
    fn forward_tools_empty_returns_none() {
        let req = ChatCompletionRequest {
            model: None,
            messages: vec![],
            temperature: None,
            max_tokens: None,
            stream: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            tools: Some(Vec::new()),
            tool_choice: None,
            oicp: None,
                    response_format: None,
        };
        assert!(SovereignInferenceAdapter::forward_tools(&req).is_none());
    }

    #[test]
    fn strip_tool_call_blocks_removes_markup() {
        let text = "Let me check.\n<tool_call>{\"name\":\"f\",\"arguments\":{}}</tool_call>\nDone.";
        let stripped = strip_tool_call_blocks(text);
        assert!(!stripped.contains("<tool_call>"));
        assert!(!stripped.contains("</tool_call>"));
        // Surrounding prose is preserved.
        assert!(stripped.contains("Let me check."));
        assert!(stripped.contains("Done."));
    }

    #[test]
    fn strip_tool_call_blocks_handles_multiple() {
        let text = "<tool_call>a</tool_call>mid<tool_call>b</tool_call>end";
        let stripped = strip_tool_call_blocks(text);
        assert_eq!(stripped, "midend");
    }

    #[test]
    fn strip_tool_call_blocks_tolerates_unterminated() {
        // Defensive: a truncated model output shouldn't panic — we
        // preserve the tail so operators can see what happened.
        let text = "ok <tool_call>{\"name\":\"never_closed\"";
        let stripped = strip_tool_call_blocks(text);
        assert!(stripped.contains("never_closed"));
    }
}

#[cfg(test)]
mod self_manifest_tests {
    //! PR-E2: verify `build_self_manifest` surfaces a third
    //! ProviderModel with a `code`-hinted claim when the underlying
    //! provider reports `code_model_id() == Some(...)`. Regression
    //! coverage for the "peer can't see my code slot" bug that
    //! would cause mesh routing to round-trip code requests through
    //! a hot-swap instead of picking the configured specialist.
    use super::{build_self_manifest, synthesize_code_slot_claims};
    use async_trait::async_trait;
    use commonwealth_inference::oicp::{Capability, CapabilityHint, CapabilityProfile, LatencyClass};
    use futures::Stream;
    use sovereign_core::traits::InferenceProvider;
    use sovereign_core::types::{
        CompletionRequest, CompletionResponse, Depth, ProviderCapabilities, Speed,
    };
    use sovereign_core::Result;
    use std::pin::Pin;

    /// Minimal stub that mimics the three-slot shape: fast + primary
    /// + optional code. We intentionally don't need a real model —
    /// only the metadata methods are consulted by
    /// `build_self_manifest`.
    struct SlotStub {
        fast_id: &'static str,
        primary_id: &'static str,
        code_id: Option<&'static str>,
    }

    #[async_trait]
    impl InferenceProvider for SlotStub {
        async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionResponse> {
            unreachable!("build_self_manifest never calls complete")
        }
        async fn complete_stream(
            &self,
            _req: &CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            unreachable!("build_self_manifest never calls complete_stream")
        }
        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            unreachable!("build_self_manifest never calls embed")
        }
        fn model_id_for(&self, speed: Speed) -> String {
            match speed {
                Speed::Fast => self.fast_id.to_string(),
                Speed::Medium | Speed::Slow => self.primary_id.to_string(),
            }
        }
        fn code_model_id(&self) -> Option<String> {
            self.code_id.map(|s| s.to_string())
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 32_768,
                supports_structured_output: false,
                relative_speed: Speed::Medium,
                relative_reasoning: Depth::Deep,
            }
        }
    }

    #[test]
    fn manifest_omits_code_entry_when_no_code_slot() {
        let stub = SlotStub {
            fast_id: "fast.Q4_0",
            primary_id: "primary.Q5_K_M",
            code_id: None,
        };
        let manifest = build_self_manifest(&stub);
        // No model in the manifest carries a `code` claim when
        // the provider has no code slot configured.
        let any_code_claim = manifest.models.iter().flat_map(|m| m.claims.iter())
            .any(|c| c.hint == CapabilityHint::code());
        assert!(
            !any_code_claim,
            "manifest leaked a code claim even without a code slot: {:#?}",
            manifest.models
        );
        assert_eq!(manifest.models.len(), 2, "expected fast + primary only");
    }

    #[test]
    fn manifest_emits_third_entry_with_code_claim_when_code_slot_configured() {
        let stub = SlotStub {
            fast_id: "fast.Q4_0",
            primary_id: "primary.Q5_K_M",
            code_id: Some("qwen-coder-32b-instruct.Q4_K_M"),
        };
        let manifest = build_self_manifest(&stub);
        assert_eq!(
            manifest.models.len(),
            3,
            "expected fast + primary + code, got {:#?}",
            manifest.models
        );
        let code_model = manifest
            .models
            .iter()
            .find(|m| m.id == "qwen-coder-32b-instruct.Q4_K_M")
            .expect("code model should be in manifest");
        assert!(
            !code_model.status.loaded,
            "code slot shares the lazy chat mutex — must not claim to be resident"
        );
        let code_claim = code_model
            .claims
            .iter()
            .find(|c| c.hint == CapabilityHint::code())
            .expect("code model must carry a `code` hint claim");
        assert_eq!(code_claim.latency_class, LatencyClass::Normal);
        // Affinity floors at 0.5 even when v2 proficiency data is
        // missing, because the filename signals code-specialist
        // strongly enough.
        assert!(
            code_claim.affinity >= 0.5,
            "code affinity should floor at 0.5 for filename-signalled BYOM coders: {}",
            code_claim.affinity
        );
    }

    #[test]
    fn manifest_does_not_duplicate_when_code_id_equals_primary_id() {
        // Defensive: misconfig where the user points the code slot
        // at the same GGUF as the primary. The manifest should not
        // emit a duplicate model entry with a different hint.
        let stub = SlotStub {
            fast_id: "fast.Q4_0",
            primary_id: "shared.Q5_K_M",
            code_id: Some("shared.Q5_K_M"),
        };
        let manifest = build_self_manifest(&stub);
        let ids: Vec<_> = manifest.models.iter().map(|m| m.id.clone()).collect();
        let mut uniq = ids.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(ids.len(), uniq.len(), "no duplicate ids: {ids:?}");
    }

    #[test]
    fn synthesize_code_claim_uses_v2_proficiency_when_available() {
        let mut profile: CapabilityProfile = std::collections::HashMap::new();
        profile.insert(Capability::Code, 4u8); // max proficiency
        let claims = synthesize_code_slot_claims("my-custom-coder", &profile);
        assert_eq!(claims.len(), 1);
        // v2 proficiency 4/4 → affinity 1.0.
        assert!((claims[0].affinity - 1.0).abs() < 1e-6);
    }

    #[test]
    fn synthesize_code_claim_uses_floor_for_unknown_byom_coder() {
        // Empty profile → 0 proficiency → floor kicks in because the
        // filename matches the heuristic.
        let profile: CapabilityProfile = std::collections::HashMap::new();
        let claims = synthesize_code_slot_claims("codellama-13b-byom.Q4_K_M", &profile);
        assert!(claims[0].affinity >= 0.5);
    }

    #[test]
    fn synthesize_code_claim_no_floor_for_non_code_filename() {
        // If the filename doesn't signal code and no v2 proficiency
        // is known, affinity stays at 0.0 — peers will rank this
        // slot behind ones with real coding signal.
        let profile: CapabilityProfile = std::collections::HashMap::new();
        let claims = synthesize_code_slot_claims("mystery-model.Q4_K_M", &profile);
        assert_eq!(claims[0].affinity, 0.0);
    }
}
