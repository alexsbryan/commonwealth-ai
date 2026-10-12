// SPDX-License-Identifier: AGPL-3.0-or-later
//! A forced-choice wave (`oicp_types::forced_choice::wave`): several closed
//! questions over one shared prompt, answered off ONE decoded state.
//!
//! Every branch is rendered and tokenized exactly as the single request
//! `prompt + suffix` with its own forced-choice body would be, so a branch's
//! tokens are that request's tokens by construction. The longest token prefix
//! every branch shares is decoded once and its sequence state held in memory;
//! each branch restores that state, decodes only its own tail, and reads its
//! distribution with the same readout a single forced choice uses
//! (`forced_choice_probs`). Restoring before every branch is what isolates
//! branches: nothing a sibling decoded survives into the next one, on the
//! attention layers or the recurrent ones (partial KV rewind is refused on
//! hybrid models, whole sequence state restore is not; measured exact against
//! independent full prefills by `tests/decide_branch_spike.rs`, 2026-10-11:
//! max |dp| 0.0000 over 24 questions on qwen35).
//!
//! What a wave saves over the same questions sent one by one: the request
//! path, the prompt rendering and tokenization that only its tail needs, and
//! the pinned-prefix file restore (40-75 ms per question on the Ward run of
//! 2026-10-11) — not the tail decode itself, which each branch still pays.

use std::collections::BTreeMap;
use std::time::Instant;

use super::ffi_trace::ClearKvPhase;
use super::model_slot::{forced_choice_probs, inference_deadline_secs, GenerationOutcome};
use super::prompt_helpers::{add_bos_for, format_prompt};
use crate::llama::cpp::context::LlamaContext;
use crate::llama::cpp::llama_batch::LlamaBatch;
use crate::llama::cpp::model::LlamaModel;
use crate::llama::cpp::token::LlamaToken;
use oicp_types::forced_choice::{self, Branch};
use sovereign_contracts::error::Error;
use sovereign_contracts::model_family::ModelQuirks;
use sovereign_contracts::types::{CompletionRequest, FinishReason};
use sovereign_contracts::Result;

/// The single request a branch stands for: the wave's prompt with the
/// branch's text appended, carrying the branch's own forced-choice body.
pub(crate) fn branch_request(request: &CompletionRequest, branch: &Branch) -> CompletionRequest {
    CompletionRequest {
        prompt: format!("{}{}", request.prompt, branch.suffix),
        structured_output: Some(forced_choice::branch_body(branch)),
        ..request.clone()
    }
}

/// How many leading tokens every stream shares, leaving each at least one
/// token of its own to decode (its distribution is read at its last token).
pub(crate) fn shared_prefix(streams: &[&[LlamaToken]]) -> usize {
    let Some(first) = streams.first() else {
        return 0;
    };
    streams
        .iter()
        .map(|s| {
            let common = s
                .iter()
                .zip(first.iter())
                .take_while(|(a, b)| a == b)
                .count();
            common.min(s.len().saturating_sub(1))
        })
        .min()
        .unwrap_or(0)
}

fn decode_span(
    ctx: &mut LlamaContext<'_>,
    tokens: &[LlamaToken],
    from: usize,
    logits_at_end: bool,
) -> Result<()> {
    let mut batch = LlamaBatch::new(tokens.len().max(1), 1);
    for (i, &tok) in tokens.iter().enumerate() {
        batch
            .add(
                tok,
                (from + i) as i32,
                &[0],
                logits_at_end && i + 1 == tokens.len(),
            )
            .map_err(|e| Error::Inference(format!("decide_wave batch add: {e}")))?;
    }
    ctx.decode(&mut batch)
        .map_err(|e| Error::Inference(format!("decide_wave decode: {e}")))
}

/// Answer a wave. Each branch's answer is its distribution or why it was
/// refused; one branch's refusal never takes another's answer with it. The
/// KV is left holding no request the prefix-cache bookkeeping describes, so
/// `cached_tokens` is emptied and the next request plans from nothing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_wave(
    model: &LlamaModel,
    model_id: &str,
    ctx: &mut LlamaContext<'_>,
    cached_tokens: &mut Vec<LlamaToken>,
    request: &CompletionRequest,
    branches: &[Branch],
    quirks: &ModelQuirks,
) -> Result<GenerationOutcome> {
    let started = Instant::now();
    let deadline = std::time::Duration::from_secs(inference_deadline_secs());
    let admit = (ctx.n_ctx() as usize).min(ctx.n_batch() as usize);
    cached_tokens.clear();

    // Render every branch as its single request; a branch that cannot be
    // rendered or does not fit is refused alone.
    let mut answers: Vec<std::result::Result<BTreeMap<String, f32>, String>> =
        Vec::with_capacity(branches.len());
    let mut streams: Vec<Option<Vec<LlamaToken>>> = Vec::with_capacity(branches.len());
    for branch in branches {
        let single = branch_request(request, branch);
        let rendered = format_prompt(model, model_id, &single, quirks).and_then(|p| {
            let (text, _) = p.into_parts();
            model
                .str_to_token(&text, add_bos_for(&single))
                .map_err(|e| Error::Inference(format!("tokenization failed: {e}")))
        });
        match rendered {
            Ok(t) if t.is_empty() => {
                streams.push(None);
                answers.push(Err("the branch renders to no tokens".to_string()));
            }
            Ok(t) if t.len() > admit => {
                answers.push(Err(format!(
                    "prompt too long: {} tokens against a window of {admit}",
                    t.len()
                )));
                streams.push(None);
            }
            Ok(t) => {
                streams.push(Some(t));
                answers.push(Ok(BTreeMap::new()));
            }
            Err(e) => {
                streams.push(None);
                answers.push(Err(e.to_string()));
            }
        }
    }
    let live: Vec<&[LlamaToken]> = streams.iter().flatten().map(Vec::as_slice).collect();
    let lcp = shared_prefix(&live);

    let t_prefix = Instant::now();
    let snapshot = if live.is_empty() {
        Vec::new()
    } else {
        ctx.clear_kv_cache(); // kv-phase: DecisionWave
        if lcp > 0 {
            decode_span(ctx, &live[0][..lcp], 0, false)?;
        }
        let mut state = vec![0u8; ctx.state_seq_get_size(0)];
        let n = ctx.state_seq_get_data(&mut state, 0);
        state.truncate(n);
        state
    };
    let prefix_ms = t_prefix.elapsed().as_millis() as u64;

    let mut restore_ms = 0u64;
    let mut tail_tokens = 0usize;
    for (i, (branch, stream)) in branches.iter().zip(&streams).enumerate() {
        let Some(tokens) = stream else {
            continue;
        };
        if started.elapsed() > deadline {
            answers[i] = Err(format!(
                "deadline: the wave ran past {}s before this branch",
                deadline.as_secs()
            ));
            continue;
        }
        let t_restore = Instant::now();
        ctx.clear_kv_cache(); // kv-phase: DecisionWave
        if lcp > 0 && ctx.state_seq_set_data(&snapshot, 0) == 0 {
            tracing::warn!(model = %model_id, branch = i, "decide_wave: the shared state did not restore; branch refused");
            answers[i] = Err("the shared state did not restore".to_string());
            continue;
        }
        restore_ms += t_restore.elapsed().as_millis() as u64;
        let tail = &tokens[lcp..];
        tail_tokens += tail.len();
        if let Err(e) = decode_span(ctx, tail, lcp, true) {
            answers[i] = Err(e.to_string());
            continue;
        }
        answers[i] = forced_choice_probs(model, ctx, &branch.labels)
            .map(|p| p.into_iter().collect())
            .map_err(|e| e.to_string());
        tracing::trace!(model = %model_id, branch = i, tail = tail.len(), answer = ?answers[i], "decide_wave: branch");
    }
    let refused = answers.iter().filter(|a| a.is_err()).count();
    tracing::info!(
        model = %model_id,
        branches = branches.len(),
        refused,
        shared_tokens = lcp,
        tail_tokens,
        prefix_ms,
        restore_ms,
        total_ms = started.elapsed().as_millis() as u64,
        "decide_wave: answered"
    );
    Ok(GenerationOutcome {
        text: forced_choice::wave_answer(&answers),
        prompt_tokens: lcp + tail_tokens,
        completion_tokens: 0,
        finish_reason: FinishReason::Stop,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(ids: &[i32]) -> Vec<LlamaToken> {
        ids.iter().map(|&i| LlamaToken(i)).collect()
    }

    #[test]
    fn the_shared_prefix_leaves_every_branch_a_token_of_its_own() {
        let a = toks(&[1, 2, 3, 4]);
        let b = toks(&[1, 2, 3, 5, 6]);
        assert_eq!(shared_prefix(&[&a, &b]), 3);
        // Identical streams still decode their last token themselves.
        assert_eq!(shared_prefix(&[&a, &a]), 3);
        let c = toks(&[9]);
        assert_eq!(shared_prefix(&[&a, &c]), 0);
        assert_eq!(shared_prefix(&[]), 0);
    }

    #[test]
    fn a_branch_is_the_single_request_its_suffix_makes() {
        let wave = CompletionRequest {
            prompt: "doc\n".to_string(),
            system_message: Some("sys".to_string()),
            temperature: Some(0.0),
            structured_output: Some(forced_choice::wave(&[])),
            ..Default::default()
        };
        let b = Branch {
            suffix: "Line 2?".to_string(),
            labels: vec!["A".to_string(), "0".to_string()],
        };
        let single = branch_request(&wave, &b);
        assert_eq!(single.prompt, "doc\nLine 2?");
        assert_eq!(single.system_message.as_deref(), Some("sys"));
        assert_eq!(single.temperature, Some(0.0));
        assert_eq!(
            single.forced_choice_candidates(),
            Some(vec!["A".to_string(), "0".to_string()])
        );
        assert_eq!(single.forced_choice_wave(), None);
    }
}
