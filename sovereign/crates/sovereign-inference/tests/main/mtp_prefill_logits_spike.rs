// SPDX-License-Identifier: AGPL-3.0-or-later
//! **Spike: does MTP prefill actually need `logits=true` on EVERY position?**
//!
//! `model_slot.rs` flags every position of the MTP prefill batch:
//!
//! ```ignore
//! for (i, &tok) in tokens[prefix_base..].iter().enumerate() {
//!     prefill.add(tok, (prefix_base + i) as i32, &[0], true)   // every position
//! }
//! ```
//!
//! and justifies it as an upstream invariant — "`get_embeddings_pre_norm_ith`
//! errors with `batch.logits[N] != true` otherwise". That flag is expensive:
//! `n_outputs_all` becomes the whole prompt (`llama-context.cpp:1700`), so
//! `output_reserve` sizes the logits buffer at `n_vocab * n_prompt_tokens * 4`
//! — 993,280 bytes PER PROMPT TOKEN at this family's 248,320 vocab. Past ~3,650
//! tokens that buffer exceeds the device's 4 GiB `maxBufferSize`, fails to pin,
//! and falls back to host memory that is never freed (measured 2026-08-27,
//! `arms/mem-forensics/PREREG-buffer-threshold.md`). At 20k tokens it is
//! ~19.9 GB from a single request.
//!
//! **Reading upstream turns the invariant into a question.** The cited error
//! comes from `output_resolve_row`, and `get_embeddings_nextn_ith` only calls
//! it on the MASKED path (`llama-context.cpp:966`). The UNMASKED path returns
//! early:
//!
//! ```c
//! if (!cparams.embeddings_nextn_masked) {
//!     // unmasked: nextn rows are stored densely, indexed by raw token position.
//!     return embd_nextn.data + (size_t) i * n_embd;      // :954-960
//! }
//! const int64_t j = output_resolve_row(i);               // :966, needs the flag
//! ```
//!
//! and `common_speculative`'s MTP init sets the TARGET unmasked
//! (`speculative.cpp:1371-1372`):
//!
//! ```c
//! llama_set_embeddings_nextn(ctx_tgt, true, /*masked*/ false);
//! llama_set_embeddings_nextn(ctx_dft, true, /*masked*/ true);
//! ```
//!
//! So on the target the flag should not be load-bearing at all. That is a
//! claim about a live model, not about source, which is what this measures.
//!
//! **What it measures.** The same prompt prefilled twice on the same context
//! params — once with `logits=true` everywhere (production today), once with
//! it on the final position only — comparing the pre-norm hidden states MTP
//! actually consumes, at every position. Identical vectors mean the flag buys
//! nothing and the buffer it forces is pure waste.
//!
//! A refusal is NOT a pass: if `get_embeddings_pre_norm_ith` returns `None` on
//! either arm the spike reports COULD-NOT-JUDGE rather than "no divergence"
//! (ARCH 18.1). And if the masked-path reading is wrong, upstream's
//! `GGML_ABORT` fires here — in a test process, not in the daemon, which is
//! the entire point of measuring it at this altitude first.
//!
//! ```text
//! SPIKE_GGUF=$PWD/sovereign/models/Qwen3.5-4B-UD-MTP-Q6_K_XL.gguf \
//!   cargo test -p sovereign-inference --test main mtp_prefill_logits_spike \
//!   -- --ignored --nocapture
//! ```

use std::num::NonZeroU32;

use llama_cpp_4::context::params::LlamaContextParams;
use llama_cpp_4::context::LlamaContext;
use llama_cpp_4::llama_backend::LlamaBackend;
use llama_cpp_4::llama_batch::LlamaBatch;
use llama_cpp_4::model::params::LlamaModelParams;
use llama_cpp_4::model::{AddBos, LlamaModel};
use llama_cpp_4::sampling::LlamaSampler;
use llama_cpp_4::token::LlamaToken;

const N_CTX: u32 = 8192;
const N_UBATCH: u32 = 512;
/// Production builds the MTP target with this ring depth (`model_slot.rs`).
const N_RS_SEQ: u32 = 4;

/// Long enough that interior positions are genuinely interior and the prefill
/// spans several ubatches, short enough to stay well under the 3,650-token
/// buffer knee — the spike is about the FLAG, not about the fallback.
fn probe_text() -> String {
    "The harbour ledger recorded each tide, each permit, and each lamp reading in turn. ".repeat(40)
}

fn ctx_params() -> LlamaContextParams {
    LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(N_CTX))
        .with_n_batch(N_CTX)
        .with_n_ubatch(N_UBATCH)
        .with_n_rs_seq(N_RS_SEQ)
}

/// Prefill `tokens` at absolute positions from 0, flagging either every
/// position (`all_logits`) or only the last — the one variable under test.
fn prefill(ctx: &mut LlamaContext<'_>, tokens: &[LlamaToken], all_logits: bool) -> bool {
    let mut batch = LlamaBatch::new(tokens.len().max(N_UBATCH as usize), 1);
    let last = tokens.len() - 1;
    for (i, &tok) in tokens.iter().enumerate() {
        let want = all_logits || i == last;
        if batch.add(tok, i as i32, &[0], want).is_err() {
            return false;
        }
    }
    ctx.decode(&mut batch).is_ok()
}

/// The pre-norm hidden state at every position, as MTP reads it.
///
/// `None` at ANY position is a refusal for the whole arm: a spike that
/// silently compared the positions it could read would report "no divergence"
/// about a subset it never chose (18.1/18.3).
fn pre_norm_rows(ctx: &LlamaContext<'_>, n: usize) -> Option<Vec<Vec<f32>>> {
    let mut rows = Vec::with_capacity(n);
    for i in 0..n {
        rows.push(ctx.get_embeddings_pre_norm_ith(i as i32)?.to_vec());
    }
    Some(rows)
}

fn max_abs_delta(a: &[Vec<f32>], b: &[Vec<f32>]) -> f32 {
    a.iter()
        .zip(b.iter())
        .flat_map(|(x, y)| x.iter().zip(y.iter()).map(|(p, q)| (p - q).abs()))
        .fold(0.0f32, f32::max)
}

/// Greedily continue from a prefilled context, returning the token stream.
///
/// Generation itself is shape-identical in both arms — single-token decodes,
/// logits on the one new token — so any divergence here is inherited from the
/// prefill, which is the variable under test.
fn generate_greedy(ctx: &mut LlamaContext<'_>, start_pos: usize, k: usize) -> Vec<LlamaToken> {
    let mut sampler = LlamaSampler::greedy();
    let mut out = Vec::with_capacity(k);
    let mut pos = start_pos;
    for _ in 0..k {
        let tok = sampler.sample(ctx, -1);
        out.push(tok);
        let mut b = LlamaBatch::new(1, 1);
        if b.add(tok, pos as i32, &[0], true).is_err() || ctx.decode(&mut b).is_err() {
            break;
        }
        pos += 1;
    }
    out
}

#[test]
#[ignore = "manual spike: needs SPIKE_GGUF pointing at a local MTP gguf"]
fn pre_norm_extraction_does_not_need_all_position_logits() {
    let Ok(gguf) = std::env::var("SPIKE_GGUF") else {
        eprintln!("SPIKE_GGUF unset — skipping (this spike needs a real MTP gguf)");
        return;
    };

    let backend = LlamaBackend::init().expect("backend");
    // `with_load_mtp(true)` mirrors `model_slot.rs`: without the MTP weights
    // there are no nextn heads and the pre-norm rows would be absent for a
    // reason that has nothing to do with the flag under test.
    let model_params = std::pin::pin!(LlamaModelParams::default()
        .with_n_gpu_layers(1_000_000)
        .with_load_mtp(true));
    let model = LlamaModel::load_from_file(&backend, &gguf, &model_params).expect("model load");
    eprintln!(
        "model loaded: n_layer_nextn={} (0 would mean no MTP heads — the spike \
         would be measuring nothing)",
        model.n_layer_nextn()
    );
    assert!(
        model.n_layer_nextn() > 0,
        "SPIKE_GGUF has no MTP heads; this spike cannot judge the flag on it"
    );

    let tokens = model
        .str_to_token(&probe_text(), AddBos::Always)
        .expect("tokenize");
    let n = tokens.len();
    eprintln!("prompt: {n} tokens");

    let mut ctx = model
        .new_context(&backend, ctx_params())
        .expect("target context");
    // Exactly what `common_speculative_init` does to the TARGET context
    // (speculative.cpp:1371). The whole claim rests on `masked = false`.
    ctx.set_embeddings_pre_norm(true, /* masked */ false);

    // A DELTA WITHOUT A FLOOR IS NOT A VERDICT. Flagging every position changes
    // `n_outputs_all` from 1 to n, which changes the output-gathering graph —
    // so two arms can differ by this model's own float noise without the FLAG
    // being responsible. The floor is the same configuration run twice; only a
    // signal that exceeds it is attributable (§18.4, and the FLOOR-arm pattern
    // `rs_rollback_spike` already uses for exactly this reason).
    let capture = |ctx: &mut LlamaContext<'_>, all: bool| {
        ctx.clear_kv_cache();
        assert!(
            prefill(ctx, &tokens, all),
            "prefill failed (all_logits={all})"
        );
        let rows = pre_norm_rows(ctx, n);
        let tail = ctx.get_logits_ith(n as i32 - 1).to_vec();
        (rows, tail)
    };

    // Two runs of PRODUCTION'S OWN configuration: the floor.
    let (a1_rows, a1_tail) = capture(&mut ctx, true);
    let (a2_rows, a2_tail) = capture(&mut ctx, true);
    // The candidate.
    let (b_rows, b_tail) = capture(&mut ctx, false);

    let (Some(a1_rows), Some(a2_rows), Some(b_rows)) = (a1_rows, a2_rows, b_rows) else {
        eprintln!(
            "COULD-NOT-JUDGE: pre-norm rows unavailable on at least one arm. If the \
             last-position arm is the one that refused, the flag IS load-bearing on \
             this build and the change must not ship."
        );
        return;
    };

    let tail_delta = |x: &[f32], y: &[f32]| {
        x.iter()
            .zip(y.iter())
            .map(|(p, q)| (p - q).abs())
            .fold(0.0f32, f32::max)
    };

    let floor_pre = max_abs_delta(&a1_rows, &a2_rows);
    let floor_tail = tail_delta(&a1_tail, &a2_tail);
    let d_pre = max_abs_delta(&a1_rows, &b_rows);
    let d_tail = tail_delta(&a1_tail, &b_tail);

    eprintln!("positions compared      : {n}");
    eprintln!("FLOOR (all vs all)      : pre-norm {floor_pre:e}  next-token {floor_tail:e}");
    eprintln!("SIGNAL (all vs last)    : pre-norm {d_pre:e}  next-token {d_tail:e}");
    eprintln!(
        "buffer the flag forces  : {:.2} GB  (n_vocab {} x {n} x 4)",
        (model.n_vocab() as f64 * n as f64 * 4.0) / 1e9,
        model.n_vocab()
    );

    assert!(
        d_pre <= floor_pre,
        "pre-norm hidden states diverged BEYOND this model's own noise \
         (signal {d_pre:e} > floor {floor_pre:e}) — the flag is load-bearing and \
         model_slot.rs:3639 must keep it"
    );
    // The next-token logits are NOT asserted equal, because they are not, and
    // a test that demanded it would either be deleted or quietly relaxed by
    // whoever ships this. Measured: floor 0e0, signal 1.46e-1 — the model is
    // bit-deterministic run-to-run, so that delta is REAL and attributable to
    // the flag, not to noise. It is not a contradiction of the pre-norm result:
    // `n_outputs_all` changes from n to 1, which changes the output-gathering
    // graph and therefore the float accumulation order for the row we read.
    //
    // What it means for the change: the flag is not needed for MTP, but
    // dropping it is NOT an output-identity no-op, and at greedy decoding a
    // 0.146 shift can flip an argmax whose top two are closer than that. So
    // this spike CLEARS THE MECHANISM ONLY. Output identity has to be settled
    // on the real workload — the compose replay is byte-deterministic
    // (note 680940ce), which is what makes that arm decisive.
    if d_tail > floor_tail {
        eprintln!(
            "CAVEAT: next-token logits shift by {d_tail:e} against a {floor_tail:e} \
             floor. The mechanism is clear but OUTPUT IDENTITY IS NOT ESTABLISHED — \
             do not ship on this spike alone; run the byte-identity arm."
        );
    }
    // THE QUESTION THE LOGIT DELTA ACTUALLY POSES: does the OUTPUT change?
    // A 0.146 shift only matters if it flips an argmax, and the only way to
    // know is to decode. Greedy, so nothing but the logits decides.
    const GEN: usize = 128;
    ctx.clear_kv_cache();
    assert!(prefill(&mut ctx, &tokens, true), "gen arm A prefill");
    let gen_all = generate_greedy(&mut ctx, n, GEN);
    ctx.clear_kv_cache();
    assert!(prefill(&mut ctx, &tokens, false), "gen arm B prefill");
    let gen_last = generate_greedy(&mut ctx, n, GEN);

    let diverge_at = gen_all
        .iter()
        .zip(gen_last.iter())
        .position(|(a, b)| a != b);
    match diverge_at {
        None if gen_all.len() == gen_last.len() => eprintln!(
            "GENERATION: IDENTICAL for all {} greedy tokens",
            gen_all.len()
        ),
        Some(i) => eprintln!(
            "GENERATION: diverges at token {i} of {GEN} — the logit shift DOES flip \
             a decision, so this is not an output-preserving change"
        ),
        None => eprintln!("GENERATION: same prefix, different lengths — treat as divergent"),
    }

    eprintln!(
        "VERDICT: pre-norm hidden states are IDENTICAL at all {n} positions \
         (signal {d_pre:e}, floor {floor_pre:e}). `logits=true` on non-final \
         prefill positions is not needed for MTP extraction on the UNMASKED \
         target, and it forces the buffer above."
    );
}

/// Locate the vendored llama.cpp tree by walking UP from this crate, rather
/// than by a deep `../../../` literal that a crate move silently breaks.
fn vendored_llama_cpp() -> Option<std::path::PathBuf> {
    let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let cand = dir.join("vendor/llama-cpp-sys-4/llama.cpp");
        if cand.is_dir() {
            return Some(cand);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// TRIPWIRE: the MTP target context must still be initialised UNMASKED.
///
/// Everything about dropping the all-position prefill flag rests on one line
/// of vendored upstream. `common_speculative`'s MTP init sets the target
/// unmasked and the DRAFT masked:
///
/// ```c
/// llama_set_embeddings_nextn(ctx_tgt, true, /*masked*/ false);
/// llama_set_embeddings_nextn(ctx_dft, true, /*masked*/ true);
/// ```
///
/// Unmasked is what lets `get_embeddings_nextn_ith` index nextn rows densely
/// by raw token position without consulting `output_resolve_row` — the
/// function that actually requires `batch.logits[i]`. Flip that argument to
/// `true` in a vendor bump and the optimisation stops being safe, silently:
/// the next daemon to prefill would take the masked path, `output_resolve_row`
/// would throw on an unflagged position, and upstream's `GGML_ABORT` would
/// SIGABRT the daemon rather than return an error.
///
/// This is deliberately a source assertion and not a runtime one. By the time
/// a runtime check could observe the masked flag, the abort has already
/// happened. We vendor the tree, so the precondition is checkable at rest.
#[test]
fn the_mtp_target_context_is_still_initialised_unmasked() {
    let Some(root) = vendored_llama_cpp() else {
        // Absent tree is COULD-NOT-JUDGE, never a pass — a green tick here
        // would be a tripwire that verified nothing (ARCH 18.1).
        panic!("vendored llama.cpp not found by walking up from CARGO_MANIFEST_DIR");
    };
    let spec = root.join("common/speculative.cpp");
    let src = std::fs::read_to_string(&spec).unwrap_or_else(|e| panic!("{}: {e}", spec.display()));

    let unmasked_target =
        src.contains("llama_set_embeddings_nextn(ctx_tgt, true, /*masked*/ false)");
    let masked_draft = src.contains("llama_set_embeddings_nextn(ctx_dft, true, /*masked*/ true)");

    assert!(
        unmasked_target,
        "vendored speculative.cpp no longer initialises the MTP TARGET unmasked. \
         Dropping `logits=true` on non-final prefill positions (model_slot.rs) is \
         only safe on the unmasked path — re-run \
         `mtp_prefill_logits_spike` before trusting it again."
    );
    assert!(
        masked_draft,
        "the MTP DRAFT context is no longer initialised masked; the two contexts \
         have different extraction rules and this spike's reasoning assumed both"
    );
}

/// This process's `RssAnon`, in GiB. `None` when unreadable — absence is
/// reported, never defaulted to zero (ARCH §18.1). Anon is the right column:
/// the fallback buffer is an unpinnable host allocation, and anon is what the
/// OOM killer counts.
fn rss_anon_gib() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = s.lines().find(|l| l.starts_with("RssAnon:"))?;
    let kb: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1_048_576.0)
}

/// **The byte-identity arm** — the evidence `DEFAULTS_LEDGER.md` names as the
/// condition for flipping `SOVEREIGN_MTP_PREFILL_TAIL_LOGITS` default-ON:
/// *"a byte-identity arm on a real composed report reproduces the control's
/// draft sha256 exactly, with the memory step measured on the same run."*
///
/// Both halves are settled HERE, in a test process, with **no daemon**. The
/// memory half is in fact measured better this way: a clean process, its own
/// `RssAnon` delta, no peer traffic and no other slots to confound it.
///
/// Reuses the spike's `ctx_params`, `prefill` and `generate_greedy` unchanged.
/// The only differences from the spike above are the three the ledger asks
/// for: a REAL composed prompt instead of `probe_text()`, a REAL draft length
/// instead of 128 tokens, and the memory step.
///
/// Identity is compared on the TOKEN STREAM, and both arms' streams are dumped
/// so the ledger's literal sha256 is one `sha256sum` away. Text is a
/// deterministic function of the ids, so the id stream is the stronger check.
///
/// ```text
/// SPIKE_GGUF=$PWD/sovereign/models/Qwen3.5-4B-UD-MTP-Q6_K_XL.gguf \
/// DRAFT_PROMPT=/var/tmp/draft-prompt.txt DUMP_DIR=/var/tmp GEN=2000 \
///   cargo test -p sovereign-inference --test main byte_identity_on_a_real \
///   -- --ignored --nocapture
/// ```
#[test]
#[ignore = "byte-identity arm: needs SPIKE_GGUF + DRAFT_PROMPT (see synthesize.rs dump test)"]
fn byte_identity_on_a_real_composed_draft() {
    let (Ok(gguf), Ok(prompt_file)) = (std::env::var("SPIKE_GGUF"), std::env::var("DRAFT_PROMPT"))
    else {
        eprintln!("SPIKE_GGUF / DRAFT_PROMPT unset — skipping");
        return;
    };
    let prompt = std::fs::read_to_string(&prompt_file).expect("read DRAFT_PROMPT");
    let env_num = |k: &str, d: u32| -> u32 {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let gen = env_num("GEN", 2000) as usize;
    let n_ctx = env_num("ARM_N_CTX", 32768);

    let backend = LlamaBackend::init().expect("backend");
    let model_params = std::pin::pin!(LlamaModelParams::default()
        .with_n_gpu_layers(1_000_000)
        .with_load_mtp(true));
    let model = LlamaModel::load_from_file(&backend, &gguf, &model_params).expect("model load");
    assert!(
        model.n_layer_nextn() > 0,
        "SPIKE_GGUF has no MTP heads; this arm cannot judge the flag on it"
    );

    let tokens = model
        .str_to_token(&prompt, AddBos::Always)
        .expect("tokenize");
    let n = tokens.len();
    eprintln!("real composed draft prompt: {n} tokens, generating {gen}");

    // THE ZERO-TEST GUARD, and the single most likely way this arm lies.
    // Below the ~3,650-token knee `output_reserve` never exceeds the device's
    // 4 GiB maxBufferSize, so BOTH arms allocate the same pinned buffer: the
    // memory step is zero BY CONSTRUCTION and identity is trivially met. That
    // would read as a green light while proving nothing (§18.1 — a check with
    // no failing input it can name is not a check).
    assert!(
        n > 3_650,
        "prompt is {n} tokens, below the ~3,650 buffer knee — this arm can \
         judge NEITHER identity nor the memory step. Use a longer composed report."
    );

    // ONE ARM PER PROCESS, and that is forced by the defect itself. The
    // fallback buffer is grow-only and "retained at high-water until the
    // process exits" (DEFAULTS_LEDGER). So running both arms in one process
    // makes the SECOND arm's RssAnon delta ~0 whichever arm it is — it simply
    // inherits the first arm's allocation. Measuring the memory step at all
    // requires a clean process per arm, so `ARM` selects one and identity is
    // settled by comparing the two dumped token streams across runs. That is
    // also exactly the ledger's phrasing: the flag arm must "reproduce the
    // CONTROL's draft sha256".
    let arm = std::env::var("ARM").unwrap_or_else(|_| "all".to_string());
    let all_logits = match arm.as_str() {
        "all" => true,   // control: production today
        "tail" => false, // the flag
        other => panic!("ARM must be `all` (control) or `tail` (the flag), got {other:?}"),
    };

    let mut ctx = model
        .new_context(
            &backend,
            ctx_params()
                .with_n_ctx(NonZeroU32::new(n_ctx))
                .with_n_batch(n_ctx),
        )
        .expect("target context");
    ctx.set_embeddings_pre_norm(true, /* masked */ false);

    let before = rss_anon_gib();
    ctx.clear_kv_cache();
    assert!(prefill(&mut ctx, &tokens, all_logits), "arm {arm} prefill");
    let after = rss_anon_gib();
    let draft = generate_greedy(&mut ctx, n, gen);

    match (before, after) {
        (Some(b), Some(a)) => eprintln!(
            "MEMORY STEP [arm={arm}]: prefill +{:.2} GiB anon. \
             Predicted for the control = n_tokens * 993,280 B = {:.2} GiB; \
             the flag arm should be ~0.",
            a - b,
            (n as f64 * 993_280.0) / 1_073_741_824.0
        ),
        _ => eprintln!("MEMORY STEP [arm={arm}]: COULD-NOT-JUDGE — RssAnon unreadable"),
    }

    // The draft, as the token stream. Text is a deterministic function of the
    // ids, so the id stream is the stronger identity check, and the ledger's
    // literal sha256 is one `sha256sum` away.
    let dir =
        std::env::var("DUMP_DIR").expect("DUMP_DIR must be set: this arm's verdict IS the dump");
    let path = format!("{dir}/draft-arm-{arm}.tokens");
    let ids = draft
        .iter()
        .map(|t| t.0.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, ids).expect("write token dump");
    eprintln!(
        "DRAFT [arm={arm}]: {} greedy tokens -> {path}\n\
         Compare the two arms with:  sha256sum {dir}/draft-arm-all.tokens {dir}/draft-arm-tail.tokens\n\
         Identical sums = the ledger's flip condition met; otherwise `diff` gives the divergence line.",
        draft.len()
    );
}
