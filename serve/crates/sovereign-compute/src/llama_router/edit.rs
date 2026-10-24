// SPDX-License-Identifier: AGPL-3.0-or-later
//! The router's editing model (svrn-docs ENGINE_SWAP_PLAN.md A3): the slot
//! `[models.edit]` fills, its FIM style asked of the server's own tokenizer,
//! and a raw prompt collected into one response.

use std::pin::Pin;
use std::time::Instant;

use futures::{Stream, StreamExt};
use sovereign_contracts::fim::{fim_style_from, probe_markers};
use sovereign_contracts::setup_config::EditSection;
use sovereign_contracts::types::{
    edit_lanes, CompletionResponse, EditSlotInfo, FimStyle, FinishReason, StreamFrame,
};
use sovereign_contracts::Result;

use super::live::LiveRouter;

/// The FIM style `model`'s vocabulary speaks: each table marker sent to the
/// router's `POST /tokenize`, atomic when it is one token, and the table's
/// one decider ([`fim_style_from`]) reading the answers.
async fn fim_style(
    router: &LiveRouter,
    model: &str,
) -> std::result::Result<Option<FimStyle>, String> {
    let mut atomic = Vec::new();
    for marker in probe_markers() {
        if router.token_count(model, marker).await? == 1 {
            atomic.push(marker);
        }
    }
    let style = fim_style_from(|m| atomic.contains(&m));
    tracing::debug!(target: "llama_router", model, ?atomic, ?style, "edit: FIM style from /tokenize");
    Ok(style)
}

/// The edit slot `[models.edit]` fills on the router, as the embedded engine
/// reports its own: `slot` is `fast` when the editing GGUF is the fast
/// model's (one section serves both), `edit` otherwise. A style the router
/// could not be asked for withholds the FIM lane and says why; next-edit is
/// served either way. Blocking: call it off the async workers, as
/// [`LiveRouter::start`].
pub(super) fn edit_slot(
    router: &LiveRouter,
    model: &str,
    section: &EditSection,
    aliased_to_fast: bool,
) -> EditSlotInfo {
    let style = match tokio::runtime::Handle::try_current() {
        Ok(runtime) => runtime.block_on(fim_style(router, model)),
        Err(_) => Err("no tokio runtime to ask the router from".to_string()),
    }
    .unwrap_or_else(|e| {
        tracing::warn!(target: "llama_router", model, error = %e, "edit: FIM style unknown, the FIM lane is withheld; next-edit still serves");
        None
    });
    let (next_edit, fim) = edit_lanes(style, section.effective_next_edit_format(), Some(section));
    let info = EditSlotInfo {
        slot: if aliased_to_fast { "fast" } else { "edit" }.to_string(),
        model_id: model.to_string(),
        aliased_to_fast,
        degraded: false,
        next_edit,
        fim,
    };
    tracing::info!(
        target: "edit_slot",
        model,
        slot = %info.slot,
        fim = info.fim.as_ref().map_or("none", |f| f.style.as_str()),
        "edit slot on the llama-server router"
    );
    info
}

/// A raw completion's frames as one response; an error frame, or a finish
/// the server reported as an error, is the call's error.
pub(super) async fn collect(
    mut frames: Pin<Box<dyn Stream<Item = StreamFrame> + Send>>,
    model: String,
    started: Instant,
) -> Result<CompletionResponse> {
    let mut text = String::new();
    while let Some(frame) = frames.next().await {
        match frame {
            StreamFrame::Token(t) => text.push_str(&t),
            StreamFrame::Error(e)
            | StreamFrame::Finish {
                reason: FinishReason::Error(e),
                ..
            } => return Err(sovereign_contracts::Error::Inference(e)),
            StreamFrame::Finish { reason, usage } => {
                let (prompt, completion) =
                    usage.map_or((0, None), |u| (u.prompt_tokens, Some(u.completion_tokens)));
                return Ok(CompletionResponse {
                    text,
                    tokens_used: (prompt + completion.unwrap_or(0)) as usize,
                    prompt_tokens: prompt as usize,
                    model_id: model,
                    latency_ms: started.elapsed().as_millis() as u64,
                    oicp_meta: None,
                    finish_reason: Some(reason),
                    completion_tokens: completion,
                    logprobs: None,
                });
            }
        }
    }
    Err(sovereign_contracts::Error::Inference(
        "the raw completion ended without a final frame".into(),
    ))
}
