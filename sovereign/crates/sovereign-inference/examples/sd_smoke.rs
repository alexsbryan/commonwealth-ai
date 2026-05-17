//! Phase 0 kill-switch for Spike 2 — speculative decoding.
//!
//! Validates three things end-to-end against the production model pair
//! BEFORE any production code is touched:
//!
//!   1. The Qwen3-Embedding-0.6B model — already always-resident as the
//!      embed slot — produces sane causal-LM logits when loaded with
//!      `with_embeddings(false)` and `with_pooling_type(None)`. If the
//!      embedding fine-tune destroyed the LM head, SD is dead with this
//!      draft and we abort the spike.
//!   2. The embed model and the target model (Darwin-36B) share a
//!      tokenizer (`str_to_token` returns identical sequences for the
//!      same input + `n_vocab()` matches). This is the most likely
//!      failure mode; Darwin is nominally a Qwen-family fine-tune but
//!      fine-tunes occasionally inject new tokens.
//!   3. A minimal speculative-decoding loop (draft proposes `n_draft`
//!      tokens, target verifies in one batched forward, accept prefix /
//!      roll back KV at divergence) clears 50% per-token acceptance on
//!      BOTH a chat-distribution prompt set (voice/hard user-turns) AND
//!      a pipeline-distribution prompt set (Drafter-style system prompts).
//!
//! Greedy/argmax decoding throughout — production temp > 0 needs
//! rejection sampling, but the smoke verifies plumbing + acceptance
//! shape, not sampling fidelity.
//!
//! Run with:
//!
//!   cargo run --release -p sovereign-inference --example sd_smoke -- \
//!     --target-model sovereign/models/FINAL-Bench_Darwin-36B-Opus-Q4_K_L.gguf \
//!     --draft-model  models/qwen-embedding-0.6b.gguf/Qwen3-Embedding-0.6B-Q8_0.gguf \
//!     --n-draft 5

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use llama_cpp_4::context::params::{LlamaContextParams, LlamaPoolingType};
use llama_cpp_4::context::LlamaContext;
use llama_cpp_4::llama_backend::LlamaBackend;
use llama_cpp_4::llama_batch::LlamaBatch;
use llama_cpp_4::model::params::LlamaModelParams;
use llama_cpp_4::model::{AddBos, LlamaModel};
use llama_cpp_4::token::LlamaToken;

const STEP1_LABEL: &str = "[step 1/3] causal-LM logits sanity";
const STEP2_LABEL: &str = "[step 2/3] tokenizer match";
const STEP3_LABEL: &str = "[step 3/3] SD acceptance";

/// Probability mass that must land in top-5 for the draft's logit
/// distribution to count as "non-degenerate." Embedding fine-tunes
/// that destroyed the LM head produce ~uniform distributions with
/// top-5 mass around 5/vocab_size (≈0.00002 on a 248k vocab). A
/// healthy small causal LM (≤1B params) on novel prompts naturally
/// produces 0.3-0.5 top-5 mass — the threshold needs to be permissive
/// enough not to false-fire on functional models. 0.20 still catches
/// the truly-broken case by orders of magnitude.
const TOP_K_MASS_THRESHOLD: f32 = 0.20;
/// Reject if any single token has > this probability — that's the
/// dual failure mode (model collapsed to one token always).
const TOP_1_PROB_CEILING: f32 = 0.9999;
/// Gate for step 3: per-token acceptance on EACH distribution.
const ACCEPTANCE_GATE: f32 = 0.50;
/// Max tokens to decode per prompt during acceptance measurement.
/// Keeps the smoke under a few minutes wall-time on the 36B target.
const SMOKE_MAX_DECODE_TOKENS: usize = 64;

fn main() -> ExitCode {
    let args = Args::parse();
    println!("sd_smoke: target={}", args.target_model.display());
    println!("sd_smoke: draft={}", args.draft_model.display());
    println!("sd_smoke: n_draft={}", args.n_draft);
    println!();

    let backend = Arc::new(LlamaBackend::init().expect("LlamaBackend::init"));

    // ── Step 1 ───────────────────────────────────────────────────
    println!("{}", STEP1_LABEL);
    let draft_model = Arc::new(load_model(&backend, &args.draft_model));
    println!(
        "  draft loaded: layers={} size_mb={} n_vocab={}",
        draft_model.n_layer(),
        draft_model.model_size() / (1024 * 1024),
        draft_model.n_vocab()
    );
    let mut draft_ctx_step1 = new_causal_context(&backend, &draft_model, 4096);
    match check_causal_logits(&draft_model, &mut draft_ctx_step1) {
        Ok(report) => println!("  PASS — {}", report),
        Err(e) => {
            eprintln!("  FAIL — {}", e);
            eprintln!("\nsd_smoke: ABORT (embed fine-tune broke LM head; pivot to Qwen3-0.6B-Base)");
            return ExitCode::from(2);
        }
    }
    drop(draft_ctx_step1);
    println!();

    // ── Step 2 ───────────────────────────────────────────────────
    println!("{}", STEP2_LABEL);
    let target_model = Arc::new(load_model(&backend, &args.target_model));
    println!(
        "  target loaded: layers={} size_mb={} n_vocab={}",
        target_model.n_layer(),
        target_model.model_size() / (1024 * 1024),
        target_model.n_vocab()
    );
    let (chat_prompts, pipeline_prompts) = sample_prompts();
    let probe_strings = build_tokenizer_probe(&chat_prompts, &pipeline_prompts);
    println!("  probing {} strings (chat + pipeline + adversarial tokens)", probe_strings.len());
    match check_tokenizer_match(&draft_model, &target_model, &probe_strings) {
        Ok(()) => println!("  PASS — tokenizer match across all probes"),
        Err(e) => {
            eprintln!("  FAIL — {}", e);
            eprintln!("\nsd_smoke: ABORT (tokenizer divergence; pivot to Qwen3-0.6B-Base)");
            return ExitCode::from(2);
        }
    }
    println!();

    // ── Step 3 ───────────────────────────────────────────────────
    println!("{}", STEP3_LABEL);
    let mut draft_ctx = new_causal_context(&backend, &draft_model, 4096);
    let mut target_ctx = new_causal_context(&backend, &target_model, 4096);

    println!("  running SD on {} chat prompts (greedy)", chat_prompts.len());
    let chat_stats = measure_acceptance(
        &draft_model,
        &mut draft_ctx,
        &target_model,
        &mut target_ctx,
        &chat_prompts,
        args.n_draft,
    );
    println!(
        "    chat:     proposed={} accepted={} acceptance={:.2} avg_tokens={:.1}",
        chat_stats.proposed,
        chat_stats.accepted,
        chat_stats.acceptance(),
        chat_stats.avg_decoded_tokens(),
    );

    println!("  running SD on {} pipeline prompts (greedy)", pipeline_prompts.len());
    let pipeline_stats = measure_acceptance(
        &draft_model,
        &mut draft_ctx,
        &target_model,
        &mut target_ctx,
        &pipeline_prompts,
        args.n_draft,
    );
    println!(
        "    pipeline: proposed={} accepted={} acceptance={:.2} avg_tokens={:.1}",
        pipeline_stats.proposed,
        pipeline_stats.accepted,
        pipeline_stats.acceptance(),
        pipeline_stats.avg_decoded_tokens(),
    );

    println!();
    println!(
        "sd_smoke: chat_acceptance={:.2}, pipeline_acceptance={:.2}",
        chat_stats.acceptance(),
        pipeline_stats.acceptance(),
    );

    let chat_ok = chat_stats.acceptance() >= ACCEPTANCE_GATE;
    let pipeline_ok = pipeline_stats.acceptance() >= ACCEPTANCE_GATE;
    if chat_ok && pipeline_ok {
        println!("sd_smoke: GATE PASSED — both distributions clear {:.0}% acceptance", ACCEPTANCE_GATE * 100.0);
        ExitCode::SUCCESS
    } else {
        if !chat_ok {
            eprintln!("sd_smoke: chat acceptance {:.2} < gate {:.2}", chat_stats.acceptance(), ACCEPTANCE_GATE);
        }
        if !pipeline_ok {
            eprintln!("sd_smoke: pipeline acceptance {:.2} < gate {:.2}", pipeline_stats.acceptance(), ACCEPTANCE_GATE);
        }
        eprintln!("sd_smoke: GATE FAILED — see Spike 2 plan Risk 1/2/6 for pivot options");
        ExitCode::from(1)
    }
}

fn load_model(backend: &Arc<LlamaBackend>, path: &PathBuf) -> LlamaModel {
    let params = LlamaModelParams::default().with_n_gpu_layers(999);
    LlamaModel::load_from_file(backend, path, &params)
        .unwrap_or_else(|e| panic!("load model {}: {e}", path.display()))
}

/// Build a causal-mode context against a model — `with_embeddings(false)`
/// and `with_pooling_type(None)` are the key flags that flip an
/// embedding-trained model back into next-token mode. Single sequence,
/// generous context.
fn new_causal_context(
    backend: &Arc<LlamaBackend>,
    model: &Arc<LlamaModel>,
    n_ctx: u32,
) -> LlamaContext<'static> {
    let params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(n_ctx))
        .with_n_batch(n_ctx)
        .with_n_ubatch(512)
        .with_embeddings(false)
        .with_pooling_type(LlamaPoolingType::None)
        .with_offload_kqv(true);
    // Same Arc-extend-to-static pattern used by embedded.rs:1267-1297
    // and bench_decode.rs:108-114 — the context borrows the model and we
    // promise to keep the Arc alive at least as long.
    unsafe {
        let model_ref: &'static LlamaModel = &*(Arc::as_ptr(model) as *const LlamaModel);
        model_ref
            .new_context(backend, params)
            .expect("new_context (causal)")
    }
}

// ── Step 1: causal-LM logits sanity ─────────────────────────────────

struct LogitsSanity {
    top1_max_prob: f32,
    top5_min_mass: f32,
    samples: usize,
}

impl std::fmt::Display for LogitsSanity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} prompts: top-1 max p={:.3} (ceiling {:.4}), top-5 min mass={:.3} (floor {:.2})",
            self.samples, self.top1_max_prob, TOP_1_PROB_CEILING, self.top5_min_mass, TOP_K_MASS_THRESHOLD
        )
    }
}

fn check_causal_logits(
    model: &LlamaModel,
    ctx: &mut LlamaContext<'_>,
) -> Result<LogitsSanity, String> {
    let prompts = [
        "The capital of France is",
        "1 + 1 =",
        "Classify the user's intent. The user said: \"I'm stuck on this bug.\" The intent is",
    ];
    let mut top1_max_prob = 0.0_f32;
    let mut top5_min_mass = 1.0_f32;
    for prompt in &prompts {
        let tokens = model
            .str_to_token(prompt, AddBos::Always)
            .map_err(|e| format!("tokenize prompt: {e}"))?;
        if tokens.is_empty() {
            return Err(format!("empty token sequence for prompt {prompt:?}"));
        }
        ctx.clear_kv_cache();
        let mut batch = LlamaBatch::new(ctx.n_batch() as usize, 1);
        let last_idx = tokens.len() - 1;
        for (i, &tok) in tokens.iter().enumerate() {
            batch
                .add(tok, i as i32, &[0], i == last_idx)
                .map_err(|e| format!("batch.add: {e}"))?;
        }
        ctx.decode(&mut batch).map_err(|e| format!("decode: {e}"))?;
        let logits = ctx.get_logits_ith(last_idx as i32);
        let probs = softmax(logits);
        let (top1_p, _, top5_mass) = top_k_stats(&probs, 5);
        if top1_p > top1_max_prob {
            top1_max_prob = top1_p;
        }
        if top5_mass < top5_min_mass {
            top5_min_mass = top5_mass;
        }
    }
    if top1_max_prob > TOP_1_PROB_CEILING {
        return Err(format!(
            "top-1 prob collapsed to {:.4} — model emits one token always",
            top1_max_prob
        ));
    }
    if top5_min_mass < TOP_K_MASS_THRESHOLD {
        return Err(format!(
            "top-5 mass only {:.3} — distribution too flat (LM head degraded)",
            top5_min_mass
        ));
    }
    Ok(LogitsSanity {
        top1_max_prob,
        top5_min_mass,
        samples: prompts.len(),
    })
}

fn softmax(logits: &[f32]) -> Vec<f32> {
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut exps: Vec<f32> = logits.iter().map(|&x| (x - max).exp()).collect();
    let sum: f32 = exps.iter().sum();
    if sum > 0.0 {
        for p in exps.iter_mut() {
            *p /= sum;
        }
    }
    exps
}

fn top_k_stats(probs: &[f32], k: usize) -> (f32, usize, f32) {
    let mut indexed: Vec<(usize, f32)> =
        probs.iter().copied().enumerate().map(|(i, p)| (i, p)).collect();
    indexed.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
    let top1 = indexed.first().copied().unwrap_or((0, 0.0));
    let top_k_mass: f32 = indexed.iter().take(k).map(|(_, p)| *p).sum();
    (top1.1, top1.0, top_k_mass)
}

fn argmax_token(logits: &[f32]) -> LlamaToken {
    let mut best_idx = 0i32;
    let mut best_val = f32::NEG_INFINITY;
    for (i, &v) in logits.iter().enumerate() {
        if v > best_val {
            best_val = v;
            best_idx = i as i32;
        }
    }
    LlamaToken(best_idx)
}

// ── Step 2: tokenizer match ────────────────────────────────────────

fn check_tokenizer_match(
    draft: &LlamaModel,
    target: &LlamaModel,
    probes: &[String],
) -> Result<(), String> {
    if draft.n_vocab() != target.n_vocab() {
        return Err(format!(
            "n_vocab mismatch: draft={} target={}",
            draft.n_vocab(),
            target.n_vocab()
        ));
    }
    for (i, s) in probes.iter().enumerate() {
        let d = draft
            .str_to_token(s, AddBos::Never)
            .map_err(|e| format!("draft tokenize probe {i}: {e}"))?;
        let t = target
            .str_to_token(s, AddBos::Never)
            .map_err(|e| format!("target tokenize probe {i}: {e}"))?;
        if d != t {
            let head = s.chars().take(60).collect::<String>();
            return Err(format!(
                "tokenizer divergence on probe {i} ({head:?}): draft={d:?} target={t:?}"
            ));
        }
    }
    Ok(())
}

fn build_tokenizer_probe(chat: &[String], pipeline: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    out.extend(chat.iter().cloned());
    out.extend(pipeline.iter().cloned());
    // Adversarial chat-template tokens — these are the classes of
    // strings where fine-tunes most often inject custom vocab.
    for tok in &[
        "<|im_start|>",
        "<|im_end|>",
        "<|endoftext|>",
        "<think>",
        "</think>",
        "<|tool_call|>",
        "system\n",
        "assistant\n",
        "user\n",
    ] {
        out.push((*tok).to_string());
    }
    out
}

// ── Step 3: SD acceptance measurement ──────────────────────────────

#[derive(Default)]
struct AcceptanceStats {
    proposed: usize,
    accepted: usize,
    decoded_tokens: usize,
    prompt_count: usize,
}

impl AcceptanceStats {
    fn acceptance(&self) -> f32 {
        if self.proposed == 0 {
            0.0
        } else {
            self.accepted as f32 / self.proposed as f32
        }
    }
    fn avg_decoded_tokens(&self) -> f32 {
        if self.prompt_count == 0 {
            0.0
        } else {
            self.decoded_tokens as f32 / self.prompt_count as f32
        }
    }
}

/// Greedy speculative-decoding loop. For each prompt:
///   1. Prefill both contexts with the prompt tokens.
///   2. Loop: draft proposes up to `n_draft` tokens autoregressively
///      (greedy). Target verifies them in one batched forward pass.
///      Walk left-to-right: accept iff target's argmax equals draft's
///      proposal; on first mismatch, accept the prefix, take target's
///      replacement, clear KV past the mismatch on both contexts.
///   3. Continue until target emits EOG or SMOKE_MAX_DECODE_TOKENS hit.
fn measure_acceptance(
    draft_model: &LlamaModel,
    draft_ctx: &mut LlamaContext<'_>,
    target_model: &LlamaModel,
    target_ctx: &mut LlamaContext<'_>,
    prompts: &[String],
    n_draft: usize,
) -> AcceptanceStats {
    let mut stats = AcceptanceStats::default();
    for prompt in prompts {
        let t0 = Instant::now();
        let prompt_tokens = target_model
            .str_to_token(prompt, AddBos::Always)
            .expect("tokenize prompt for SD");

        // Prefill both contexts. The last-token logits are what we'll
        // sample to bootstrap the first draft proposal.
        target_ctx.clear_kv_cache();
        draft_ctx.clear_kv_cache();
        let prompt_len = prompt_tokens.len();
        prefill(target_ctx, &prompt_tokens);
        prefill(draft_ctx, &prompt_tokens);

        let mut decoded = 0usize;
        let mut next_pos = prompt_len as i32;
        // The "last accepted token" — what both contexts have committed.
        // After prefill, the bootstrap is target's argmax on the prompt's
        // last-position logits.
        let mut last_token = argmax_token(target_ctx.get_logits_ith(prompt_len as i32 - 1));
        if target_model.is_eog_token(last_token) {
            stats.prompt_count += 1;
            continue;
        }

        while decoded < SMOKE_MAX_DECODE_TOKENS {
            // Draft proposes `n_draft` tokens, starting from last_token.
            // Each proposal is committed into the draft KV cache.
            let proposals = draft_propose(
                draft_model,
                draft_ctx,
                last_token,
                next_pos,
                n_draft,
                SMOKE_MAX_DECODE_TOKENS - decoded,
            );
            if proposals.is_empty() {
                break;
            }
            stats.proposed += proposals.len();

            // Target verifies — submit the same N tokens at positions
            // [next_pos .. next_pos + n], with logits=true at every
            // position so we can read target's choice at each step.
            let target_choices = target_verify(target_ctx, &proposals, next_pos);

            // Walk and accept the matching prefix.
            let mut accepted_in_round = 0usize;
            for (i, &proposed) in proposals.iter().enumerate() {
                let target_choice = target_choices[i];
                if target_choice == proposed {
                    accepted_in_round += 1;
                    last_token = proposed;
                    if target_model.is_eog_token(proposed) {
                        decoded += 1;
                        break;
                    }
                } else {
                    // Mismatch at index i. Accept everything before; take
                    // target's replacement; roll KV past this point on
                    // BOTH contexts so subsequent proposals start fresh.
                    last_token = target_choice;
                    break;
                }
            }
            let target_bonus = accepted_in_round == proposals.len();
            decoded += accepted_in_round + 1; // +1 for either target's replacement OR target's bonus
            stats.accepted += accepted_in_round;

            // KV rollback. The accepted-prefix length is `accepted_in_round`
            // (positions [next_pos .. next_pos + accepted_in_round]). The
            // resume point is `next_pos + accepted_in_round`. Then we
            // commit `last_token` at that resume position on both contexts
            // before the next round.
            let resume_pos = next_pos + accepted_in_round as i32;
            // Trim everything past resume_pos on both contexts. On full
            // acceptance we have one extra position (target's bonus) to
            // keep on the target context only.
            let target_trim_from = if target_bonus { resume_pos + 1 } else { resume_pos };
            let _ = target_ctx.clear_kv_cache_seq(Some(0), Some(target_trim_from as u32), None);
            let _ = draft_ctx.clear_kv_cache_seq(Some(0), Some(resume_pos as u32), None);

            // Commit last_token at resume_pos on both contexts so the
            // next iteration's draft can start from a consistent point.
            // (On full acceptance: target's bonus IS last_token and is
            // already in target_ctx at resume_pos via the verify pass;
            // we only need to commit it to the draft. On mismatch:
            // last_token is target's replacement and needs committing
            // to both.)
            if target_bonus {
                commit_token(draft_ctx, last_token, resume_pos);
                next_pos = resume_pos + 1;
            } else {
                commit_token(target_ctx, last_token, resume_pos);
                commit_token(draft_ctx, last_token, resume_pos);
                next_pos = resume_pos + 1;
            }

            if target_model.is_eog_token(last_token) {
                break;
            }
        }
        stats.prompt_count += 1;
        stats.decoded_tokens += decoded;
        eprintln!(
            "    [prompt {}] decoded={} elapsed={}ms",
            stats.prompt_count,
            decoded,
            t0.elapsed().as_millis()
        );
    }
    stats
}

/// Prefill `tokens` into `ctx`, requesting logits only on the last
/// position. Standard prefill shape (matches bench_decode.rs:151-159).
fn prefill(ctx: &mut LlamaContext<'_>, tokens: &[LlamaToken]) {
    let n_batch = ctx.n_batch() as usize;
    let mut batch = LlamaBatch::new(n_batch, 1);
    let last = tokens.len() - 1;
    for (i, &tok) in tokens.iter().enumerate() {
        batch
            .add(tok, i as i32, &[0], i == last)
            .expect("prefill batch.add");
    }
    ctx.decode(&mut batch).expect("prefill decode");
}

/// Append `token` at `pos` on `ctx`, requesting logits=true so the
/// next draft proposal can read them.
fn commit_token(ctx: &mut LlamaContext<'_>, token: LlamaToken, pos: i32) {
    let mut batch = LlamaBatch::new(1, 1);
    batch
        .add(token, pos, &[0], true)
        .expect("commit_token batch.add");
    ctx.decode(&mut batch).expect("commit_token decode");
}

/// Draft proposes up to `n_draft` tokens autoregressively starting
/// from `last_token` at `start_pos`. Each proposal is committed into
/// the draft KV cache. Greedy / argmax.
fn draft_propose(
    draft_model: &LlamaModel,
    draft_ctx: &mut LlamaContext<'_>,
    last_token: LlamaToken,
    start_pos: i32,
    n_draft: usize,
    budget: usize,
) -> Vec<LlamaToken> {
    // First commit last_token at start_pos and read its logits to
    // produce the first proposal. Subsequent proposals chain on the
    // draft's own previous output.
    let n = n_draft.min(budget);
    if n == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(n);
    let mut current = last_token;
    let mut pos = start_pos;
    for _ in 0..n {
        let mut batch = LlamaBatch::new(1, 1);
        batch
            .add(current, pos, &[0], true)
            .expect("draft batch.add");
        draft_ctx.decode(&mut batch).expect("draft decode");
        let logits = draft_ctx.get_logits_ith(0);
        let next = argmax_token(logits);
        out.push(next);
        if draft_model.is_eog_token(next) {
            break;
        }
        current = next;
        pos += 1;
    }
    out
}

/// Target verifies all `proposals` in a single batched forward pass at
/// positions [start_pos .. start_pos + proposals.len()]. Returns the
/// argmax target chose at each of those positions.
fn target_verify(
    target_ctx: &mut LlamaContext<'_>,
    proposals: &[LlamaToken],
    start_pos: i32,
) -> Vec<LlamaToken> {
    let mut batch = LlamaBatch::new(proposals.len().max(1), 1);
    for (i, &tok) in proposals.iter().enumerate() {
        batch
            .add(tok, start_pos + i as i32, &[0], true)
            .expect("verify batch.add");
    }
    target_ctx.decode(&mut batch).expect("verify decode");
    proposals
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let logits = target_ctx.get_logits_ith(i as i32);
            argmax_token(logits)
        })
        .collect()
}

// ── Prompt sample bank ──────────────────────────────────────────────

/// Returns (chat_prompts, pipeline_prompts). 5 + 5 each; deliberately
/// short so the smoke completes in minutes on a 36B target.
fn sample_prompts() -> (Vec<String>, Vec<String>) {
    // Chat distribution: 5 voice-eval user-turns covering the witness
    // contract surface (Relational/Expressive/DeepQuery shapes).
    let chat = vec![
        "Is Jordan a recurring source of stress in my life?".to_string(),
        "Why is this job decision feeling so heavy?".to_string(),
        "I had a fight with my mom yesterday and I haven't been able to stop replaying it.".to_string(),
        "Remember last month when I told you about my dad's diagnosis? I want to come back to that.".to_string(),
        "I'm just so tired. Like, every day feels heavy and I dread waking up.".to_string(),
    ];
    // Pipeline distribution: 5 Drafter-style synthesis prompts. The
    // shape is the same one pipeline/runner.rs builds before calling
    // Speed::Slow — a directive header + a package wrapper.
    let pipeline = vec![
        "You are answering a knowledge query from a vector-retrieved package. Synthesize, do not infer.\n\n<package>\nTopic: World War II naval engagements\n\n[chunk 1] The Battle of Midway, fought June 4-7 1942, marked a strategic turning point...\n[chunk 2] Japanese carrier losses included Akagi, Kaga, Soryu, and Hiryu.\n</package>\n\nWrite a 3-paragraph synthesis.".to_string(),
        "You are answering a reasoning query from a vector-retrieved package. Synthesize, do not infer.\n\n<package>\n[chunk 1] Plate tectonics describes the large-scale motion of seven large plates...\n[chunk 2] Continental drift was first proposed by Alfred Wegener in 1912...\n</package>\n\nExplain how the theory of plate tectonics accounts for mountain range distribution.".to_string(),
        "You are answering a comparison query. Contrast bounded.\n\n<package>\n[chunk 1] TCP provides reliable, ordered, error-checked delivery...\n[chunk 2] UDP is connectionless, with no delivery guarantees...\n</package>\n\nCompare TCP and UDP delivery guarantees.".to_string(),
        "You are answering a knowledge query from a vector-retrieved package. Synthesize, do not infer.\n\n<package>\n[chunk 1] The Eiffel Tower was constructed between 1887 and 1889 for the Exposition Universelle...\n[chunk 2] Gustave Eiffel's engineering firm designed and built the tower.\n</package>\n\nWhat year was the Eiffel Tower built?".to_string(),
        "You are answering a reasoning query from a vector-retrieved package. Synthesize, do not infer.\n\n<package>\n[chunk 1] The 2008 financial crisis was precipitated by the collapse of the US housing bubble...\n[chunk 2] Subprime mortgage lending and complex derivative instruments amplified systemic risk...\n</package>\n\nWhat were the main causes of the 2008 financial crisis?".to_string(),
    ];
    (chat, pipeline)
}

// ── CLI ────────────────────────────────────────────────────────────

struct Args {
    target_model: PathBuf,
    draft_model: PathBuf,
    n_draft: usize,
}

impl Args {
    fn parse() -> Self {
        let raw: Vec<String> = std::env::args().collect();
        let mut target = None;
        let mut draft = None;
        let mut n_draft = 5usize;
        let mut i = 1;
        while i < raw.len() {
            match raw[i].as_str() {
                "--target-model" => {
                    i += 1;
                    target = raw.get(i).map(PathBuf::from);
                }
                "--draft-model" => {
                    i += 1;
                    draft = raw.get(i).map(PathBuf::from);
                }
                "--n-draft" => {
                    i += 1;
                    n_draft = raw.get(i).and_then(|s| s.parse().ok()).unwrap_or(5);
                }
                "-h" | "--help" => {
                    println!("usage: sd_smoke --target-model <path> --draft-model <path> [--n-draft N]");
                    std::process::exit(0);
                }
                _ => {}
            }
            i += 1;
        }
        let target_model = target.unwrap_or_else(|| {
            eprintln!("error: --target-model <path.gguf> required");
            std::process::exit(2);
        });
        let draft_model = draft.unwrap_or_else(|| {
            eprintln!("error: --draft-model <path.gguf> required");
            std::process::exit(2);
        });
        Args {
            target_model,
            draft_model,
            n_draft,
        }
    }
}
