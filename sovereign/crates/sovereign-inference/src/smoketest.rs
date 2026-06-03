//! Subprocess crash-isolation smoke test for chat-slot models.
//!
//! Some (model × backend) combos null-deref inside ggml's GPU
//! kernel-pipeline lookup at decode time — observed for Gemma 4
//! E4B on Apple Metal with `llama-cpp-2 0.1.145`. A SIGSEGV in C
//! code is unrecoverable in-process: the OS kills the process and
//! Rust's `catch_unwind` doesn't fire.
//!
//! The desktop sidesteps this by running this smoke test as a
//! subprocess of itself (via a hidden `--smoketest` flag on its
//! own binary) *before* loading any model into the user-facing
//! slot. If the child crashes, the parent stays alive and can
//! offer a CPU fallback or steer the user to a different model.
//!
//! Lives in `sovereign-inference` rather than the desktop crate
//! because all the real work touches `llama-cpp-2`, which is
//! already configured here with the right backend features
//! (`metal` on macOS, `rocm` on Linux, `llguidance` everywhere).
//! The desktop crate just calls [`run_from_argv`] from inside its
//! `main()` and stays free of llama-cpp-2 details.
//!
//! ## What's tested
//!
//! - Backend init (`LlamaBackend::init`).
//! - Model load (`LlamaModel::load_from_file`) — exercises GPU
//!   weight upload paths.
//! - Context creation with the production offload flags —
//!   exercises Metal kernel-pipeline compilation.
//! - One single-token decode — exercises `mul_mat`, the kernel
//!   that null-derefs in the Gemma-4-Metal failure mode.
//!
//! Exit codes:
//!   `0`  — load + decode ok.
//!   `1`  — Rust-side error (model load rejected, etc.).
//!   `*`  — process aborted via SIGSEGV / SIGABRT (the upstream
//!          crash; parent reads via `ExitStatus::signal()`).

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use crate::llama::cpp::context::params::LlamaContextParams;
use crate::llama::cpp::llama_backend::LlamaBackend;
use crate::llama::cpp::llama_batch::LlamaBatch;
use crate::llama::cpp::model::params::LlamaModelParams;
use crate::llama::cpp::model::{AddBos, LlamaModel};

/// Marker flag the parent passes to its own exe to enter
/// smoketest mode. Hidden — never advertised to end users.
pub const SMOKETEST_FLAG: &str = "--smoketest";

/// Top-level smoketest entry point. Call from `main()`:
///
/// ```ignore
/// let argv: Vec<String> = std::env::args().collect();
/// if let Some(code) = sovereign_inference::smoketest::run_from_argv(&argv) {
///     return code;
/// }
/// ```
///
/// Returns `Some(code)` when the smoketest flag was seen and the
/// caller should exit with that code; `None` when the flag wasn't
/// present and the caller should proceed with normal startup.
pub fn run_from_argv(argv: &[String]) -> Option<ExitCode> {
    let pos = argv.iter().position(|a| a == SMOKETEST_FLAG)?;
    let args = parse_args(&argv[pos + 1..]);
    Some(run(args))
}

struct SmokeArgs {
    model_path: PathBuf,
    n_gpu_layers: u32,
    n_ctx: u32,
}

fn parse_args(rest: &[String]) -> SmokeArgs {
    let mut model_path: Option<PathBuf> = None;
    let mut n_gpu_layers: u32 = 999;
    let mut n_ctx: u32 = 8192;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--model" => {
                i += 1;
                if i >= rest.len() {
                    eprintln!("smoketest: --model requires a path");
                    std::process::exit(2);
                }
                model_path = Some(PathBuf::from(&rest[i]));
            }
            "--gpu-layers" => {
                i += 1;
                n_gpu_layers = rest.get(i).and_then(|s| s.parse().ok()).unwrap_or_else(|| {
                    eprintln!("smoketest: --gpu-layers requires a u32");
                    std::process::exit(2);
                });
            }
            "--ctx" => {
                i += 1;
                n_ctx = rest.get(i).and_then(|s| s.parse().ok()).unwrap_or_else(|| {
                    eprintln!("smoketest: --ctx requires a u32");
                    std::process::exit(2);
                });
            }
            _ => {
                // Unknown args are ignored (forward-compat).
            }
        }
        i += 1;
    }
    let model_path = model_path.unwrap_or_else(|| {
        eprintln!("smoketest: --model <path> is required");
        std::process::exit(2);
    });
    SmokeArgs {
        model_path,
        n_gpu_layers,
        n_ctx,
    }
}

fn run(args: SmokeArgs) -> ExitCode {
    eprintln!(
        "smoketest: model={} gpu_layers={} n_ctx={}",
        args.model_path.display(),
        args.n_gpu_layers,
        args.n_ctx,
    );

    let mut backend = match LlamaBackend::init() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("smoketest: backend init failed: {e}");
            return ExitCode::from(1);
        }
    };
    // Suppress C-level stderr so the parent's status decisions are
    // based on exit code / signal, not stderr noise. Set
    // `SOVEREIGN_LLAMA_LOGS=1` to see them when debugging this path.
    if std::env::var("SOVEREIGN_LLAMA_LOGS").ok().as_deref() != Some("1") {
        backend.void_logs();
    }
    let backend = Arc::new(backend);

    let model_params = LlamaModelParams::default().with_n_gpu_layers(args.n_gpu_layers);
    let model = match LlamaModel::load_from_file(&backend, &args.model_path, &model_params) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("smoketest: model load failed: {e}");
            return ExitCode::from(1);
        }
    };

    // Match production `ModelSlot::load` chat-slot context params
    // so we exercise the same kernel-pipeline cache. The flags
    // that matter for the Metal-pipeline-nil bug:
    //   - offload_kqv(true) + op_offload(true) + n_gpu_layers>0
    //     → all-GPU forward pass; this is what crashes for Gemma 4.
    let wants_gpu = args.n_gpu_layers > 0;
    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(args.n_ctx))
        // MIGRATION 2026-05-17: .with_n_seq_max(...) retired in llama-cpp-4 0.2.x — see crate::llama
        .with_n_batch(args.n_ctx)
        .with_n_ubatch(512)
        .with_offload_kqv(wants_gpu);
    // MIGRATION 2026-05-17: .with_op_offload(...) retired in llama-cpp-4 0.2.x — see crate::llama
    let mut ctx = match model.new_context(&backend, ctx_params) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("smoketest: ctx create failed: {e}");
            return ExitCode::from(1);
        }
    };

    // Decode a tiny prompt. The exact tokens don't matter — we
    // just need ggml's compute graph to run mul_mat through the
    // backend's kernel-pipeline cache. If the kernel for this
    // model+backend combo is absent, the lookup returns nil and
    // C derefs it → SIGSEGV → process dies before this returns.
    let prompt = "hello";
    let tokens = match model.str_to_token(prompt, AddBos::Always) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("smoketest: tokenize failed: {e}");
            return ExitCode::from(1);
        }
    };
    if tokens.is_empty() {
        eprintln!("smoketest: tokenizer returned 0 tokens");
        return ExitCode::from(1);
    }
    let mut batch = LlamaBatch::new(tokens.len().max(8), 1);
    let last_idx = tokens.len() - 1;
    for (i, &tok) in tokens.iter().enumerate() {
        if let Err(e) = batch.add(tok, i as i32, &[0], i == last_idx) {
            eprintln!("smoketest: batch add failed: {e}");
            return ExitCode::from(1);
        }
    }
    let started = Instant::now();
    if let Err(e) = ctx.decode(&mut batch) {
        eprintln!("smoketest: decode returned error: {e}");
        return ExitCode::from(1);
    }
    eprintln!(
        "smoketest: ok — {} tokens decoded in {} ms",
        tokens.len(),
        started.elapsed().as_millis()
    );
    ExitCode::SUCCESS
}
