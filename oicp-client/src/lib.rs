// SPDX-License-Identifier: AGPL-3.0-or-later
// Contract crate: the public surface IS the product — every pub item needs
// docs (count-ratcheted by lint-gate, never a hard deny).
#![warn(missing_docs)]
//! `oicp-client` — a pure-HTTP OICP / OpenAI-compatible inference client.
//!
//! `RemoteApiProvider` speaks the OpenAI chat/embeddings wire (plus the OICP
//! request envelope and `/oicp/v1/capabilities` manifest fetch);
//! `SplitInferenceProvider` fans chat and embed to two model ids over one
//! endpoint. Both implement `sovereign_contracts::traits::InferenceProvider`,
//! so a package can drive a Sovereign daemon (or any OICP-conforming host)
//! without linking the local llama.cpp engine. Moved wholesale from
//! `sovereign-inference/src/remote.rs`; the daemon crate re-exports it at the
//! historical `sovereign_inference::remote::*` path.

use std::pin::Pin;
use std::time::Instant;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use serde::Deserialize;

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::oicp::{OicpResponseMeta, ProviderManifest};
use sovereign_contracts::traits::{InferenceProvider, ResidentSlot};
use sovereign_contracts::types::*;

/// OpenAI-compatible API client.
///
/// Works with any endpoint implementing the OpenAI chat/completions API:
/// vLLM, Ollama, llama.cpp server, text-generation-inference, etc.
pub struct RemoteApiProvider {
    client: reqwest::Client,
    endpoint: String,
    api_key: Option<String>,
    model_id: String,
    /// When true, `model_id` is a routing/attribution LABEL rather than
    /// a name the remote endpoint can resolve, and must never be put on
    /// the wire as the `model` field.
    ///
    /// The mesh scheduler builds one provider per peer with the literal
    /// id `"mesh-peer"` (`peer_inference.rs::provider_for_peer`), which
    /// exists so logs and `CompletionResponse::model_id` can say where a
    /// turn went. It names no model anywhere in the fleet. Sending it as
    /// `model` puts the receiving node on its explicit-name path, where
    /// it resolves to nobody and returns `ModelNotLoaded` — the origin
    /// then books a peer failure and falls back to local, quarantining a
    /// healthy peer after three strikes. See the regression test
    /// `an_unnamed_ranked_dispatch_sends_a_model_the_peer_can_resolve`.
    model_id_is_placeholder: bool,
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
    /// Texts per `/embeddings` request in [`InferenceProvider::embed_batch`].
    ///
    /// Sized to keep one request's payload modest while giving the
    /// daemon's embed slot several full multi-sequence decodes to chew
    /// on (it packs 16 sequences per decode). Larger batches stop paying
    /// — the slot's packing, not the request count, is the throughput
    /// bound past this point.
    pub const EMBED_BATCH_INPUTS: usize = 64;

    pub fn new(endpoint: &str, api_key: Option<String>, model_id: &str, context_size: u32) -> Self {
        Self::with_timeout(endpoint, api_key, model_id, context_size, DEFAULT_TIMEOUT)
    }

    /// Declare that `model_id` is a routing/attribution label, not a
    /// name the remote can serve. See the field docs on
    /// `model_id_is_placeholder`.
    ///
    /// Opt-in rather than inferred: a provider pointed at a real model
    /// must keep pinning its name on the wire, which is what the
    /// 2026-07-23 fast-slot fix (c8b0519b) exists to guarantee. Only the
    /// caller knows whether the id it supplied names anything.
    pub fn with_placeholder_model_id(mut self) -> Self {
        self.model_id_is_placeholder = true;
        self
    }

    /// One `/embeddings` call carrying every text as an array `input`.
    ///
    /// Rows come back with an `index` field; we sort by it rather than
    /// trusting arrival order, and verify the count matches so a partial
    /// response can never silently misalign embeddings with chunks —
    /// that would corrupt retrieval in a way no test downstream would
    /// catch.
    async fn embed_many_one_request(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let url = format!("{}/embeddings", self.endpoint);
        let body = serde_json::json!({
            "model": &self.model_id,
            "input": texts,
        });

        let mut req = self.client.post(&url).json(&body);
        if let Some(ref auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }

        let response = req
            .send()
            .await
            .map_err(|e| Error::Inference(format!("Batch embedding request failed: {e}")))?;

        if !response.status().is_success() {
            return Err(Error::NotImplemented(format!(
                "Batch embedding not supported by this endpoint (status {})",
                response.status()
            )));
        }

        #[derive(Deserialize)]
        struct EmbedResponse {
            data: Vec<EmbedData>,
        }
        #[derive(Deserialize)]
        struct EmbedData {
            embedding: Vec<f32>,
            #[serde(default)]
            index: usize,
        }

        let parsed: EmbedResponse = response.json().await.map_err(|e| {
            Error::Inference(format!("Failed to parse batch embedding response: {e}"))
        })?;

        if parsed.data.len() != texts.len() {
            return Err(Error::Inference(format!(
                "Batch embedding returned {} rows for {} inputs",
                parsed.data.len(),
                texts.len()
            )));
        }

        let mut rows = parsed.data;
        rows.sort_by_key(|d| d.index);
        Ok(rows.into_iter().map(|d| d.embedding).collect())
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
            model_id_is_placeholder: false,
            context_size,
            // The embed query-instruction prefix is model-family knowledge
            // that this pure HTTP client no longer computes. Callers that
            // need it (the embed slot of `SplitInferenceProvider`) set it via
            // `with_query_instruction`; chat providers and document-embed
            // (`embed`, which ignores the prefix) leave it empty.
            query_instruction: String::new(),
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
            model_id_is_placeholder: false,
            context_size,
            // The embed query-instruction prefix is model-family knowledge
            // that this pure HTTP client no longer computes. Callers that
            // need it (the embed slot of `SplitInferenceProvider`) set it via
            // `with_query_instruction`; chat providers and document-embed
            // (`embed`, which ignores the prefix) leave it empty.
            query_instruction: String::new(),
        }
    }

    /// Set the query-side embedding instruction prefix (empty by default).
    /// Applied by `embed_query` so a remote query embedding is bit-identical
    /// to the embedded engine's. The prefix is model-family knowledge the
    /// caller resolves (from the OICP manifest's `EmbedModelInfo`, or —
    /// pre-v0.4 — from `ModelsManifest::embed_query_instruction`).
    pub fn with_query_instruction(mut self, query_instruction: String) -> Self {
        self.query_instruction = query_instruction;
        self
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

        // Pin the OpenAI `model` field when the caller asked for a
        // specific model — and, since 2026-07-23, ALSO for slot-routed
        // Medium/Slow requests, pinned to this provider's resolved
        // chat model. The previous behaviour (empty model + auto
        // latency envelope, "the daemon's local pick maps Normal and
        // Extended to the same primary slot") was wrong in practice:
        // the daemon's Priority-1 OICP routing claim-scores EVERY
        // loaded model against the envelope, its scheduler-side claims
        // are synthesized with a hardcoded 32k context (feasibility
        // gates never bind), and capability-profile affinities tie
        // across model sizes — so Normal-class traffic was routed by
        // plan-iteration order and consistently served by the FAST 4B
        // slot while callers believed they were on the primary
        // (observed 2026-07-23: all 496 enrichment calls of a
        // book-report run attributed to Qwen3.5-4B).
        //
        // Fast requests keep the empty-model + envelope form: a
        // fast-class pick is the desired outcome there, and the
        // FastShort overflow lane still engages daemon-side.
        // A placeholder id names nothing the remote can resolve, so the
        // Medium/Slow pin below must not fire for it — an unnamed mesh
        // dispatch stays unnamed on the wire and routes on the envelope
        // instead. Every provider pointed at a real model is unaffected
        // and still pins, which is the fast-slot guarantee.
        let pinnable = (!self.model_id_is_placeholder).then_some(self.model_id.as_str());
        let model_field = match request.model_id.as_deref() {
            Some(mid) => mid,
            None => match request.preferred_speed {
                Speed::Fast => "",
                Speed::Medium | Speed::Slow => pinnable.unwrap_or(""),
            },
        };
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
        } else if model_field.is_empty() {
            // Canonical Speed→LatencyClass map (SLOT_POLICY §8). Slow
            // derives Normal, not Extended (rule 4.4). Attached ONLY
            // when no model is pinned above: the daemon's Priority-1
            // OICP routing treats any envelope as an explicit routing
            // opinion and would override the pinned model name — the
            // 2026-07-23 fast-slot hijack described at `model_field`.
            let class = sovereign_contracts::slot_policy::speed_to_latency(request.preferred_speed);
            let mut req =
                sovereign_contracts::oicp::InferenceRequirements::new().with_latency_class(class);
            if let Some(n) = request.max_tokens {
                req = req.with_max_output_tokens(n as u32);
            }
            serde_json::to_value(&req).ok()
        } else {
            None
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

        // Commonwealth extension: forward the caller-directed
        // stable-prefix declaration (bytes of the user prompt shared
        // byte-identically across sibling requests). The daemon's
        // inference_adapter carries it onto
        // `CompletionRequest.stable_prefix_len`; the engine uses it to
        // checkpoint/restore decode state at that boundary
        // (prefix_state.rs). Advisory — dropping it costs prefill
        // time, never correctness — but without this forward the
        // per-claim grounding gate loses its evidence-prefix reuse on
        // every daemon-routed (CLI `chat ask`) path.
        if let Some(n) = request.stable_prefix_len {
            body["stable_prefix_len"] = serde_json::json!(n);
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

    /// The daemon root: `self.endpoint` with any `/v1` suffix stripped. Routes
    /// mounted at the daemon root (not under `/v1`) — warmup and the OICP
    /// capabilities manifest — resolve from here. Two endpoint shapes appear in
    /// the codebase: callers like `chat_cmd/bootstrap` pass
    /// `http://host:9741/v1`; peer-inference callers pass `http://peer:9741`.
    /// Both resolve to the same root here.
    fn daemon_root(&self) -> &str {
        self.endpoint.strip_suffix("/v1").unwrap_or(&self.endpoint)
    }

    fn warmup_url(&self) -> String {
        format!("{}/internal/inference/warmup", self.daemon_root())
    }

    /// Fetch the OICP capabilities manifest from a provider.
    /// Returns None if the provider doesn't support OICP (404 or parse failure).
    pub async fn fetch_oicp_manifest(&self) -> Option<ProviderManifest> {
        // `/oicp/v1/capabilities` is mounted at the daemon root, NOT under
        // `/v1` (same as warmup) — strip a `/v1` endpoint suffix so a caller
        // holding the OpenAI `/v1` URL still reaches it. Without this, a
        // `/v1`-shaped endpoint hit `…/v1/oicp/v1/capabilities` → 404 → None.
        let url = format!("{}/oicp/v1/capabilities", self.daemon_root());

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

/// Fetch the OICP capabilities manifest from a daemon at `endpoint`, which may
/// be either the daemon root (`http://host:9741`) or the OpenAI `/v1` shape
/// (`http://host:9741/v1`) — both resolve, since the manifest lives at the
/// daemon root (see [`RemoteApiProvider::daemon_root`]). `bearer` is the
/// optional auth token for a non-loopback host.
///
/// Returns `None` on a v0.3 host (no `/oicp/v1/capabilities`) or any transport
/// or parse failure. Callers treat `None` as "degrade to v0.3 client defaults"
/// — never a hard error. This is the ergonomic entry a package uses to source
/// context length + the embed query-instruction prefix from the host's own
/// manifest (v0.4 §7 context discoverability, §4 embed completeness) rather
/// than compiling those values in.
pub async fn fetch_manifest(endpoint: &str, bearer: Option<String>) -> Option<ProviderManifest> {
    // A throwaway provider carries no model/context — `fetch_oicp_manifest`
    // only reads the endpoint, HTTP client, and auth header.
    RemoteApiProvider::new(endpoint, bearer, "", 0)
        .fetch_oicp_manifest()
        .await
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
    ) -> Result<Pin<Box<dyn Stream<Item = sovereign_contracts::types::StreamFrame> + Send>>> {
        use sovereign_contracts::types::{FinishReason, StreamFrame, StreamUsage};
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

    /// Embed many texts with ONE request per chunk of
    /// [`Self::EMBED_BATCH_INPUTS`], using the `/embeddings` endpoint's
    /// array `input` form.
    ///
    /// Without this override the trait default loops `embed()` — one
    /// HTTP round-trip and one single-sequence decode per text. Measured
    /// on the 2026-07-24 book-ingest arc: the daemon had served 8959
    /// consecutive embed calls at `sequences=1`, and a 301-chunk
    /// document spent 78s of a 149s ingest embedding, ~250ms per chunk.
    /// The server side was batch-capable the whole time
    /// (`routes_inference::embeddings` → `LocalInferenceService::
    /// embed_batch` → the embed slot's multi-sequence decode, 16
    /// sequences per decode); only the client never asked for it.
    ///
    /// Chunks are sent sequentially on purpose: the embed slot
    /// serializes on a single context lock, so overlapping requests
    /// would queue inside the daemon rather than add throughput.
    ///
    /// Falls back to the sequential default if the endpoint rejects an
    /// array payload — third-party OpenAI-compatible servers vary, and
    /// an ingest must not fail over a request-shape difference.
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut out: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(Self::EMBED_BATCH_INPUTS) {
            match self.embed_many_one_request(chunk).await {
                Ok(vectors) => out.extend(vectors),
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        inputs = chunk.len(),
                        "embed_batch: array request failed; falling back to per-text embed"
                    );
                    for text in chunk {
                        out.push(self.embed(text).await?);
                    }
                }
            }
        }
        Ok(out)
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
    /// Kept so `embed_model_id()` can vouch for persisted embeddings
    /// (the T1 memory-embedding staleness guard) without a daemon
    /// round-trip.
    embed_model_id: String,
    /// Daemon-side chat slot context window, captured at construction (the same
    /// `SetupConfig.effective_context_size()` value the daemon's slot loader
    /// uses) so `effective_context_size` answers without a daemon round-trip —
    /// the runtime's budget-aware compaction arm reads it.
    context_size: u32,
    /// Absolute URL of the daemon's `/status`, derived once from the `/v1`
    /// endpoint. Sole consumer is [`Self::primary_slot_status`]: this
    /// provider holds no weights, so the only honest answer to "is the
    /// deep slot cold?" comes from the node that does.
    status_url: String,
}

impl SplitInferenceProvider {
    pub fn new(
        endpoint_v1: &str,
        chat_model_id: String,
        embed_model_id: String,
        context_size: u32,
        embed_query_instruction: String,
    ) -> Self {
        let chat = std::sync::Arc::new(RemoteApiProvider::new(
            endpoint_v1,
            None,
            &chat_model_id,
            context_size,
        ));
        // The embed slot carries the query-instruction prefix so
        // `embed_query` stays bit-identical to the embedded engine. The chat
        // slot never embeds, so it leaves the prefix empty.
        let embed = std::sync::Arc::new(
            RemoteApiProvider::new(endpoint_v1, None, &embed_model_id, context_size)
                .with_query_instruction(embed_query_instruction),
        );
        Self {
            chat,
            embed,
            chat_model_id,
            embed_model_id,
            context_size,
            // `endpoint_v1` is the OpenAI-shaped base (".../v1"); `/status`
            // is its sibling, not a child. Trim exactly one trailing "/v1"
            // (and any trailing slash) rather than string-replacing, so a
            // host whose path legitimately contains "v1" elsewhere survives.
            status_url: format!(
                "{}/status",
                endpoint_v1
                    .trim_end_matches('/')
                    .trim_end_matches("/v1")
                    .trim_end_matches('/')
            ),
        }
    }

    /// Build from an OICP manifest (v0.4 §7 context discoverability): resolve
    /// the chat slot's context window from the advertised
    /// [`ProviderModel::context_tokens`] rather than a hardcoded default, so a
    /// client's budget-aware compaction matches the host's real window.
    ///
    /// Falls back to the historical 8192 when the manifest doesn't advertise
    /// the chat model's context (a v0.3 host, or a model absent from
    /// `/v1/models`) — never a hard failure.
    pub fn from_manifest(
        endpoint_v1: &str,
        manifest: &ProviderManifest,
        chat_model_id: String,
        embed_model_id: String,
    ) -> Self {
        /// The pre-v0.4 client default, used when the host doesn't advertise a
        /// truthful `context_tokens` for the chat model.
        const V03_FALLBACK_CONTEXT: u32 = 8192;
        let context_size = manifest
            .models
            .iter()
            .find(|m| m.id == chat_model_id)
            .map(|m| m.context_tokens)
            .filter(|&c| c > 0)
            .unwrap_or(V03_FALLBACK_CONTEXT);
        // v0.4 §4: the embed model's query-instruction prefix is advertised in
        // the knowledge section; empty on a v0.3 host (or one without a
        // knowledge plane).
        let embed_query_instruction = manifest
            .knowledge
            .as_ref()
            .and_then(|k| k.embed_model.as_ref())
            .map(|e| e.query_instruction_prefix.clone())
            .unwrap_or_default();
        Self::new(
            endpoint_v1,
            chat_model_id,
            embed_model_id,
            context_size,
            embed_query_instruction,
        )
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

    /// Stream with a TYPED terminal frame.
    ///
    /// Third instance of the same defect, found while auditing the
    /// first two: `self.chat` parses the real `finish_reason` and
    /// `usage` off the SSE wire, but without this forward the trait
    /// default wraps the UNTYPED `complete_stream` and appends a
    /// `Finish { reason: Stop, usage: None }` it never observed. The
    /// trait doc is explicit that silent truncation "is the bug this
    /// method exists to make impossible" — and inheriting the default
    /// reintroduced exactly that: a `max_tokens` cutoff rendered
    /// identically to a clean stop, and token accounting read `None`,
    /// on every streaming turn in Attach mode.
    ///
    /// `complete_stream_with_id_and_finish` needs no forward of its
    /// own: its default composes this method with `model_id_for`, and
    /// both are now honest here.
    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = sovereign_contracts::types::StreamFrame> + Send>>> {
        self.chat.complete_stream_with_finish(request).await
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

    /// Route to the embed provider's batch path. Without this the trait
    /// default would loop `Self::embed` — the exact one-round-trip-per-
    /// chunk behaviour that made corpus ingest embed-bound.
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed.embed_batch(texts).await
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

    fn embed_model_id(&self) -> String {
        self.embed_model_id.clone()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.chat.capabilities()
    }

    fn effective_context_size(&self) -> Option<u32> {
        Some(self.context_size)
    }

    /// Ask the daemon whether its deep-reasoning slot is loaded.
    ///
    /// This provider owns no weights, so the inherited default (read the
    /// sync `resident_slots()`, which is empty here) would answer `None`
    /// forever — and the caller would stay silent through the exact wait
    /// it exists to explain. The attach-mode desktop runs its Runtime
    /// in-process against this provider, so that silence was the bug:
    /// a 95s cold load with a frozen counter and no stated cause.
    ///
    /// Fails soft in every direction — unreachable daemon, non-200,
    /// unparseable body, no primary row — all yield `None`, i.e. "can't
    /// say", never a fabricated verdict and never a blocked turn. The
    /// timeout is deliberately tight: this runs on the critical path
    /// immediately before synthesis, and a narration frame is never
    /// worth delaying the answer it narrates.
    async fn primary_slot_status(&self) -> Option<ResidentSlot> {
        #[derive(Deserialize)]
        struct StatusBody {
            inference: StatusInference,
        }
        #[derive(Deserialize)]
        struct StatusInference {
            #[serde(default)]
            resident: Vec<StatusSlot>,
        }
        #[derive(Deserialize)]
        struct StatusSlot {
            role: String,
            #[serde(default)]
            model_id: String,
            #[serde(default)]
            resident: bool,
            #[serde(default)]
            size_bytes: Option<u64>,
            #[serde(default)]
            transitioning: bool,
        }

        let resp = reqwest::Client::new()
            .get(&self.status_url)
            .timeout(std::time::Duration::from_millis(1500))
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: StatusBody = resp.json().await.ok()?;
        let slot = body.inference.resident.into_iter().find(|s| s.role == "primary")?;
        Some(ResidentSlot {
            role: slot.role,
            model_id: slot.model_id,
            resident: slot.resident,
            size_bytes: slot.size_bytes,
            transitioning: slot.transitioning,
            placement: None,
        })
    }

    /// Ask the daemon to load its deep-reasoning slot now.
    ///
    /// Same shape of bug as [`Self::primary_slot_status`] above, and it
    /// is worth stating plainly because this is the second instance:
    /// the trait's default `warmup_primary` is `Ok(())`, a SILENT
    /// no-op. A weight-less provider that doesn't override it therefore
    /// reports success while doing nothing, and the caller has no way
    /// to tell "warmed" from "never happened". The desktop's two warm
    /// triggers — window-focus and chat-mount — both ran through this
    /// provider in Attach mode, so both were dead: the app looked like
    /// it had warm-up wired end to end while every deep turn still paid
    /// the full cold load.
    ///
    /// `self.chat` already implements this correctly against the
    /// daemon's HTTP warmup route, so the fix is delegation, not new
    /// transport. Best-effort by the same reasoning as the rest of the
    /// warm path: an unwarmed slot is a slow first turn, never a
    /// blocked one. The embed provider is deliberately not warmed —
    /// the embed slot is eagerly loaded at daemon startup and never
    /// idle-unloaded, so it has nothing to warm.
    async fn warmup_primary(&self) -> Result<()> {
        self.chat.warmup_primary().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fetch_manifest_degrades_to_none_on_unreachable_host() {
        // Contract: a v0.3 host (or any transport failure) yields None so the
        // caller falls back to v0.3 defaults rather than hard-erroring. Port 1
        // is reserved/unbound, so this never touches a real daemon.
        let m = fetch_manifest("http://127.0.0.1:1", None).await;
        assert!(m.is_none());
        // Same for the `/v1`-shaped endpoint — `daemon_root` strips the suffix
        // before joining `/oicp/v1/capabilities`, then still can't connect.
        let m = fetch_manifest("http://127.0.0.1:1/v1", None).await;
        assert!(m.is_none());
    }

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
    fn build_request_slow_pins_provider_model_fast_stays_slot_routed() {
        let provider = RemoteApiProvider::new("http://localhost:8000/v1", None, "test-model", 4096);

        // Default (Slow) with model_id = None: pinned to the provider's
        // resolved chat model, and NO auto OICP envelope — the daemon's
        // Priority-1 envelope routing would override the pin (the
        // 2026-07-23 fast-slot hijack; see `build_request` docs).
        let request = CompletionRequest::new("Hello");
        let body = provider.build_request(&request);
        assert_eq!(body["model"], "test-model");
        assert!(body.get("oicp").is_none());

        // Fast with model_id = None keeps the empty model + envelope
        // form so the daemon routes it to a fast-class slot.
        let fast = CompletionRequest::new("Hello").with_speed(Speed::Fast);
        let body = provider.build_request(&fast);
        assert_eq!(body["model"], "");
        assert_eq!(body["oicp"]["latency_class"], "fast");
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
            prompt_shape: None,
            stable_prefix_len: None,
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
        use sovereign_contracts::oicp::{CapabilityHint, InferenceRequirements, LatencyClass};

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
            prompt_shape: None,
            stable_prefix_len: None,
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

    /// `SplitInferenceProvider` must actually ISSUE the warm request,
    /// not inherit the trait's silent `Ok(())`. This is the second bug
    /// of that exact shape on this provider (see `primary_slot_status`),
    /// and both were invisible because the default returns success — so
    /// the test asserts the wire behaviour, which is the only thing a
    /// no-op cannot fake. A one-shot TCP listener stands in for the
    /// daemon so this stays dependency-free and offline.
    #[tokio::test]
    async fn split_provider_actually_sends_the_warmup_request() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\n\r\n{\"latency_ms\":0}")
                .unwrap();
            stream.flush().unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });

        let provider = SplitInferenceProvider::new(
            &format!("http://127.0.0.1:{port}/v1"),
            "chat-model".to_string(),
            "embed-model".to_string(),
            4096,
            String::new(),
        );
        provider.warmup_primary().await.unwrap();

        let request = server.join().unwrap();
        assert!(
            request.starts_with("POST /internal/inference/warmup "),
            "warm-up must reach the daemon's warmup route; got request line: {}",
            request.lines().next().unwrap_or("<empty>")
        );
    }

    #[test]
    fn capabilities_url_resolves_at_daemon_root_for_both_endpoint_shapes() {
        // `/oicp/v1/capabilities` is mounted at the daemon root, so a `/v1`
        // endpoint (chat-bootstrap shape) must strip it — otherwise the fetch
        // hits `…/v1/oicp/v1/capabilities` (404) and manifest-driven context
        // silently falls back to the hardcoded default.
        let with_v1 = RemoteApiProvider::new("http://host:9741/v1", None, "m", 4096);
        let bare = RemoteApiProvider::new("http://host:9741", None, "m", 4096);
        assert_eq!(with_v1.daemon_root(), "http://host:9741");
        assert_eq!(bare.daemon_root(), "http://host:9741");
        assert_eq!(
            format!("{}/oicp/v1/capabilities", with_v1.daemon_root()),
            "http://host:9741/oicp/v1/capabilities"
        );
    }
}
