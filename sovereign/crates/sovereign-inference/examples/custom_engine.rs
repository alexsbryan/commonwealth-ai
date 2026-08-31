// SPDX-License-Identifier: AGPL-3.0-or-later
//! **The template for swapping in your own inference engine.**
//!
//! Run it: `cargo run -p sovereign-inference --example custom_engine`
//!
//! This is the whole contract for an engine that is not llama.cpp, executed
//! end to end against the real types — no mocks of the seam itself. It
//! walks the five steps a third party actually performs:
//!
//!   1. Implement [`InferenceProvider`] for your engine. Four methods are
//!      required; every other one has a default that degrades honestly.
//!   2. Implement [`EngineBuilder`] so the factory can construct it from
//!      the operator's `[engine]` section.
//!   3. `register_engine("your-name", ...)` in `main()`, before the host
//!      builds its provider. Rust has no safe cross-crate ABI for
//!      `dyn Trait`, so an in-process engine is always compiled into a
//!      binary you control — registration at `main()` is the mechanism,
//!      not a workaround.
//!   4. Point a real `config.toml` at it: `[engine] kind = "your-name"`.
//!   5. Prove it: run `engine_conformance` before you ship.
//!
//! The engine below is deliberately trivial (it holds no weights and does
//! no I/O) so the example runs anywhere in milliseconds. Everything around
//! it — the config parse, the factory dispatch, the trait surface, the
//! conformance suite — is the production path, unmodified.
//!
//! If your engine already speaks an OpenAI-compatible HTTP API (vLLM, TGI,
//! SGLang, llama-server, or your own tuned server), you do NOT need any of
//! this: set `[engine] kind = "remote"` with an `endpoint` and you are
//! done. This file is for putting an engine *in process*.

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;

// The crate's `Result<T>` is single-generic (error is always
// `sovereign_core::Error`). Aliased so std's two-generic `Result`
// stays available for `EngineBuilder::build`, which reports a plain
// operator-facing `String`.
use sovereign_core::error::Result as SovResult;
use sovereign_core::setup_config::{EngineSection, SetupConfig};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, Depth, FinishReason, ProviderCapabilities, Speed,
    StreamFrame,
};
use sovereign_inference::engine_conformance::{check_serving, check_sync};
use sovereign_inference::engine_factory::{
    available_engines, build_engine, register_engine, BuiltEngine, EngineBuilder,
};

/// The name the operator writes in `config.toml`.
const ENGINE_NAME: &str = "hypertuned-demo";

// ─────────────────────────────────────────────────────────────────────
// STEP 1 — your engine
// ─────────────────────────────────────────────────────────────────────

/// Stand-in for a hand-tuned kernel targeting one specific machine.
///
/// It reads its own configuration out of the operator's `[engine]`
/// section, which is the realistic shape: `endpoint` names the device,
/// `context_size` the window the tuned kernels were built for.
struct HypertunedEngine {
    device: String,
    context_size: u32,
}

#[async_trait]
impl InferenceProvider for HypertunedEngine {
    // ── The four required methods ────────────────────────────────────

    async fn complete(&self, request: &CompletionRequest) -> SovResult<CompletionResponse> {
        let text = format!(
            "[{ENGINE_NAME} on {}] answered {} prompt chars",
            self.device,
            request.prompt.len()
        );
        Ok(CompletionResponse {
            tokens_used: 8,
            prompt_tokens: 4,
            model_id: self.model_id_for(request.preferred_speed),
            latency_ms: 0,
            oicp_meta: None,
            // Real engines report the truth here; it becomes the OpenAI
            // wire `finish_reason`.
            finish_reason: Some(FinishReason::Stop),
            completion_tokens: Some(4),
            text,
        })
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> SovResult<Pin<Box<dyn Stream<Item = SovResult<String>> + Send>>> {
        let words: Vec<SovResult<String>> = format!("[{ENGINE_NAME}] {}", request.prompt)
            .split_whitespace()
            .map(|w| Ok(format!("{w} ")))
            .collect();
        Ok(Box::pin(futures::stream::iter(words)))
    }

    async fn embed(&self, text: &str) -> SovResult<Vec<f32>> {
        // A real engine runs its embedding kernel. This one hashes, so the
        // vectors are at least deterministic and non-degenerate.
        let mut v = vec![0.0f32; 8];
        for (i, b) in text.bytes().enumerate() {
            v[i % 8] += f32::from(b) / 255.0;
        }
        Ok(v)
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: self.context_size as usize,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Moderate,
        }
    }

    // ── Overrides worth making. Everything not listed here keeps the
    //    trait's default, which is the honest answer for an engine that
    //    does not have the feature.

    fn model_id_for(&self, _speed: Speed) -> String {
        format!("{ENGINE_NAME}/{}", self.device)
    }

    /// Persisted next to every cached embedding as a staleness guard, so
    /// it must change whenever the embedding kernel does. Returning the
    /// default `"unknown"` here would be honest but would stop callers
    /// caching at all.
    fn embed_model_id(&self) -> String {
        format!("{ENGINE_NAME}-embed-v1")
    }

    fn effective_context_size(&self) -> Option<u32> {
        Some(self.context_size)
    }

    /// Override this ONLY if you can report the real reason generation
    /// stopped. The default wraps `complete_stream` and synthesises a
    /// terminal `Stop`, which is correct for engines that cannot tell —
    /// and a stream with NO terminal frame is read by every receiver as a
    /// cancellation, which is why `check_serving` tests for it.
    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> SovResult<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        let mut frames: Vec<StreamFrame> = format!("[{ENGINE_NAME}] {}", request.prompt)
            .split_whitespace()
            .map(|w| StreamFrame::Token(format!("{w} ")))
            .collect();
        frames.push(StreamFrame::Finish {
            reason: FinishReason::Stop,
            usage: None,
        });
        Ok(Box::pin(futures::stream::iter(frames)))
    }
}

// ─────────────────────────────────────────────────────────────────────
// STEP 2 — teach the factory to build it
// ─────────────────────────────────────────────────────────────────────

struct HypertunedBuilder;

impl EngineBuilder for HypertunedBuilder {
    fn build(&self, section: &EngineSection) -> Result<BuiltEngine, String> {
        // Refuse on missing configuration rather than defaulting. A
        // defaulted device makes a misconfigured node look healthy right
        // up until the first request (ARCH §18.3).
        let device = section.endpoint.clone().ok_or_else(|| {
            format!("[engine] kind = \"{ENGINE_NAME}\" requires `endpoint` naming the device")
        })?;
        Ok(BuiltEngine::external(Arc::new(HypertunedEngine {
            device,
            context_size: section.context_size,
        })))
    }
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // ── STEP 3 — register, before anything builds a provider ─────────
    register_engine(ENGINE_NAME, Arc::new(HypertunedBuilder))?;
    println!("registered. this binary can serve: {:?}\n", available_engines());

    // ── STEP 4 — a REAL config.toml, parsed by the real loader ───────
    let dir = tempfile::tempdir()?;
    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[models]
# Required by the schema, and deliberately NONEXISTENT here: a non-llama
# engine must never touch them. If anything on this path opened a GGUF,
# this example would fail rather than pass.
primary = "/nonexistent/primary.gguf"
embed   = "/nonexistent/embed.gguf"

[engine]
kind = "{ENGINE_NAME}"
endpoint = "gpu0:tuned-kernels-v3"
context_size = 32768
"#
        ),
    )?;
    let config = SetupConfig::load_from(&config_path)?;
    println!("config.toml says [engine] kind = {}", config.engine.kind);

    // ── The factory picks the engine. Same call the daemon makes. ─────
    let built = build_engine(&config)?;
    println!("built: {built:?}");
    assert!(
        built.llama.is_none(),
        "a custom engine must not carry a llama handle"
    );

    // From here the rest of the system sees only the trait.
    let engine: Arc<dyn InferenceProvider> = built.provider;

    // ── STEP 5 — prove it before shipping ────────────────────────────
    let sync_violations = check_sync(engine.as_ref());
    let serving_violations = check_serving(engine.as_ref()).await;
    println!(
        "\nconformance: {} sync violation(s), {} serving violation(s)",
        sync_violations.len(),
        serving_violations.len()
    );
    for v in sync_violations.iter().chain(serving_violations.iter()) {
        println!("  ✘ {v}");
    }
    if !sync_violations.is_empty() || !serving_violations.is_empty() {
        return Err("engine does not conform — see violations above".into());
    }

    // ── And actually serve, through the same seam the runtime uses ───
    let answer = engine.complete(&CompletionRequest::new("what hardware are you?")).await?;
    println!("\ncomplete()      -> {}", answer.text);
    println!("model_id        -> {}", answer.model_id);

    use futures::StreamExt;
    let mut stream = engine
        .complete_stream_with_finish(&CompletionRequest::new("stream me"))
        .await?;
    let mut streamed = String::new();
    let mut terminal = None;
    while let Some(frame) = stream.next().await {
        match frame {
            StreamFrame::Token(t) => streamed.push_str(&t),
            other => terminal = Some(other),
        }
    }
    println!("stream()        -> {}", streamed.trim());
    println!("terminal frame  -> {terminal:?}");
    println!(
        "embed()         -> {} dims, model {}",
        engine.embed("hello").await?.len(),
        engine.embed_model_id()
    );
    println!("context window  -> {:?}", engine.effective_context_size());

    println!("\n✓ a non-llama engine was selected by config, built by the factory,");
    println!("  passed the conformance suite, and served real traffic — with no GGUF");
    println!("  on disk and llama.cpp never entering the request path.");
    Ok(())
}
