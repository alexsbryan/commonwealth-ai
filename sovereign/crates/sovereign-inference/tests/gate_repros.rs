// SPDX-License-Identifier: AGPL-3.0-or-later
//! Canonical repros for the hazards behind the `embedded/gates.rs`
//! decision gates.
//!
//! Every gate in `gates.rs` is a WORKAROUND that forfeits performance
//! (prefix cache off → full prefill every turn; FastShort off → no
//! continuous batching). Each repro here exercises the underlying
//! hazard with the gate's diagnostic force-override enabled, against
//! a real model. Run them when bumping llama.cpp / llama-cpp-4, or
//! when suspecting a gate is over-applied.
//!
//! **Verdict semantics:**
//! - Repro test PASSES → the hazard still reproduces; the gate is
//!   still justified. (Status quo — nothing to do.)
//! - Repro test FAILS with "HAZARD NO LONGER REPRODUCES" → upstream
//!   (llama.cpp or the binding) fixed the underlying bug for the
//!   model you pointed it at. That's actionable good news: consider
//!   narrowing or removing the corresponding gate in
//!   `src/embedded/gates.rs`, gate-test matrix and all.
//!
//! **How to run** (never in CI — needs real GGUF weights):
//! ```sh
//! ./scripts/gate-repros.sh \
//!     --recurrent ~/models/Qwen3.5-2B.Q6_K.gguf \
//!     --attention ~/models/Qwen3-4B.Q8_0.gguf
//! # or directly:
//! SOVEREIGN_REPRO_RECURRENT_GGUF=... SOVEREIGN_REPRO_ATTENTION_GGUF=... \
//!   cargo test -p sovereign-inference --test gate_repros --release \
//!   -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! `--test-threads=1` is REQUIRED: the repros toggle process-global
//! `SOVEREIGN_*` env flags around each scenario.
//!
//! | Env var | Points at | Drives |
//! |---|---|---|
//! | `SOVEREIGN_REPRO_RECURRENT_GGUF` | a recurrent or hybrid NON-MTP chat model (dense Qwen3.5, qwen*moe, mamba…) — one `prefix_cache_gate` vetoes today | prefix-cache hazard repro |
//! | `SOVEREIGN_REPRO_FASTSHORT_GGUF` | a model `fast_short_gate` STILL vetoes after the 2026-06-11 narrowing (mamba/rwkv/deltanet/ssm arch). Falls back to `SOVEREIGN_REPRO_RECURRENT_GGUF` | FastShort hazard repro (force-cleared) |
//! | `SOVEREIGN_REPRO_FASTSHORT_CLEARED_GGUF` | a model the narrowing CLEARED (qwen*moe like APEX, or an MTP-by-name model) | the burst canary — guards the narrowing itself |
//! | `SOVEREIGN_REPRO_ATTENTION_GGUF` | a pure-attention model (qwen3, gemma, llama…) | the harness-sanity control |
//!
//! The prefix-cache repros run with `SOVEREIGN_FAST_SHORT_DISABLE=1`:
//! small requests would otherwise route to the FastShort batched
//! slot, whose decode path never exercises the partial-KV-keep code
//! under test.
//!
//! **Hazards not reproducible here** (tracked in `gates.rs` unit
//! tests instead): tools-never-enter-MTP is a missing *feature*
//! (the `ToolStopTracker` isn't threaded through the MTP verify
//! loop), not an upstream bug — "fixed" means someone does that
//! threading, at which point the `mtp_dispatch_eligible` gate and
//! its tests are updated deliberately, not discovered via repro.

use std::path::PathBuf;

use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, Speed};
use sovereign_inference::embedded::EmbeddedLlamaCpp;

/// The repros adjudicate via log lines (force-override warns, the
/// FastShort construction-failure fallback) — make them visible.
fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_test_writer()
        .try_init();
}

/// Read a repro model path from the environment; None skips the test
/// with a loud message (an #[ignore]d test that silently passes when
/// unconfigured would look like a verdict).
fn repro_model(var: &str) -> Option<PathBuf> {
    match std::env::var(var) {
        Ok(p) if !p.is_empty() => {
            let path = PathBuf::from(p);
            assert!(
                path.exists(),
                "{var} points at {path:?} which does not exist"
            );
            Some(path)
        }
        _ => {
            eprintln!("SKIP: {var} not set — see module doc for how to run gate repros");
            None
        }
    }
}

/// Sets `flag=1` for the guard's lifetime, restoring the prior state
/// on drop (even on panic) so the next repro doesn't inherit a stale
/// force. Tests run with `--test-threads=1`; env is process-global.
struct EnvFlagGuard {
    flag: String,
    prior: Option<String>,
}

impl EnvFlagGuard {
    fn set(flag: &str) -> Self {
        let prior = std::env::var(flag).ok();
        std::env::set_var(flag, "1");
        Self {
            flag: flag.to_string(),
            prior,
        }
    }
}

impl Drop for EnvFlagGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(v) => std::env::set_var(&self.flag, v),
            None => std::env::remove_var(&self.flag),
        }
    }
}

/// A prompt long enough that two requests sharing it produce a
/// non-trivial token LCP (the partial-keep path), but well inside the
/// 2048-token default context.
fn shared_prefix() -> String {
    "You are a careful assistant. Consider the following background before \
     answering. The quick brown fox jumps over the lazy dog. "
        .repeat(8)
}

fn small_request(prompt: String) -> CompletionRequest {
    let mut r = CompletionRequest::new(&prompt);
    r.max_tokens = Some(8);
    // CompletionRequest::new defaults to Speed::Slow (primary);
    // FastShort routing (pick_slot gate 2) requires Fast — without
    // this the FastShort repro silently exercises the single-token slot.
    r.preferred_speed = Speed::Fast;
    r
}

/// **Hazard: partial KV keep on recurrent/hybrid architectures.**
///
/// Gate: `gates::prefix_cache_gate` (clauses 0–2). Incidents:
/// `Decode Error -1` on tail decode (Qwen3.6-MoE APEX, 2026-05-20;
/// dense Qwen3.5 hybrid, 2026-06-09). Cost of the gate: full prefill
/// on every turn for these models — if this stops reproducing, that
/// cost is being paid for nothing.
#[tokio::test]
#[ignore = "needs real GGUF weights — see module doc"]
async fn prefix_cache_partial_keep_hazard_still_reproduces() {
    init_tracing();
    let Some(model) = repro_model("SOVEREIGN_REPRO_RECURRENT_GGUF") else {
        return;
    };
    let _force = EnvFlagGuard::set("SOVEREIGN_PREFIX_CACHE_FORCE");
    // Route to the single-token fast slot — FastShort's batched path
    // doesn't run the partial-keep code under test.
    let _no_fast_short = EnvFlagGuard::set("SOVEREIGN_FAST_SHORT_DISABLE");
    let engine = EmbeddedLlamaCpp::load(&model).expect("model load failed — wrong path?");
    let prefix = shared_prefix();

    // First call: cold cache, full prefill — must succeed on any
    // loadable model (this is not the hazard yet).
    let first = engine
        .complete(&small_request(format!(
            "{prefix}\nQuestion one: what animal jumps?"
        )))
        .await;
    assert!(
        first.is_ok(),
        "first (cold, full-prefill) completion must succeed; load/setup \
         problem, not the hazard: {:?}",
        first.err()
    );

    // Second call shares the prefix → lcp > 0 → partial keep (forced
    // past the gate). THIS is the hazard site.
    let second = engine
        .complete(&small_request(format!(
            "{prefix}\nQuestion two: what animal sleeps?"
        )))
        .await;
    assert!(
        second.is_err(),
        "HAZARD NO LONGER REPRODUCES: partial KV keep succeeded on a \
         recurrent/hybrid model with SOVEREIGN_PREFIX_CACHE_FORCE=1. \
         Upstream may have fixed recurrent-state partial keep — consider \
         narrowing `gates::prefix_cache_gate` (reclaiming the full-prefill \
         cost paid on every turn for these models). Before removing the \
         gate, verify OUTPUT CORRECTNESS too, not just absence of errors: \
         run the same prompts with and without force and compare. Got: {:?}",
        second.map(|r| r.text)
    );
    eprintln!("hazard still reproduces — prefix_cache_gate remains justified");
}

/// Harness-sanity control: the same flow on a pure-attention model
/// must succeed — proving the repro measures the hazard, not a bug in
/// the harness itself. (The blindfold-control discipline from the
/// mechanism-fidelity bench.)
#[tokio::test]
#[ignore = "needs real GGUF weights — see module doc"]
async fn prefix_cache_partial_keep_control_on_attention_model() {
    init_tracing();
    let Some(model) = repro_model("SOVEREIGN_REPRO_ATTENTION_GGUF") else {
        return;
    };
    // Same slot routing as the hazard repro (see there).
    let _no_fast_short = EnvFlagGuard::set("SOVEREIGN_FAST_SHORT_DISABLE");
    let engine = EmbeddedLlamaCpp::load(&model).expect("model load failed — wrong path?");
    let prefix = shared_prefix();
    let first = engine
        .complete(&small_request(format!(
            "{prefix}\nQuestion one: what animal jumps?"
        )))
        .await;
    let second = engine
        .complete(&small_request(format!(
            "{prefix}\nQuestion two: what animal sleeps?"
        )))
        .await;
    assert!(
        first.is_ok(),
        "control first call failed: {:?}",
        first.err()
    );
    assert!(
        second.is_ok(),
        "CONTROL BROKEN: partial keep failed on a pure-attention model — \
         the repro harness (not the gated hazard) has a bug: {:?}",
        second.err()
    );
    eprintln!("control passes — harness measures the hazard, not itself");
}

/// **Hazard: FastShort continuous batching on recurrent/MTP models.**
///
/// Gate: `gates::fast_short_gate`. Incident: `from_existing_model`
/// doesn't propagate `n_rs_seq`, so the FastShort ctx fails its first
/// continuous-batched decode with `Decode Error -3` (2026-05-24).
/// Cost of the gate: these models forfeit the ~2× batched-call
/// speedup on Phase-1b-style workloads. "Fixed" can come from either
/// side: upstream llama.cpp, or someone threading `n_rs_seq` through
/// `from_existing_model` (the long-term fix the gate's comment names).
///
/// CAVEAT: if FastShort fails at CONSTRUCTION (not first decode), the
/// engine warn-logs "failed to build continuous-batched companion"
/// and silently falls back to the `fast` slot — the request then
/// succeeds and this repro reports NOT-REPRODUCING for the wrong
/// reason. Run with `--nocapture` and check for that warn line before
/// acting on a green verdict here.
#[tokio::test]
#[ignore = "needs real GGUF weights — see module doc"]
async fn fast_short_recurrent_batched_decode_hazard_still_reproduces() {
    init_tracing();
    // Needs a model fast_short_gate STILL vetoes after the 2026-06-11
    // narrowing: mamba/rwkv/deltanet/ssm arch. (qwen*moe and
    // MTP-by-name were cleared by the campaign and no longer veto —
    // point those at the burst CANARY below instead.)
    let model = match repro_model("SOVEREIGN_REPRO_FASTSHORT_GGUF") {
        Some(m) => m,
        None => match repro_model("SOVEREIGN_REPRO_RECURRENT_GGUF") {
            Some(m) => m,
            None => return,
        },
    };
    let _force = EnvFlagGuard::set("SOVEREIGN_FAST_SHORT_FORCE");
    // Force builds the FastShort companion despite the unsafe verdict
    // (warn-logged at load). A concurrent burst then routes to it —
    // concurrency matters: the hazard is per-SEQUENCE recurrent state
    // (n_rs_seq), so a single batched call may succeed while a full
    // n_seq_max=8 batch crashes. The incident workloads (Phase 1b
    // pipelines) were exactly such bursts.
    let engine = std::sync::Arc::new(
        EmbeddedLlamaCpp::load(&model).expect("model load failed — wrong path?"),
    );
    let mut joins = Vec::new();
    for i in 0..8 {
        let engine = std::sync::Arc::clone(&engine);
        joins.push(tokio::spawn(async move {
            engine
                .complete(&small_request(format!(
                    "Reply with one word, the name of animal number {i}:"
                )))
                .await
        }));
    }
    let mut errors = Vec::new();
    let mut ok = 0usize;
    for j in joins {
        match j.await.expect("repro task panicked") {
            Ok(_) => ok += 1,
            Err(e) => errors.push(e.to_string()),
        }
    }
    eprintln!("burst of 8: {ok} ok, {} failed", errors.len());
    assert!(
        !errors.is_empty(),
        "HAZARD NO LONGER REPRODUCES: a full n_seq_max burst of 8 concurrent \
         continuous-batched FastShort decodes succeeded on this vetoed arch \
         with SOVEREIGN_FAST_SHORT_FORCE=1 — consider clearing it from the \
         remaining UnsafeRecurrent arm of `gates::fast_short_gate` \
         (reclaiming the batched-call speedup), the way qwen-MoE and \
         MTP-by-name were cleared on 2026-06-11. FIRST check the load log \
         for 'failed to build continuous-batched companion' — a \
         construction-failure fallback to the fast slot also produces this \
         verdict, falsely; the burst must log slot=\"fast_short\"."
    );
    eprintln!(
        "hazard still reproduces ({}/8 failed: {}) — fast_short_gate remains justified",
        errors.len(),
        errors.first().map(String::as_str).unwrap_or("")
    );
}

/// **Burst canary for the 2026-06-11 gate narrowing.** Point this at
/// a model the narrowing CLEARED (qwen*moe like the APEX fast-slot
/// model, or an MTP-by-name model): with NO force flag, the narrowed
/// gate must build FastShort naturally and a saturated 8-concurrent
/// burst must succeed 8/8. If this starts failing — possibly only on
/// shapes the original clearing didn't exercise (long generations,
/// sustained pipelines) — follow the bite-back protocol in
/// `gates::fast_short_gate`'s doc: SOVEREIGN_FAST_SHORT_DISABLE=1 to
/// mitigate, then restore the veto WITH the failing shape added here.
#[tokio::test]
#[ignore = "needs real GGUF weights — see module doc"]
async fn fast_short_cleared_model_burst_canary() {
    init_tracing();
    let Some(model) = repro_model("SOVEREIGN_REPRO_FASTSHORT_CLEARED_GGUF") else {
        return;
    };
    // NO force flag — the point is that the narrowed gate clears this
    // model on its own.
    let engine = std::sync::Arc::new(
        EmbeddedLlamaCpp::load(&model).expect("model load failed — wrong path?"),
    );
    let mut joins = Vec::new();
    for i in 0..8 {
        let engine = std::sync::Arc::clone(&engine);
        joins.push(tokio::spawn(async move {
            // Mix shapes: one LARGE prefill (~1100+ tokens — the
            // original 2026-05-24 incident was `Decode Error -3` at
            // batch_n_tokens=1121, a router-classifier call carrying
            // full conversation context; still under the ~6000-char
            // FastShort routing cap) + seven small calls saturating
            // the remaining sequences.
            let prompt = if i == 0 {
                format!(
                    "Classify the intent of the final question given this \
                     conversation history. {} Final question: what animal \
                     jumps highest?",
                    shared_prefix().repeat(5)
                )
            } else {
                format!("Reply with one word, the name of animal number {i}:")
            };
            engine.complete(&small_request(prompt)).await
        }));
    }
    let mut failures = Vec::new();
    for j in joins {
        if let Err(e) = j.await.expect("canary task panicked") {
            failures.push(e.to_string());
        }
    }
    assert!(
        failures.is_empty(),
        "CLEARED-MODEL CANARY FAILED ({}/8): the 2026-06-11 fast_short_gate \
         narrowing may have been wrong for this model/workload — follow the \
         bite-back protocol in gates::fast_short_gate's doc. First failure: {}",
        failures.len(),
        failures.first().map(String::as_str).unwrap_or("")
    );
    eprintln!("cleared-model burst canary: 8/8 ok — narrowing holds");
}
