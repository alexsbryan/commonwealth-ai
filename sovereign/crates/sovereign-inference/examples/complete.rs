// SPDX-License-Identifier: AGPL-3.0-or-later
use std::path::PathBuf;

use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::CompletionRequest;
use sovereign_inference::embedded::EmbeddedLlamaCpp;

fn print_usage() {
    eprintln!("Usage: complete --model <path.gguf> --prompt <text> [--stream] [--max-tokens N]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --model <path>       Path to a GGUF model file");
    eprintln!("  --prompt <text>      Prompt text to complete");
    eprintln!("  --stream             Stream tokens as they're generated");
    eprintln!("  --max-tokens <N>     Maximum tokens to generate (default: 256)");
    eprintln!("  --temperature <T>    Sampling temperature (default: 0.7)");
}

fn parse_args() -> Option<Args> {
    let args: Vec<String> = std::env::args().collect();
    let mut model = None;
    let mut prompt = None;
    let mut stream = false;
    let mut max_tokens = 256usize;
    let mut temperature = 0.7f32;
    let mut ctx = 2048u32;
    let mut gpu_layers: Option<u32> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--model" => {
                i += 1;
                model = args.get(i).map(PathBuf::from);
            }
            "--prompt" => {
                i += 1;
                prompt = args.get(i).cloned();
            }
            "--stream" => {
                stream = true;
            }
            "--max-tokens" => {
                i += 1;
                max_tokens = args.get(i)?.parse().ok()?;
            }
            "--temperature" => {
                i += 1;
                temperature = args.get(i)?.parse().ok()?;
            }
            "--ctx" => {
                i += 1;
                ctx = args.get(i)?.parse().ok()?;
            }
            "--gpu-layers" => {
                i += 1;
                gpu_layers = Some(args.get(i)?.parse().ok()?);
            }
            _ => {
                eprintln!("Unknown argument: {}", args[i]);
                return None;
            }
        }
        i += 1;
    }

    Some(Args {
        model: model?,
        prompt: prompt?,
        stream,
        max_tokens,
        temperature,
        ctx,
        gpu_layers,
    })
}

struct Args {
    model: PathBuf,
    prompt: String,
    stream: bool,
    max_tokens: usize,
    temperature: f32,
    ctx: u32,
    gpu_layers: Option<u32>,
}

#[tokio::main]
async fn main() {
    // Glassbox: surface sovereign-inference's RPC registration / prune / split
    // logs (otherwise dropped — this example had no subscriber). RUST_LOG-driven.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let args = match parse_args() {
        Some(a) => a,
        None => {
            print_usage();
            std::process::exit(1);
        }
    };

    eprintln!(
        "Loading model: {} (ctx={}, gpu_layers={:?})",
        args.model.display(),
        args.ctx,
        args.gpu_layers
    );
    let provider =
        match EmbeddedLlamaCpp::load_full(&args.model, None, None, args.ctx, args.gpu_layers) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Failed to load model: {e}");
                std::process::exit(1);
            }
        };

    let caps = provider.capabilities();
    eprintln!(
        "Capabilities: max_ctx={}, speed={:?}, reasoning={:?}",
        caps.max_context_tokens, caps.relative_speed, caps.relative_reasoning
    );

    let request = CompletionRequest {
        prompt: args.prompt.clone(),
        system_message: None,
        preferred_speed: sovereign_core::types::Speed::Slow,
        max_tokens: Some(args.max_tokens),
        temperature: Some(args.temperature),
        structured_output: None,
        think_budget: None,
        top_k: None,
        top_p: None,
        oicp: None,
        tools: None,
        tool_choice: None,
        model_id: None,
        enable_thinking: None,
        sampling_mode: None,
        assistant_prefix: None,
        cmd_prefix: None,
        url_allowlist: None,
        evidence_id_allowlist: None,
        lark_grammar: None,
        prompt_shape: None,
        stable_prefix_len: None,
        ..Default::default()
    };

    if args.stream {
        eprintln!("--- Streaming response ---");
        use futures::StreamExt;
        match provider.complete_stream(&request).await {
            Ok(mut stream) => {
                while let Some(result) = stream.next().await {
                    match result {
                        Ok(token) => print!("{token}"),
                        Err(e) => {
                            eprintln!("\nStream error: {e}");
                            break;
                        }
                    }
                }
                println!();
            }
            Err(e) => eprintln!("Failed to start stream: {e}"),
        }
    } else {
        eprintln!("--- Generating response ---");
        match provider.complete(&request).await {
            Ok(response) => {
                println!("{}", response.text);
                eprintln!(
                    "--- {} tokens, {}ms, model: {} ---",
                    response.tokens_used, response.latency_ms, response.model_id
                );
            }
            Err(e) => eprintln!("Completion failed: {e}"),
        }
    }
}
