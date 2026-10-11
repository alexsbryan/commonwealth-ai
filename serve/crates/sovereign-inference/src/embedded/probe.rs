// SPDX-License-Identifier: AGPL-3.0-or-later
//! `InferenceProvider::probe_logprobs` on the embedded engine: the engine
//! conformance battery's compute instrument (`compute.logits` in
//! `bench/lanes/engine-swap/conformance.toml`).
//!
//! The probe decodes on a context of its own, built from the slot's own
//! parameters the way a speculative slot's demotion builds one
//! ([`build_target_ctx_for_slot`]): no draft, no `n_rs_seq`, nothing the
//! serving context's prefix cache holds. A slot with no such parameters (a
//! plain one) lends its own context, cleared before and after. The caller
//! holds the slot's context lock throughout, so the probe never decodes
//! beside a request on the same weights.
//!
//! Each step's distribution is computed as llama-server's
//! `get_token_probabilities` computes it (LS/tools/server/server-common.cpp
//! :1496-1545): in f32, `expf(logit - max)` normalised by the sum, then
//! `log`. The sum here runs in token-id order and llama-server's in the order
//! `std::partial_sort` leaves the array, so the two can differ in the last
//! bits of each logprob.

use sovereign_contracts::engine_observe::{ProbeAnswer, ProbeRequest, ProbeStep};
use sovereign_contracts::error::{Error, Result};

use super::model_slot::{build_target_ctx_for_slot, ModelSlot, SlotContext};
use crate::llama::cpp::context::LlamaContext;
use crate::llama::cpp::llama_batch::LlamaBatch;
use crate::llama::cpp::model::AddBos;
use crate::llama::cpp::token::LlamaToken;

/// Prompt tokens decoded per batch: the chat slots' `n_ubatch`, so a chunk
/// boundary is never also a micro-batch boundary llama.cpp would not make.
const PROMPT_CHUNK: usize = 512;

/// Run the probe on `slot`, whose context lock the caller holds as `held`.
pub(crate) fn probe_slot(
    slot: &ModelSlot,
    held: &mut SlotContext,
    probe: &ProbeRequest,
) -> Result<ProbeAnswer> {
    let prompt = slot
        .model
        .str_to_token(&probe.text, AddBos::Always)
        .map_err(|e| Error::InvalidInput(format!("probe: the prompt does not tokenize: {e}")))?;
    if prompt.is_empty() || probe.steps == 0 || probe.top_k == 0 {
        return Err(Error::InvalidInput(
            "probe: an empty prompt, zero steps or zero top_k measures nothing".into(),
        ));
    }
    let own = match held.mtp_rebuild.as_ref() {
        Some(params) => Some(build_target_ctx_for_slot(
            &held._model,
            params,
            &slot.model_id,
        )?),
        None => None,
    };
    tracing::debug!(
        target: "engine_probe",
        model = %slot.model_id,
        prompt = prompt.len(),
        steps = probe.steps,
        top_k = probe.top_k,
        own_context = own.is_some(),
        "probing"
    );
    match own {
        Some(mut ctx) => decode_steps(slot, &mut ctx, &prompt, probe),
        None => {
            // The serving context's cache no longer matches its KV, either
            // way the probe ends.
            held.cached_tokens.clear();
            let ctx = held.ctx_mut();
            ctx.clear_kv_cache();
            let answer = decode_steps(slot, ctx, &prompt, probe);
            ctx.clear_kv_cache();
            answer
        }
    }
}

fn decode_steps(
    slot: &ModelSlot,
    ctx: &mut LlamaContext<'static>,
    prompt: &[LlamaToken],
    probe: &ProbeRequest,
) -> Result<ProbeAnswer> {
    let n_ctx = ctx.n_ctx() as usize;
    if prompt.len() + probe.steps > n_ctx {
        return Err(Error::ContextExceeded {
            prompt_tokens: (prompt.len() + probe.steps) as u64,
            n_ctx: n_ctx as u64,
        });
    }
    let decode_err = |e| Error::Inference(format!("probe: decode failed: {e}"));
    for (c, chunk) in prompt.chunks(PROMPT_CHUNK).enumerate() {
        let mut batch = LlamaBatch::new(chunk.len(), 1);
        for (i, tok) in chunk.iter().enumerate() {
            let pos = c * PROMPT_CHUNK + i;
            batch
                .add(*tok, pos as i32, &[0], pos + 1 == prompt.len())
                .map_err(|e| Error::Inference(format!("probe: batch: {e}")))?;
        }
        ctx.decode(&mut batch).map_err(decode_err)?;
    }
    let n_vocab = slot.model.n_vocab() as usize;
    let mut steps = Vec::with_capacity(probe.steps);
    let mut pos = prompt.len();
    loop {
        let logits = ctx.get_logits();
        let (token, top) = step_distribution(&logits[..n_vocab.min(logits.len())], probe.top_k);
        steps.push(ProbeStep {
            token: i64::from(token),
            top,
        });
        // A server stops generating at end-of-generation, and suppressing
        // it would change the very distribution measured; so does the probe.
        if steps.len() == probe.steps || slot.model.is_eog_token(LlamaToken(token)) {
            break;
        }
        let mut batch = LlamaBatch::new(1, 1);
        batch
            .add(LlamaToken(token), pos as i32, &[0], true)
            .map_err(|e| Error::Inference(format!("probe: batch: {e}")))?;
        ctx.decode(&mut batch).map_err(decode_err)?;
        pos += 1;
    }
    tracing::debug!(target: "engine_probe", generated = steps.len(), "probe done");
    Ok(ProbeAnswer {
        prompt_ids: prompt.iter().map(|t| i64::from(t.0)).collect(),
        steps,
    })
}

/// The greedy token and the `top_k` most likely tokens with their
/// log-probabilities over all of `logits`, most likely first. A tie goes to
/// the lower token id, as llama.cpp's greedy sampler takes the first maximum.
pub(crate) fn step_distribution(logits: &[f32], top_k: usize) -> (i32, Vec<(i64, f64)>) {
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut sum = 0.0f32;
    for &l in logits {
        sum += (l - max).exp();
    }
    let mut order: Vec<usize> = (0..logits.len()).collect();
    order.sort_by(|&a, &b| logits[b].total_cmp(&logits[a]).then(a.cmp(&b)));
    let top = order
        .iter()
        .take(top_k)
        .map(|&i| {
            let p = (logits[i] - max).exp() / sum;
            (i as i64, f64::from(p.ln()))
        })
        .collect();
    (order.first().map_or(0, |&i| i as i32), top)
}

#[cfg(test)]
mod tests {
    use super::step_distribution;

    #[test]
    fn the_distribution_is_llama_servers_softmax_most_likely_first() {
        let (greedy, top) = step_distribution(&[1.0, 3.0, 2.0, -1.0], 2);
        assert_eq!(greedy, 1);
        assert_eq!(top.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [1, 2]);
        let sum: f32 = [1.0f32, 3.0, 2.0, -1.0]
            .iter()
            .map(|l| (l - 3.0).exp())
            .sum();
        assert_eq!(top[0].1, f64::from((1.0f32 / sum).ln()));
        assert_eq!(top[1].1, f64::from(((-1.0f32).exp() / sum).ln()));
    }

    #[test]
    fn a_tie_goes_to_the_lower_token_id_and_top_k_clamps() {
        let (greedy, top) = step_distribution(&[0.5, 2.0, 2.0], 9);
        assert_eq!(
            greedy, 1,
            "the first maximum, as the greedy sampler takes it"
        );
        assert_eq!(top.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [1, 2, 0]);
    }
}
