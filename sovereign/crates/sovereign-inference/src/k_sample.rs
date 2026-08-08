// SPDX-License-Identifier: AGPL-3.0-or-later
//! k-sample short-form value decoder — H2's draw (`NATIVE_GROUNDING.md` §5 H2,
//! step 1).
//!
//! Given one `(question, sealed evidence)` turn, draw **k independent
//! short-form answer VALUES** — not prose — in one lockstep multi-sequence
//! decode, paying the evidence prefill exactly once. The k values are the input
//! to the meaning clusterer, whose entropy over meaning-clusters is the
//! statistic H2 is about.
//!
//! **This module has zero callers in the runtime, by construction**, on the
//! same terms as `runtime::grounding::native_grounding`: Phase 1 is measurement
//! only, the incumbent grounding stack is untouched, and nothing here is wired
//! behind a request path.
//!
//! ## Why this is not `ModelSlot::generate_sync_batched`
//!
//! That function is the right shape for the FastShort coalescer, whose job is
//! **N different prompts** arriving from N unrelated callers: it packs every
//! prompt into one prefill batch and pays N prefills' worth of tokens. This
//! draw is the opposite shape — **one prompt, N times** — so the entire prompt
//! is the shared prefix. Decoding it once on seq 0 and fanning the KV out with
//! `copy_kv_cache_seq` turns `k · n_prompt` prefill tokens into `n_prompt`,
//! which for a sealed-evidence prompt (the long part) is the whole cost.
//!
//! The fanout mechanics — a **unified** KV cache so a partial
//! `copy_kv_cache_seq` is a cell-tag update rather than an aborting
//! cross-stream copy — are lifted verbatim from `RerankSlot::score_batch`
//! (`embedded/rerank_slot.rs:530`), which already does exactly this for
//! query-prefix fanout. That is where the `with_kv_unified(true)` below comes
//! from, and it is load-bearing: llama.cpp's default per-sequence streams make
//! `seq_cp()` abort with *"seq_cp() is only supported for full KV buffers"*.
//!
//! ## The seeds are the point
//!
//! Before this order nothing on the multi-sequence path was reproducible:
//! `sampler.rs`'s `rand_seed()` read `SystemTime::now().subsec_nanos()`, so k
//! sequences built microseconds apart could draw the same seed — and two runs
//! of the same measurement never drew the same seed set at all. [`seeds_for`]
//! replaces that with a pinned, distinct, reproducible seed per sequence, and
//! the seed rides `CompletionRequest::seed` so `build_sampler` stays the one
//! implementation of the sampling chain (ARCH §10.6).
//!
//! Both failure directions are real and both are pinned by test:
//! - identical seeds ⇒ k identical values ⇒ one cluster ⇒
//!   `semantic_entropy = 0` read as unanimity from a model asked once;
//! - unpinned seeds ⇒ a statistic that cannot be reproduced, i.e. not an
//!   instrument (ARCH §18.4).

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::llama::cpp::context::params::LlamaContextParams;
use crate::llama::cpp::llama_backend::LlamaBackend;
use crate::llama::cpp::llama_batch::LlamaBatch;
use crate::llama::cpp::model::params::LlamaModelParams;
use crate::llama::cpp::model::{AddBos, LlamaModel};
use crate::llama::cpp::token::LlamaToken;
use crate::llama::LlamaModelExt;

use sovereign_core::error::{Error, Result};
use sovereign_core::model_family::{ModelFamily, ModelQuirks};
use sovereign_core::types::CompletionRequest;

use crate::embedded::{build_sampler, format_prompt, llama_threads_for_host, SamplerRole};

/// Per-sample output budget. **24 tokens, and the number is inherited, not
/// chosen**: it is exactly `extract_answer_value`'s budget
/// (`sovereign-core/src/runtime/grounding/value_presence.rs:89`, `max_tokens:
/// Some(24)`), which is the incumbent's own answer-value extractor. §5 H2 names
/// that budget verbatim ("≤24 tokens each — the same budget
/// `extract_answer_value` uses today"), so the draw and the incumbent measure
/// the same unit and a value-length difference can never explain a gate result.
pub const VALUE_TOKEN_BUDGET: u32 = 24;

/// Sampling temperature for the draw. §5 H2 says "temp ~0.7". At temperature 0
/// the chain is greedy, `dist` is never built, and k samples are k identical
/// strings by construction — the draw would measure nothing.
pub const DRAW_TEMPERATURE: f32 = 0.7;

/// Derive `k` distinct, reproducible sampling seeds from one base.
///
/// **The one implementation of H2's seed derivation** (ARCH §10.6): the decoder
/// calls it, the tests call it, and a replay reproduces a run by passing the
/// same base. Deterministic and pure — no clock, no RNG state, no model.
///
/// The step is an odd 32-bit constant (the golden-ratio conjugate scaled to
/// 2^32, `0x9E37_79B9`), applied with wrapping arithmetic. Two properties
/// matter and both are pinned by test:
///
/// 1. **Distinctness.** An odd multiplier's additive orbit visits `2^32`
///    distinct values before repeating, so for any `k ≤ MAX_SAMPLES` the seeds
///    are pairwise distinct regardless of `base` — including `base = 0` and
///    `base = u32::MAX`. Equal seeds would collapse two sequences onto one
///    sample and inflate agreement.
/// 2. **Reproducibility.** Same base, same seeds, forever.
///
/// It is not a cryptographic PRNG and does not need to be: the requirement is
/// distinct-and-repeatable, not unpredictable.
pub fn seeds_for(base: u32, k: u8) -> Vec<u32> {
    const STEP: u32 = 0x9E37_79B9;
    (0..k as u32)
        .map(|i| base.wrapping_add(STEP.wrapping_mul(i)))
        .collect()
}

/// The prompt the draw poses, as the model sees it (before the chat template).
///
/// Deliberately the **short-form value** question, not "answer this". §5 H2's
/// unit is the asserted value, so the k samples must vary in *what the answer
/// is*, not in how it is worded — surface-form variation over the same value is
/// noise the clusterer then has to spend a reranker pass removing.
///
/// The `NONE` escape hatch is inherited from `extract_answer_value` for the
/// same reason the token budget is: a turn whose evidence does not carry the
/// value must be able to say so, and "no value" is itself a meaning that
/// clusters. Dropping it would force every sample to invent something and turn
/// the honest-abstention case into manufactured divergence.
pub fn build_value_prompt(question: &str, evidence: &[String]) -> String {
    let mut ev = String::new();
    for (i, chunk) in evidence.iter().enumerate() {
        ev.push_str(&format!("[{}] {}\n\n", i + 1, chunk.trim()));
    }
    format!(
        "EVIDENCE:\n{ev}\nQUESTION: {q}\n\n\
         Reply with only the specific value the EVIDENCE gives in reply to the \
         QUESTION — the complete value with its qualifiers (e.g. a full name, or a \
         place with its modifiers), but not a surrounding sentence. If the EVIDENCE \
         does not state it, reply with exactly NONE.",
        q = question.trim(),
    )
}

/// The system message for the draw. Matches `extract_answer_value`'s.
pub const VALUE_SYSTEM_MESSAGE: &str = "Extract only the specific value, or NONE.";

/// Normalise one raw sample into the value it asserts, or `None` for "this
/// sample asserts no value".
///
/// **A port, and it is declared as one.** The logic is
/// `extract_answer_value`'s post-processing
/// (`value_presence.rs:130-146`), which is a private `async fn` behind an
/// `InferenceProvider` and so cannot be called from here. The repo's precedent
/// for exactly this situation is `flywheel/det_checks.rs::value_present`,
/// which ports `value_present_in_chunks` and says so in its doc comment with
/// parity tests below it. Same contract here: the parity tests at the bottom of
/// this module pin every behaviour the incumbent's version has, so a drift
/// between the two shows up as a failing test rather than as a gate result
/// nobody can explain.
///
/// `None` is a *meaning*, not a failure — the clusterer treats all `None`
/// samples as one cluster ("no value"), which is the honest reading of k
/// samples that all declined.
pub fn clean_value(raw: &str) -> Option<String> {
    let v = raw.trim().trim_matches('"').trim();
    let low = v.to_lowercase();
    if v.is_empty()
        || low == "none"
        || low.starts_with("none")
        || low.starts_with("n/a")
        || low.starts_with("not ")
        || low.starts_with("unknown")
    {
        None
    } else {
        Some(v.to_string())
    }
}

/// One k-sample draw over one turn.
#[derive(Debug, Clone, PartialEq)]
pub struct KSampleDraw {
    /// The k cleaned values, in sequence order. `None` = this sample asserted
    /// no value (see [`clean_value`]).
    pub values: Vec<Option<String>>,
    /// The k raw decoded strings, before cleaning — kept because a draw whose
    /// values all cleaned to `None` looks identical to a draw that produced
    /// nothing, and a glassbox artifact must be able to tell those apart.
    pub raw: Vec<String>,
    /// The seeds actually used, in sequence order. Replaying with these
    /// reproduces the draw.
    pub seeds: Vec<u32>,
    /// Prompt tokens — paid ONCE for the whole draw, not once per sample.
    pub prompt_tokens: usize,
    /// Decoded tokens summed across the k sequences.
    pub decoded_tokens: usize,
    /// Wall-clock for the draw, prefill included.
    pub elapsed_ms: u128,
}

impl KSampleDraw {
    /// Are all k raw samples byte-identical?
    ///
    /// This is the order's named sampler stop condition: *"if multi-seq
    /// sampling cannot produce non-degenerate value diversity (all k samples
    /// byte-identical at temp 0.7 across turns — that is a sampler finding,
    /// report it)"*. Reported as a property of the draw so the smoke run does
    /// not have to re-derive it, and so a degenerate draw can never be read as
    /// a confident unanimous one without the artifact saying which it was.
    ///
    /// Note this is `raw`, not `values`: two samples that both cleaned to
    /// `None` from *different* prose are not a degenerate draw, they are a draw
    /// that twice declined.
    pub fn all_identical(&self) -> bool {
        self.raw
            .first()
            .is_some_and(|first| self.raw.iter().all(|r| r == first))
    }

    /// How many distinct raw strings the draw produced. `1` with `k > 1` is the
    /// degenerate case; `k` is maximal surface divergence (which is NOT the
    /// same as maximal meaning divergence — that is the clusterer's job).
    pub fn distinct_raw(&self) -> usize {
        let mut seen: Vec<&String> = Vec::new();
        for r in &self.raw {
            if !seen.contains(&r) {
                seen.push(r);
            }
        }
        seen.len()
    }
}

/// A model loaded solely to draw k short-form values.
///
/// Stands up its own backend + model + multi-sequence unified-KV context, in
/// the shape of [`crate::reranker_standalone::StandaloneReranker`]: no chat
/// slot, no embed slot, no engine, no daemon. The measurement harness owns it
/// for the length of a run and drops it.
pub struct KSampleDecoder {
    // Held so the backend outlives the context.
    _backend: Arc<LlamaBackend>,
    model: Arc<LlamaModel>,
    // `'static` is a lie the same way `ModelSlot`'s is: the context borrows
    // `model`, which this struct owns and drops after it. Guarded by the same
    // field-order + Arc discipline the sibling slots use.
    ctx: Mutex<crate::llama::cpp::context::LlamaContext<'static>>,
    model_id: String,
    quirks: ModelQuirks,
    n_ctx: u32,
    /// Sequence cap the context was built with — the hard ceiling on `k`.
    n_seq_max: u32,
}

// Same rationale as `SlotContext`: the raw context pointer is not `Send`, and
// the `Mutex` is what makes single-threaded access true rather than assumed.
unsafe impl Send for KSampleDecoder {}
unsafe impl Sync for KSampleDecoder {}

impl KSampleDecoder {
    /// Load a generator GGUF and stand up a `k`-sequence unified-KV context.
    ///
    /// `n_seq_max` is the hard ceiling on `k` for every later [`Self::draw`]
    /// call; a draw asking for more REFUSES by name rather than silently
    /// drawing fewer (ARCH §18.3 — a short draw would make the entropy
    /// statistic wrong in a way no consumer could detect).
    pub fn load(
        model_path: &Path,
        family: ModelFamily,
        n_ctx: u32,
        n_seq_max: u32,
        gpu_layers: Option<u32>,
    ) -> Result<Self> {
        if n_seq_max == 0 {
            return Err(Error::Inference(
                "k-sample decoder needs n_seq_max >= 1 — a zero-sequence context can draw nothing"
                    .into(),
            ));
        }
        let backend = crate::llama::shared_backend().map_err(Error::Inference)?;

        // Same GPU-first / CPU-fallback policy as the sibling slots.
        let gpu_default_available =
            cfg!(all(target_os = "macos", target_arch = "aarch64")) || cfg!(target_os = "linux");
        let requested_gpu_layers = match gpu_layers {
            Some(n) => n,
            None if gpu_default_available => 999,
            None => 0,
        };
        let model_params = LlamaModelParams::default().with_n_gpu_layers(requested_gpu_layers);
        let model = LlamaModel::load_from_file(&backend, model_path, &model_params).map_err(|e| {
            Error::Inference(crate::llama::describe_model_load_failure(
                "k-sample generator",
                model_path,
                e,
            ))
        })?;
        let model = Arc::new(model);

        let n_threads = llama_threads_for_host();
        let wants_gpu =
            cfg!(any(target_os = "macos", target_os = "linux")) && requested_gpu_layers > 0;
        let build_params = |gpu: bool| {
            LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(n_ctx))
                .with_n_batch(n_ctx)
                .with_n_seq_max(n_seq_max)
                // LOAD-BEARING, not a tuning knob. A unified cache is one
                // stream shared across sequences, and only in that mode is the
                // partial `copy_kv_cache_seq(0..prefix_len)` this draw depends
                // on a cheap cell-tag update. With llama.cpp's default
                // per-sequence streams the same call is a cross-stream copy
                // that ABORTS the process ("seq_cp() is only supported for full
                // KV buffers"). Verbatim the reason `RerankSlot` sets it
                // (`rerank_slot.rs:230-237`).
                .with_kv_unified(true)
                .with_n_ubatch(n_ctx.min(2048))
                .with_n_threads(n_threads as i32)
                .with_n_threads_batch(n_threads as i32)
                .with_offload_kqv(gpu)
        };

        let (ctx, used_gpu) = match if wants_gpu {
            unsafe {
                let model_ref: &'static LlamaModel = &*(Arc::as_ptr(&model));
                model_ref
                    .new_context(&backend, build_params(true))
                    .map(|c| (c, true))
            }
        } else {
            Err(crate::llama::cpp::LlamaContextLoadError::NullReturn)
        } {
            Ok(pair) => pair,
            Err(e) => {
                if wants_gpu {
                    tracing::warn!(error = ?e, "k-sample GPU context failed, falling back to CPU");
                }
                let ctx = unsafe {
                    let model_ref: &'static LlamaModel = &*(Arc::as_ptr(&model));
                    model_ref
                        .new_context(&backend, build_params(false))
                        .map_err(|e| {
                            Error::Inference(format!("k-sample context creation failed: {e}"))
                        })?
                };
                (ctx, false)
            }
        };

        let model_id = model_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("k-sample")
            .to_string();

        tracing::info!(
            model_id = %model_id,
            n_ctx,
            n_seq_max,
            used_gpu,
            "k-sample decoder ready — offline H2 draw, zero runtime callers"
        );

        Ok(Self {
            _backend: backend,
            model: Arc::clone(&model),
            ctx: Mutex::new(ctx),
            model_id,
            quirks: family.default_quirks(),
            n_ctx,
            n_seq_max,
        })
    }

    /// Draw `k` short-form values for one `(question, evidence)` turn.
    ///
    /// One prefill, `k` lockstep decode streams, `≤ VALUE_TOKEN_BUDGET` tokens
    /// each. Deterministic given `seed_base`: the same base over the same turn
    /// on the same weights reproduces the draw.
    pub fn draw(
        &self,
        question: &str,
        evidence: &[String],
        k: u8,
        seed_base: u32,
    ) -> Result<KSampleDraw> {
        if k == 0 {
            return Err(Error::Inference(
                "k=0 draws nothing — a zero-sample entropy is not a measurement".into(),
            ));
        }
        if k as u32 > self.n_seq_max {
            return Err(Error::Inference(format!(
                "k={k} exceeds this decoder's n_seq_max={} — refusing rather than drawing \
                 {} samples and reporting an entropy computed over fewer draws than asked for",
                self.n_seq_max, self.n_seq_max
            )));
        }
        let started = Instant::now();
        let seeds = seeds_for(seed_base, k);

        // The k requests differ ONLY in seed. Identical prompt ⇒ identical
        // tokenization ⇒ the whole prompt is the shared prefix.
        let prompt = build_value_prompt(question, evidence);
        let base_request = CompletionRequest {
            prompt,
            system_message: Some(VALUE_SYSTEM_MESSAGE.to_string()),
            max_tokens: Some(VALUE_TOKEN_BUDGET as usize),
            temperature: Some(DRAW_TEMPERATURE),
            think_budget: Some(0),
            enable_thinking: Some(false),
            ..Default::default()
        };
        let requests: Vec<CompletionRequest> = seeds
            .iter()
            .map(|&s| base_request.clone().with_seed(s))
            .collect();

        let formatted = format_prompt(&self.model, &self.model_id, &requests[0], &self.quirks)?;
        let tokens = self
            .model
            .str_to_token(&formatted, AddBos::Always)
            .map_err(|e| Error::Inference(format!("k-sample tokenization failed: {e}")))?;
        if tokens.is_empty() {
            return Err(Error::Inference(
                "k-sample prompt tokenized to zero tokens — refusing to decode an empty prefix"
                    .into(),
            ));
        }
        let shared_len = tokens.len();
        let budget = VALUE_TOKEN_BUDGET as usize;
        // Unified KV: the prefix is stored once, and each sequence's tail costs
        // its own cells. Refuse up front rather than overflowing mid-decode.
        let needed = shared_len + budget * k as usize;
        if needed > self.n_ctx as usize {
            return Err(Error::Inference(format!(
                "k-sample draw needs {needed} KV tokens (prefix {shared_len} + {k}x{budget}) \
                 but the context is {} — refusing rather than truncating the evidence a \
                 measurement is about",
                self.n_ctx
            )));
        }

        let mut ctx = self
            .ctx
            .lock()
            .map_err(|e| Error::Inference(format!("k-sample lock poisoned: {e}")))?;
        ctx.clear_kv_cache();

        // ── One prefill, on seq 0 ───────────────────────────────────
        let mut batch = LlamaBatch::new(shared_len, 1);
        for (i, &tok) in tokens.iter().enumerate() {
            batch
                .add(tok, i as i32, &[0], i + 1 == shared_len)
                .map_err(|e| Error::Inference(format!("k-sample prefill add: {e}")))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| Error::Inference(format!("k-sample prefill decode: {e}")))?;

        // ── Fan the prefix out to seqs 1..k (cell-tag update) ───────
        for s in 1..k as i32 {
            ctx.copy_kv_cache_seq(0, s, Some(0), Some(shared_len as u32))
                .map_err(|e| Error::Inference(format!("k-sample prefix fanout: {e}")))?;
        }

        // ── Per-sequence decode state ───────────────────────────────
        let mut samplers: Vec<_> = requests
            .iter()
            .map(|r| build_sampler(&self.model, r, &self.quirks))
            .collect();
        let mut decoders: Vec<encoding_rs::Decoder> =
            (0..k as usize).map(|_| encoding_rs::UTF_8.new_decoder()).collect();
        let mut outputs: Vec<String> = vec![String::new(); k as usize];
        let mut n_generated: Vec<usize> = vec![0; k as usize];
        let mut active = vec![true; k as usize];
        let mut next_pos: Vec<i32> = vec![shared_len as i32; k as usize];
        // Step 0: every sequence reads the SAME logit row — the prefill's last
        // position, which they all share. Different seeds on identical logits
        // is precisely the mechanism that makes the draw a draw.
        let prefill_last = (shared_len - 1) as i32;
        let mut logit_idx: Vec<i32> = vec![prefill_last; k as usize];

        let model: &LlamaModel = &self.model;
        loop {
            let mut next_tokens: Vec<Option<LlamaToken>> = vec![None; k as usize];
            for seq in 0..k as usize {
                if !active[seq] {
                    continue;
                }
                let token = samplers[seq].sample(&ctx, logit_idx[seq], SamplerRole::Explore);
                samplers[seq].accept(token);

                if model.is_eog_token(token) {
                    active[seq] = false;
                    continue;
                }
                if let Ok(piece) = model.token_to_piece(token, &mut decoders[seq], true, None) {
                    outputs[seq].push_str(&piece);
                }
                n_generated[seq] += 1;
                if n_generated[seq] >= budget {
                    active[seq] = false;
                    continue;
                }
                next_tokens[seq] = Some(token);
            }

            if !active.iter().any(|&a| a) {
                break;
            }

            batch.clear();
            let mut new_logit_idx = vec![-1i32; k as usize];
            let mut bi: i32 = 0;
            for seq in 0..k as usize {
                if !active[seq] {
                    continue;
                }
                let Some(tok) = next_tokens[seq] else {
                    continue;
                };
                batch
                    .add(tok, next_pos[seq], &[seq as i32], true)
                    .map_err(|e| Error::Inference(format!("k-sample decode add: {e}")))?;
                new_logit_idx[seq] = bi;
                next_pos[seq] += 1;
                bi += 1;
            }
            logit_idx = new_logit_idx;

            ctx.decode(&mut batch)
                .map_err(|e| Error::Inference(format!("k-sample decode step: {e}")))?;
        }

        ctx.clear_kv_cache();
        drop(ctx);

        let decoded_tokens: usize = n_generated.iter().sum();
        let values: Vec<Option<String>> = outputs.iter().map(|o| clean_value(o)).collect();
        let draw = KSampleDraw {
            values,
            raw: outputs,
            seeds,
            prompt_tokens: shared_len,
            decoded_tokens,
            elapsed_ms: started.elapsed().as_millis(),
        };
        tracing::debug!(
            k,
            seed_base,
            prompt_tokens = draw.prompt_tokens,
            decoded_tokens = draw.decoded_tokens,
            distinct_raw = draw.distinct_raw(),
            all_identical = draw.all_identical(),
            elapsed_ms = draw.elapsed_ms,
            "k-sample draw complete"
        );
        Ok(draw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── The reproducibility pin (watched to fail, both directions) ──

    #[test]
    fn the_same_base_yields_the_same_seeds_forever() {
        // Reproducibility half of the pin. If this ever moves, every
        // committed H2 artifact stops being replayable.
        assert_eq!(seeds_for(1592590337, 5), seeds_for(1592590337, 5));
        assert_eq!(
            seeds_for(0, 5),
            vec![0, 0x9E37_79B9, 0x3C6E_F372, 0xDAA6_6D2B, 0x78DD_E6E4],
            "the derivation is frozen — a change here silently invalidates every \
             committed draw"
        );
    }

    #[test]
    fn seeds_are_pairwise_distinct_at_every_base_we_can_reach() {
        // Distinctness half. Equal seeds on two sequences means two
        // sequences draw the SAME token stream from the same logits, which
        // the entropy statistic reads as agreement — a confidently wrong
        // number indistinguishable from a real one.
        for base in [0u32, 1, 7, u32::MAX, u32::MAX / 2, 1592590337] {
            let s = seeds_for(base, sovereign_core::types::MAX_SAMPLES);
            for i in 0..s.len() {
                for j in (i + 1)..s.len() {
                    assert_ne!(
                        s[i], s[j],
                        "seeds {i} and {j} collide at base {base} — two sequences would \
                         draw identically and inflate agreement"
                    );
                }
            }
        }
    }

    #[test]
    fn a_naive_derivation_that_would_collide_is_rejected_by_this_pin() {
        // The gate has to be watched to fail (ARCH §18.1). This is the
        // failing input: the derivation we did NOT use — `base + i % 2`,
        // standing in for any scheme with a short orbit — collides at k=5,
        // and the distinctness assertion above is what catches it.
        let naive: Vec<u32> = (0..5u32).map(|i| 1000 + (i % 2)).collect();
        let mut collided = false;
        for i in 0..naive.len() {
            for j in (i + 1)..naive.len() {
                if naive[i] == naive[j] {
                    collided = true;
                }
            }
        }
        assert!(
            collided,
            "the negative control must actually collide, or the distinctness \
             test above proves nothing"
        );
        // And the real derivation does not.
        let real = seeds_for(1000, 5);
        assert_eq!(
            real.iter().collect::<std::collections::BTreeSet<_>>().len(),
            5
        );
    }

    #[test]
    fn seeds_for_zero_k_is_empty_not_one() {
        assert!(seeds_for(42, 0).is_empty());
    }

    // ── Value cleaning: parity with `extract_answer_value` ──────────
    //
    // Every arm of value_presence.rs:130-146, pinned here so a drift
    // between the port and the incumbent fails a test rather than a gate.

    #[test]
    fn clean_value_strips_quotes_and_whitespace() {
        assert_eq!(clean_value("  \"Karl Yundt\" \n"), Some("Karl Yundt".into()));
    }

    #[test]
    fn clean_value_reads_a_declined_sample_as_no_value_not_as_a_value() {
        for decline in [
            "NONE", "none", "None.", "n/a", "N/A — not stated", "not stated",
            "unknown", "  ", "",
        ] {
            assert_eq!(
                clean_value(decline),
                None,
                "{decline:?} asserts no value and must not enter the clusterer as one"
            );
        }
    }

    #[test]
    fn clean_value_keeps_a_real_value_that_merely_starts_with_an_n() {
        // Guard against the obvious over-eager prefix match: "Nottingham"
        // starts with "no" but is not a decline. The incumbent's list is
        // `none` / `n/a` / `not ` / `unknown`, and "not " carries its space
        // deliberately.
        assert_eq!(clean_value("Nottingham"), Some("Nottingham".into()));
        assert_eq!(clean_value("Notting Hill"), Some("Notting Hill".into()));
        assert_eq!(
            clean_value("nothing of the sort"),
            Some("nothing of the sort".into()),
            "`not ` requires the space — `nothing` is a value"
        );
    }

    // ── Draw properties ────────────────────────────────────────────

    fn draw_of(raw: &[&str]) -> KSampleDraw {
        let raw: Vec<String> = raw.iter().map(|s| s.to_string()).collect();
        KSampleDraw {
            values: raw.iter().map(|r| clean_value(r)).collect(),
            seeds: seeds_for(1, raw.len() as u8),
            raw,
            prompt_tokens: 100,
            decoded_tokens: 20,
            elapsed_ms: 1,
        }
    }

    #[test]
    fn a_degenerate_draw_is_reported_as_degenerate() {
        // The order's named sampler stop condition. Watched to fail: this
        // is the input that trips it.
        let d = draw_of(&["Karl Yundt", "Karl Yundt", "Karl Yundt"]);
        assert!(d.all_identical());
        assert_eq!(d.distinct_raw(), 1);
    }

    #[test]
    fn a_diverse_draw_is_not() {
        let d = draw_of(&["Karl Yundt", "Ossipon", "Karl Yundt"]);
        assert!(!d.all_identical());
        assert_eq!(d.distinct_raw(), 2);
    }

    #[test]
    fn two_declines_from_different_prose_are_not_a_degenerate_draw() {
        // `values` both clean to None, but the model was genuinely asked
        // twice and answered differently. Reading this as degenerate would
        // blame the sampler for an abstention.
        let d = draw_of(&["NONE", "none — the evidence does not say"]);
        assert!(!d.all_identical(), "degeneracy is a property of `raw`");
        assert_eq!(d.values, vec![None, None]);
    }

    // ── Prompt construction ────────────────────────────────────────

    #[test]
    fn the_prompt_carries_every_chunk_and_the_question() {
        let p = build_value_prompt(
            "  Who giggled?  ",
            &["Karl Yundt giggled grimly.".into(), "A second chunk.".into()],
        );
        assert!(p.contains("Karl Yundt giggled grimly."));
        assert!(p.contains("A second chunk."));
        assert!(p.contains("QUESTION: Who giggled?"));
        assert!(p.contains("[1] "), "chunks are numbered so a value can be traced");
        assert!(p.contains("[2] "));
        assert!(p.contains("NONE"), "the abstention escape hatch must survive");
    }

    #[test]
    fn the_prompt_is_byte_stable_for_the_same_turn() {
        // Identical prompts across the k requests is what makes the whole
        // prompt the shared prefix. If this ever varies, the fanout is
        // silently wrong (each seq would need its own prefill).
        let a = build_value_prompt("Q?", &["E".into()]);
        let b = build_value_prompt("Q?", &["E".into()]);
        assert_eq!(a, b);
    }

    #[test]
    fn the_token_budget_is_the_incumbents() {
        // §5 H2 pins the draw to `extract_answer_value`'s budget so the two
        // measure the same unit. A change here is a spec change.
        assert_eq!(VALUE_TOKEN_BUDGET, 24);
    }

    #[test]
    fn the_draw_temperature_is_not_greedy() {
        // At temp 0 `build_sampler` never builds a `dist` stage, the seed
        // is inert, and k samples are k identical strings. A greedy draw is
        // not a draw.
        assert!(
            DRAW_TEMPERATURE >= 0.01,
            "a greedy k-sample draw measures nothing"
        );
    }
}
