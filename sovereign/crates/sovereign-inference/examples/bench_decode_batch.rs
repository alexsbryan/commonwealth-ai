//! Multi-sequence autoregressive decode microbench.
//!
//! The chat slot in production is pinned to `n_seq_max = 1` because
//! Phase 1 enrichment over a long chapter wants the full ctx window
//! per request (see `embedded.rs::with_n_seq_max(1)` and the comment
//! block above it). But the *short-call* enrichment phases — Phase 3
//! cluster naming, Phase 5 position extraction, Phase 6 tension
//! classification — have small input + small output and run hundreds
//! of times per corpus. They'd happily trade per-request context for
//! parallel decoding.
//!
//! This bench answers: at what (n_seq_max, n_ctx_per_seq) does
//! continuous batching actually win wall-clock for K identical short
//! calls, and by how much, before we commit to refactoring
//! `generate_sync` to support it.
//!
//! Methodology mirrors `bench_embed.rs` and `bench_decode.rs`:
//! constructs a raw `LlamaContext` (bypassing `ModelSlot`), drives K
//! calls in batches of size `n_seq_max`, with per-seq EOS tracking
//! and per-seq greedy sampling. Greedy is fine — we're measuring
//! throughput, not generation quality.
//!
//! Run with:
//!
//!   cargo run --release -p sovereign-inference --example bench_decode_batch -- \
//!       --model ~/sovereign/models/Qwen3.5-2B.Q6_K.gguf \
//!       --backend gpu --threads 8 \
//!       --total-ctx 16384 --n-seq 1,2,4,8 \
//!       --k 32 --prompt-tokens 900 --gen-tokens 128 --iters 2
//!
//! Default sweep keeps **total ctx constant**: every (n_seq, ctx_per_seq)
//! pair covers the same KV-cache memory budget, so the comparison
//! isolates the parallelism effect.

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
use llama_cpp_4::token::LlamaToken;

#[derive(Clone, Copy, Debug)]
enum Backend {
    CpuOnly,
    GpuFull,    // n_gpu_layers=999, offload_kqv=true, op_offload=true
    GpuOpsOnly, // n_gpu_layers=999, op_offload=true, kqv on CPU
}

fn main() {
    let args = Args::parse();
    println!("bench_decode_batch: model={}", args.model.display());
    println!(
        "config: K={} prompt_tokens~={} gen_tokens={} backend={:?} threads={} n_ubatch={} iters={}",
        args.k,
        args.prompt_tokens,
        args.gen_tokens,
        args.backend,
        args.threads,
        args.n_ubatch,
        args.iters
    );
    println!("sweep (constant total_ctx={}):", args.total_ctx);
    for cfg in &args.sweep {
        println!(
            "  n_seq={:>2}  n_ctx_per_seq={:>5}  fits_per_seq={}",
            cfg.n_seq,
            cfg.n_ctx_per_seq,
            args.prompt_tokens + args.gen_tokens <= cfg.n_ctx_per_seq as usize,
        );
    }
    println!();

    // Refuse configurations that don't fit prompt + gen in n_ctx_per_seq —
    // llama.cpp will error mid-decode and the per-seq comparison would be
    // contaminated by partial runs.
    for cfg in &args.sweep {
        if args.prompt_tokens + args.gen_tokens > cfg.n_ctx_per_seq as usize {
            eprintln!(
                "error: prompt({}) + gen({}) > n_ctx_per_seq({}) for n_seq={}; \
                 raise --total-ctx or drop --n-seq",
                args.prompt_tokens, args.gen_tokens, cfg.n_ctx_per_seq, cfg.n_seq
            );
            std::process::exit(2);
        }
    }

    let backend = Arc::new(LlamaBackend::init().expect("LlamaBackend::init"));
    let gpu_layers = match args.backend {
        Backend::CpuOnly => 0,
        Backend::GpuFull | Backend::GpuOpsOnly => 999,
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

    // One synthetic prompt re-used across all configs and all K calls.
    // Real Phase 3 prompts vary per cluster, but the throughput question
    // only depends on token counts, not contents.
    let prompt_text = synthetic_prompt(args.prompt_tokens);

    println!(
        "{:>6}  {:>10}  {:>4}  {:>11}  {:>11}  {:>11}  {:>13}  {:>13}",
        "n_seq", "ctx_perseq", "it", "prefill_ms", "decode_ms", "wall_ms", "calls/s", "total_tok/s"
    );

    for cfg in &args.sweep {
        let total_ctx = cfg.n_seq * cfg.n_ctx_per_seq;
        let (offload_kqv, _op_offload) = match args.backend {
            Backend::CpuOnly => (false, false),
            Backend::GpuFull => (true, true),
            Backend::GpuOpsOnly => (false, true),
        };
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(total_ctx))
            .with_n_batch(total_ctx)
            .with_n_ubatch(args.n_ubatch)
            .with_n_threads(args.threads as i32)
            .with_n_threads_batch(args.threads as i32)
            .with_offload_kqv(offload_kqv);
        let mut ctx = unsafe {
            let model_ref: &'static LlamaModel = &*(Arc::as_ptr(&model));
            model_ref
                .new_context(&backend, ctx_params)
                .expect("new_context")
        };

        // Warmup: trigger KV allocation, GPU buffer setup, mmap touch.
        let _ = run_k_calls(
            &model,
            &mut ctx,
            &prompt_text,
            args.k,
            cfg.n_seq,
            args.gen_tokens,
        );

        for it in 0..args.iters {
            let r = run_k_calls(
                &model,
                &mut ctx,
                &prompt_text,
                args.k,
                cfg.n_seq,
                args.gen_tokens,
            );
            let secs = (r.wall_ms.max(1) as f64) / 1000.0;
            let calls_per_s = args.k as f64 / secs;
            let total_tok = r.prompt_tokens + r.decode_tokens;
            let tok_per_s = total_tok as f64 / secs;
            println!(
                "{:>6}  {:>10}  {:>4}  {:>11}  {:>11}  {:>11}  {:>13.2}  {:>13.1}",
                cfg.n_seq,
                cfg.n_ctx_per_seq,
                it,
                r.prefill_ms,
                r.decode_ms,
                r.wall_ms,
                calls_per_s,
                tok_per_s
            );
        }
        println!();
    }
}

#[derive(Default)]
struct RunResult {
    prefill_ms: u64,
    decode_ms: u64,
    wall_ms: u64,
    prompt_tokens: usize,
    decode_tokens: usize,
}

/// Drive K identical-shape short calls through the context, packing
/// up to `n_seq` of them into each batch.
///
/// Per K-batch:
///   1. Prefill: pack `calls_in_batch` sequences (same prompt tokens,
///      different seq_id) into one `LlamaBatch` and `decode()` once.
///   2. Sequential autoregressive decode: each iteration samples one
///      token per still-active seq, builds a fresh batch with one
///      token per active seq, decodes, and updates per-seq state.
///   3. EOS retirement: a seq drops out of the active set on EOS or
///      when it hits `gen_tokens`. Wall stops when all seqs retire.
fn run_k_calls(
    model: &LlamaModel,
    ctx: &mut llama_cpp_4::context::LlamaContext<'_>,
    prompt_text: &str,
    k: usize,
    n_seq: u32,
    gen_tokens: usize,
) -> RunResult {
    let tokens = model
        .str_to_token(prompt_text, AddBos::Always)
        .expect("tokenize prompt");
    let n_prompt = tokens.len();
    let n_seq = n_seq as usize;
    let n_batches = k.div_ceil(n_seq);
    let n_batch_buf = ctx.n_batch() as usize;

    let mut acc = RunResult::default();
    let mut checksum: i64 = 0;

    let wall_t0 = Instant::now();

    for batch_i in 0..n_batches {
        ctx.clear_kv_cache();
        let calls_in_batch = (k - batch_i * n_seq).min(n_seq);

        // -- Prefill --------------------------------------------------------
        let mut batch = LlamaBatch::new(n_batch_buf, n_seq as i32);
        let mut logit_idx_for_seq: Vec<i32> = Vec::with_capacity(calls_in_batch);
        let mut batch_pos: i32 = 0;
        for seq in 0..calls_in_batch {
            for (pos, &tok) in tokens.iter().enumerate() {
                let last = pos == n_prompt - 1;
                batch
                    .add(tok, pos as i32, &[seq as i32], last)
                    .expect("batch.add prefill");
                if last {
                    logit_idx_for_seq.push(batch_pos);
                }
                batch_pos += 1;
            }
        }
        acc.prompt_tokens += n_prompt * calls_in_batch;

        let t0 = Instant::now();
        ctx.decode(&mut batch).expect("prefill decode");
        acc.prefill_ms += t0.elapsed().as_millis() as u64;

        // -- Decode ---------------------------------------------------------
        // Per-seq state: a fresh greedy sampler per call (sampler state
        // would otherwise leak between conceptually-independent calls),
        // active flag, next-position counter, and the batch index of the
        // most recent logit-bearing token for that seq.
        let mut samplers: Vec<LlamaSampler> = (0..calls_in_batch)
            .map(|_| LlamaSampler::greedy())
            .collect();
        let mut active = vec![true; calls_in_batch];
        let mut next_pos: Vec<i32> = vec![n_prompt as i32; calls_in_batch];
        let mut decoded_per_seq = vec![0usize; calls_in_batch];
        let mut current_logit_idx = logit_idx_for_seq;

        let t1 = Instant::now();

        loop {
            let mut next_tokens: Vec<Option<LlamaToken>> = vec![None; calls_in_batch];
            for seq in 0..calls_in_batch {
                if !active[seq] {
                    continue;
                }
                let tok = samplers[seq].sample(ctx, current_logit_idx[seq]);
                samplers[seq].accept(tok);
                checksum ^= tok.0 as i64;

                let hit_cap = decoded_per_seq[seq] + 1 >= gen_tokens;
                let is_eos = model.is_eog_token(tok);
                if is_eos {
                    // Don't count the EOS token itself.
                    active[seq] = false;
                } else if hit_cap {
                    // Count this final token, then retire.
                    decoded_per_seq[seq] += 1;
                    acc.decode_tokens += 1;
                    active[seq] = false;
                } else {
                    decoded_per_seq[seq] += 1;
                    acc.decode_tokens += 1;
                    next_tokens[seq] = Some(tok);
                }
            }

            if !active.iter().any(|&a| a) {
                break;
            }

            // Pack one new token per still-active seq into the next batch.
            // Active seqs preserve their seq_id; the batch index of seq's
            // token is the cumulative count of active seqs ahead of it.
            batch.clear();
            current_logit_idx = vec![-1; calls_in_batch];
            let mut bi: i32 = 0;
            for seq in 0..calls_in_batch {
                if !active[seq] {
                    continue;
                }
                let tok = next_tokens[seq].expect("active seq must have a token to feed");
                batch
                    .add(tok, next_pos[seq], &[seq as i32], true)
                    .expect("batch.add decode");
                current_logit_idx[seq] = bi;
                next_pos[seq] += 1;
                bi += 1;
            }

            ctx.decode(&mut batch).expect("decode step");
        }

        acc.decode_ms += t1.elapsed().as_millis() as u64;
    }

    std::hint::black_box(checksum);
    acc.wall_ms = wall_t0.elapsed().as_millis() as u64;
    acc
}

/// Synthetic prompt of ~target_tokens English words. Phase 3 cluster-
/// naming prompts are ~800–1200 tokens; the bench's default is 900.
fn synthetic_prompt(target_tokens: usize) -> String {
    let sentence = "The quick brown fox jumps over the lazy dog in the forest at dawn. ";
    let words_per_sentence = sentence.split_whitespace().count();
    let repeats = (target_tokens / words_per_sentence).max(1);
    let mut out = String::with_capacity(sentence.len() * repeats);
    for _ in 0..repeats {
        out.push_str(sentence);
    }
    out.push_str("\n\nName a single concept that connects these ideas in one short phrase.");
    out
}

#[derive(Clone, Copy, Debug)]
struct SweepCfg {
    n_seq: u32,
    n_ctx_per_seq: u32,
}

struct Args {
    model: PathBuf,
    threads: usize,
    n_ubatch: u32,
    sweep: Vec<SweepCfg>,
    total_ctx: u32,
    k: usize,
    prompt_tokens: usize,
    gen_tokens: usize,
    iters: usize,
    backend: Backend,
}

impl Args {
    fn parse() -> Self {
        let raw: Vec<String> = std::env::args().collect();
        let mut model: Option<PathBuf> = None;
        let mut threads = 8usize;
        let mut n_ubatch = 512u32;
        let mut n_seq_list: Vec<u32> = vec![1, 2, 4, 8];
        let mut total_ctx = 16384u32;
        let mut k = 32usize;
        let mut prompt_tokens = 900usize;
        let mut gen_tokens = 128usize;
        let mut iters = 2usize;
        let mut backend = Backend::GpuFull;

        let mut i = 1;
        while i < raw.len() {
            match raw[i].as_str() {
                "--model" => {
                    i += 1;
                    model = raw.get(i).map(PathBuf::from);
                }
                "--threads" => {
                    i += 1;
                    threads = raw[i].parse().unwrap_or(8);
                }
                "--n-ubatch" => {
                    i += 1;
                    n_ubatch = raw[i].parse().unwrap_or(512);
                }
                "--n-seq" => {
                    i += 1;
                    n_seq_list = raw[i]
                        .split(',')
                        .filter_map(|s| s.trim().parse().ok())
                        .collect();
                    if n_seq_list.is_empty() {
                        eprintln!("error: --n-seq must list one or more positive integers");
                        std::process::exit(2);
                    }
                }
                "--total-ctx" => {
                    i += 1;
                    total_ctx = raw[i].parse().unwrap_or(16384);
                }
                "--k" => {
                    i += 1;
                    k = raw[i].parse().unwrap_or(32);
                }
                "--prompt-tokens" => {
                    i += 1;
                    prompt_tokens = raw[i].parse().unwrap_or(900);
                }
                "--gen-tokens" => {
                    i += 1;
                    gen_tokens = raw[i].parse().unwrap_or(128);
                }
                "--iters" => {
                    i += 1;
                    iters = raw[i].parse().unwrap_or(2);
                }
                "--backend" => {
                    i += 1;
                    backend = match raw[i].as_str() {
                        "cpu" => Backend::CpuOnly,
                        // gpu/metal/vulkan/rocm all map to "full GPU offload" —
                        // llama.cpp picks the compiled backend at build time.
                        "gpu" | "metal" | "vulkan" | "rocm" => Backend::GpuFull,
                        "gpu-ops" | "metal-ops" => Backend::GpuOpsOnly,
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

        let sweep: Vec<SweepCfg> = n_seq_list
            .into_iter()
            .map(|n| SweepCfg {
                n_seq: n,
                n_ctx_per_seq: total_ctx / n,
            })
            .collect();

        Args {
            model,
            threads,
            n_ubatch,
            sweep,
            total_ctx,
            k,
            prompt_tokens,
            gen_tokens,
            iters,
            backend,
        }
    }
}
