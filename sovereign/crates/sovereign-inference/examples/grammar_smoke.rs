// SPDX-License-Identifier: AGPL-3.0-or-later
//! Minimal `LlamaSampler::grammar` reproducer.
//!
//! Loads a GGUF, attaches a 14-byte grammar (`root ::= "yes"`) plus
//! a dist sampler, runs ONE decode step, and prints either
//! `GRAMMAR_SMOKE_OK` or aborts via the upstream assertion.
//!
//! Used to confirm whether `LlamaSampler::grammar` is broken on
//! a given backend (Vulkan / ROCm / Metal / CPU) — the Strix Halo
//! Vulkan build crashes the daemon process via
//! `GGML_ASSERT(!stacks.empty())` at `llama-grammar.cpp:940` on the
//! first apply call for ANY grammar, including this 14-byte case.
//! See `memory/project_grammar_alpha_blocker.md` for the bug
//! history. Self-contained — no daemon, no corpus — so it's the
//! cheapest possible repro to share upstream or run on a different
//! backend.
//!
//! Run with:
//!
//!   cargo run --release -p sovereign-inference --example grammar_smoke -- \
//!       --model <path-to-any-gguf>
//!
//! Optional flags:
//!   --grammar <str>     Override the grammar string (default: `root ::= "yes"`).
//!   --gpu-layers <n>    Layers to offload (default: 999 = all).
//!   --prompt <str>      The user prompt (default: a one-line greeting).
//!
//! Exit codes:
//!   0   sampler returned a token without crashing.
//!   1   `LlamaSampler::grammar` returned an error (init failure, recoverable).
//!   *   process aborted via assertion / SIGABRT — the upstream crash.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;

use llama_cpp_4::context::params::LlamaContextParams;
use llama_cpp_4::llama_backend::LlamaBackend;
use llama_cpp_4::llama_batch::LlamaBatch;
use llama_cpp_4::model::params::LlamaModelParams;
use llama_cpp_4::model::{AddBos, LlamaModel};
use llama_cpp_4::sampling::LlamaSampler;
// llama-cpp-4 0.2.x retired the streaming `token_to_piece` shape; the
// crate-internal `crate::llama` shim restores the 0.1.x call signature
// via `LlamaModelExt`. Examples import it the same way production does.
use sovereign_inference::llama::LlamaModelExt;

struct Args {
    model: PathBuf,
    grammar: String,
    gpu_layers: u32,
    prompt: String,
}

fn parse() -> Args {
    let mut model: Option<PathBuf> = None;
    let mut grammar = "root ::= \"yes\"".to_string();
    let mut gpu_layers = 999u32;
    let mut prompt = "Say yes:".to_string();

    let argv: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--model" => {
                i += 1;
                model = Some(PathBuf::from(&argv[i]));
            }
            "--grammar" => {
                i += 1;
                grammar = argv[i].clone();
            }
            "--gpu-layers" => {
                i += 1;
                gpu_layers = argv[i].parse().expect("--gpu-layers must be a u32");
            }
            "--prompt" => {
                i += 1;
                prompt = argv[i].clone();
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: grammar_smoke --model <gguf> [--grammar <str>] \
                     [--gpu-layers <n>] [--prompt <str>]"
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown flag: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    let model = model.unwrap_or_else(|| {
        eprintln!("error: --model is required");
        std::process::exit(2);
    });
    Args {
        model,
        grammar,
        gpu_layers,
        prompt,
    }
}

fn main() {
    let args = parse();
    println!("grammar_smoke");
    println!("  model:   {}", args.model.display());
    println!("  grammar: {}", args.grammar);
    println!("  gpu_layers: {}", args.gpu_layers);
    println!();

    let backend = Arc::new(LlamaBackend::init().expect("LlamaBackend::init"));
    let model_params = LlamaModelParams::default().with_n_gpu_layers(args.gpu_layers);
    let model =
        LlamaModel::load_from_file(&backend, &args.model, &model_params).expect("load model");
    println!(
        "model loaded: layers={} size_mb={}",
        model.n_layer(),
        model.model_size() / (1024 * 1024)
    );

    // Match the daemon's effective n_ctx so we exercise the same
    // KV/buffer dimensions. The daemon bench is on 32768 (Strix Halo
    // batch tuning); 2048 was the original smoke default.
    let n_ctx_smoke: u32 = std::env::var("SMOKE_N_CTX")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2048);
    println!("  n_ctx:   {n_ctx_smoke}");
    // Match the daemon's chat-slot ctx params exactly (see
    // `embedded.rs::ModelSlot::load`). The grammar bug we're chasing
    // doesn't reproduce with a vanilla LlamaContextParams default;
    // these flags are the variables.
    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(n_ctx_smoke))
        // The daemon pins n_seq_max=1 to avoid llama-cpp-2's default
        // of 16 splitting the context window 16 ways. Most likely
        // suspect for the empty-stacks bug — the grammar engine may
        // assume seq_max>1 somewhere.
        .with_n_batch(n_ctx_smoke)
        .with_n_ubatch(512)
        .with_offload_kqv(true);
    let mut ctx = model
        .new_context(&backend, ctx_params)
        .expect("new_context");

    // SMOKE_CHAT_TEMPLATE path retired with the 0.1.x → 0.2.x
    // llama-cpp migration: `apply_chat_template(&LlamaChatTemplate, ...)`
    // and the `LlamaChatTemplate` type itself were both removed. Smoke
    // runs against the raw prompt; the production prompt-formatting
    // path is exercised by `cargo test -p sovereign-inference` via
    // `format_prompt`. If smoke ever needs chat-templated input again,
    // pull the template string via `sovereign_inference::llama::chat_template(&model)`
    // and run it through a Jinja renderer at the call site.
    let prompt_text = args.prompt.clone();

    // Prefill the prompt so the slot has something to decode FROM.
    // The grammar starts fresh per request; this just makes sure the
    // model's KV cache + state are properly initialised before we
    // attach the grammar sampler.
    let tokens = model
        .str_to_token(&prompt_text, AddBos::Always)
        .expect("tokenize prompt");
    let mut batch = LlamaBatch::new(tokens.len().max(8), 1);
    let last_idx = tokens.len() - 1;
    for (i, &tok) in tokens.iter().enumerate() {
        batch
            .add(tok, i as i32, &[0], i == last_idx)
            .expect("batch add");
    }
    ctx.decode(&mut batch).expect("prefill decode");
    println!("prefill ok: {} tokens", tokens.len());

    // Mimic the daemon: do a no-grammar decode first (the warmup
    // we observe in real traffic — initial chat slot load),
    // re-prefill, THEN attach the grammar sampler. The smoke worked
    // without this; the daemon crashes on the second-request grammar
    // attach. If THIS crashes, the trigger is "grammar attached to
    // a context that previously decoded".
    if std::env::var("SMOKE_WARMUP")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        println!("warmup: decoding 5 tokens without grammar…");
        let mut warmup_sampler = LlamaSampler::chain_simple([LlamaSampler::dist(0xCAFE)]);
        let mut pos = tokens.len() as i32;
        for _ in 0..5 {
            let tok = warmup_sampler.sample(&ctx, -1);
            warmup_sampler.accept(tok);
            if model.is_eog_token(tok) {
                break;
            }
            let mut b = LlamaBatch::new(1, 1);
            b.add(tok, pos, &[0], true).expect("batch add");
            ctx.decode(&mut b).expect("warmup decode");
            pos += 1;
        }
        println!("warmup ok");
        // Re-prefill so the grammar starts in a clean kv-cache
        // position — matches the daemon, which also clears KV
        // between requests.
        ctx.clear_kv_cache();
        let mut batch2 = LlamaBatch::new(tokens.len().max(8), 1);
        for (i, &tok) in tokens.iter().enumerate() {
            batch2
                .add(tok, i as i32, &[0], i == tokens.len() - 1)
                .expect("batch add");
        }
        ctx.decode(&mut batch2).expect("re-prefill decode");
        println!("re-prefill ok");
    }

    // Build the grammar sampler. `LlamaSampler::grammar` returns the
    // sampler directly in 0.2.x (no Result wrapper); a panicking
    // initialiser is fine for a smoke test — the original recoverable
    // path was there to distinguish init failure from the process-
    // abort case, but the 0.2.x API no longer exposes init failure
    // as a recoverable error.
    let grammar_sampler = LlamaSampler::grammar(&model, &args.grammar, "root");
    println!("grammar init ok");

    // Chain: by default just `[grammar, dist]`. With
    // `SMOKE_FULL_CHAIN=1`, mirror the daemon's build_sampler chain
    // minus DRY — `LlamaSampler::dry` became a method on an existing
    // sampler in 0.2.x rather than a free constructor, and threading
    // it through the smoke wasn't worth the rewrite cost for a path
    // that's only there to reproduce the grammar crash.
    let mut sampler = if std::env::var("SMOKE_FULL_CHAIN")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        println!("using full daemon sampler chain (DRY omitted post-migration)");
        LlamaSampler::chain_simple([
            grammar_sampler,
            LlamaSampler::penalties(128, 1.15, 0.1, 0.1),
            LlamaSampler::top_k(40),
            LlamaSampler::min_p(0.05, 1),
            LlamaSampler::temp(0.2),
            LlamaSampler::dist(0xC0FFEE),
        ])
    } else {
        LlamaSampler::chain_simple([grammar_sampler, LlamaSampler::dist(0xC0FFEE)])
    };

    // The moment of truth: this calls llama_grammar_apply_impl,
    // which asserts `!stacks.empty()`. If init produced empty
    // stacks (the bug we're chasing), the process aborts here and
    // the rest of this program never runs. If it survives, we
    // print success.
    println!("calling sample (apply + accept)…");
    let token = sampler.sample(&ctx, -1);
    println!("sampled token id: {}", token.0);
    sampler.accept(token);

    let piece = model
        .token_to_piece(token, &mut encoding_rs::UTF_8.new_decoder(), false, None)
        .unwrap_or_else(|_| "<undecodable>".into());
    println!("token piece: {piece:?}");
    println!();
    println!("GRAMMAR_SMOKE_OK");
}
