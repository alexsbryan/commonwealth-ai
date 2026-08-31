// SPDX-License-Identifier: AGPL-3.0-or-later
//! Cartridge Step-1 spike — full-context-state save/restore on our
//! hybrid (attention + Gated DeltaNet) architectures.
//!
//! Motivation (TEACHABLE.md §11 rung 5 / the cartridge exploration):
//! any KV-artifact scheme — from zero-training "context cartridges"
//! (precomputed prefix state) to trained Cartridges — needs ONE
//! serving primitive: restore a saved context state and generate from
//! it WITHOUT re-prefilling. Partial KV-keep is architecturally
//! broken on our recurrent hybrids (`is_recurrent_arch`,
//! embedded.rs — the qwen*moe prefix-cache veto), but FULL-state
//! restore serializes the recurrent buffers too, so it may be sound
//! where partial keep is not. This spike measures exactly that.
//!
//! Protocol (the cartridge serving path, verbatim):
//!   ctx A: prefill an N-token prefix → SAVE session file (state =
//!          prefix only, the cartridge build step) → decode a short
//!          question suffix → greedy-decode M tokens → continuation A.
//!   ctx B: FRESH context → load session file (no prefix prefill!) →
//!          decode the SAME question suffix at positions N.. →
//!          greedy-decode M tokens → continuation B.
//!   Verdict: A == B token-for-token (restored KV + recurrent state
//!   are faithful), plus prefix-prefill-vs-restore latency and the
//!   state-file size (artifact economics).
//!
//!   Note the suffix decode is NOT optional ceremony: llama.cpp state
//!   files restore the memory module but not the output logits
//!   (n_outputs=0 after load — verified on the first run of this
//!   spike), so generation after restore must begin with ≥1 new
//!   token. Real serving always does (the user's question); the
//!   classic "re-eval the last prefix token" resume trick is UNSOUND
//!   here anyway — it would double-apply that token to the recurrent
//!   state on hybrid architectures.
//!
//! Run explicitly (never in CI — needs a local GGUF + Metal):
//!   SPIKE_GGUF=sovereign/models/Qwopus3.5-4B-v3-MTP-Q8_0.gguf \
//!     cargo test -p sovereign-inference --test main state_cartridge_spike \
//!     -- --ignored --nocapture
use std::num::NonZeroU32;
use std::time::Instant;

use llama_cpp_4::context::params::LlamaContextParams;
use llama_cpp_4::llama_backend::LlamaBackend;
use llama_cpp_4::llama_batch::LlamaBatch;
use llama_cpp_4::model::params::LlamaModelParams;
use llama_cpp_4::model::{AddBos, LlamaModel};
use llama_cpp_4::sampling::LlamaSampler;
use llama_cpp_4::token::LlamaToken;

const CONTINUATION_TOKENS: usize = 32;
const N_CTX: u32 = 4096;

/// A deterministic ~1k-token synthetic "corpus briefing" — the shape a
/// context cartridge would hold (stable per-corpus orientation text).
fn briefing_text() -> String {
    let mut s = String::from(
        "You are the resident assistant for the Saltgrass Archive, a \
         collection of maritime survey records from 1890 to 1954. ",
    );
    for i in 0..60 {
        s.push_str(&format!(
            "Volume {i} covers the {} coastal survey of sector {}, \
             including tide tables, dredging permits, and the keeper's \
             log for lighthouse station {}. ",
            ["spring", "summer", "autumn", "winter"][i % 4],
            (i * 7) % 23,
            (i * 3) % 11,
        ));
    }
    s.push_str("When asked, orient the reader to the right volume first. ");
    s
}

/// Decode `suffix` starting at `start_pos` (logits on its last token),
/// then greedy-decode `n` continuation tokens. This is the serve-time
/// path: fresh user tokens appended onto a (live or restored) prefix.
fn ask_and_continue(
    ctx: &mut llama_cpp_4::context::LlamaContext,
    suffix: &[LlamaToken],
    start_pos: i32,
    n: usize,
) -> Vec<LlamaToken> {
    let mut batch = LlamaBatch::new(suffix.len(), 1);
    for (i, tok) in suffix.iter().enumerate() {
        batch
            .add(*tok, start_pos + i as i32, &[0], i == suffix.len() - 1)
            .expect("batch add suffix");
    }
    ctx.decode(&mut batch).expect("decode question suffix");

    let sampler = LlamaSampler::greedy();
    let mut out = Vec::with_capacity(n);
    let mut pos = start_pos + suffix.len() as i32;
    let mut next = sampler.sample(ctx, -1);
    out.push(next);
    for _ in 1..n {
        let mut batch = LlamaBatch::new(1, 1);
        batch.add(next, pos, &[0], true).expect("batch add");
        ctx.decode(&mut batch).expect("decode continuation token");
        pos += 1;
        next = sampler.sample(ctx, -1);
        out.push(next);
    }
    out
}

#[test]
#[ignore = "manual spike: needs SPIKE_GGUF pointing at a local model"]
fn full_state_restore_is_faithful_on_this_arch() {
    let Ok(gguf) = std::env::var("SPIKE_GGUF") else {
        eprintln!("SKIP: set SPIKE_GGUF to a local gguf path");
        return;
    };

    let backend = LlamaBackend::init().expect("backend");
    let model_params = std::pin::pin!(LlamaModelParams::default().with_n_gpu_layers(1_000_000));
    let model = LlamaModel::load_from_file(&backend, &gguf, &model_params).expect("model load");

    let arch = model
        .meta_val_str("general.architecture", 64)
        .unwrap_or_else(|_| "?".into());
    eprintln!("── gate facts ─────────────────────────────────────────");
    eprintln!("  model: {gguf}");
    eprintln!("  arch = {arch}");
    eprintln!("  is_recurrent = {}", model.is_recurrent());
    eprintln!("  is_hybrid    = {}", model.is_hybrid());

    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(Some(NonZeroU32::new(N_CTX).unwrap()))
        .with_n_batch(2048)
        .with_n_ubatch(512);

    let mut tokens = model
        .str_to_token(&briefing_text(), AddBos::Always)
        .expect("tokenize briefing");
    // Keep the prefix comfortably under n_batch (2048) regardless of
    // tokenizer density — first run tokenized past it and tripped
    // GGML_ASSERT(n_tokens_all <= n_batch) on the prefill decode.
    tokens.truncate(1500);
    let tokens = tokens;
    let n_prefix = tokens.len();
    eprintln!("  prefix tokens = {n_prefix}");

    // ── ctx A: live prefill → save → continue ─────────────────────
    let mut ctx_a = model
        .new_context(&backend, ctx_params.clone())
        .expect("ctx A");
    eprintln!("  memory_can_shift = {}", ctx_a.memory_can_shift());

    let mut batch = LlamaBatch::new(n_prefix, 1);
    for (i, tok) in tokens.iter().enumerate() {
        // No logits needed on the prefix — the suffix decode computes
        // the logits both contexts sample from (see module docs).
        batch
            .add(*tok, i as i32, &[0], false)
            .expect("batch add prefix");
    }
    let t0 = Instant::now();
    ctx_a.decode(&mut batch).expect("prefill decode");
    ctx_a.synchronize();
    let prefill_ms = t0.elapsed().as_millis();

    let session_path =
        std::env::temp_dir().join(format!("cartridge-spike-{}.session", std::process::id()));
    let t0 = Instant::now();
    ctx_a
        .save_session_file(&session_path, &tokens)
        .expect("save session");
    let save_ms = t0.elapsed().as_millis();
    let file_bytes = std::fs::metadata(&session_path)
        .map(|m| m.len())
        .unwrap_or(0);

    // The serve-time "user question" appended after the cartridge
    // prefix — identical in both contexts.
    let suffix = model
        .str_to_token(
            "\nQuestion: which volume covers coastal sector 3, and in which season?\nAnswer:",
            AddBos::Never,
        )
        .expect("tokenize suffix");

    let continuation_a =
        ask_and_continue(&mut ctx_a, &suffix, n_prefix as i32, CONTINUATION_TOKENS);
    drop(ctx_a);

    // ── ctx B: FRESH context → restore → same question (no prefix prefill)
    let mut ctx_b = model
        .new_context(&backend, ctx_params.clone())
        .expect("ctx B");
    let t0 = Instant::now();
    let restored_tokens = ctx_b
        .load_session_file(&session_path, N_CTX as usize)
        .expect("load session — restore itself failed on this arch");
    ctx_b.synchronize();
    let restore_ms = t0.elapsed().as_millis();
    assert_eq!(
        restored_tokens.len(),
        n_prefix,
        "restored token prefix must match what was saved"
    );

    let continuation_b =
        ask_and_continue(&mut ctx_b, &suffix, n_prefix as i32, CONTINUATION_TOKENS);

    // ── verdicts ───────────────────────────────────────────────────
    let text_a = model
        .detokenize(&continuation_a, false, true)
        .unwrap_or_default();
    let text_b = model
        .detokenize(&continuation_b, false, true)
        .unwrap_or_default();
    let match_len = continuation_a
        .iter()
        .zip(continuation_b.iter())
        .take_while(|(a, b)| a == b)
        .count();

    eprintln!("── verdicts ───────────────────────────────────────────");
    eprintln!("  prefill_ms = {prefill_ms}  save_ms = {save_ms}  restore_ms = {restore_ms}");
    eprintln!(
        "  state file = {:.1} MB ({:.1} KB/token)",
        file_bytes as f64 / 1e6,
        file_bytes as f64 / 1e3 / n_prefix as f64
    );
    eprintln!("  continuation match: {match_len}/{CONTINUATION_TOKENS} tokens");
    eprintln!("  A: {text_a:?}");
    eprintln!("  B: {text_b:?}");

    let _ = std::fs::remove_file(&session_path);

    assert_eq!(
        match_len, CONTINUATION_TOKENS,
        "restored-state continuation diverged at token {match_len} — \
         full-state restore is NOT faithful on arch {arch}"
    );
}
