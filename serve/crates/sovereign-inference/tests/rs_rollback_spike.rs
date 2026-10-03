// SPDX-License-Identifier: AGPL-3.0-or-later
//! **Spike: is the recurrent-arch prefix-cache veto still earned, or is
//! it a context-construction setting we never turned on?**
//!
//! `gates::prefix_cache_gate` refuses partial KV keep on recurrent and
//! hybrid architectures, so every turn on `qwen35` / `qwen35moe`
//! re-prefills from zero. The 2026-08-10 gate-repro re-verification
//! confirmed the failure is real — but also pinned the mechanism to a
//! REFUSAL rather than to corruption: `llama_memory_seq_rm` returns
//! false and removes nothing, and the next decode then trips M-RoPE's
//! `X < Y` position check.
//!
//! Reading upstream's implementation for the *reason* it refuses turns
//! the veto into a question (`llama-memory-recurrent.cpp`):
//!
//! ```c
//! // partial rollback via per-token snapshot index (bounded by n_rs_seq)
//! if (0 < p0 && p0 <= cell.pos && p1 > cell.pos) {
//!     const llama_pos rollback = cell.pos - (p0 - 1);
//!     if (rollback >= 1 && rollback <= (llama_pos) n_rs_seq) {
//!         set_rs_idx(seq_id, (uint32_t) rollback);
//!         cell.pos = p0 - 1;
//!         return true;          // <-- partial rollback IS supported
//!     }
//!     return false;
//! }
//! ```
//!
//! Recurrent memory keeps a ring of per-token state snapshots
//! `n_rs_seq` deep, and a partial removal succeeds when the rollback
//! fits in that ring. `llm_arch_supports_rs_rollback` (`llama-arch.cpp`)
//! lists exactly `QWEN35`, `QWEN35MOE` and `DEEPSEEK4` — our two
//! hybrids are both on it. But `model_slot.rs` sets `with_n_rs_seq(4)`
//! ONLY when `is_mtp_model`; every non-MTP chat context is built with
//! the default 0, which disables rollback outright. So the capability
//! is not absent — it is unconfigured, and the string ladder in
//! `is_recurrent_arch` has been reading "hybrid" as "incapable".
//!
//! **What this spike measures**, per `n_rs_seq` value:
//!   1. Does `seq_rm` ACCEPT the partial removal? (the boundary)
//!   2. Is the resulting generation IDENTICAL to a full prefill on a
//!      fresh context? (the part that matters — the old repro's own
//!      warning was "verify OUTPUT CORRECTNESS too, not just absence
//!      of errors", and an accepted rollback that silently degrades
//!      quality would be far worse than the veto.)
//!   3. What does the snapshot ring cost? Recurrent tensors are
//!      widened to `mem_size * (1 + n_rs_seq)` rows, so the price is
//!      linear; llama.cpp logs it as `RS buffer size` / `size = … MiB
//!      (… rs_seq)` and tracing forwards those lines.
//!
//! Run explicitly (never in CI — needs local GGUF weights):
//! ```sh
//! SPIKE_GGUF=sovereign/models/Qwen3.5-2B.Q6_K.gguf \
//!   cargo test -p sovereign-inference --test rs_rollback_spike --release \
//!   -- --ignored --nocapture
//! ```
//!
//! A NEGATIVE result (accepted but divergent, or prohibitively
//! expensive) is a real finding and should be written up — it retires
//! the question rather than leaving it open.

use std::num::NonZeroU32;
use std::time::Instant;

use llama_cpp_4::context::params::LlamaContextParams;
use llama_cpp_4::context::LlamaContext;
use llama_cpp_4::llama_backend::LlamaBackend;
use llama_cpp_4::llama_batch::LlamaBatch;
use llama_cpp_4::model::params::LlamaModelParams;
use llama_cpp_4::model::{AddBos, LlamaModel};
use llama_cpp_4::sampling::LlamaSampler;
use llama_cpp_4::token::LlamaToken;
use sovereign_inference::embedded::capabilities::{
    probe_partial_kv, ModelCapabilities, PartialKvVerdict, Provenance,
};

/// Snapshot-ring depths to sweep. 0 is today's non-MTP setting, 4 is
/// today's MTP setting (deliberately too shallow for a prefix-cache
/// rollback — it exists to serve draft rejection), and the rest
/// bracket the depth a real prefix reuse needs.
const RS_SEQ_SWEEP: [u32; 4] = [0, 4, 64, 512];

const CONTINUATION_TOKENS: usize = 24;
const N_CTX: u32 = 4096;
/// Must exceed `n_rs_seq + 1`: upstream requires the trailing
/// `1 + n_rs_seq` tokens of a sequence to land in one ubatch
/// (`split_equal(n_ubatch, true, n_rs_seq + 1)`).
const N_UBATCH: u32 = 2048;
/// Tokens generated after the first prompt, so the cache holds
/// prompt+generation when the second request arrives — exactly what
/// the real prefix-cache path faces (`cached_tokens` tracks only the
/// PROMPT, but the KV holds the generated tail too, which is what
/// makes the rollback deeper than the prompt divergence alone).
const GENERATE_AFTER_FIRST: usize = 4;

/// A shared briefing followed by a per-request question — the shape of
/// every prefix-reuse workload we have (system core + evidence stable,
/// question varying at the end).
fn prompt_with_question(question: &str) -> String {
    let mut s = String::from(
        "You are the resident assistant for the Saltgrass Archive, a \
         collection of maritime survey records from 1890 to 1954. ",
    );
    for i in 0..40 {
        s.push_str(&format!(
            "Volume {i} covers the {} coastal survey of sector {}, \
             including tide tables, dredging permits, and the keeper's \
             log for lighthouse station {}. ",
            ["spring", "summer", "autumn", "winter"][i % 4],
            (i * 7) % 23,
            (i * 3) % 11,
        ));
    }
    s.push_str("\nQuestion: ");
    s.push_str(question);
    s.push_str("\nAnswer: ");
    s
}

/// Longest common prefix, with the reserve-last-token rule the
/// production `gates::compute_lcp` uses (an identical prompt must
/// still re-decode ≥1 token so the sampler gets fresh logits).
fn lcp_len(a: &[LlamaToken], b: &[LlamaToken]) -> usize {
    a.iter()
        .zip(b.iter())
        .take(b.len().saturating_sub(1))
        .take_while(|(x, y)| x == y)
        .count()
}

/// Decode `tokens[from..]` at absolute positions `from..`, then greedy
/// continue for `n`. Mirrors the production prefill + decode loop.
fn decode_tail_and_continue(
    ctx: &mut LlamaContext,
    tokens: &[LlamaToken],
    from: usize,
    n: usize,
) -> Vec<LlamaToken> {
    let tail = &tokens[from..];
    let mut batch = LlamaBatch::new(tail.len().max(1), 1);
    for (i, tok) in tail.iter().enumerate() {
        batch
            .add(*tok, (from + i) as i32, &[0], i == tail.len() - 1)
            .expect("batch add tail");
    }
    ctx.decode(&mut batch).expect("decode tail");

    let sampler = LlamaSampler::greedy();
    let mut out = Vec::with_capacity(n);
    let mut pos = tokens.len() as i32;
    let mut next = sampler.sample(ctx, -1);
    out.push(next);
    for _ in 1..n {
        let mut b = LlamaBatch::new(1, 1);
        b.add(next, pos, &[0], true)
            .expect("batch add continuation");
        ctx.decode(&mut b).expect("decode continuation");
        pos += 1;
        next = sampler.sample(ctx, -1);
        out.push(next);
    }
    out
}

fn ctx_params(n_rs_seq: u32) -> LlamaContextParams {
    let p = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(N_CTX))
        .with_n_batch(N_CTX)
        .with_n_ubatch(N_UBATCH);
    if n_rs_seq > 0 {
        p.with_n_rs_seq(n_rs_seq)
    } else {
        p
    }
}

/// One sweep row.
///
/// Two comparisons, because one is not enough to attribute a
/// divergence. Widening the recurrent tensors to `1 + n_rs_seq` groups
/// changes the allocation the kernels run over, so an arm's output
/// could differ from the `n_rs_seq=0` baseline WITHOUT the rollback
/// being at fault. `matches_same_params` is therefore the load-bearing
/// check (same context params, rollback vs full prefill — isolates the
/// rollback), and `same_params_matches_baseline` reports whether merely
/// having the ring moved the numbers at all.
struct Arm {
    n_rs_seq: u32,
    rollback_needed: usize,
    accepted: bool,
    /// Rollback-reuse vs FULL PREFILL AT THE SAME `n_rs_seq`. This is
    /// the rollback's own correctness.
    matches_same_params: Option<bool>,
    /// Full prefill at this `n_rs_seq` vs full prefill at 0. Isolates
    /// the ring's numerical footprint from the rollback's.
    same_params_matches_baseline: bool,
    reuse_ms: u128,
    full_ms: u128,
}

#[test]
#[ignore = "manual spike: needs SPIKE_GGUF pointing at a local hybrid gguf"]
fn partial_keep_works_on_hybrids_when_the_snapshot_ring_is_deep_enough() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_test_writer()
        .try_init();

    let Ok(gguf) = std::env::var("SPIKE_GGUF") else {
        eprintln!("SKIP: set SPIKE_GGUF to a local gguf path");
        return;
    };

    let backend = LlamaBackend::init().expect("backend");
    let model_params = std::pin::pin!(LlamaModelParams::default().with_n_gpu_layers(1_000_000));
    let model = LlamaModel::load_from_file(&backend, &gguf, &model_params).expect("model load");
    let arch = model
        .meta_val_str("general.architecture", 64)
        .unwrap_or_else(|_| "<unknown>".into());
    eprintln!("\n=== rs-rollback spike · arch={arch} · {gguf} ===");

    let prompt_a = prompt_with_question("Which volume holds the autumn survey of sector 14?");
    let prompt_b = prompt_with_question("Which volume holds the winter survey of sector 21?");
    let tokens_a = model
        .str_to_token(&prompt_a, AddBos::Always)
        .expect("tokenize A");
    let tokens_b = model
        .str_to_token(&prompt_b, AddBos::Always)
        .expect("tokenize B");
    let lcp = lcp_len(&tokens_a, &tokens_b);
    // What the cache actually holds when request B arrives: A's prompt
    // plus what A generated. THIS is what the rollback has to unwind.
    let cache_depth = tokens_a.len() + GENERATE_AFTER_FIRST;
    let rollback_needed = cache_depth - lcp;
    eprintln!(
        "prompt_a={} tok · prompt_b={} tok · lcp={lcp} · cache_depth={cache_depth} \
         · ROLLBACK NEEDED={rollback_needed}",
        tokens_a.len(),
        tokens_b.len(),
    );
    assert!(
        lcp > 0 && lcp < tokens_b.len(),
        "spike prompts must share a non-trivial prefix and then diverge"
    );

    // Baseline: today's production shape — n_rs_seq=0, fresh context,
    // full prefill of B. Only used to detect whether the ring itself
    // perturbs the numbers; it is NOT what rollback correctness is
    // judged against (see `Arm`).
    let mut base_ctx = model
        .new_context(&backend, ctx_params(0))
        .expect("baseline context");
    let baseline = decode_tail_and_continue(&mut base_ctx, &tokens_b, 0, CONTINUATION_TOKENS);
    drop(base_ctx);
    eprintln!("baseline captured (n_rs_seq=0, full prefill)\n");

    let mut arms = Vec::new();
    for n_rs_seq in RS_SEQ_SWEEP {
        // CONTROL for this arm: same context params, but a full
        // prefill with no reuse at all. Any divergence between this
        // and `baseline` is the RING's doing, not the rollback's.
        let mut ctl_ctx = match model.new_context(&backend, ctx_params(n_rs_seq)) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("n_rs_seq={n_rs_seq}: context construction FAILED — {e}");
                continue;
            }
        };
        let t_full = Instant::now();
        let same_params_full =
            decode_tail_and_continue(&mut ctl_ctx, &tokens_b, 0, CONTINUATION_TOKENS);
        let full_ms = t_full.elapsed().as_millis();
        drop(ctl_ctx);
        let same_params_matches_baseline = same_params_full == baseline;

        let mut ctx = model
            .new_context(&backend, ctx_params(n_rs_seq))
            .expect("reuse context");

        // Request 1: full prefill of A, then generate — leaving the
        // cache in the state a real second request would find it.
        let _ = decode_tail_and_continue(&mut ctx, &tokens_a, 0, GENERATE_AFTER_FIRST);

        // Request 2: attempt the partial keep the prefix cache wants.
        let accepted = ctx
            .clear_kv_cache_seq(Some(0), Some(lcp as u32), None)
            .expect("seq_rm argument conversion");

        let (matches_same_params, reuse_ms) = if accepted {
            let t = Instant::now();
            let reused = decode_tail_and_continue(&mut ctx, &tokens_b, lcp, CONTINUATION_TOKENS);
            (Some(reused == same_params_full), t.elapsed().as_millis())
        } else {
            (None, 0)
        };

        eprintln!(
            "n_rs_seq={n_rs_seq:<4} accepted={accepted:<5} needed={rollback_needed:<5} \
             rollback_correct={:<5} ring_perturbs_output={:<5} full={full_ms} ms reuse={reuse_ms} ms",
            match matches_same_params {
                Some(true) => "YES",
                Some(false) => "NO",
                None => "n/a",
            },
            !same_params_matches_baseline,
        );
        arms.push(Arm {
            n_rs_seq,
            rollback_needed,
            accepted,
            matches_same_params,
            same_params_matches_baseline,
            reuse_ms,
            full_ms,
        });
    }

    eprintln!("\n--- verdict ---");
    let deep_enough: Vec<&Arm> = arms
        .iter()
        .filter(|a| a.n_rs_seq as usize >= a.rollback_needed)
        .collect();
    let too_shallow: Vec<&Arm> = arms
        .iter()
        .filter(|a| (a.n_rs_seq as usize) < a.rollback_needed)
        .collect();

    for a in &too_shallow {
        assert!(
            !a.accepted,
            "n_rs_seq={} is SHALLOWER than the {}-token rollback yet seq_rm \
             accepted it — upstream's bound is not what this spike assumed; \
             re-read llama_memory_recurrent::seq_rm before trusting any row here",
            a.n_rs_seq, a.rollback_needed
        );
    }
    if too_shallow.is_empty() {
        eprintln!("(no too-shallow arm in the sweep — boundary unprobed)");
    } else {
        eprintln!(
            "too-shallow arms refused as expected: {:?}",
            too_shallow.iter().map(|a| a.n_rs_seq).collect::<Vec<_>>()
        );
    }

    assert!(
        !deep_enough.is_empty(),
        "SWEEP TOO NARROW: no arm had n_rs_seq >= the {rollback_needed}-token \
         rollback, so the spike's central question was never asked. Widen \
         RS_SEQ_SWEEP."
    );
    // Instrument check before result (§18.4): if merely allocating the
    // ring changes a no-reuse full prefill's output, then a rollback
    // divergence cannot be attributed to the rollback.
    for a in &arms {
        if !a.same_params_matches_baseline {
            eprintln!(
                "NOTE n_rs_seq={}: a NO-REUSE full prefill already diverges from \
                 the n_rs_seq=0 baseline — the snapshot ring perturbs the \
                 numerics on its own. Rollback correctness below is judged \
                 against this arm's OWN full prefill, which is the right \
                 comparison; but any production flip would also have to \
                 accept this shift.",
                a.n_rs_seq
            );
        }
    }

    for a in &deep_enough {
        assert!(
            a.accepted,
            "n_rs_seq={} is deep enough for a {}-token rollback but seq_rm \
             still REFUSED. Either this arch is absent from \
             llm_arch_supports_rs_rollback (it lists QWEN35, QWEN35MOE, \
             DEEPSEEK4 — check `arch={arch}`), or cparams clamped n_rs_seq \
             to 0 at context construction.",
            a.n_rs_seq, a.rollback_needed
        );
        assert_eq!(
            a.matches_same_params,
            Some(true),
            "n_rs_seq={}: partial keep was ACCEPTED but the continuation \
             DIVERGED from a full prefill ON THE SAME CONTEXT PARAMS — so \
             this is the rollback itself, not the ring's numerics. That is \
             the outcome worse than the veto: a silent quality regression \
             rather than a loud failure. `prefix_cache_gate` must stay.",
            a.n_rs_seq
        );
        let speedup = a.full_ms as f64 / a.reuse_ms.max(1) as f64;
        eprintln!(
            "n_rs_seq={}: partial keep CORRECT and {speedup:.2}x vs full prefill \
             ({} ms → {} ms)",
            a.n_rs_seq, a.full_ms, a.reuse_ms
        );
    }
    eprintln!(
        "\nPARTIAL KEEP IS AVAILABLE ON arch={arch} with a deep enough \
         snapshot ring. Read the `RS buffer size` / `rs_seq` lines above for \
         what each depth cost, then decide whether prefix_cache_gate should \
         consult a MEASURED rollback budget instead of an arch string."
    );
}

/// **Discriminating probe: is the rollback state-faithful AT ALL, and
/// if not, does it break past some depth?**
///
/// The sweep above changes two things at once (rollback depth AND the
/// diverging suffix), so a mismatch there cannot say whether the
/// recurrent state is restored wrongly or merely restored to a
/// slightly different place. This is the clean identity:
///
///   prefill P → generate → reference continuation
///   prefill P → roll back D → RE-DECODE THE SAME D TOKENS → continue
///
/// The second path removes and then replays identical tokens at
/// identical positions, so a faithful memory module MUST produce the
/// reference continuation exactly. Any divergence is the rollback
/// corrupting recurrent state, with no confound left. Sweeping D shows
/// whether there is a usable shallow regime (which would explain why
/// MTP's ≤4-token draft rollback works in production) or none at all.
#[test]
#[ignore = "manual spike: needs SPIKE_GGUF pointing at a local hybrid gguf"]
fn rollback_then_replay_is_an_identity_at_each_depth() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_test_writer()
        .try_init();

    let Ok(gguf) = std::env::var("SPIKE_GGUF") else {
        eprintln!("SKIP: set SPIKE_GGUF to a local gguf path");
        return;
    };
    const RING: u32 = 64;
    const DEPTHS: [usize; 6] = [1, 2, 4, 8, 16, 32];

    let backend = LlamaBackend::init().expect("backend");
    let model_params = std::pin::pin!(LlamaModelParams::default().with_n_gpu_layers(1_000_000));
    let model = LlamaModel::load_from_file(&backend, &gguf, &model_params).expect("model load");
    let arch = model
        .meta_val_str("general.architecture", 64)
        .unwrap_or_else(|_| "<unknown>".into());

    let prompt = prompt_with_question("Which volume holds the autumn survey of sector 14?");
    let tokens = model
        .str_to_token(&prompt, AddBos::Always)
        .expect("tokenize");
    eprintln!(
        "\n=== rollback/replay identity · arch={arch} · n_rs_seq={RING} · \
         prompt={} tok ===",
        tokens.len()
    );

    // Reference: straight full prefill + continuation, same params.
    let mut ref_ctx = model
        .new_context(&backend, ctx_params(RING))
        .expect("reference ctx");
    let reference = decode_tail_and_continue(&mut ref_ctx, &tokens, 0, CONTINUATION_TOKENS);
    drop(ref_ctx);

    // `gen_before` is the discriminator. The prompt is decoded as ONE
    // multi-token ubatch; generated tokens are single-token decodes.
    // With gen_before=0 the rollback stays inside the prompt's own
    // decode pass; with gen_before>0 it must unwind those single-token
    // passes and then re-enter the prompt pass. If fidelity depends on
    // this, the bug is about crossing decode-pass boundaries, not about
    // depth — and that distinction decides whether the zero-generation
    // workloads (the forced-choice grounding judge emits NO completion
    // tokens) can use partial keep even though chat cannot.
    let mut rows: Vec<(usize, usize, bool)> = Vec::new();
    for gen_before in [0usize, 4usize] {
        for d in DEPTHS {
            assert!(d < tokens.len(), "depth {d} exceeds the prompt");
            let mut ctx = model
                .new_context(&backend, ctx_params(RING))
                .expect("probe ctx");
            // Prefill the whole prompt (logits on its last token).
            let mut batch = LlamaBatch::new(tokens.len(), 1);
            for (i, tok) in tokens.iter().enumerate() {
                batch
                    .add(*tok, i as i32, &[0], i == tokens.len() - 1)
                    .expect("batch add prompt");
            }
            ctx.decode(&mut batch).expect("prefill");

            // Optionally generate first, so the rollback has to unwind
            // single-token decode passes before reaching the prompt's.
            let mut extra = 0usize;
            if gen_before > 0 {
                let sampler = LlamaSampler::greedy();
                let mut pos = tokens.len() as i32;
                let mut next = sampler.sample(&ctx, -1);
                for _ in 0..gen_before {
                    let mut b = LlamaBatch::new(1, 1);
                    b.add(next, pos, &[0], true).expect("batch add gen");
                    ctx.decode(&mut b).expect("decode gen");
                    pos += 1;
                    next = sampler.sample(&ctx, -1);
                    extra += 1;
                }
            }

            // Roll back to `keep`, discarding the generated tail (if
            // any) plus `d` prompt tokens.
            let keep = tokens.len() - d;
            let rollback_span = tokens.len() + extra - keep;
            let accepted = ctx
                .clear_kv_cache_seq(Some(0), Some(keep as u32), None)
                .expect("seq_rm conversion");
            if !accepted {
                eprintln!(
                    "gen_before={gen_before} depth={d:<3} span={rollback_span:<3} \
                     REFUSED (ring={RING}) — cannot judge fidelity"
                );
                continue;
            }
            // ...then replay exactly those prompt tokens at the same
            // positions and continue. A faithful rollback makes the
            // whole detour a no-op.
            let replayed = decode_tail_and_continue(&mut ctx, &tokens, keep, CONTINUATION_TOKENS);
            let identical = replayed == reference;
            let first_div = replayed
                .iter()
                .zip(reference.iter())
                .position(|(a, b)| a != b);
            eprintln!(
                "gen_before={gen_before} depth={d:<3} span={rollback_span:<3} \
                 identical={:<5} first_divergent_token={:?}",
                identical, first_div
            );
            rows.push((gen_before, d, identical));
        }
    }

    let faithful_depths: Vec<usize> = rows
        .iter()
        .filter(|(g, _, ok)| *g == 0 && *ok)
        .map(|(_, d, _)| *d)
        .collect();
    let corrupt_depths: Vec<usize> = rows
        .iter()
        .filter(|(g, _, ok)| *g == 0 && !*ok)
        .map(|(_, d, _)| *d)
        .collect();
    let gen_faithful: Vec<usize> = rows
        .iter()
        .filter(|(g, _, ok)| *g > 0 && *ok)
        .map(|(_, d, _)| *d)
        .collect();
    let gen_corrupt: Vec<usize> = rows
        .iter()
        .filter(|(g, _, ok)| *g > 0 && !*ok)
        .map(|(_, d, _)| *d)
        .collect();
    eprintln!("\ngen_before=0 → faithful {faithful_depths:?} · corrupt {corrupt_depths:?}");
    eprintln!("gen_before=4 → faithful {gen_faithful:?} · corrupt {gen_corrupt:?}");
    if corrupt_depths.is_empty() && !gen_corrupt.is_empty() {
        eprintln!(
            "\n>>> LOCALISED: rollback is faithful when it stays INSIDE one \
             decode pass, and corrupts state when it must unwind generated \
             single-token passes first. Depth is not the variable — the \
             decode-pass boundary is."
        );
    }

    eprintln!("\n--- identity verdict ---");
    // The within-pass regime is a genuine invariant: MTP's draft
    // rollback lives here, so a regression would be load-bearing.
    assert!(
        corrupt_depths.is_empty(),
        "REGRESSION: rollback is not faithful even WITHIN a single decode \
         pass on arch={arch} — corrupt at {corrupt_depths:?}, faithful at \
         {faithful_depths:?}. MTP's draft/verify rollback depends on this \
         regime; investigate before trusting speculative decoding output."
    );
    // The cross-pass regime is the one that would buy prefix caching,
    // and it is the finding. A spike that printed "viable" while its own
    // table showed corruption would be exactly the false green this
    // codebase keeps getting bitten by.
    assert!(
        gen_corrupt.is_empty(),
        "PARTIAL KEEP IS NOT SAFE FOR PREFIX CACHING on arch={arch}.\n\
         Within one decode pass, rollback is faithful at every probed depth \
         {faithful_depths:?}. But once ANY token has been generated before \
         the rollback — i.e. every real second request, which must unwind \
         the previous turn's single-token decode passes — the replay of \
         IDENTICAL tokens at IDENTICAL positions produces a DIFFERENT \
         continuation: corrupt at depth(s) {gen_corrupt:?}, apparently \
         clean at {gen_faithful:?}.\n\
         Treat the 'apparently clean' set as luck, not safety: the pattern \
         is deterministic run-to-run, so the state is wrong in all of them \
         and greedy argmax merely absorbs the perturbation at some depths. \
         An intermittent, depth-dependent, SILENT quality regression is \
         strictly worse than the full prefill it would replace.\n\
         CONCLUSION: `prefix_cache_gate` stays, and `n_rs_seq` must NOT be \
         raised on chat contexts to chase prefix caching. The ring costs \
         (1 + n_rs_seq)x the recurrent buffer besides — measured 19 MiB -> \
         1252 MiB at n_rs_seq=64 on a 2B. Full-state save/restore \
         (`prefix_state.rs`) remains the sound reuse path on these arches."
    );
    eprintln!(
        "rollback is state-faithful in BOTH regimes — partial keep may be viable; \
         re-check the memory cost before acting"
    );
}

/// **Instrument validation for the production capability probe** (§18.1
/// — a check nobody has watched fail is not a check).
///
/// `embedded::capabilities::probe_partial_kv` is what decides, at slot
/// load, whether partial keep is safe. Its whole reason for existing is
/// to catch `AcceptedButUnfaithful` — llama.cpp saying "I truncated"
/// while the state is wrong. That state **cannot be reached through the
/// normal chat-slot config** (`n_rs_seq=0` refuses outright), so
/// without this test the probe would ship never having been shown the
/// failure it was built for.
///
/// Sweeping `n_rs_seq` here reaches both regimes on one hybrid:
///   - `n_rs_seq=0`  → refused outright  → `Refused`
///   - `n_rs_seq=64` → accepted, corrupt → `AcceptedButUnfaithful`
///
/// On a pure-attention model the same probe must return `Safe` — run it
/// with `SPIKE_GGUF` pointing at gemma/llama to check the other side.
#[test]
#[ignore = "manual spike: needs SPIKE_GGUF pointing at a local gguf"]
fn capability_probe_detects_both_regimes() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_test_writer()
        .try_init();

    let Ok(gguf) = std::env::var("SPIKE_GGUF") else {
        eprintln!("SKIP: set SPIKE_GGUF to a local gguf path");
        return;
    };
    let backend = LlamaBackend::init().expect("backend");
    let model_params = std::pin::pin!(LlamaModelParams::default().with_n_gpu_layers(1_000_000));
    let model = LlamaModel::load_from_file(&backend, &gguf, &model_params).expect("model load");
    let arch = model
        .meta_val_str("general.architecture", 64)
        .unwrap_or_else(|_| "<unknown>".into());
    eprintln!("\n=== capability probe · arch={arch} ===");

    let mut verdicts = Vec::new();
    for n_rs_seq in [0u32, 64u32] {
        let mut ctx = model
            .new_context(&backend, ctx_params(n_rs_seq))
            .expect("probe ctx");
        let t = Instant::now();
        let v = probe_partial_kv(&model, &mut ctx);
        eprintln!(
            "n_rs_seq={n_rs_seq:<4} verdict={:<24} ({} ms)",
            v.label(),
            t.elapsed().as_millis()
        );
        verdicts.push((n_rs_seq, v));
    }

    // Whatever the arch, the probe must never return Safe on a config
    // the rollback sweep proved corrupting.
    for (n, v) in &verdicts {
        assert!(
            !matches!(v, PartialKvVerdict::CouldNotJudge { .. }),
            "n_rs_seq={n}: probe could not judge — it is supposed to be able to run \
             here; a could-not-judge would silently veto forever and look like caution"
        );
    }

    let is_hybrid_family = arch.starts_with("qwen35") || arch.contains("moe");
    if is_hybrid_family {
        assert_eq!(
            verdicts[0].1,
            PartialKvVerdict::Refused,
            "hybrid at n_rs_seq=0 must be Refused — that is production's config, and \
             the 2026-08-10 gate repro confirms it on both qwen35 and qwen35moe"
        );
        // Both hybrids on this host are corroborated CORRUPT in the
        // cross-pass regime by the fuller `rollback_then_replay…` sweep
        // (`qwen35` corrupt at [1,2,16]; `qwen35moe` corrupt at every
        // depth), so this is an assertion backed by measurement rather
        // than a family-level guess.
        //
        // It is also the regression guard for the detector itself: the
        // TOKEN-based detector reported `Safe` here on the 36B — a false
        // negative that would have cleared a model that silently
        // degrades answers. If this ever goes green-by-Safe again, the
        // logit detector has lost its sensitivity.
        assert_eq!(
            verdicts[1].1,
            PartialKvVerdict::AcceptedButUnfaithful,
            "n_rs_seq=64 on {arch}: the rollback sweep proves this config ACCEPTS the \
             truncation and CORRUPTS the state. A `Safe` here is the false negative the \
             logit detector was built to eliminate — check NOISE_RATIO_LIMIT and the \
             floor/signal arms in `capabilities::probe_inner`. (If this is a NEW hybrid \
             the sweep has not covered, run `rollback_then_replay_is_an_identity_at_each_depth` \
             on it before assuming the probe is at fault — capability is per-model.)"
        );
        eprintln!(
            "probe detects BOTH regimes on this model — instrument validated \
             (accepted-but-unfaithful reproduced at n_rs_seq=64)"
        );
    } else {
        assert_eq!(
            verdicts[0].1,
            PartialKvVerdict::Safe,
            "pure-attention must probe Safe; a false Refused costs a full prefill on \
             every turn for nothing"
        );
        eprintln!("probe clears this pure-attention model — control arm validated");
    }
}

/// **Shadow-data collector for the commit-2 flip** — the measurement
/// the plan gates the flip on, not a check.
///
/// It answers one question per model: *would flipping
/// `prefix_cache_gate` onto the measured verdict change what this
/// machine does?* Every row goes through
/// `ModelCapabilities::observe`, the same assembly the slot-load path
/// uses, so a row cannot report agreement that production would not
/// see.
///
/// Production config only (`n_rs_seq=0`) — the regime-sweep to 64 is
/// `capability_probe_detects_both_regimes`'s job.
///
/// ```sh
/// SPIKE_GGUFS="sovereign/models/a.gguf,sovereign/models/b.gguf" \
///   cargo test -p sovereign-inference --test rs_rollback_spike --release \
///   -- --ignored --nocapture capability_shadow_sweep
/// ```
///
/// A model that fails to LOAD is reported as its own outcome and does
/// not abort the sweep — losing eight rows because the ninth has no
/// weights would be the §18.2 failure (never-ran read as passed).
#[test]
#[ignore = "manual sweep: needs SPIKE_GGUFS pointing at local gguf paths"]
fn capability_shadow_sweep() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_test_writer()
        .try_init();

    let Ok(list) = std::env::var("SPIKE_GGUFS") else {
        eprintln!("SKIP: set SPIKE_GGUFS to a comma-separated list of gguf paths");
        return;
    };
    let paths: Vec<&str> = list
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    // GPU offload is all-layers by default. `SPIKE_NGL=0` runs a model
    // that does not fit in available memory off its mmap instead — the
    // verdict is a property of the memory module's rollback semantics,
    // and the detector is a per-model RATIO, so it survives the kernel
    // change that CPU execution brings.
    let n_gpu_layers: u32 = std::env::var("SPIKE_NGL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);
    let backend = LlamaBackend::init().expect("backend");

    eprintln!(
        "\n=== capability shadow sweep · {} model(s) · n_rs_seq=0 ===",
        paths.len()
    );
    eprintln!(
        "{:<44} {:<16} {:<7} {:<7} {:<7} {:<24} {:<8} {:>6}",
        "model", "arch", "declared", "libllama", "ladder", "probe", "contra?", "ms"
    );

    let mut load_failures = Vec::new();
    let mut could_not_judge = Vec::new();
    let mut disagreements = Vec::new();
    let mut ladder_wrong = Vec::new();
    let mut ladder_load_bearing = Vec::new();

    for path in &paths {
        let name = std::path::Path::new(path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| (*path).to_string());
        let model_params =
            std::pin::pin!(LlamaModelParams::default().with_n_gpu_layers(n_gpu_layers));
        let model = match LlamaModel::load_from_file(&backend, path, &model_params) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("{name:<44} {:<16} {e}", "LOAD-FAILED");
                load_failures.push((name, e.to_string()));
                continue;
            }
        };
        let arch = model
            .meta_val_str("general.architecture", 64)
            .unwrap_or_else(|_| "<unknown>".into());
        let mut ctx = match model.new_context(&backend, ctx_params(0)) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{name:<44} {arch:<16} CTX-FAILED {e}");
                load_failures.push((name, format!("context: {e}")));
                continue;
            }
        };
        let t = Instant::now();
        let verdict = probe_partial_kv(&model, &mut ctx);
        let ms = t.elapsed().as_millis();
        let caps = ModelCapabilities::observe(&model, &arch, verdict, Provenance::Measured);

        if matches!(caps.partial_kv, PartialKvVerdict::CouldNotJudge { .. }) {
            could_not_judge.push(name.clone());
        }
        if caps.measurement_contradicts_declaration() {
            disagreements.push((name.clone(), arch.clone(), caps.partial_kv.label()));
        }
        if caps.arch_ladder_alone_is_wrong() {
            ladder_wrong.push((name.clone(), arch.clone()));
        }
        // The ladder is load-bearing only where it vetoes something
        // libllama's flags would have cleared. Anywhere else it is a
        // clause that cannot change the answer.
        if caps.arch_ladder_alone_says_unsafe && !caps.libllama_flags_say_unsafe {
            ladder_load_bearing.push((name.clone(), arch.clone()));
        }
        eprintln!(
            "{name:<44} {arch:<16} {:<7} {:<7} {:<7} {:<24} {:<8} {ms:>6}",
            if caps.declarations_say_unsafe {
                "veto"
            } else {
                "allow"
            },
            if caps.libllama_flags_say_unsafe {
                "veto"
            } else {
                "allow"
            },
            if caps.arch_ladder_alone_says_unsafe {
                "veto"
            } else {
                "allow"
            },
            caps.partial_kv.label(),
            if caps.measurement_contradicts_declaration() {
                "CONTRA"
            } else {
                "agree"
            },
        );
    }

    eprintln!("\n--- shadow summary ---");
    eprintln!(
        "models swept ............ {}",
        paths.len() - load_failures.len()
    );
    eprintln!("load/ctx failures ....... {}", load_failures.len());
    eprintln!("could-not-judge ......... {}", could_not_judge.len());
    eprintln!("measurement CONTRADICTS .. {}", disagreements.len());
    for (n, a, v) in &disagreements {
        eprintln!("    {n} ({a}) — probe says {v}, the declarations disagree");
    }
    eprintln!("arch ladder alone wrong . {}", ladder_wrong.len());
    for (n, a) in &ladder_wrong {
        eprintln!("    {n} ({a})");
    }
    // The question that decides whether the ladder can lose its
    // authority: is there any model it vetoes that libllama's flags
    // would clear? An empty list means the ladder never changes an
    // answer on this zoo.
    eprintln!("ladder LOAD-BEARING ..... {}", ladder_load_bearing.len());
    for (n, a) in &ladder_load_bearing {
        eprintln!("    {n} ({a}) — ladder vetoes, libllama flags do not");
    }

    // The sweep is a measurement, so it asserts only on things that
    // would make the measurement itself worthless.
    assert!(
        load_failures.is_empty(),
        "models failed to load — their rows are MISSING, not passing (§18.2): {load_failures:?}"
    );
    assert!(
        could_not_judge.is_empty(),
        "probe could not judge {could_not_judge:?} — a could-not-judge vetoes forever and \
         reads as caution; find out why before treating this sweep as coverage"
    );
}

/// **Diagnostic: which probe parameter decides whether corruption is
/// visible?**
///
/// 2026-08-10: `capabilities::probe_partial_kv` (prefill=192,
/// generate=2, rollback=16, continue=6) reported **Safe** on
/// `qwen35moe` at `n_rs_seq=64`, while the fuller sweep
/// (prefill=1425, generate=4, continue=24) showed that model corrupt at
/// EVERY depth. A false negative in the probe is the worst outcome
/// available — it would clear a model that silently degrades answers.
///
/// This isolates the difference instead of guessing at it: hold depth
/// fixed and vary prefill length and pre-rollback generation.
#[test]
#[ignore = "manual spike: needs SPIKE_GGUF pointing at a local hybrid gguf"]
fn probe_parameter_sensitivity() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_test_writer()
        .try_init();
    let Ok(gguf) = std::env::var("SPIKE_GGUF") else {
        eprintln!("SKIP: set SPIKE_GGUF to a local gguf path");
        return;
    };
    const RING: u32 = 64;
    const DEPTH: usize = 16;
    const CONT: usize = 24;

    let backend = LlamaBackend::init().expect("backend");
    let model_params = std::pin::pin!(LlamaModelParams::default().with_n_gpu_layers(1_000_000));
    let model = LlamaModel::load_from_file(&backend, &gguf, &model_params).expect("model load");
    let arch = model
        .meta_val_str("general.architecture", 64)
        .unwrap_or_else(|_| "<unknown>".into());
    let all = model
        .str_to_token(
            &prompt_with_question("which volume covers sector 14?"),
            AddBos::Always,
        )
        .expect("tokenize");
    eprintln!("\n=== probe parameter sensitivity · arch={arch} · depth={DEPTH} · ring={RING} ===");

    for prefill_len in [192usize, 512, all.len().min(1425)] {
        if prefill_len > all.len() || prefill_len <= DEPTH + 8 {
            continue;
        }
        let tokens = &all[..prefill_len];
        for gen_before in [0usize, 1, 2, 4] {
            // Reference: straight prefill + continue, same params.
            let mut rc = model.new_context(&backend, ctx_params(RING)).expect("ctx");
            let reference = decode_tail_and_continue(&mut rc, tokens, 0, CONT);
            drop(rc);

            let mut ctx = model.new_context(&backend, ctx_params(RING)).expect("ctx");
            let mut batch = LlamaBatch::new(tokens.len(), 1);
            for (i, t) in tokens.iter().enumerate() {
                batch
                    .add(*t, i as i32, &[0], i == tokens.len() - 1)
                    .expect("add");
            }
            ctx.decode(&mut batch).expect("prefill");
            if gen_before > 0 {
                let sampler = LlamaSampler::greedy();
                let mut pos = tokens.len() as i32;
                let mut next = sampler.sample(&ctx, -1);
                for _ in 0..gen_before {
                    let mut b = LlamaBatch::new(1, 1);
                    b.add(next, pos, &[0], true).expect("add");
                    ctx.decode(&mut b).expect("decode");
                    pos += 1;
                    next = sampler.sample(&ctx, -1);
                }
            }
            let keep = tokens.len() - DEPTH;
            let accepted = ctx
                .clear_kv_cache_seq(Some(0), Some(keep as u32), None)
                .expect("seq_rm");
            let verdict = if !accepted {
                "refused".to_string()
            } else {
                let replayed = decode_tail_and_continue(&mut ctx, tokens, keep, CONT);
                if replayed == reference {
                    "FAITHFUL".to_string()
                } else {
                    let d = replayed
                        .iter()
                        .zip(reference.iter())
                        .position(|(a, b)| a != b);
                    format!("CORRUPT (first divergence at {d:?})")
                }
            };
            eprintln!("prefill={prefill_len:<5} gen_before={gen_before} → {verdict}");
        }
    }
    eprintln!(
        "\nRead the rows: any (prefill, gen_before) that is CORRUPT but which the \
         production probe's constants do NOT exercise is a false-negative hole in \
         `capabilities::probe_partial_kv`."
    );
}

fn softmax(logits: &[f32]) -> Vec<f32> {
    let m = logits.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let mut e: Vec<f32> = logits.iter().map(|x| (x - m).exp()).collect();
    let s: f32 = e.iter().sum();
    for v in &mut e {
        *v /= s;
    }
    e
}

fn argmax(v: &[f32]) -> usize {
    v.iter()
        .enumerate()
        .fold((0usize, f32::NEG_INFINITY), |(bi, bv), (i, &x)| {
            if x > bv {
                (i, x)
            } else {
                (bi, bv)
            }
        })
        .0
}

/// **Calibration for a LOGIT-based capability detector.**
///
/// The sampled-token detector has a measured false-negative rate
/// (`probe_parameter_sensitivity`): greedy argmax absorbs a perturbed
/// state whenever the top token wins by more than the perturbation, so
/// "same tokens" does not mean "same state". The logit vector is the
/// direct observable.
///
/// The threshold cannot be guessed, so this measures the separation
/// first (§18.4 — validate the instrument before the result):
///
///   - `gen_before=0` is a CORRECT rollback that still changes batch
///     shape (a 16-token replay batch vs a 192-token prefill batch).
///     Its delta is the pure float-NOISE FLOOR.
///   - `gen_before>=1` crosses the decode-pass boundary. On a
///     corrupting model this is the SIGNAL.
///
/// A usable detector needs signal >> floor. If they overlap, logits are
/// no better than tokens and the whole approach is wrong — which is a
/// finding, not a failure.
#[test]
#[ignore = "manual spike: needs SPIKE_GGUF pointing at a local gguf"]
fn logit_delta_calibration() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_test_writer()
        .try_init();
    let Ok(gguf) = std::env::var("SPIKE_GGUF") else {
        eprintln!("SKIP: set SPIKE_GGUF to a local gguf path");
        return;
    };
    const RING: u32 = 64;
    const P: usize = 192;
    const D: usize = 16;

    let backend = LlamaBackend::init().expect("backend");
    let model_params = std::pin::pin!(LlamaModelParams::default().with_n_gpu_layers(1_000_000));
    let model = LlamaModel::load_from_file(&backend, &gguf, &model_params).expect("model load");
    let arch = model
        .meta_val_str("general.architecture", 64)
        .unwrap_or_else(|_| "<unknown>".into());
    let all = model
        .str_to_token(
            &prompt_with_question("which volume covers sector 14?"),
            AddBos::Always,
        )
        .expect("tokenize");
    let tokens = &all[..P];
    eprintln!(
        "\n=== logit-delta calibration · arch={arch} · prefill={P} depth={D} ring={RING} ==="
    );
    eprintln!("gen_before=0 is the NOISE FLOOR (correct rollback, different batch shape)");
    eprintln!(
        "{:<10} {:>12} {:>12} {:>12} {:>8}",
        "gen_before", "max|dlogit|", "max|dprob|", "L2", "top1"
    );

    for gen_before in [0usize, 1, 2, 4] {
        // Reference: straight prefill, logits at the last position.
        let mut rc = model.new_context(&backend, ctx_params(RING)).expect("ctx");
        let mut b = LlamaBatch::new(P, 1);
        for (i, t) in tokens.iter().enumerate() {
            b.add(*t, i as i32, &[0], i == P - 1).expect("add");
        }
        rc.decode(&mut b).expect("ref prefill");
        let l_ref: Vec<f32> = rc.get_logits().to_vec();
        drop(rc);

        // Test: prefill, optionally generate, roll back, replay.
        let mut ctx = model.new_context(&backend, ctx_params(RING)).expect("ctx");
        let mut b = LlamaBatch::new(P, 1);
        for (i, t) in tokens.iter().enumerate() {
            b.add(*t, i as i32, &[0], i == P - 1).expect("add");
        }
        ctx.decode(&mut b).expect("prefill");
        if gen_before > 0 {
            let sampler = LlamaSampler::greedy();
            let mut pos = P as i32;
            let mut next = sampler.sample(&ctx, -1);
            for _ in 0..gen_before {
                let mut bb = LlamaBatch::new(1, 1);
                bb.add(next, pos, &[0], true).expect("add");
                ctx.decode(&mut bb).expect("gen");
                pos += 1;
                next = sampler.sample(&ctx, -1);
            }
        }
        let keep = P - D;
        let accepted = ctx
            .clear_kv_cache_seq(Some(0), Some(keep as u32), None)
            .expect("seq_rm");
        if !accepted {
            eprintln!("{gen_before:<10} {:>12}", "REFUSED");
            continue;
        }
        let mut bb = LlamaBatch::new(D, 1);
        for (i, t) in tokens[keep..].iter().enumerate() {
            bb.add(*t, (keep + i) as i32, &[0], keep + i == P - 1)
                .expect("add");
        }
        ctx.decode(&mut bb).expect("replay");
        let l_test: Vec<f32> = ctx.get_logits().to_vec();

        let max_dl = l_ref
            .iter()
            .zip(&l_test)
            .map(|(a, b)| (a - b).abs())
            .fold(0f32, f32::max);
        let p_ref = softmax(&l_ref);
        let p_test = softmax(&l_test);
        let max_dp = p_ref
            .iter()
            .zip(&p_test)
            .map(|(a, b)| (a - b).abs())
            .fold(0f32, f32::max);
        let l2: f32 = l_ref
            .iter()
            .zip(&l_test)
            .map(|(a, b)| (a - b) * (a - b))
            .sum::<f32>()
            .sqrt();
        let top1 = argmax(&l_ref) == argmax(&l_test);
        eprintln!(
            "{gen_before:<10} {max_dl:>12.6} {max_dp:>12.6} {l2:>12.4} {:>8}",
            if top1 { "same" } else { "DIFF" }
        );
    }
    eprintln!(
        "\nPick the threshold from the gap between the gen_before=0 row and the rest. \
         If there is no gap, logits are no better than tokens here."
    );
}
