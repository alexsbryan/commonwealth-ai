// SPDX-License-Identifier: AGPL-3.0-or-later
//! The OpenAI wire's rendering, shared by every host that answers it: the
//! daemon's `/v1/*` routes and `serve`'s (phase-b pb-serve-program).
//!
//! Four pieces a host would otherwise re-derive: a chat stream frame as an
//! SSE chunk ([`sse_item`]), a prompt over the context window answered as
//! llama-server answers it ([`context_exceeded_response`]), an embeddings
//! request answered against a provider ([`embeddings_response`]), and
//! `/v1/models` rows from the manifests a host can dispatch ([`model_rows`]). What a host records about
//! the work (ledgers, activity) and where its provider comes from stay its
//! own.
//!
//! In the contracts leaf since pb-svrn-serving-ports (§12 3a rung 2), with
//! no server named: a chunk is an [`SseItem`], and each host's axum wrapper
//! turns it into its `Event` (svrn's `sovereign_daemon::openai_http`, serve's
//! `sovereign_serving_host::openai_http`).

use std::collections::HashMap;

use http::StatusCode;
use oicp_types::openai_types::{
    EmbeddingData, EmbeddingInput, EmbeddingRequest, EmbeddingResponse, ModelObject,
    ModelPerformance, Residency, StreamFrame, Usage,
};
use oicp_types::ProviderModel;
use tracing::warn;

use crate::InferenceProvider;

/// One server-sent event of an OpenAI stream, before a host renders it: a
/// `data:` payload, or a comment line that keeps the stream well-formed where
/// a frame is dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseItem {
    /// A `data:` line.
    Data(String),
    /// A `:` comment line.
    Comment(&'static str),
}

/// The fields every chunk of one chat stream repeats.
pub struct ChunkHeader {
    /// `chatcmpl-<unix ms>`.
    pub id: String,
    /// Unix seconds at stream start.
    pub created: u64,
    /// The model the request named, else `local`.
    pub model: String,
}

impl ChunkHeader {
    /// A header for a stream starting now. `id` / `created` follow the
    /// OpenAI convention of `chatcmpl-*` + unix timestamp; clients that care
    /// about stable ids set them on their side.
    pub fn new(model: Option<String>) -> Self {
        Self {
            id: format!("chatcmpl-{}", sovereign_time::unix_millis()),
            created: sovereign_time::unix_now_u64(),
            model: model.unwrap_or_else(|| "local".into()),
        }
    }
}

/// One chat stream frame as its OpenAI SSE item. The host appends the
/// `[DONE]` sentinel ([`DONE`]) when the stream ends.
pub fn sse_item(header: &ChunkHeader, frame: StreamFrame) -> SseItem {
    match frame {
        StreamFrame::Token(delta) => {
            let chunk = serde_json::json!({
                "id": header.id,
                "object": "chat.completion.chunk",
                "created": header.created,
                "model": header.model,
                "choices": [{
                    "index": 0,
                    "delta": { "content": delta },
                    "finish_reason": null
                }]
            });
            SseItem::Data(chunk.to_string())
        }
        StreamFrame::ToolCalls(calls) => {
            // Synthetic tools-streaming chunk. Local backends
            // parse `<tool_call>` markup post-generation, so we
            // emit one chunk carrying every parsed call rather
            // than the per-fragment `arguments` deltas the
            // OpenAI spec also permits. Both shapes are
            // wire-legal — clients accumulate by `tool_calls[i].
            // index` regardless of chunk count.
            let tool_calls_json: Vec<serde_json::Value> = calls
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    serde_json::json!({
                        "index": i,
                        "id": c.id,
                        "type": c.kind,
                        "function": {
                            "name": c.function.name,
                            "arguments": c.function.arguments,
                        }
                    })
                })
                .collect();
            let chunk = serde_json::json!({
                "id": header.id,
                "object": "chat.completion.chunk",
                "created": header.created,
                "model": header.model,
                "choices": [{
                    "index": 0,
                    "delta": {
                        "role": "assistant",
                        "tool_calls": tool_calls_json,
                    },
                    "finish_reason": null
                }]
            });
            SseItem::Data(chunk.to_string())
        }
        StreamFrame::Finish { reason, usage } => {
            // Terminal frame: emit an OpenAI-shaped chunk with
            // an empty delta and the real `finish_reason`. This
            // is the bug fix that motivated the typed surface —
            // the legacy `Result<String>` couldn't carry the
            // signal so every truncation looked like a clean
            // stop on the wire.
            let mut payload = serde_json::json!({
                "id": header.id,
                "object": "chat.completion.chunk",
                "created": header.created,
                "model": header.model,
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": reason.as_openai_str()
                }]
            });
            if let Some(u) = usage {
                payload["usage"] = serde_json::json!({
                    "prompt_tokens": u.prompt_tokens,
                    "completion_tokens": u.completion_tokens,
                    "total_tokens": u.total_tokens,
                });
            }
            SseItem::Data(payload.to_string())
        }
        StreamFrame::Error(e) => {
            // Surface the error as a final event then let the
            // stream close — clients handle the abrupt end.
            warn!(error = %e, "chat_completions: local stream error frame");
            SseItem::Data(format!(
                "{{\"error\":{{\"message\":\"{}\"}}}}",
                e.replace('"', "\\\"")
            ))
        }
        StreamFrame::Debug(_) => {
            // FIM-only glassbox frame; the chat path never
            // produces it. Drop defensively so a future producer
            // can't leak internals onto an unrelated surface.
            SseItem::Comment("debug frame dropped")
        }
    }
}

/// The OpenAI `[DONE]` sentinel that closes a chat stream.
/// `RemoteApiProvider::complete_stream` breaks its loop on it.
pub const DONE: &str = "[DONE]";

/// A prompt that fills the context window, answered as llama-server answers
/// it: `400`, `type: "exceed_context_size_error"`, the two numbers, and its
/// sentence (`server-common.cpp` `format_error_response` and `server-task.cpp`
/// `server_task_result_error::to_json` at the vendored 035e227).
///
/// The sentence matters as much as the status: litellm keys
/// `ContextWindowExceededError` on "exceeds the available context size" and
/// does not retry it. Rendered as a `503 backend_error`, one overflowing
/// prompt was retried 30 times by an agent's client before it gave up with
/// `ServiceUnavailableError` (the 35B e2eswe battery, cement task). Both
/// hosts' chat routes answer through this.
pub fn context_exceeded_response(prompt_tokens: u64, n_ctx: u64) -> http::Response<String> {
    warn!(
        prompt_tokens,
        n_ctx,
        "chat_completions: prompt exceeds the context window — 400, the caller must shorten it"
    );
    let body = serde_json::json!({
        "error": {
            "code": 400,
            "message": format!(
                "request ({prompt_tokens} tokens) exceeds the available context size \
                 ({n_ctx} tokens), try increasing it"
            ),
            "type": "exceed_context_size_error",
            "n_prompt_tokens": prompt_tokens,
            "n_ctx": n_ctx,
        }
    });
    let mut response = http::Response::new(body.to_string());
    *response.status_mut() = StatusCode::BAD_REQUEST;
    response.headers_mut().insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    response
}

/// Why an embeddings request was not answered: the status, the message and
/// the OpenAI error type the host renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingRefusal {
    /// 400 for a bad body, 503 for a backend that failed.
    pub status: StatusCode,
    /// The sentence the caller reads.
    pub message: String,
    /// The OpenAI error type.
    pub error_type: &'static str,
}

/// Answer an OpenAI embeddings request against `provider` in one batch call.
pub async fn embeddings_response(
    provider: &dyn InferenceProvider,
    request: EmbeddingRequest,
) -> Result<EmbeddingResponse, EmbeddingRefusal> {
    let inputs: Vec<String> = match request.input {
        EmbeddingInput::Single(s) => vec![s],
        EmbeddingInput::Batch(v) => v,
    };
    if inputs.is_empty() {
        return Err(EmbeddingRefusal {
            status: StatusCode::BAD_REQUEST,
            message: "embeddings request: `input` must be a non-empty string or array".into(),
            error_type: "invalid_request_error",
        });
    }

    let total_chars: usize = inputs.iter().map(|t| t.len()).sum();

    // One batch call: a single multi-sequence decode on the embedded engine,
    // or sharded across compute-child replicas by the routing facade. Both
    // beat the former per-input sequential loop for bulk ingest.
    let embeddings = match provider.embed_batch(&inputs).await {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "embeddings: local embed_batch failed");
            return Err(EmbeddingRefusal {
                status: StatusCode::SERVICE_UNAVAILABLE,
                message: format!("embedding batch failed: {e}"),
                error_type: "backend_error",
            });
        }
    };
    if embeddings.len() != inputs.len() {
        warn!(
            got = embeddings.len(),
            want = inputs.len(),
            "embeddings: backend returned the wrong number of vectors"
        );
        return Err(EmbeddingRefusal {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: format!(
                "embedding backend returned {} vectors for {} inputs",
                embeddings.len(),
                inputs.len()
            ),
            error_type: "backend_error",
        });
    }
    let data: Vec<EmbeddingData> = embeddings
        .into_iter()
        .enumerate()
        .map(|(i, embedding)| EmbeddingData {
            object: "embedding".into(),
            embedding,
            index: i,
        })
        .collect();

    // The OpenAI spec counts token usage; we only have char count, so
    // we produce a conservative ~4 chars/token estimate rather than
    // leaving the field out (some clients require it to be present).
    let approx_tokens = total_chars.div_ceil(4) as u32;
    Ok(EmbeddingResponse {
        object: "list".into(),
        data,
        model: request.model,
        usage: Usage {
            prompt_tokens: approx_tokens,
            completion_tokens: 0,
            total_tokens: approx_tokens,
        },
    })
}

/// `/v1/models` rows from `(holder, model)` pairs, holders unioned: one row
/// per distinct id, because two holders advertising the same model are ONE
/// dispatchable name. An id in `aliases` is owned by `alias→<target>` (this
/// node's binding); every other row by `owner`.
pub fn model_rows(
    holders: Vec<(String, ProviderModel)>,
    aliases: &HashMap<String, String>,
    owner: &str,
) -> Vec<ModelObject> {
    let mut rows: Vec<ModelObject> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();

    for (holder, model) in holders {
        let resident = model.status.loaded;
        if let Some(&row) = index.get(&model.id) {
            let existing: &mut ModelObject = &mut rows[row];
            if !existing.advertised_by.contains(&holder) {
                existing.advertised_by.push(holder);
            }
            // ANY holder with the weights in memory makes the name warm:
            // the resolver load-balances across holders and will pick one.
            // A cold row upgrading to Resident is the honest direction; the
            // reverse would let one cold peer mask a warm local slot.
            if resident {
                existing.residency = Some(Residency::Resident);
                if let Some(perf) = existing.performance.as_mut() {
                    perf.loaded = true;
                }
            }
            continue;
        }

        let residency = if resident {
            Residency::Resident
        } else {
            Residency::Cold
        };
        index.insert(model.id.clone(), rows.len());
        rows.push(ModelObject {
            // An alias (`primary`, `commonwealth/fast`) is a first-class
            // dispatchable name, not a synthetic decoration: it appears here
            // because a manifest advertised it, so it is resolvable by
            // definition. The pre-2026-08-27 handler appended aliases from
            // `slot_aliases` unconditionally, which is how `embed` came to be
            // listed on a node whose manifest never advertised it.
            //
            // The target named here is THIS node's binding. That is the right
            // one to show even on a row a peer also advertises: an alias is
            // dereferenced by whichever node ends up serving, so "what does
            // `primary` resolve to" is node-relative by design, and this is
            // the answer that applies if the request stays here.
            owned_by: match aliases.get(&model.id) {
                Some(target) => format!("alias→{target}"),
                None => owner.into(),
            },
            id: model.id,
            object: "model".into(),
            created: 0,
            // The manifest's capability CLAIMS, which is what the scheduler
            // actually scores. The store path published a `CapabilityProfile`
            // here instead — a different shape for the same field, and the
            // one further from the routing decision.
            capabilities: serde_json::to_value(&model.claims).ok(),
            performance: Some(ModelPerformance {
                // The manifest carries per-claim throughput, not a per-model
                // estimate; the orchestrator's shard plan was the only source
                // of these and it does not exist on the embedded path. Zeroed
                // rather than omitted so `loaded` stays readable — absence
                // here is what made availability invisible before.
                estimated_tokens_per_sec: 0.0,
                estimated_ttft_ms: 0,
                loaded: resident,
            }),
            residency: Some(residency),
            advertised_by: vec![holder],
        });
    }
    rows
}
