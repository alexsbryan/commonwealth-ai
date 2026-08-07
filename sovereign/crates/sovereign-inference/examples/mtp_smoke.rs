// SPDX-License-Identifier: AGPL-3.0-or-later
//! **MTP speculative-decode smoke test against a real MTP gguf.**
//!
//! The workspace test suite cannot cover this path: it needs a
//! multi-gigabyte MTP-bearing gguf and a GPU. So `generate_sync_mtp_impl`
//! — the most delicate function in `sovereign-inference` — is exercised
//! by nothing until someone runs this.
//!
//! ```text
//! cargo run -p sovereign-inference --example mtp_smoke -- \
//!     --model sovereign/models/Qwen3.5-4B-UD-MTP-Q6_K_XL.gguf
//! ```
//!
//! **What it checks, and why each check exists.**
//!
//! 1. **The slot actually reached `Speculative` mode.** Without this the
//!    whole run is vacuous: a slot that silently fell back to
//!    `SingleToken` records no MTP calls at all, and an empty transcript
//!    passes every sequence verifier trivially. This is the check that
//!    stops the other checks from being theatre.
//!
//! 2. **The FFI call order holds, per request**, via
//!    `ffi_trace::verify_transcript` — `MtpSessionBuilt` before any
//!    session use, `SessionProcess` after EVERY `MtpDecode`. These are
//!    the invariants in [[project_mtp_invariants]].
//!
//! 3. **Sequential requests on ONE slot.** This is the regression case,
//!    not a formality. The session's internal draft-scheduler state
//!    survives a KV-cache clear, so a session reused across requests
//!    desynchronises after 1-2 of them and the draft head proposes from
//!    stale branches — observed 2026-05-17 as fluent, confident,
//!    completely unrelated answers ("Python sorting algorithm" to an
//!    unrelated question) once the search-gym ran `--replays > 1`.
//!    Request 1 alone cannot catch it.
//!
//! **What it does NOT check.** Nothing here judges output quality.
//! Failure mode 3 produces well-formed prose, so the transcript
//! verifiers cannot see it — only a human reading the answers can. That
//! is why every response is printed in full rather than summarised, and
//! why the exit code is necessary but not sufficient. Read the answers.

use std::path::PathBuf;
use std::time::Instant;

use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::CompletionRequest;
use sovereign_inference::embedded::ffi_trace::{self, FfiCall};
use sovereign_inference::embedded::EmbeddedLlamaCpp;

/// Distinct, factual, and mutually unrelated on purpose: if request N's
/// answer drifts toward request N-1's topic, that is the stale-draft
/// signature rather than an unlucky sample.
const PROMPTS: &[&str] = &[
    "In one sentence: what is the capital of Japan?",
    "In one sentence: what does a thermostat do?",
    "In one sentence: why is the sky blue?",
];

fn request_for(prompt: &str) -> CompletionRequest {
    CompletionRequest {
        prompt: prompt.to_string(),
        system_message: None,
        preferred_speed: sovereign_core::types::Speed::Slow,
        max_tokens: Some(96),
        // Greedy. A stale-draft desync should not be maskable as an
        // unlucky sample, and run-to-run comparison should be honest.
        temperature: Some(0.0),
        structured_output: None,
        think_budget: None,
        top_k: None,
        top_p: None,
        oicp: None,
        tools: None,
        tool_choice: None,
        model_id: None,
        enable_thinking: Some(false),
        sampling_mode: None,
        assistant_prefix: None,
        cmd_prefix: None,
        url_allowlist: None,
        evidence_id_allowlist: None,
        lark_grammar: None,
        prompt_shape: None,
        stable_prefix_len: None,
    }
}

fn count(t: &[FfiCall], want: FfiCall) -> usize {
    t.iter().filter(|c| **c == want).count()
}

#[tokio::main]
async fn main() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let mut model: Option<PathBuf> = None;
    let mut ctx = 4096u32;
    let mut gpu_layers: Option<u32> = None;
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--model" => {
                i += 1;
                model = args.get(i).map(PathBuf::from);
            }
            "--ctx" => {
                i += 1;
                ctx = args[i].parse().expect("--ctx must be a number");
            }
            "--gpu-layers" => {
                i += 1;
                gpu_layers = Some(args[i].parse().expect("--gpu-layers must be a number"));
            }
            other => {
                eprintln!("unknown argument: {other}");
                eprintln!("usage: mtp_smoke --model <mtp.gguf> [--ctx N] [--gpu-layers N]");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    let Some(model) = model else {
        eprintln!("usage: mtp_smoke --model <mtp.gguf> [--ctx N] [--gpu-layers N]");
        eprintln!("  the gguf MUST be an MTP build — a non-MTP model loads as SingleToken");
        eprintln!("  and this harness will (correctly) fail rather than report a vacuous pass.");
        std::process::exit(2);
    };

    eprintln!("=== mtp_smoke ===");
    eprintln!("model:      {}", model.display());
    eprintln!("ctx:        {ctx}");
    eprintln!("gpu_layers: {gpu_layers:?}");

    let t_load = Instant::now();
    let provider = match EmbeddedLlamaCpp::load_full(&model, None, None, ctx, gpu_layers) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("FAIL: model load: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("loaded in {:?}\n", t_load.elapsed());

    let mut failures: Vec<String> = Vec::new();

    for (n, prompt) in PROMPTS.iter().enumerate() {
        let n = n + 1;
        // Arm per request: `take()` disables recording, so each request
        // gets its own transcript and the verifiers see one request's
        // call sequence rather than a concatenation of all of them.
        ffi_trace::enable();
        let t0 = Instant::now();
        let result = provider.complete(&request_for(prompt)).await;
        let elapsed = t0.elapsed();
        let transcript = ffi_trace::take();

        let built = count(&transcript, FfiCall::MtpSessionBuilt);
        let decodes = count(&transcript, FfiCall::MtpDecode);
        let processes = count(&transcript, FfiCall::SessionProcess);

        println!(
            "── request {n}/{} ─────────────────────────────",
            PROMPTS.len()
        );
        println!("prompt:   {prompt}");
        match &result {
            Ok(r) => println!("response: {}", r.text.trim()),
            Err(e) => println!("response: <ERROR> {e}"),
        }
        println!(
            "ffi:      MtpSessionBuilt={built}  MtpDecode={decodes}  \
             SessionProcess={processes}  (transcript {} calls)",
            transcript.len()
        );
        println!("elapsed:  {elapsed:?}");

        if let Err(e) = &result {
            failures.push(format!("request {n}: completion failed: {e}"));
        }

        // CHECK 1 — the run is not vacuous. A SingleToken fallback
        // records zero MTP calls, and zero MTP calls satisfy every
        // sequence verifier. Assert the path was actually taken before
        // trusting anything the verifiers say about it.
        if built == 0 {
            failures.push(format!(
                "request {n}: NO MtpSessionBuilt — the slot is not in Speculative mode. \
                 Either the gguf is not an MTP build, or the speculative upgrade probe \
                 failed at load and the slot silently fell back to SingleToken. Every \
                 other check this harness makes is vacuous until this one passes."
            ));
        } else if built != 1 {
            failures.push(format!(
                "request {n}: MtpSessionBuilt fired {built}x — expected exactly 1 per \
                 request. More than one session over the same contexts is UB inside \
                 common_speculative_*."
            ));
        }
        if decodes == 0 {
            failures.push(format!(
                "request {n}: no MtpDecode recorded — the MTP loop never decoded."
            ));
        }

        // CHECK 2 — the call ORDER invariants, on this request's own
        // transcript.
        if let Err(violations) = ffi_trace::verify_transcript(&transcript) {
            for v in violations {
                failures.push(format!("request {n}: {v}"));
            }
        }
        println!();
    }

    println!("════════════════════════════════════════════════");
    if failures.is_empty() {
        println!(
            "PASS — {} sequential MTP requests, call-order invariants hold.",
            PROMPTS.len()
        );
        println!();
        println!("This exit code proves the PATH, not the OUTPUT. Read the three");
        println!("responses above: each must answer ITS OWN prompt. Fluent prose on");
        println!("the wrong topic is the stale-draft signature and no verifier here");
        println!("can see it.");
    } else {
        println!("FAIL — {} problem(s):", failures.len());
        for f in &failures {
            println!("  - {f}");
        }
        std::process::exit(1);
    }
}
