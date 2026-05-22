//! LlguidanceConstraint perf bench.
//!
//! Drives the constraint engine against a real `LlamaModel` and
//! reports warm tok/s, per-token mask latency, and forced-fast-forward
//! yield. Post-migration (2026-05-22) this is llguidance-only — the
//! prior JsonConstraint A/B arm went away with `json_constraint.rs`.
//! See `LLGUIDANCE_MIGRATION_AUDIT.md` for the migration history.
//!
//! What the numbers mean:
//!
//!   - **decode tok/s** — wall throughput. Use as a regression
//!     baseline when changing the constraint hot path.
//!   - **mask_ms p50/p99** — per-token mask latency. llguidance
//!     returns a precomputed bitmask, so this should stay flat under
//!     normal load.
//!   - **ff_yield** — ratio of deterministic-prefix tokens
//!     (`Matcher::compute_ff_tokens`) to total tokens decoded. Low
//!     ff_yield (>50% empty) means `ApproximateTokEnv` is dropping
//!     forced runs because tokenisation isn't canonical. That's the
//!     open question for the custom `TokenizerEnv` future PR
//!     (re-adoption plan Q1 path B).
//!
//! Run with:
//!
//!   cargo run --release -p sovereign-inference --example bench_constraint -- \
//!       --model ~/.sovereign/models/Qwen3.5-9B.Q8_0.1.gguf \
//!       --iters 5 --gen-tokens 200
//!
//! Defaults to the audit row #1 titles-expansion schema — the
//! hottest schema-constrained path in production. Override with
//! `--schema-file <path.json>` to pin a different schema.

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

use sovereign_inference::llguidance_constraint::LlguidanceConstraint;

#[derive(Clone, Copy, Debug)]
enum Backend {
    CpuOnly,
    GpuFull,
}

#[derive(Default, Clone)]
struct IterResult {
    decode_ms: u64,
    decoded_tokens: usize,
    mask_us_samples: Vec<u128>,
    ff_tokens_total: usize,
    ff_runs_total: usize,
    ff_runs_empty: usize,
}

fn main() {
    let args = Args::parse();
    println!("bench_constraint: model={}", args.model.display());
    println!(
        "config: iters={} gen_tokens={} n_ctx={} backend={:?}",
        args.iters, args.gen_tokens, args.n_ctx, args.backend
    );

    let schema = load_schema(&args);
    let schema_str = serde_json::to_string(&schema).expect("serialise schema");
    println!("schema ({} bytes): {}", schema_str.len(), short(&schema_str));
    println!();

    let backend = Arc::new(LlamaBackend::init().expect("LlamaBackend::init"));
    let gpu_layers = match args.backend {
        Backend::CpuOnly => 0,
        Backend::GpuFull => 999,
    };
    let model_params = LlamaModelParams::default().with_n_gpu_layers(gpu_layers);
    let model = Arc::new(
        LlamaModel::load_from_file(&backend, &args.model, &model_params)
            .expect("load model"),
    );
    println!(
        "model loaded: layers={} size_mb={}",
        model.n_layer(),
        model.model_size() / (1024 * 1024)
    );
    println!();

    let prompt = synthetic_prompt(&args.prompt_extra);

    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(args.n_ctx))
        .with_n_batch(args.n_ctx)
        .with_n_ubatch(512)
        .with_n_threads(args.threads as i32)
        .with_n_threads_batch(args.threads as i32);

    // SAFETY: ctx lives strictly inside this fn; model outlives ctx
    // via the outer Arc, but llama_cpp_4's new_context takes a borrow.
    let mut ctx = unsafe {
        let model_ref: &'static LlamaModel =
            &*(Arc::as_ptr(&model) as *const LlamaModel);
        model_ref
            .new_context(&backend, ctx_params)
            .expect("new_context")
    };

    // Warm pass — first turn pays KV-alloc + GPU buffer setup.
    let _ = drive_turn(&model, &mut ctx, &args, &prompt, &schema);

    let mut iters = Vec::with_capacity(args.iters);
    for iter in 0..args.iters {
        let r = drive_turn(&model, &mut ctx, &args, &prompt, &schema);
        println!(
            "  iter {iter:>2}: decode_ms={:>6}  decoded={:>4}  mask_p50_us={:>5}  mask_p99_us={:>6}  ff={:>3}/{:>3} empty={}",
            r.decode_ms,
            r.decoded_tokens,
            percentile_us(&r.mask_us_samples, 50),
            percentile_us(&r.mask_us_samples, 99),
            r.ff_tokens_total,
            r.ff_runs_total,
            r.ff_runs_empty,
        );
        iters.push(r);
    }

    report(&iters);
}

fn drive_turn(
    model: &LlamaModel,
    ctx: &mut llama_cpp_4::context::LlamaContext<'_>,
    args: &Args,
    prompt: &str,
    schema: &serde_json::Value,
) -> IterResult {
    ctx.clear_kv_cache();

    let tokens = model
        .str_to_token(prompt, AddBos::Always)
        .expect("tokenize prompt");
    let n_prompt = tokens.len();

    let n_batch = ctx.n_batch() as usize;
    let mut batch = LlamaBatch::new(n_batch, 1);
    let last_idx = n_prompt - 1;
    for (i, &tok) in tokens.iter().enumerate() {
        batch
            .add(tok, i as i32, &[0], i == last_idx)
            .expect("batch.add prefill");
    }
    ctx.decode(&mut batch).expect("prefill decode");

    let mut llg = LlguidanceConstraint::from_schema_value(schema, model)
        .expect("LlguidanceConstraint compile");

    // Greedy sampling so only the mask path varies between runs.
    let mut sampler = LlamaSampler::greedy();
    let mut decoded = 0usize;
    let mut mask_us_samples = Vec::with_capacity(args.gen_tokens);
    let mut ff_tokens_total = 0usize;
    let mut ff_runs_total = 0usize;
    let mut ff_runs_empty = 0usize;

    let t0 = Instant::now();
    while decoded < args.gen_tokens {
        // ff-tokens probe: counts deterministic-prefix yield without
        // emitting them as forced. Audit §3.C signal — answers
        // whether `ApproximateTokEnv` produces useful runs on the
        // model's vocab or if the custom `TokenizerEnv` path is needed.
        let ff = llg.forced_ff_tokens();
        ff_runs_total += 1;
        if ff.is_empty() {
            ff_runs_empty += 1;
        }
        ff_tokens_total += ff.len();

        let mask_t = Instant::now();
        let mut data = ctx.token_data_array();
        llg.mask(&mut data);
        mask_us_samples.push(mask_t.elapsed().as_micros());

        data.apply_sampler(&mut sampler);
        let token = data
            .selected_token()
            .expect("sampler should select a token");

        sampler.accept(token);
        llg.accept_llama(token);

        if model.is_eog_token(token) {
            decoded += 1;
            break;
        }
        if llg.is_stopped() {
            decoded += 1;
            break;
        }

        batch.clear();
        let pos = (n_prompt + decoded) as i32;
        batch
            .add(token, pos, &[0], true)
            .expect("batch.add decode");
        ctx.decode(&mut batch).expect("decode step");
        decoded += 1;
    }
    let decode_ms = t0.elapsed().as_millis() as u64;

    IterResult {
        decode_ms,
        decoded_tokens: decoded,
        mask_us_samples,
        ff_tokens_total,
        ff_runs_total,
        ff_runs_empty,
    }
}

fn percentile_us(samples: &[u128], pct: usize) -> u128 {
    if samples.is_empty() {
        return 0;
    }
    let mut s = samples.to_vec();
    s.sort_unstable();
    let idx = (s.len() * pct / 100).min(s.len() - 1);
    s[idx]
}

fn report(iters: &[IterResult]) {
    println!();
    println!("─── summary ───");
    let total_decoded: usize = iters.iter().map(|x| x.decoded_tokens).sum();
    let total_ms: u64 = iters.iter().map(|x| x.decode_ms).sum();
    let tok_per_s = total_decoded as f64 / (total_ms.max(1) as f64 / 1000.0);
    let all_mask: Vec<u128> =
        iters.iter().flat_map(|x| x.mask_us_samples.iter().copied()).collect();
    let p50 = percentile_us(&all_mask, 50);
    let p99 = percentile_us(&all_mask, 99);

    let ff_total: usize = iters.iter().map(|x| x.ff_tokens_total).sum();
    let ff_runs: usize = iters.iter().map(|x| x.ff_runs_total).sum();
    let ff_empty: usize = iters.iter().map(|x| x.ff_runs_empty).sum();
    let ff_yield = if ff_runs == 0 {
        "—".to_string()
    } else {
        format!(
            "{:.2} ({}/{} empty)",
            ff_total as f64 / ff_runs as f64,
            ff_empty,
            ff_runs,
        )
    };

    println!(
        "  iters={} decode tok/s={:.1} mask p50={}us mask p99={}us ff_yield={}",
        iters.len(),
        tok_per_s,
        p50,
        p99,
        ff_yield,
    );
    println!();
    println!("Audit §6 #1: ff_yield reveals whether ApproximateTokEnv is viable.");
    println!("Low yield (>50% empty runs) means the custom TokenizerEnv");
    println!("(re-adoption plan Q1 path B) would unlock more jump-forward.");
}

fn load_schema(args: &Args) -> serde_json::Value {
    if let Some(p) = &args.schema_file {
        let s = std::fs::read_to_string(p).unwrap_or_else(|e| {
            eprintln!("error: --schema-file {}: {e}", p.display());
            std::process::exit(2);
        });
        serde_json::from_str(&s).unwrap_or_else(|e| {
            eprintln!("error: --schema-file parse: {e}");
            std::process::exit(2);
        })
    } else {
        // Audit §2 row #1 — title-expansion. Smallest of the active
        // schemas; exercises object + array + string + minItems +
        // maxItems.
        serde_json::json!({
            "type": "object",
            "properties": {
                "titles": {
                    "type": "array",
                    "items": {"type": "string"},
                    "minItems": 1,
                    "maxItems": 3
                }
            },
            "required": ["titles"]
        })
    }
}

fn short(s: &str) -> String {
    if s.len() <= 80 {
        s.to_string()
    } else {
        format!("{}…", &s[..77])
    }
}

fn synthetic_prompt(extra: &str) -> String {
    let mut p = String::from(
        "You are a title expander.\nQuestion: Tell me about the photoelectric effect.\n\n\
         Reply with JSON only:\n{\"titles\": [\"Title 1\", \"Title 2\"]}\n",
    );
    if !extra.is_empty() {
        p.push('\n');
        p.push_str(extra);
    }
    p
}

struct Args {
    model: PathBuf,
    schema_file: Option<PathBuf>,
    iters: usize,
    gen_tokens: usize,
    n_ctx: u32,
    threads: usize,
    backend: Backend,
    prompt_extra: String,
}

impl Args {
    fn parse() -> Self {
        let raw: Vec<String> = std::env::args().collect();
        let mut model = None;
        let mut schema_file = None;
        let mut iters = 5usize;
        let mut gen_tokens = 200usize;
        let mut n_ctx = 8192u32;
        let mut threads = 8usize;
        let mut backend = Backend::GpuFull;
        let mut prompt_extra = String::new();

        let mut i = 1;
        while i < raw.len() {
            match raw[i].as_str() {
                "--model" => {
                    i += 1;
                    model = raw.get(i).map(PathBuf::from);
                }
                "--schema-file" => {
                    i += 1;
                    schema_file = raw.get(i).map(PathBuf::from);
                }
                "--iters" => {
                    i += 1;
                    iters = raw[i].parse().unwrap_or(5);
                }
                "--gen-tokens" => {
                    i += 1;
                    gen_tokens = raw[i].parse().unwrap_or(200);
                }
                "--n-ctx" => {
                    i += 1;
                    n_ctx = raw[i].parse().unwrap_or(8192);
                }
                "--threads" => {
                    i += 1;
                    threads = raw[i].parse().unwrap_or(8);
                }
                "--backend" => {
                    i += 1;
                    backend = match raw[i].as_str() {
                        "cpu" => Backend::CpuOnly,
                        "gpu" => Backend::GpuFull,
                        other => {
                            eprintln!("unknown --backend: {other}");
                            std::process::exit(2);
                        }
                    };
                }
                "--prompt-extra" => {
                    i += 1;
                    prompt_extra = raw.get(i).cloned().unwrap_or_default();
                }
                _ => {}
            }
            i += 1;
        }

        let model = model.unwrap_or_else(|| {
            eprintln!("error: --model <path.gguf> is required");
            eprintln!();
            eprintln!(
                "usage: bench_constraint --model <gguf> [--iters 5] [--gen-tokens 200]"
            );
            eprintln!("       [--n-ctx 8192] [--threads 8] [--backend cpu|gpu]");
            eprintln!("       [--schema-file <path>] [--prompt-extra \"...\"]");
            std::process::exit(2);
        });
        Args {
            model,
            schema_file,
            iters,
            gen_tokens,
            n_ctx,
            threads,
            backend,
            prompt_extra,
        }
    }
}
