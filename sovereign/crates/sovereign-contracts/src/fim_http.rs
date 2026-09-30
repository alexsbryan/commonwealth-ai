// SPDX-License-Identifier: AGPL-3.0-or-later
//! The FIM route's response half: a [`FimStreamStart`] rendered as one OpenAI
//! `text_completion` object or as SSE chunks + `[DONE]`. Shared by every door
//! that answers `POST /v1/completions` — the svrn daemon's (the editor's) and
//! serve's (pb-svrn-dials-serve) — so the two cannot render one stream two
//! ways. Parsing the wire into a `FimCompletionRequest` stays with each door.
//!
//! In the contracts leaf since pb-svrn-serving-ports (§12 3a rung 2), with no
//! server named: the aggregate is an `http::Response<String>` and the stream
//! a run of [`SseItem`]s; each door's axum wrapper renders them (svrn's
//! `sovereign_daemon::openai_http`, serve's `sovereign_serving_host::fim_http`).
//! [`edit_status`], `/status.inference.edit`, joined at pb-serve-ranks: serve's
//! adapter and svrn's OpenAI pass-through both answer it.

use futures::stream::BoxStream;
use futures::StreamExt;
use http::{header::CONTENT_TYPE, HeaderValue, Response, StatusCode};
use oicp_types::openai_types::{ErrorResponse, StreamFrame};
use oicp_types::{EditSlotStatus, FimStreamStart};

use crate::openai_http::SseItem;

/// A JSON body with its status, as axum's `(StatusCode, Json(..))` rendered it.
fn json_response(status: StatusCode, body: &serde_json::Value) -> Response<String> {
    let mut response = Response::new(body.to_string());
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

/// Envelope ids follow the OpenAI convention (`cmpl-*` + ms epoch).
fn completion_id() -> String {
    format!("cmpl-{}", sovereign_time::unix_millis())
}

/// Non-streaming: consume the whole stream, aggregate the text, and
/// return one OpenAI `text_completion` object. `sovereign_debug` is
/// attached when (and only when) the request opted in.
pub async fn fim_aggregated(
    start: FimStreamStart,
    debug_wanted: bool,
    model_echo: Option<String>,
) -> Response<String> {
    let FimStreamStart {
        stream,
        model_id,
        slot: _,
        fim_style: _,
    } = start;
    let mut text = String::new();
    let mut finish_reason = "stop".to_string();
    let mut usage = None;
    let mut debug_payload = None;
    let mut stream = Box::pin(stream);
    while let Some(frame) = stream.next().await {
        match frame {
            StreamFrame::Token(t) => text.push_str(&t),
            StreamFrame::Finish { reason, usage: u } => {
                finish_reason = reason.as_openai_str().to_string();
                usage = u;
            }
            StreamFrame::Error(e) => {
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::to_value(ErrorResponse::new(
                        format!("generation failed mid-stream: {e}"),
                        "backend_error",
                    ))
                    .unwrap_or_default(),
                );
            }
            StreamFrame::Debug(v) => debug_payload = Some(v),
            StreamFrame::ToolCalls(_) => {
                // Never produced by the FIM adapter; ignore defensively.
            }
        }
    }
    let mut body = serde_json::json!({
        "id": completion_id(),
        "object": "text_completion",
        "created": sovereign_time::unix_now_u64(),
        "model": model_echo.unwrap_or(model_id),
        "choices": [{
            "text": text,
            "index": 0,
            "finish_reason": finish_reason,
        }],
    });
    if let Some(u) = usage {
        body["usage"] = serde_json::json!({
            "prompt_tokens": u.prompt_tokens,
            "completion_tokens": u.completion_tokens,
            "total_tokens": u.total_tokens,
        });
    }
    if debug_wanted {
        if let Some(d) = debug_payload {
            body["sovereign_debug"] = d;
        }
    }
    json_response(StatusCode::OK, &body)
}

/// Streaming: bridge frames to SSE chunks (`text_completion` object
/// shape), a terminal chunk carrying the real `finish_reason` (+ the
/// opt-in `sovereign_debug`), then the `[DONE]` sentinel.
pub fn fim_sse_items(
    start: FimStreamStart,
    debug_wanted: bool,
    model_echo: Option<String>,
) -> BoxStream<'static, SseItem> {
    let id = completion_id();
    let created = sovereign_time::unix_now_u64();
    let model = model_echo.unwrap_or_else(|| start.model_id.clone());

    let chunks = start.stream.map(move |frame| {
        let id = id.clone();
        let model = model.clone();
        match frame {
            StreamFrame::Token(delta) => {
                let chunk = serde_json::json!({
                    "id": id,
                    "object": "text_completion",
                    "created": created,
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "text": delta,
                        "finish_reason": null
                    }]
                });
                SseItem::Data(chunk.to_string())
            }
            StreamFrame::Finish { reason, usage } => {
                let mut chunk = serde_json::json!({
                    "id": id,
                    "object": "text_completion",
                    "created": created,
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "text": "",
                        "finish_reason": reason.as_openai_str()
                    }]
                });
                if let Some(u) = usage {
                    chunk["usage"] = serde_json::json!({
                        "prompt_tokens": u.prompt_tokens,
                        "completion_tokens": u.completion_tokens,
                        "total_tokens": u.total_tokens,
                    });
                }
                SseItem::Data(chunk.to_string())
            }
            StreamFrame::Debug(v) => {
                if debug_wanted {
                    let chunk = serde_json::json!({
                        "id": id,
                        "object": "text_completion",
                        "created": created,
                        "model": model,
                        "choices": [],
                        "sovereign_debug": v,
                    });
                    SseItem::Data(chunk.to_string())
                } else {
                    // Opted-out debug frames vanish — the comment event
                    // keeps the stream well-formed without payload.
                    SseItem::Comment("debug dropped")
                }
            }
            StreamFrame::Error(e) => SseItem::Data(format!(
                "{{\"error\":{{\"message\":\"{}\"}}}}",
                e.replace('"', "\\\"")
            )),
            StreamFrame::ToolCalls(_) => {
                // Never produced by the FIM adapter.
                SseItem::Comment("tool_calls dropped")
            }
        }
    });
    let done = futures::stream::once(async { SseItem::Data("[DONE]".to_string()) });
    chunks.chain(done).boxed()
}

/// Static editing-slot description for `/status.inference.edit`.
///
/// The one translation across the commonwealth-api seam (that crate
/// deliberately names no `sovereign_*` types — same convention as
/// `ResidentSlot`). Each lane maps to an `Option`: absent means the
/// slot cannot serve that lane, which is a reportable state rather
/// than an error.
pub fn edit_status(
    provider: &std::sync::Arc<dyn crate::traits::InferenceProvider>,
) -> Option<EditSlotStatus> {
    provider.edit_slot_info().map(|info| {
        let advice = edit_slot_advice(&info);
        EditSlotStatus {
            slot: info.slot,
            model_id: info.model_id,
            aliased_to_fast: info.aliased_to_fast,
            degraded: info.degraded,
            next_edit_format: info.next_edit.map(|l| l.format.as_str().to_string()),
            fim_style: info.fim.map(|l| l.style.as_str().to_string()),
            advice,
        }
    })
}

/// The operator-facing next step for this arrangement, or `None` when
/// nothing is worth saying.
///
/// One decider for the nudge (ARCH §10.6): `doctor`, `svrn status`, the
/// desktop and the editor extension all render `/status`, and each
/// composing its own advice string is how three surfaces end up giving
/// three different answers to "what should I do about this".
///
/// Deliberately silent for a fully-specialised slot — advice nobody
/// needs is noise, and a status field that always has content stops
/// being read.
fn edit_slot_advice(info: &crate::types::EditSlotInfo) -> Option<String> {
    match (info.degraded, info.fim.is_some()) {
        // The graceful-degradation case: suggestions work off the
        // resident chat model. Name the trade, not just the state —
        // measured 2026-08-07, a 1.5B specialist matched this quality
        // (19/30 vs 21/30 on the 60-case gen bank) at ~3x the speed.
        (true, _) => Some(
            "Next-edit is being served by the resident chat model because no \
             [models.edit] is configured. Suggestions work. A dedicated edit \
             model (~1.5 GB) returns them roughly 3x faster and adds \
             /v1/completions: set [models.edit].path in ~/.sovereign/config.toml."
                .to_string(),
        ),
        // Operator chose this model, but it cannot do FIM. Worth
        // saying once, because /v1/completions will 503 and the cause
        // is invisible from the route's perspective.
        (false, false) => Some(
            "This editing model serves next-edit but not fill-in-the-middle: its \
             tokenizer carries no FIM markers, so /v1/completions returns 503. \
             Point [models.edit].path at a coder GGUF (Mellum2, Qwen2.5-Coder) \
             if you need inline completion."
                .to_string(),
        ),
        (false, true) => None,
    }
}
