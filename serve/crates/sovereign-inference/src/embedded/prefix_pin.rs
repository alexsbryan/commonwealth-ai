// SPDX-License-Identifier: AGPL-3.0-or-later
//! The request-side half of the pinned-prefix cache. `prefix_state.rs`
//! decides from tokens alone; this file reads the request: which planner a
//! request takes, and the conversation re-pin once its prompt is prefilled.
//! Both decode paths in `model_slot.rs` call these, so they cannot disagree
//! on either.

use std::time::Instant;

use super::prefix_state::{PrefixPlan, PrefixStateCache};
use crate::llama::cpp::context::LlamaContext;
use crate::llama::cpp::model::{AddBos, LlamaModel};
use crate::llama::cpp::token::LlamaToken;
use sovereign_contracts::types::{CompletionRequest, PromptShape};

/// The plan for this request, and the family key to re-pin under once the
/// prompt is prefilled (conversations only — pass it to [`repin`]).
///
/// The planner is picked by what the request declares about itself: a
/// declared `stable_prefix_len` takes `plan_directed`, an external client's
/// conversation takes `plan_conversation`, anything else the sighting-learned
/// `plan`. A conversation on a slot that keeps a partial KV prefix in memory
/// (`partial_keep_ok`, attention-only models) takes no pin at all: partial
/// keep already reuses the whole previous prompt without a state file, and a
/// pin would override it with a restore of something shorter.
pub(super) fn plan_for(
    model: &LlamaModel,
    request: &CompletionRequest,
    full_prompt: &str,
    tokens: &[LlamaToken],
    partial_keep_ok: bool,
    prefix_state: &mut PrefixStateCache,
) -> (PrefixPlan, Option<u64>) {
    if let Some(pin) = directed_pin_tokens(model, request, full_prompt, tokens) {
        return (prefix_state.plan_directed(tokens, pin), None);
    }
    if !matches!(request.prompt_shape, Some(PromptShape::Conversation { .. })) {
        return (prefix_state.plan(tokens), None);
    }
    if partial_keep_ok {
        tracing::debug!(
            target: "prefix_state",
            prompt_tokens = tokens.len(),
            "prefix_state: PASS — conversation on a partial-keep slot; the in-memory prefix serves it"
        );
        return (PrefixPlan::Pass, None);
    }
    prefix_state.plan_conversation(tokens)
}

/// Save the prefilled prompt's state and file it as the family's pin — the
/// re-pin [`plan_for`] asked for. `ctx` must hold exactly `tokens`, and
/// `output_rows` is how many positions the prefill flagged for logits: the
/// state file carries the output buffer (~1 MB per row at this model
/// family's 248k vocabulary), so a prefill that flagged more than its last
/// position is not saved. A save that fails or that the byte budget refuses
/// leaves the previous pin serving (`repin_path` never collides with it).
pub(super) fn repin(
    ctx: &LlamaContext<'_>,
    prefix_state: &mut PrefixStateCache,
    key: u64,
    tokens: &[LlamaToken],
    output_rows: usize,
    model_id: &str,
    path: &str,
) {
    if output_rows > 1 {
        tracing::info!(
            target: "prefix_state",
            model = %model_id,
            key = format_args!("{key:016x}"),
            output_rows,
            "prefix_state: conversation re-pin skipped — the prefill flagged {output_rows} output rows and the state file would carry them ({path} path)"
        );
        return;
    }
    let t0 = Instant::now();
    let file = prefix_state.repin_path(key, tokens);
    let saved = prefix_state
        .ensure_dir()
        .map_err(|e| e.to_string())
        .and_then(|()| {
            ctx.save_session_file(&file, tokens)
                .map_err(|e| e.to_string())
        });
    if let Err(error) = saved {
        // Best-effort: a partial file costs disk until this process's state
        // dir is swept after it exits, never a restore — no entry names it.
        let _ = std::fs::remove_file(&file);
        tracing::warn!(
            target: "prefix_state",
            model = %model_id,
            key = format_args!("{key:016x}"),
            %error,
            "prefix_state: conversation re-pin save failed — the previous pin keeps serving ({path} path)"
        );
        return;
    }
    let save_ms = t0.elapsed().as_millis() as u64;
    let bytes = std::fs::metadata(&file).ok().map(|m| m.len());
    // A refusal is logged by `commit`.
    if prefix_state.commit(key, tokens.to_vec(), file) {
        tracing::info!(
            target: "prefix_state",
            model = %model_id,
            key = format_args!("{key:016x}"),
            pinned_tokens = tokens.len(),
            bytes,
            save_ms,
            "prefix_state: REPINNED — conversation turn pinned whole ({path} path)"
        );
    }
}

/// Map a caller-declared stable-prefix byte length (over the RAW user
/// prompt, `CompletionRequest.stable_prefix_len`) to a conservative
/// token boundary in the rendered prompt's token stream, for the
/// pinned-prefix cache's directed plan. `None` (→ sighting-based plan)
/// when the declaration is absent, malformed, or unlocatable.
///
/// Method: locate the declared prefix substring inside the rendered
/// prompt (chat templates concatenate message content verbatim),
/// tokenize the rendered text UP TO that boundary, and take its LCP
/// with the full token stream, backing off 2 tokens. The back-off
/// matters: BPE merges at the cut can differ from the full stream's
/// (the boundary token may fuse with suffix bytes), and LCP+back-off
/// makes the pin a guaranteed common token prefix of every sibling
/// sharing the declared bytes. Cost: one extra tokenize of the prefix
/// per request. Failures degrade to the undirected plan — full
/// prefill at worst, never wrong output.
fn directed_pin_tokens(
    model: &LlamaModel,
    request: &CompletionRequest,
    full_prompt: &str,
    tokens: &[LlamaToken],
) -> Option<usize> {
    let n = request.stable_prefix_len?;
    // `.get` enforces both the range and the char-boundary contract.
    let raw_prefix = request.prompt.get(..n)?;
    if raw_prefix.is_empty() {
        return None;
    }
    let start = full_prompt.find(raw_prefix)?;
    let rendered_prefix = &full_prompt[..start + raw_prefix.len()];
    let prefix_tokens = model.str_to_token(rendered_prefix, AddBos::Always).ok()?;
    let lcp = prefix_tokens
        .iter()
        .zip(tokens.iter())
        .take_while(|(a, b)| a == b)
        .count();
    // Same margin as the undirected path, from the same constant, so the
    // two planners cannot drift apart on how much tail a restore needs.
    // Kept as a plain subtraction rather than `pin_with_tail` on purpose:
    // this path is measured and working (4,881 tokens restored in 45 ms),
    // and widening its pin would rotate `directed_key` for every existing
    // judge family to buy two tokens.
    let pin = lcp.saturating_sub(super::prefix_state::PIN_TAIL_MARGIN);
    (pin > 0).then_some(pin)
}
