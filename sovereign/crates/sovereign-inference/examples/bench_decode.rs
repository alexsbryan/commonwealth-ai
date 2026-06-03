//! Microbenchmark for the chat-slot decode path.
//!
//! Stands up a raw llama.cpp context (bypassing `ModelSlot`) with a
//! configurable backend and thread count, then runs a canonical
//! workload: prefill a prompt of `prompt_tokens` tokens, then generate
//! `gen_tokens` tokens one-at-a-time (the production decode shape).
//! Reports prompt-eval tok/sec (how fast llama.cpp digests the
//! retrieved-context + question) and decode tok/sec (how fast it
//! produces the answer).
//!
//! Purpose: prove whether the chat slot is actually on Metal. The
//! canonical Joan Robinson turn on Qwen3.5-9B.Q8_0 has:
//!   - ~900 prompt tokens (system + 8 retrieved chunks + question)
//!   - ~600 generated tokens (Fast-path budget)
//! and was observed at ~49 tok/sec total wall throughput. If this
//! bench reports 60-80+ tok/sec decode on `--backend metal` but the
//! production log shows 49, the chat slot isn't actually taking the
//! Metal ctx_params path and there's a silent regression.
//!
//! Run with:
//!
//!   cargo run --release -p sovereign-inference --example bench_decode -- \
//!       --model ~/.sovereign/models/Qwen3.5-9B.Q8_0.1.gguf \
//!       --backend metal --threads 8 \
//!       --prompt-tokens 900 --gen-tokens 600 --iters 2
//!
//! Canonical sweeps:
//!
//!   # CPU vs Metal head-to-head (keep everything else identical):
//!   cargo run --release -p sovereign-inference --example bench_decode -- \
//!       --model <path> --backend cpu    --threads 8 --iters 2
//!   cargo run --release -p sovereign-inference --example bench_decode -- \
//!       --model <path> --backend metal  --threads 8 --iters 2
//!
//!   # Thread scaling on CPU (confirms P-core cap is still right):
//!   cargo run --release -p sovereign-inference --example bench_decode -- \
//!       --model <path> --backend cpu --threads 1,4,8,12

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use llama_cpp_4::context::params::LlamaContextParams;
use llama_cpp_4::llama_backend::LlamaBackend;
use llama_cpp_4::llama_batch::LlamaBatch;
use llama_cpp_4::model::params::LlamaModelParams;
use llama_cpp_4::model::{AddBos, LlamaModel};
use llama_cpp_4::sampling::LlamaSampler;

#[derive(Clone, Copy, Debug)]
enum Backend {
    CpuOnly,
    MetalFull,    // offload_kqv=true, op_offload=true, n_gpu_layers=999
    MetalOpsOnly, // op_offload=true, offload_kqv=false, n_gpu_layers=999
}

fn main() {
    let args = Args::parse();
    println!("bench_decode: model={}", args.model.display());
    println!(
        "config: prompt_tokens~={} gen_tokens={} n_ctx={} iters={} backend={:?}",
        args.prompt_tokens, args.gen_tokens, args.n_ctx, args.iters, args.backend
    );
    println!("thread counts to test: {:?}", args.thread_counts);
    println!();

    let backend = Arc::new(LlamaBackend::init().expect("LlamaBackend::init"));
    let gpu_layers = match args.backend {
        Backend::CpuOnly => 0,
        Backend::MetalFull | Backend::MetalOpsOnly => 999,
    };
    let model_params = LlamaModelParams::default().with_n_gpu_layers(gpu_layers);
    let model = Arc::new(
        LlamaModel::load_from_file(&backend, &args.model, &model_params).expect("load model"),
    );
    println!(
        "model loaded: layers={} size_mb={}",
        model.n_layer(),
        model.model_size() / (1024 * 1024)
    );
    println!();

    // Build the synthetic prompt once; same tokens for every thread config.
    let prompt_text = synthetic_prompt(args.prompt_tokens);

    println!(
        "{:>10}  {:>4}  {:>11}  {:>11}  {:>14}  {:>13}",
        "n_threads", "it", "prompt_ms", "decode_ms", "prompt tok/s", "decode tok/s"
    );

    for &n_threads in &args.thread_counts {
        let (offload_kqv, _op_offload) = match args.backend {
            Backend::CpuOnly => (false, false),
            Backend::MetalFull => (true, true),
            Backend::MetalOpsOnly => (false, true),
        };
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(args.n_ctx))
            .with_n_batch(args.n_ctx)
            .with_n_ubatch(512)
            .with_n_threads(n_threads as i32)
            .with_n_threads_batch(n_threads as i32)
            .with_offload_kqv(offload_kqv);

        let mut ctx = unsafe {
            let model_ref: &'static LlamaModel = &*(Arc::as_ptr(&model));
            model_ref
                .new_context(&backend, ctx_params)
                .expect("new_context")
        };

        // Warm: one full run to trigger KV allocation, Metal buffer
        // setup, mmap touch. Not timed.
        let _ = run_turn(&model, &mut ctx, &prompt_text, args.gen_tokens);

        for iter in 0..args.iters {
            let (prompt_ms, decode_ms, prompt_toks, decode_toks) =
                run_turn(&model, &mut ctx, &prompt_text, args.gen_tokens);
            let prompt_per_s = prompt_toks as f64 / (prompt_ms.max(1) as f64 / 1000.0);
            let decode_per_s = decode_toks as f64 / (decode_ms.max(1) as f64 / 1000.0);
            println!(
                "{:>10}  {:>4}  {:>11}  {:>11}  {:>14.1}  {:>13.1}",
                n_threads, iter, prompt_ms, decode_ms, prompt_per_s, decode_per_s
            );
        }
        println!();
    }
}

/// Run one prefill + sequential decode. Returns:
///   (prompt_eval_ms, decode_ms, prompt_tokens, decode_tokens).
fn run_turn(
    model: &LlamaModel,
    ctx: &mut llama_cpp_4::context::LlamaContext<'_>,
    prompt_text: &str,
    gen_tokens: usize,
) -> (u64, u64, usize, usize) {
    ctx.clear_kv_cache();

    let tokens = model
        .str_to_token(prompt_text, AddBos::Always)
        .expect("tokenize prompt");
    let n_prompt = tokens.len();

    // Prefill: pack every prompt token into one batch, decode once.
    let n_batch = ctx.n_batch() as usize;
    let mut batch = LlamaBatch::new(n_batch, 1);
    let last_idx = n_prompt - 1;
    for (i, &tok) in tokens.iter().enumerate() {
        batch
            .add(tok, i as i32, &[0], i == last_idx)
            .expect("batch.add prefill");
    }
    let t0 = Instant::now();
    ctx.decode(&mut batch).expect("prefill decode");
    let prompt_ms = t0.elapsed().as_millis() as u64;

    // Sequential decode: one token at a time, greedy (temperature=0).
    // Greedy is fine for a pure throughput bench — we aren't grading
    // the output, just measuring tok/sec.
    let mut sampler = LlamaSampler::greedy();
    let mut decoded = 0usize;
    let mut checksum: i64 = 0;
    let t1 = Instant::now();
    while decoded < gen_tokens {
        let token = sampler.sample(ctx, -1);
        sampler.accept(token);
        if model.is_eog_token(token) {
            break;
        }
        checksum ^= token.0 as i64;

        batch.clear();
        let pos = (n_prompt + decoded) as i32;
        batch.add(token, pos, &[0], true).expect("batch.add decode");
        ctx.decode(&mut batch).expect("decode step");
        decoded += 1;
    }
    let decode_ms = t1.elapsed().as_millis() as u64;
    std::hint::black_box(checksum);

    (prompt_ms, decode_ms, n_prompt, decoded)
}

/// Produce a prompt of roughly `target_tokens` English tokens by
/// repeating a filler sentence. Good enough to exercise matmul and
/// KV-cache writes — we aren't grading semantics.
fn synthetic_prompt(target_tokens: usize) -> String {
    let sentence = "The quick brown fox jumps over the lazy dog in the forest at dawn. ";
    let words_per_sentence = sentence.split_whitespace().count();
    let repeats = (target_tokens / words_per_sentence).max(1);
    let mut out = String::with_capacity(sentence.len() * repeats);
    for _ in 0..repeats {
        out.push_str(sentence);
    }
    out.push_str("\n\nSummarise the passage above in one sentence.");
    out
}

struct Args {
    model: PathBuf,
    thread_counts: Vec<usize>,
    n_ctx: u32,
    prompt_tokens: usize,
    gen_tokens: usize,
    iters: usize,
    backend: Backend,
}

impl Args {
    fn parse() -> Self {
        let raw: Vec<String> = std::env::args().collect();
        let mut model = None;
        let mut thread_counts = vec![8usize];
        let mut n_ctx = 8192u32;
        let mut prompt_tokens = 900usize;
        let mut gen_tokens = 300usize;
        let mut iters = 2usize;
        let mut backend = Backend::MetalFull;

        let mut i = 1;
        while i < raw.len() {
            match raw[i].as_str() {
                "--model" => {
                    i += 1;
                    model = raw.get(i).map(PathBuf::from);
                }
                "--threads" => {
                    i += 1;
                    thread_counts = raw[i]
                        .split(',')
                        .filter_map(|s| s.trim().parse().ok())
                        .collect();
                }
                "--n-ctx" => {
                    i += 1;
                    n_ctx = raw[i].parse().unwrap_or(8192);
                }
                "--prompt-tokens" => {
                    i += 1;
                    prompt_tokens = raw[i].parse().unwrap_or(900);
                }
                "--gen-tokens" => {
                    i += 1;
                    gen_tokens = raw[i].parse().unwrap_or(300);
                }
                "--iters" => {
                    i += 1;
                    iters = raw[i].parse().unwrap_or(2);
                }
                "--backend" => {
                    i += 1;
                    backend = match raw[i].as_str() {
                        "cpu" => Backend::CpuOnly,
                        "metal" => Backend::MetalFull,
                        "metal-ops" => Backend::MetalOpsOnly,
                        other => {
                            eprintln!("unknown --backend: {other}");
                            std::process::exit(2);
                        }
                    };
                }
                _ => {}
            }
            i += 1;
        }

        let model = model.unwrap_or_else(|| {
            eprintln!("error: --model <path.gguf> is required");
            std::process::exit(2);
        });
        Args {
            model,
            thread_counts,
            n_ctx,
            prompt_tokens,
            gen_tokens,
            iters,
            backend,
        }
    }
}
