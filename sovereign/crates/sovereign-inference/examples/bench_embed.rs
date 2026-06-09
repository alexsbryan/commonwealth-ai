// SPDX-License-Identifier: AGPL-3.0-or-later
//! Microbenchmark for the embed-slot decode path.
//!
//! Stands up a raw llama.cpp context (bypassing `EmbedSlot`) with a
//! configurable `n_threads_batch`, `n_ctx`, and `n_seq_max`, then
//! runs a fixed synthetic batch of 16 sequences through a packed
//! decode — once to warm mmap + KV allocation, then three timed
//! iterations.
//!
//! Purpose: prove whether llama.cpp's CPU backend actually uses
//! the thread count we hand it. If wall-clock scales linearly with
//! `n_threads_batch`, threading works and we should aim higher. If
//! it plateaus at ~4 threads (llama.cpp's internal split), threads
//! are a dead end and we need to go wide in Rust via multiple
//! contexts or switch to Metal/Candle.
//!
//! Run with:
//!
//!   cargo run --release -p sovereign-inference --example bench_embed -- \
//!       --model /path/to/qwen-embedding-0.6b.gguf \
//!       [--threads 1,2,4,8,12,16] \
//!       [--n-ctx 16384] [--n-seq-max 16] [--seqs 16] [--tokens-per-seq 400]

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use llama_cpp_4::context::params::{LlamaContextParams, LlamaPoolingType};
use llama_cpp_4::llama_backend::LlamaBackend;
use llama_cpp_4::llama_batch::LlamaBatch;
use llama_cpp_4::model::params::LlamaModelParams;
use llama_cpp_4::model::{AddBos, LlamaModel};

// Metal / CPU toggles for the experiment.
#[derive(Clone, Copy, Debug)]
enum Backend {
    CpuOnly,
    MetalFull,    // offload_kqv=true, op_offload=true, n_gpu_layers=999
    MetalOpsOnly, // op_offload=true, kqv on CPU
}

fn main() {
    let args = Args::parse();
    println!("bench_embed: model={}", args.model.display());
    println!(
        "config: seqs={} tokens_per_seq~={} n_ctx={} n_seq_max={} iters={}",
        args.seqs, args.tokens_per_seq, args.n_ctx, args.n_seq_max, args.iters
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
    println!("backend: {:?}", args.backend);
    let n_embd = model.n_embd() as usize;
    println!(
        "model loaded: dims={} layers={} size_mb={}",
        n_embd,
        model.n_layer(),
        model.model_size() / (1024 * 1024)
    );
    println!();

    // Build synthetic input once; reuse across all thread configs
    // so the workload is identical.
    let texts = synthetic_texts(args.seqs, args.tokens_per_seq);

    // Header
    println!(
        "{:>10}  {:>10}  {:>12}  {:>12}  {:>12}",
        "n_threads", "iter", "wall_ms", "seqs/sec", "tok/sec"
    );

    for &n_threads in &args.thread_counts {
        // A fresh context per configuration — context params
        // include n_threads, and llama-cpp-2 doesn't let us
        // change them mid-flight.
        let (offload_kqv, _op_offload) = match args.backend {
            Backend::CpuOnly => (false, false),
            Backend::MetalFull => (true, true),
            Backend::MetalOpsOnly => (false, true),
        };
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(args.n_ctx))
            .with_n_batch(args.n_ctx)
            .with_n_ubatch(args.n_ubatch)
            .with_embeddings(true)
            .with_pooling_type(LlamaPoolingType::Mean)
            .with_offload_kqv(offload_kqv)
            .with_n_threads(n_threads as i32)
            .with_n_threads_batch(n_threads as i32);
        let mut ctx = unsafe {
            let model_ref: &'static LlamaModel = &*(Arc::as_ptr(&model));
            model_ref
                .new_context(&backend, ctx_params)
                .expect("new_context")
        };

        // Warm: one decode to trigger KV allocation, Accelerate
        // fast-path selection, and mmap touch.
        let _ = run_batch(&model, &mut ctx, &texts, args.n_ctx, args.n_seq_max);

        for iter in 0..args.iters {
            let start = Instant::now();
            let total_tokens = run_batch(&model, &mut ctx, &texts, args.n_ctx, args.n_seq_max);
            let elapsed_ms = start.elapsed().as_millis() as u64;
            let secs = elapsed_ms as f64 / 1000.0;
            let seqs_per_sec = args.seqs as f64 / secs.max(1e-9);
            let tok_per_sec = total_tokens as f64 / secs.max(1e-9);
            println!(
                "{:>10}  {:>10}  {:>12}  {:>12.2}  {:>12.1}",
                n_threads, iter, elapsed_ms, seqs_per_sec, tok_per_sec
            );
        }
        println!();
    }
}

/// Decode one packed batch and read pooled embeddings. Returns
/// the total tokens processed so the caller can compute tok/sec.
fn run_batch(
    model: &LlamaModel,
    ctx: &mut llama_cpp_4::context::LlamaContext<'_>,
    texts: &[String],
    n_ctx: u32,
    n_seq_max: u32,
) -> usize {
    ctx.clear_kv_cache();
    let mut batch = LlamaBatch::new(n_ctx as usize, n_seq_max as i32);
    let mut total_tokens = 0usize;
    for (seq_id, text) in texts.iter().enumerate() {
        let tokens = model.str_to_token(text, AddBos::Always).expect("tokenize");
        total_tokens += tokens.len();
        for (pos, &tok) in tokens.iter().enumerate() {
            batch
                .add(tok, pos as i32, &[seq_id as i32], true)
                .expect("batch.add");
        }
    }
    ctx.decode(&mut batch).expect("decode");
    // Actually read the embeddings so compiler / llama.cpp can't
    // skip anything speculatively.
    let mut checksum = 0.0f64;
    for seq_id in 0..texts.len() {
        let emb = ctx
            .embeddings_seq_ith(seq_id as i32)
            .expect("read embedding");
        checksum += emb.iter().map(|&x| x as f64).sum::<f64>();
    }
    // Prevent dead-code elimination on the checksum.
    std::hint::black_box(checksum);
    total_tokens
}

fn synthetic_texts(n: usize, target_tokens: usize) -> Vec<String> {
    // A rough knob: one English word ~= one token for typical
    // Qwen tokenizers. Repeat a sentence until we have ~target
    // tokens per sequence.
    let sentence = "The quick brown fox jumps over the lazy dog in the forest. ";
    let words_per_sentence = sentence.split_whitespace().count(); // ~11
    let repeats = (target_tokens / words_per_sentence).max(1);
    (0..n)
        .map(|i| format!("[doc {i}] {}", sentence.repeat(repeats)))
        .collect()
}

struct Args {
    model: PathBuf,
    thread_counts: Vec<usize>,
    n_ctx: u32,
    n_ubatch: u32,
    n_seq_max: u32,
    seqs: usize,
    tokens_per_seq: usize,
    iters: usize,
    backend: Backend,
}

impl Args {
    fn parse() -> Self {
        let raw: Vec<String> = std::env::args().collect();
        let mut model = None;
        let mut thread_counts = vec![1, 2, 4, 8, 12];
        let mut n_ctx = 16384u32;
        let mut n_ubatch = 2048u32;
        let mut n_seq_max = 16u32;
        let mut seqs = 16usize;
        let mut tokens_per_seq = 400usize;
        let mut iters = 3usize;
        let mut backend = Backend::CpuOnly;

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
                    n_ctx = raw[i].parse().unwrap_or(16384);
                }
                "--n-ubatch" => {
                    i += 1;
                    n_ubatch = raw[i].parse().unwrap_or(2048);
                }
                "--n-seq-max" => {
                    i += 1;
                    n_seq_max = raw[i].parse().unwrap_or(16);
                }
                "--seqs" => {
                    i += 1;
                    seqs = raw[i].parse().unwrap_or(16);
                }
                "--tokens-per-seq" => {
                    i += 1;
                    tokens_per_seq = raw[i].parse().unwrap_or(400);
                }
                "--iters" => {
                    i += 1;
                    iters = raw[i].parse().unwrap_or(3);
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
            n_ubatch,
            n_seq_max,
            seqs,
            tokens_per_seq,
            iters,
            backend,
        }
    }
}
