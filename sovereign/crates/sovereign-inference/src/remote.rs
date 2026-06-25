// SPDX-License-Identifier: AGPL-3.0-or-later
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
    /// Query-side instruction prefix for this model, resolved once at
    /// construction from the bundled manifest (empty for chat / non-embedding
    /// models). The embedded engine applies this in `embed_query_sync`; the
    /// remote `/embeddings` API has no query/document distinction, so the
    /// client prepends it before sending — making the remote query-embedding
    /// path bit-identical to the embedded one. See
    /// `ModelsManifest::embed_query_instruction`.
    query_instruction: String,
}

/// Default request timeout for `RemoteApiProvider`. Matches the
/// local-inference path's `CHAT_TIMEOUT` (1800s / 30 min) so a remote
/// peer isn't artificially capped tighter than the same call would be
/// locally — a Phase 1 enrichment call that takes 3 minutes on a slow
/// CPU-bound peer (grammar masking is single-threaded) would time out
/// at the previous 120s default before the peer could return, even
/// though the peer was healthy and producing tokens.
///
/// Embed callers reuse this provider; their response is <1s in
/// practice so the long timeout never fires for them in the happy
/// path. Tests/customization can adjust via `with_timeout`.
const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1800);

impl RemoteApiProvider {
    pub fn new(endpoint: &str, api_key: Option<String>, model_id: &str, context_size: u32) -> Self {
        Self::with_timeout(endpoint, api_key, model_id, context_size, DEFAULT_TIMEOUT)
    }

    /// Construct with an explicit request timeout. Use for tests or
    /// for short-lived health probes where waiting 30 min on a
    /// hanging peer is wrong.
    pub fn with_timeout(
        endpoint: &str,
        api_key: Option<String>,
        model_id: &str,
        context_size: u32,
        timeout: std::time::Duration,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_default();

        Self {
            client,
            endpoint: endpoint.trim_end_matches('/').to_string(),
            api_key,
            model_id: model_id.to_string(),
            context_size,
            query_instruction: sovereign_core::models_manifest::DEFAULT_MANIFEST
                .embed_query_instruction(model_id),
        }
    }

    /// Construct with a pre-built `reqwest::Client` and an explicit
    /// bearer token. Used by the mesh scheduler when routing to a
    /// pinned worker pod: the client carries a TLS pin to the pod's
    /// seed-derived cert and the bearer is the owner-signed
    /// `WorkerToken` the worker daemon's auth middleware validates.
    ///
    /// Equivalent to `new` in every other respect — request build,
    /// streaming, and OICP envelope handling are unchanged because
    /// they only consume `self.client` and `self.api_key`.
    pub fn with_client_and_bearer(
        endpoint: &str,
        client: reqwest::Client,
        bearer: String,
        model_id: &str,
        context_size: u32,
    ) -> Self {
        Self {
            client,
            endpoint: endpoint.trim_end_matches('/').to_string(),
            api_key: Some(bearer),
            model_id: model_id.to_string(),
            context_size,
            query_instruction: sovereign_core::models_manifest::DEFAULT_MANIFEST
                .embed_query_instruction(model_id),
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

        // Pin the OpenAI `model` field only when the caller asked
        // for a specific model. The runtime sets
        // `request.model_id = None` for slot-routed calls (router
        // classifier, synthesis, etc.) to let the daemon's OICP
        // picker decide. Hardcoding `self.model_id` here would defeat
        // that — `embedded::select_slot_for_request` matches the
        // model field against fast/primary slot file stems and routes
        // by name, bypassing OICP. With None we send an empty model
        // and the picker uses the OICP envelope (latency_class)
        // we attached below.
        let model_field = request.model_id.as_deref().unwrap_or("");
        let mut body = serde_json::json!({
            "model": model_field,
            "messages": messages,
        });

        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        if let Some(temperature) = request.temperature {
            body["temperature"] = serde_json::json!(temperature);
        }

        // OICP envelope. The runtime's local `Speed` enum is mapped
        // to `latency_class` (v0.3 §2.2) — internal types stay off
        // the wire while the daemon's slot picker routes by the
        // protocol's standard signal. If the caller attached an
        // explicit `oicp`, we honor it as-is.
        //
        // Privacy: deliberately left at the protocol default
        // (`LocalOnly` per §3.1). We do NOT silently downgrade the
        // privacy contract here — that would violate ARCH_PRINCIPLES
        // §7 (privacy invariants must be structural). Privacy-aware
        // callers attach their own oicp envelope above. The daemon's
        // privacy gate is responsible for serving LocalOnly via
        // local_inference rather than rejecting it.
        let oicp_val = if let Some(ref oicp) = request.oicp {
            serde_json::to_value(oicp).ok()
        } else {
            let class = match request.preferred_speed {
                Speed::Fast => sovereign_core::oicp::LatencyClass::Fast,
                Speed::Medium => sovereign_core::oicp::LatencyClass::Normal,
                Speed::Slow => sovereign_core::oicp::LatencyClass::Extended,
            };
            let mut req =
                sovereign_core::oicp::InferenceRequirements::new().with_latency_class(class);
            if let Some(n) = request.max_tokens {
                req = req.with_max_output_tokens(n as u32);
            }
            serde_json::to_value(&req).ok()
        };
        if let Some(v) = oicp_val {
            body["oicp"] = v;
        }

        // Forward the per-request `enable_thinking` toggle as
        // `chat_template_kwargs: { enable_thinking: <bool> }` —
        // the convention vLLM and llama-server both accept on
        // OpenAI-compatible endpoints. Without this the daemon
        // falls through to its hardcoded default
        // (`embedded.rs::apply_chat_template_oaicompat` historically
        // pinned `enable_thinking: false`). With it set explicitly
        // by the caller, the relational/witness path can flip
        // thinking ON so the chat template wraps the model's
        // planning trace in `<think>...</think>` — and the
        // post-process `strip_think_blocks` (eval-side runner +
        // production runtime where wired) can drop it cleanly.
        // Daemon-side unwrap: `inference_adapter::extract_enable_thinking`.
        if let Some(enable) = request.enable_thinking {
            body["chat_template_kwargs"] = serde_json::json!({
                "enable_thinking": enable,
            });
        }

        // Forward `think_budget` as the Commonwealth extension field the
        // daemon's `resolve_think_budget` reads. Without this the
        // runtime's `think_budget: Some(0)` (FastFocused synthesis, gap
        // check, router — every "don't think, just answer" call) dies at
        // the HTTP boundary and the engine-side thinking suppression in
        // `format_prompt` never engages: the chat template pre-opens
        // `<think>` and the model spends its whole `max_tokens` budget
        // on CoT (2026-06-10 fabrication burn-down — chaos honesty 0.45,
        // every fast-slot KQ answer was truncated raw deliberation).
        if let Some(tb) = request.think_budget {
            body["think_budget"] = serde_json::json!(tb);
        }

        // Forward `structured_output` to the daemon as the OpenAI
        // `response_format: {type: "json_schema", json_schema: {...}}`
        // envelope. Without this, the schema is dropped at the HTTP
        // boundary and the daemon's grammar-constraint layer never
        // sees it (silent fallback to free-form sampling). The daemon
        // unwraps it back into `request.structured_output` via
        // `inference_adapter::extract_response_format_schema`.
        if let Some(schema) = &request.structured_output {
            body["response_format"] = serde_json::json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "structured",
                    "schema": schema,
                },
            });
        }

        // Forward `lark_grammar` to the daemon as a sovereign-specific
        // extension field. The daemon's inference_adapter unwraps it
        // back onto CompletionRequest.lark_grammar; embedded.rs then
        // compiles it via llguidance and constrains decoding.
        //
        // Why this exists separately from `response_format`: lark
        // grammars are strictly more expressive than JSON-Schema
        // (regex tokens, recursion, custom productions) and the
        // OpenAI envelope has no slot for free-form lark. The
        // structured_output → response_format path remains the
        // canonical route for schema-shaped output; this is the
        // escape hatch for non-schema constraints like
        // `(entity ("," entity)*)?` per-line lists or strict
        // BREAK/CONTINUE alternations.
        //
        // The wire field name `lark_grammar` mirrors the
        // CompletionRequest field exactly — chosen for symmetry
        // with the in-process path so daemon-side debugging
        // doesn't need a translation table.
        if let Some(grammar) = &request.lark_grammar {
            body["lark_grammar"] = serde_json::json!(grammar);
        }

        // Forward the tool catalog + `tool_choice` so the daemon presents the
        // tools to the model in its chat template. Without this, a daemon-routed
        // agent/authoring loop sends `lark_grammar` (the call SHAPE) but the model
        // never sees the tools it's meant to call — so it emits a prose/markdown
        // description instead, the grammar's permitted plain-text branch lets it,
        // and the loop captures zero tool calls. (Proven 2026-06-24: workflow
        // authoring worked embedded but not attach-routed for exactly this reason.)
        // The embedded path receives `tools`/`tool_choice` directly; mirror that on
        // the wire. The daemon's inference_adapter rebuilds the tool-envelope grammar
        // from these, equivalent to the forwarded `lark_grammar`.
        if let Some(tools) = &request.tools {
            if !tools.is_empty() {
                body["tools"] = serde_json::Value::Array(
                    tools
                        .iter()
                        .map(|t| {
                            serde_json::json!({
                                "type": "function",
                                "function": {
                                    "name": t.name,
                                    "description": t.description,
                                    "parameters": t.parameters,
                                }
                            })
                        })
                        .collect(),
                );
            }
        }
        if let Some(tc) = &request.tool_choice {
            body["tool_choice"] = tc.clone();
        }

        body
    }

    fn auth_header(&self) -> Option<String> {
        self.api_key.as_ref().map(|k| format!("Bearer {k}"))
    }

    /// Daemon root + warmup path. The route is mounted at the daemon
    /// root (not under `/v1`), so we strip a `/v1` suffix from
    /// `self.endpoint` if present. Two endpoint shapes appear in the
    /// codebase: callers like `chat_cmd/bootstrap` pass
    /// `http://host:9741/v1`; peer-inference callers pass
    /// `http://peer:9741`. Both resolve to the same warmup URL here.
    fn warmup_url(&self) -> String {
        let base = self.endpoint.strip_suffix("/v1").unwrap_or(&self.endpoint);
        format!("{base}/internal/inference/warmup")
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
    /// OpenAI-compatible finish_reason carried on the terminal
    /// choice — `"stop"` / `"length"` / `"content_filter"` /
    /// `"tool_calls"`. Parsed into [`FinishReason`] at the consume
    /// site so the desktop cutoff chip + non-streaming surfacing
    /// behave identically across local and remote providers.
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct UsageInfo {
    #[serde(default)]
    total_tokens: usize,
    #[serde(default)]
    prompt_tokens: usize,
    /// Completion tokens generated. Distinct from `total_tokens -
    /// prompt_tokens` only when the server emits all three (some
    /// proxies don't); kept as the explicit source so the cutoff
    /// chip can read the authoritative split.
    #[serde(default)]
    completion_tokens: u32,
}

// ─── SSE Streaming Types ─────────────────────────────────────

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    /// OpenAI emits a final post-DONE chunk carrying token usage on
    /// some servers (vLLM, recent llama.cpp). `None` on servers that
    /// don't emit it — the cutoff chip still works via finish_reason
    /// alone, just without the precise generated-token count.
    #[serde(default)]
    usage: Option<UsageInfo>,
}

#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: StreamDelta,
    /// Set on the terminal chunk (and only the terminal chunk in
    /// OpenAI-compliant servers). Parsed via
    /// [`FinishReason::from_openai_str`] at the consume site so a
    /// stray non-OpenAI string (server bug) round-trips to None and
    /// we synthesise a `Stop` rather than panic.
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
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

        let first_choice = chat_response.choices.first();
        let text = first_choice
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default();
        let finish_reason = first_choice
            .and_then(|c| c.finish_reason.as_deref())
            .and_then(FinishReason::from_openai_str);

        let (tokens_used, prompt_tokens, completion_tokens) = chat_response
            .usage
            .map(|u| (u.total_tokens, u.prompt_tokens, Some(u.completion_tokens)))
            .unwrap_or((0, 0, None));

        let model_id = chat_response.model.unwrap_or_else(|| self.model_id.clone());

        if let Some(ref fr) = finish_reason {
            tracing::debug!(
                model = %model_id,
                finish_reason = %fr.as_openai_str(),
                completion_tokens = ?completion_tokens,
                "remote: chat_completions - finish_reason"
            );
        }

        Ok(CompletionResponse {
            text,
            tokens_used,
            prompt_tokens,
            model_id,
            latency_ms: start.elapsed().as_millis() as u64,
            oicp_meta: chat_response.oicp,
            finish_reason,
            completion_tokens,
        })
    }

    /// Typed-Finish streaming override. Parses the SSE
    /// `choices[].finish_reason` and `usage` fields and emits a
    /// terminal [`StreamFrame::Finish`] frame so peer-routed mesh
    /// streams surface real Length truncation instead of the trait
    /// default's synthetic `Stop`. Pairs with
    /// `MeshInferenceProvider::complete_stream_with_id_and_finish`,
    /// which is what carries the typed frame all the way to the
    /// runtime's cutoff-chip wiring.
    ///
    /// Note on OpenAI SSE shapes: `finish_reason` typically lands on
    /// the last `delta`-bearing chunk (or one just before `[DONE]`).
    /// `usage` lands either on the same chunk (vLLM, recent llama.cpp)
    /// or in a separate post-DONE chunk on some servers. We
    /// accumulate both lazily and emit them on the terminal Finish
    /// frame regardless of which chunk carried them.
    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = sovereign_core::types::StreamFrame> + Send>>> {
        use sovereign_core::types::{FinishReason, StreamFrame, StreamUsage};
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
            .map_err(|e| Error::Inference(format!("Remote typed stream request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            return Err(Error::Inference(format!(
                "Remote typed stream API returned {status}"
            )));
        }

        let byte_stream = response.bytes_stream();
        // Carry parser state across the byte-stream's filter_map by
        // streaming into a channel: parsing SSE line-by-line is
        // stateful (finish_reason + usage may land on any chunk
        // before [DONE]) and async-stream combinators can't carry
        // mutable state across yields cleanly. Channel-driven actor
        // keeps the parser straightforward.
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamFrame>(32);
        tokio::spawn(async move {
            use futures::StreamExt;
            let mut byte_stream = byte_stream;
            let mut buf = String::new();
            let mut finish_reason: Option<FinishReason> = None;
            let mut usage: Option<StreamUsage> = None;
            'outer: while let Some(chunk) = byte_stream.next().await {
                let Ok(bytes) = chunk else { continue };
                buf.push_str(&String::from_utf8_lossy(&bytes));
                // Process complete lines; leave the tail in buf for
                // the next iteration so a chunk-split SSE line
                // doesn't drop tokens.
                while let Some(pos) = buf.find('\n') {
                    let line = buf[..pos].trim().to_string();
                    buf.drain(..=pos);
                    if line == "data: [DONE]" {
                        break 'outer;
                    }
                    let Some(data) = line.strip_prefix("data: ") else {
                        continue;
                    };
                    let Ok(parsed) = serde_json::from_str::<StreamChunk>(data) else {
                        continue;
                    };
                    if let Some(u) = parsed.usage {
                        usage = Some(StreamUsage {
                            prompt_tokens: u.prompt_tokens as u32,
                            completion_tokens: u.completion_tokens,
                            total_tokens: u.total_tokens as u32,
                        });
                    }
                    for choice in parsed.choices {
                        if let Some(text) = choice.delta.content {
                            if !text.is_empty() && tx.send(StreamFrame::Token(text)).await.is_err()
                            {
                                return;
                            }
                        }
                        if let Some(reason_str) = choice.finish_reason {
                            finish_reason = FinishReason::from_openai_str(&reason_str);
                            tracing::debug!(
                                finish_reason = %reason_str,
                                "remote: stream - terminal finish_reason captured"
                            );
                        }
                    }
                }
            }
            let _ = tx
                .send(StreamFrame::Finish {
                    reason: finish_reason.unwrap_or(FinishReason::Stop),
                    usage,
                })
                .await;
        });
        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
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

        let token_stream = byte_stream.filter_map(|chunk| async move {
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
                        if let Some(content) =
                            chunk.choices.first().and_then(|c| c.delta.content.clone())
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

    /// Dispatch requests concurrently via HTTP connection pooling.
    /// Against a server with `--parallel N`, N requests run simultaneously.
    async fn complete_batch(
        &self,
        requests: &[CompletionRequest],
    ) -> Result<Vec<CompletionResponse>> {
        let futures: Vec<_> = requests.iter().map(|req| self.complete(req)).collect();
        futures::future::join_all(futures)
            .await
            .into_iter()
            .collect()
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
            return Err(Error::NotImplemented(
                "Embedding not supported by this endpoint".to_string(),
            ));
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
            .ok_or(Error::Inference(
                "No embedding data in response".to_string(),
            ))
    }

    /// Embed a *query* with this model's query-side instruction prefix.
    ///
    /// The OpenAI `/embeddings` endpoint has no query/document distinction, so
    /// the prefix is applied client-side: prepend it, then embed via the same
    /// HTTP path. This makes the result bit-identical to the embedded engine's
    /// `embed_query_sync` (which prepends the same `query_instruction`). When
    /// the model declares no query instruction (chat / non-embedding ids), the
    /// prefix is empty and this is exactly `embed()` — no behaviour change.
    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        if self.query_instruction.is_empty() {
            return self.embed(query).await;
        }
        let prefixed = format!("{}{query}", self.query_instruction);
        self.embed(&prefixed).await
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: self.context_size as usize,
            supports_structured_output: false,
            relative_speed: Speed::Medium,
            relative_reasoning: Depth::Deep,
        }
    }

    /// POSTs to the daemon's loopback warmup endpoint
    /// (`/internal/inference/warmup`, see
    /// `commonwealth-api::routes_internal::mesh_admin::inference_warmup`).
    /// Used by the desktop's child-process supervisor path so a
    /// window-focus warmup flows over HTTP to the supervised daemon
    /// rather than into an in-process slot.
    ///
    /// Best-effort and silent on failure (network error, 4xx/5xx) —
    /// the trait's default impl is a no-op for the same reason: an
    /// unwarmed slot is a slow first turn, not a broken caller.
    async fn warmup_primary(&self) -> Result<()> {
        let url = self.warmup_url();
        let mut req = self.client.post(&url).json(&serde_json::json!({}));
        if let Some(ref auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        match req.send().await {
            Ok(r) if r.status().is_success() => Ok(()),
            Ok(r) => {
                tracing::debug!(
                    status = %r.status(),
                    "RemoteApiProvider::warmup_primary: non-success, treating as no-op"
                );
                Ok(())
            }
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "RemoteApiProvider::warmup_primary: transport error, treating as no-op"
                );
                Ok(())
            }
        }
    }
}

/// Wraps two [`RemoteApiProvider`]s — one per endpoint — and routes
/// `InferenceProvider` trait calls to the correct one. `RemoteApiProvider` is
/// constructed with a single `model_id` used for BOTH `/chat/completions` and
/// `/embeddings`; sending a chat model to the embeddings endpoint returns
/// non-embedding shapes (or errors). Keeping two instances and routing by
/// method keeps the daemon honest: the chat endpoint never sees an embed model
/// id, and vice versa.
///
/// This is the "talk to the daemon over HTTP, own no weights" provider for
/// **both** `sovereign chat` (the daemon-backed CLI) and the desktop's Attach
/// mode (a CLI daemon already owns the models on `:9741`). Promoted here from
/// `sovereign-cli-llm::chat_cmd::bootstrap` (2026-06-16) so the two callers
/// share one impl rather than diverging.
pub struct SplitInferenceProvider {
    chat: std::sync::Arc<RemoteApiProvider>,
    embed: std::sync::Arc<RemoteApiProvider>,
    chat_model_id: String,
    /// Daemon-side chat slot context window, captured at construction (the same
    /// `SetupConfig.effective_context_size()` value the daemon's slot loader
    /// uses) so `effective_context_size` answers without a daemon round-trip —
    /// the runtime's budget-aware compaction arm reads it.
    context_size: u32,
}

impl SplitInferenceProvider {
    pub fn new(
        endpoint_v1: &str,
        chat_model_id: String,
        embed_model_id: String,
        context_size: u32,
    ) -> Self {
        let chat = std::sync::Arc::new(RemoteApiProvider::new(
            endpoint_v1,
            None,
            &chat_model_id,
            context_size,
        ));
        let embed = std::sync::Arc::new(RemoteApiProvider::new(
            endpoint_v1,
            None,
            &embed_model_id,
            context_size,
        ));
        Self {
            chat,
            embed,
            chat_model_id,
            context_size,
        }
    }
}

#[async_trait]
impl InferenceProvider for SplitInferenceProvider {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        self.chat.complete(request).await
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        self.chat.complete_stream(request).await
    }

    async fn complete_batch(
        &self,
        requests: &[CompletionRequest],
    ) -> Result<Vec<CompletionResponse>> {
        self.chat.complete_batch(requests).await
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.embed.embed(text).await
    }

    /// Route to the embed provider's `embed_query` so its model-specific
    /// query-instruction prefix is applied (the trait default would call
    /// `Self::embed`, the document path, silently dropping the prefix).
    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        self.embed.embed_query(query).await
    }

    fn model_id_for(&self, _speed: Speed) -> String {
        // Only one chat slot over HTTP; the daemon's own engine maps the
        // request (Speed / max_tokens) to its loaded fast/primary slots.
        // Reporting the request model is the most honest client-side signal.
        self.chat_model_id.clone()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.chat.capabilities()
    }

    fn effective_context_size(&self) -> Option<u32> {
        Some(self.context_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_request_basic() {
        let provider = RemoteApiProvider::new("http://localhost:8000/v1", None, "test-model", 4096);

        // Caller-specified `model_id` flows to the wire `model` field.
        // When `request.model_id = None` (default), the field is left
        // empty so the daemon's OICP slot picker decides — see the
        // doc comment on `build_request`.
        let request = CompletionRequest::new("Hello, world!").with_model_id("test-model");
        let body = provider.build_request(&request);

        assert_eq!(body["model"], "test-model");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Hello, world!");
    }

    #[test]
    fn build_request_default_model_id_is_empty_for_oicp_slot_routing() {
        let provider = RemoteApiProvider::new("http://localhost:8000/v1", None, "test-model", 4096);
        // CompletionRequest::new defaults model_id = None — the wire
        // `model` field is empty so the daemon picks via OICP envelope.
        let request = CompletionRequest::new("Hello");
        let body = provider.build_request(&request);
        assert_eq!(body["model"], "");
    }

    #[test]
    fn build_request_with_system() {
        let provider = RemoteApiProvider::new("http://localhost:8000/v1", None, "test-model", 4096);

        let request = CompletionRequest {
            prompt: "Hi".to_string(),
            system_message: Some("You are helpful.".to_string()),
            preferred_speed: Speed::Fast,
            max_tokens: Some(100),
            temperature: Some(0.5),
            structured_output: None,
            think_budget: None,
            top_k: None,
            top_p: None,
            oicp: None,
            tools: None,
            tool_choice: None,
            model_id: None,
            enable_thinking: None,
            sampling_mode: None,
            assistant_prefix: None,
            cmd_prefix: None,
            url_allowlist: None,
            evidence_id_allowlist: None,
            lark_grammar: None,
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
        use sovereign_core::oicp::{CapabilityHint, InferenceRequirements, LatencyClass};

        let provider = RemoteApiProvider::new("http://localhost:8000/v1", None, "test-model", 4096);

        let request = CompletionRequest {
            prompt: "Review this code".to_string(),
            system_message: None,
            preferred_speed: Speed::Slow,
            max_tokens: None,
            temperature: None,
            structured_output: None,
            think_budget: None,
            top_k: None,
            top_p: None,
            oicp: Some(
                InferenceRequirements::new()
                    .with_hint(CapabilityHint::code())
                    .with_latency_class(LatencyClass::Normal)
                    .with_context_tokens(8192),
            ),
            tools: None,
            tool_choice: None,
            model_id: None,
            enable_thinking: None,
            sampling_mode: None,
            assistant_prefix: None,
            cmd_prefix: None,
            url_allowlist: None,
            evidence_id_allowlist: None,
            lark_grammar: None,
        };

        let body = provider.build_request(&request);
        assert!(body.get("oicp").is_some());
        // v0.3: hint + latency class + sizing live at the top level
        // of the OICP envelope.
        assert_eq!(body["oicp"]["capability_hint"], "code");
        assert_eq!(body["oicp"]["latency_class"], "normal");
        assert_eq!(body["oicp"]["context_tokens"].as_u64(), Some(8192));
    }

    #[test]
    fn auth_header_present() {
        let provider = RemoteApiProvider::new(
            "http://localhost:8000/v1",
            Some("test-key-not-real".to_string()),
            "model",
            4096,
        );
        assert_eq!(
            provider.auth_header(),
            Some("Bearer test-key-not-real".to_string())
        );
    }

    #[test]
    fn auth_header_absent() {
        let provider = RemoteApiProvider::new("http://localhost:8000/v1", None, "model", 4096);
        assert_eq!(provider.auth_header(), None);
    }

    #[test]
    fn warmup_url_strips_v1_suffix() {
        let provider = RemoteApiProvider::new("http://localhost:9741/v1", None, "model", 4096);
        assert_eq!(
            provider.warmup_url(),
            "http://localhost:9741/internal/inference/warmup",
        );
    }

    #[test]
    fn warmup_url_preserves_bare_host() {
        let provider = RemoteApiProvider::new("http://peer:9741", None, "mesh-peer", 32_768);
        assert_eq!(
            provider.warmup_url(),
            "http://peer:9741/internal/inference/warmup",
        );
    }
}
