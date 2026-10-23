// SPDX-License-Identifier: AGPL-3.0-or-later
//! The OpenAI-wire fields a client sends for a hosted vendor: its thinking
//! switch and the token logprobs it reports.
//!
//! A client that talks to DeepSeek through this daemon sends what it would
//! send DeepSeek: `thinking: {type: disabled}`, `logprobs`, `top_logprobs`.
//! Until 2026-10-20 the wire type had none of the three, so they were dropped
//! where the body was parsed: the vendor reasoned (its default), spent a
//! one-token budget on it, and answered no logprobs, where the same body sent
//! straight to it answered a letter with its top logprobs.
//!
//! Thinking off is honoured for a third-party engine only, and goes out in
//! every spelling `oicp_client::chat_wire::write_thinking` knows, by way of
//! the zero budget it writes them from. A local engine keeps reading only
//! `chat_template_kwargs` and `think_budget`, as before: llama-server ignores
//! `thinking`, and so does an engine here.

use oicp_types::openai_types::ChatCompletionRequest;
use sovereign_contracts::traits::ServingLocus;
use sovereign_contracts::types::{CompletionRequest, CompletionResponse};

/// Whether the client switched thinking off, in any spelling a client sends:
/// DeepSeek's `thinking: {type: disabled}`, the template kwarg
/// `enable_thinking: false`, or this daemon's `think_budget: 0`.
fn thinking_off(request: &ChatCompletionRequest) -> bool {
    let vendor = request
        .thinking
        .as_ref()
        .and_then(|t| t.get("type"))
        .and_then(|t| t.as_str())
        == Some("disabled");
    let kwarg =
        super::extract_enable_thinking(request.chat_template_kwargs.as_ref()) == Some(false);
    vendor || kwarg || request.think_budget == Some(0)
}

/// For a third-party engine, a client's thinking off becomes a zero budget,
/// which the client writes in every spelling a host reads. A budget the
/// client named stays (`resolve_think_budget`: the caller's value wins).
pub(super) fn thinking_for_a_vendor(
    req: &mut CompletionRequest,
    request: &ChatCompletionRequest,
    locus: ServingLocus,
) {
    if locus != ServingLocus::ForwardsToThirdParty {
        if request.thinking.is_some() {
            tracing::debug!(
                ?locus,
                thinking = ?request.thinking,
                "inference_adapter: a vendor's `thinking` field ignored, the engine is not a third party"
            );
        }
        return;
    }
    let off = thinking_off(request);
    if off && req.think_budget.is_none() {
        req.think_budget = Some(0);
    }
    tracing::debug!(
        thinking = ?request.thinking,
        thinking_off = off,
        think_budget = ?req.think_budget,
        "inference_adapter: thinking for a third-party engine"
    );
}

/// The logprobs the client asked for, as `CompletionRequest::logprobs`:
/// `Some(top_logprobs)` when `logprobs` is true, `None` otherwise.
pub(super) fn logprobs_asked(request: &ChatCompletionRequest) -> Option<u32> {
    (request.logprobs == Some(true)).then(|| request.top_logprobs.unwrap_or(0))
}

/// The engine's logprobs for the reply. An engine that reports none answers
/// without them, traced, so the absence is never read as an empty list.
pub(super) fn logprobs_answered(
    req: &CompletionRequest,
    resp: &CompletionResponse,
) -> Option<serde_json::Value> {
    if req.logprobs.is_some() && resp.logprobs.is_none() {
        tracing::debug!(
            model = %resp.model_id,
            top_logprobs = ?req.logprobs,
            "inference_adapter: logprobs asked, the engine reported none"
        );
    }
    resp.logprobs.clone()
}
