use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use futures::Stream;
use tokio::sync::Mutex;

use llama_cpp_2::context::params::{LlamaContextParams, LlamaPoolingType};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

use sovereign_core::error::Error;
use sovereign_core::model_family::{EmbedQuirks, ModelFamily, ModelQuirks, PoolingStrategy, ThinkingControl};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::*;
use sovereign_core::Result;

use crate::hardware::HardwareProfile;

// ─── ModelSlot ─────────────────────────────────────────────────

struct SlotContext {
    ctx: llama_cpp_2::context::LlamaContext<'static>,
    _model: Arc<LlamaModel>,
}

unsafe impl Send for SlotContext {}
unsafe impl Sync for SlotContext {}

struct ModelSlot {
    model: Arc<LlamaModel>,
    context: Mutex<SlotContext>,
    model_id: String,
}

impl ModelSlot {
    fn load(
        backend: &Arc<LlamaBackend>,
        model_path: &Path,
        context_size: u32,
        n_gpu_layers: u32,
    ) -> Result<Self> {
        let model_params = LlamaModelParams::default().with_n_gpu_layers(n_gpu_layers);

        let model = LlamaModel::load_from_file(backend, model_path, &model_params)
            .map_err(|e| Error::Inference(format!("Failed to load model: {e}")))?;

        let model_id = model_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let model = Arc::new(model);

        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(context_size))
            // n_batch = context_size so the full context window is available
            // for prompt processing. llama.cpp automatically splits the batch
            // into n_ubatch=512 micro-batches for GPU/CPU kernel calls, so
            // memory pressure is unchanged — only the Rust-side assertion limit
            // is relaxed.
            .with_n_batch(context_size)
            .with_n_ubatch(512);

        let ctx = unsafe {
            let model_ref: &'static LlamaModel =
                &*(Arc::as_ptr(&model) as *const LlamaModel);
            model_ref
                .new_context(backend, ctx_params)
                .map_err(|e| Error::Inference(format!("Failed to create context: {e}")))?
        };

        tracing::info!(
            model_id = %model_id,
            params = model.n_params(),
            layers = model.n_layer(),
            size_mb = model.size() / (1024 * 1024),
            "ModelSlot::load — slot ready"
        );

        Ok(Self {
            model: model.clone(),
            context: Mutex::new(SlotContext {
                ctx,
                _model: model,
            }),
            model_id,
        })
    }

    fn generate_sync(
        model: &LlamaModel,
        ctx: &mut llama_cpp_2::context::LlamaContext<'_>,
        request: &CompletionRequest,
        quirks: &ModelQuirks,
    ) -> Result<(String, usize)> {
        // Clear KV cache before each new inference sequence.
        // `clear_kv_cache()` is also called at the end of a successful run, but
        // if a prior call failed mid-decode the cache is left dirty. Pre-clearing
        // guarantees a clean slate regardless of the prior exit path, preventing
        // M-RoPE position mismatch errors ("X = 830, Y = 0: X < Y required").
        ctx.clear_kv_cache();

        let full_prompt = format_prompt(model, request, quirks)?;

        let tokens = model
            .str_to_token(&full_prompt, AddBos::Always)
            .map_err(|e| Error::Inference(format!("Tokenization failed: {e}")))?;

        let max_tokens = request.max_tokens.unwrap_or(1024);

        let n_batch = ctx.n_batch() as usize;
        let n_ctx = ctx.n_ctx() as usize;

        // Guard: reject only if the prompt + response won't fit in the context
        // window at all. n_batch == context_size (set at load time) so the
        // llama.cpp assertion n_tokens_all <= cparams.n_batch is automatically
        // satisfied for any prompt that passes this check.
        if tokens.len() + max_tokens > n_ctx {
            return Err(Error::Inference(format!(
                "Prompt too long: {} tokens + {} max response tokens exceeds \
                 context window of {}. Try a shorter message.",
                tokens.len(),
                max_tokens,
                n_ctx
            )));
        }

        let mut batch = LlamaBatch::new(n_batch, 1);
        let last_idx = tokens.len() - 1;
        for (i, &token) in tokens.iter().enumerate() {
            batch
                .add(token, i as i32, &[0], i == last_idx)
                .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;
        }

        ctx.decode(&mut batch)
            .map_err(|e| Error::Inference(format!("Prompt decode failed: {e}")))?;

        let mut sampler = build_sampler(model, request, quirks);
        let mut output = String::new();
        let mut n_generated = 0usize;
        let mut decoder = encoding_rs::UTF_8.new_decoder();

        // Think-block budget forcing: track position inside <think>…</think>
        // and inject a closing tag once the budget is exhausted so the model
        // is forced to summarise rather than spiral indefinitely.
        let mut tail = String::with_capacity(32);
        let mut in_think = false;
        let mut think_tokens = 0usize;
        let mut think_budget_fired = false;

        while n_generated < max_tokens {
            let token = sampler.sample(ctx, -1);
            sampler.accept(token);

            if model.is_eog_token(token) {
                break;
            }

            if let Ok(piece) = model.token_to_piece(token, &mut decoder, true, None) {
                // Update sliding tail for tag detection.
                tail.push_str(&piece);
                if tail.len() > 32 {
                    // Walk back from len-32 to the nearest char boundary so we
                    // never slice through a multi-byte UTF-8 sequence (Qwen3).
                    let mut drain_to = tail.len() - 32;
                    while drain_to > 0 && !tail.is_char_boundary(drain_to) {
                        drain_to -= 1;
                    }
                    tail.drain(..drain_to);
                }
                if !in_think && tail.contains("<think>") {
                    in_think = true;
                } else if in_think && tail.contains("</think>") {
                    in_think = false;
                }
                if in_think {
                    think_tokens += 1;
                }

                output.push_str(&piece);
            }

            n_generated += 1;

            batch.clear();
            let pos = (tokens.len() + n_generated - 1) as i32;
            batch
                .add(token, pos, &[0], true)
                .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;

            ctx.decode(&mut batch)
                .map_err(|e| Error::Inference(format!("Decode failed: {e}")))?;

            // Budget forcing: inject </think> if the think block runs too long.
            if in_think && think_tokens >= request.think_budget.unwrap_or(THINK_BUDGET) && !think_budget_fired {
                think_budget_fired = true;
                let force = "\n</think>\n\n";
                if let Ok(close_tokens) = model.str_to_token(force, AddBos::Never) {
                    for &ct in &close_tokens {
                        if let Ok(fp) = model.token_to_piece(ct, &mut decoder, true, None) {
                            output.push_str(&fp);
                        }
                        sampler.accept(ct);
                        batch.clear();
                        batch
                            .add(ct, (tokens.len() + n_generated) as i32, &[0], true)
                            .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;
                        ctx.decode(&mut batch)
                            .map_err(|e| Error::Inference(format!("Decode failed: {e}")))?;
                        n_generated += 1;
                    }
                }
                in_think = false;
            }
        }

        ctx.clear_kv_cache();
        let total_tokens = tokens.len() + n_generated;
        Ok((output, total_tokens))
    }

    fn generate_stream_sync(
        model: &LlamaModel,
        ctx: &mut llama_cpp_2::context::LlamaContext<'_>,
        request: &CompletionRequest,
        tx: &tokio::sync::mpsc::Sender<Result<String>>,
        quirks: &ModelQuirks,
    ) -> Result<()> {
        // Pre-clear for the same reason as generate_sync: a prior failed decode
        // leaves the cache dirty, causing M-RoPE position errors on the next call.
        ctx.clear_kv_cache();

        let full_prompt = format_prompt(model, request, quirks)?;

        let tokens = model
            .str_to_token(&full_prompt, AddBos::Always)
            .map_err(|e| Error::Inference(format!("Tokenization failed: {e}")))?;

        let max_tokens = request.max_tokens.unwrap_or(1024);

        let mut batch = LlamaBatch::new(tokens.len().max(512), 1);
        let last_idx = tokens.len() - 1;
        for (i, &token) in tokens.iter().enumerate() {
            batch
                .add(token, i as i32, &[0], i == last_idx)
                .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;
        }

        ctx.decode(&mut batch)
            .map_err(|e| Error::Inference(format!("Prompt decode failed: {e}")))?;

        let mut sampler = build_sampler(model, request, quirks);
        let mut n_generated = 0usize;
        let mut decoder = encoding_rs::UTF_8.new_decoder();

        let mut tail = String::with_capacity(32);
        let mut in_think = false;
        let mut think_tokens = 0usize;
        let mut think_budget_fired = false;

        while n_generated < max_tokens {
            let token = sampler.sample(ctx, -1);
            sampler.accept(token);

            if model.is_eog_token(token) {
                break;
            }

            if let Ok(piece) = model.token_to_piece(token, &mut decoder, true, None) {
                tail.push_str(&piece);
                if tail.len() > 32 {
                    // Walk back from len-32 to the nearest char boundary so we
                    // never slice through a multi-byte UTF-8 sequence (Qwen3).
                    let mut drain_to = tail.len() - 32;
                    while drain_to > 0 && !tail.is_char_boundary(drain_to) {
                        drain_to -= 1;
                    }
                    tail.drain(..drain_to);
                }
                if !in_think && tail.contains("<think>") {
                    in_think = true;
                } else if in_think && tail.contains("</think>") {
                    in_think = false;
                }
                if in_think {
                    think_tokens += 1;
                }

                if tx.blocking_send(Ok(piece)).is_err() {
                    break;
                }
            }

            n_generated += 1;

            batch.clear();
            batch
                .add(token, (tokens.len() + n_generated - 1) as i32, &[0], true)
                .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;

            ctx.decode(&mut batch)
                .map_err(|e| Error::Inference(format!("Decode failed: {e}")))?;

            if in_think && think_tokens >= request.think_budget.unwrap_or(THINK_BUDGET) && !think_budget_fired {
                think_budget_fired = true;
                let force = "\n</think>\n\n";
                if let Ok(close_tokens) = model.str_to_token(force, AddBos::Never) {
                    for &ct in &close_tokens {
                        let fp = model
                            .token_to_piece(ct, &mut decoder, true, None)
                            .unwrap_or_default();
                        sampler.accept(ct);
                        batch.clear();
                        batch
                            .add(ct, (tokens.len() + n_generated) as i32, &[0], true)
                            .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;
                        ctx.decode(&mut batch)
                            .map_err(|e| Error::Inference(format!("Decode failed: {e}")))?;
                        n_generated += 1;
                        if tx.blocking_send(Ok(fp)).is_err() {
                            break;
                        }
                    }
                }
                in_think = false;
            }
        }

        ctx.clear_kv_cache();
        Ok(())
    }
}

// ─── EmbedSlot ────────────────────────────────────────────────

/// Dedicated embedding slot. Loads an embedding model (e.g.
/// `nomic-embed-text-v1.5.Q4_K_M.gguf`) with `embeddings = true` and
/// mean pooling, so a single decode pass yields one normalized
/// sentence vector.
///
/// The slot stores its `LlamaContext` with an extended `'static`
/// lifetime via the same `Arc<LlamaModel>` trick the chat slot uses.
/// We clear the KV cache between calls so each `embed()` invocation
/// is an independent sequence.
struct EmbedSlot {
    model: Arc<LlamaModel>,
    /// Pool of contexts for parallel batch embedding. contexts[0] is
    /// the primary context used by single `embed()` calls. All contexts
    /// share the same model weights (via Arc) — only the KV cache is
    /// per-context (~50MB each for a 2048-token embedding model).
    contexts: Vec<Mutex<EmbedSlotContext>>,
    /// Embedding dimensionality reported by the model. Cached so the
    /// hot path doesn't have to call `model.n_embd()` on every embed.
    n_embd: usize,
    /// Maximum tokens we can pack into the batch / context. Used to
    /// truncate long inputs gracefully instead of crashing llama.cpp.
    max_tokens: usize,
    /// Family-specific embedding configuration: pooling strategy,
    /// instruction prefixes, EOS appending. None = nomic/mxbai-style
    /// defaults (mean pooling, no instructions, no EOS).
    embed_quirks: Option<EmbedQuirks>,
}

struct EmbedSlotContext {
    ctx: llama_cpp_2::context::LlamaContext<'static>,
    _model: Arc<LlamaModel>,
}

unsafe impl Send for EmbedSlotContext {}
unsafe impl Sync for EmbedSlotContext {}

impl EmbedSlot {
    fn load(
        backend: &Arc<LlamaBackend>,
        model_path: &Path,
        n_gpu_layers: u32,
        embed_quirks: Option<EmbedQuirks>,
    ) -> Result<Self> {
        let model_params = LlamaModelParams::default().with_n_gpu_layers(n_gpu_layers);

        let model = LlamaModel::load_from_file(backend, model_path, &model_params)
            .map_err(|e| Error::Inference(format!("Failed to load embed model: {e}")))?;

        let n_embd = model.n_embd() as usize;
        let max_tokens = 2048;
        let model = Arc::new(model);

        let pooling_type = match embed_quirks.as_ref().map(|q| &q.pooling) {
            Some(PoolingStrategy::Last) => LlamaPoolingType::Last,
            Some(PoolingStrategy::Cls)  => LlamaPoolingType::Cls,
            _                           => LlamaPoolingType::Mean,
        };

        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(max_tokens as u32))
            .with_n_batch(max_tokens as u32)
            .with_n_ubatch(max_tokens as u32)
            .with_embeddings(true)
            .with_pooling_type(pooling_type)
            // Keep KV cache on CPU. With Metal enabled, the default (true)
            // can cause GGML_ASSERT(buf_src) in ggml_metal_buffer_set_tensor
            // when the KV tensor has no registered CPU backing buffer.
            .with_offload_kqv(false);

        let ctx = unsafe {
            let model_ref: &'static LlamaModel =
                &*(Arc::as_ptr(&model) as *const LlamaModel);
            model_ref
                .new_context(backend, ctx_params)
                .map_err(|e| Error::Inference(format!("Failed to create embed context: {e}")))?
        };

        let pooling_name = match embed_quirks.as_ref().map(|q| &q.pooling) {
            Some(PoolingStrategy::Last) => "last-token",
            Some(PoolingStrategy::Cls)  => "cls",
            _                           => "mean",
        };
        tracing::info!(
            dims = n_embd,
            layers = model.n_layer(),
            size_mb = model.size() / (1024 * 1024),
            pooling = pooling_name,
            n_ctx = max_tokens,
            "EmbedSlot::load — slot ready"
        );

        Ok(Self {
            model: model.clone(),
            contexts: vec![Mutex::new(EmbedSlotContext {
                ctx,
                _model: model,
            })],
            n_embd,
            max_tokens,
            embed_quirks,
        })
    }

    /// Embed a document string, applying the document-side instruction prefix
    /// and EOS token from the embed quirks (when configured).
    fn embed_sync(
        model: &LlamaModel,
        ctx: &mut llama_cpp_2::context::LlamaContext<'_>,
        text: &str,
        n_embd: usize,
        max_tokens: usize,
        embed_quirks: Option<&EmbedQuirks>,
    ) -> Result<Vec<f32>> {
        let prepared = if let Some(eq) = embed_quirks {
            let prefixed = format!("{}{text}", eq.document_instruction);
            if eq.append_eos_token {
                format!("{prefixed}<|endoftext|>")
            } else {
                prefixed
            }
        } else {
            text.to_string()
        };
        Self::run_embed_sync(model, ctx, &prepared, n_embd, max_tokens)
    }

    /// Embed a query string, applying the query-side instruction prefix
    /// and EOS token from the embed quirks (when configured).
    fn embed_query_sync(
        model: &LlamaModel,
        ctx: &mut llama_cpp_2::context::LlamaContext<'_>,
        query: &str,
        n_embd: usize,
        max_tokens: usize,
        embed_quirks: Option<&EmbedQuirks>,
    ) -> Result<Vec<f32>> {
        let prepared = if let Some(eq) = embed_quirks {
            let prefixed = format!("{}{query}", eq.query_instruction);
            if eq.append_eos_token {
                format!("{prefixed}<|endoftext|>")
            } else {
                prefixed
            }
        } else {
            query.to_string()
        };
        Self::run_embed_sync(model, ctx, &prepared, n_embd, max_tokens)
    }

    /// Core embedding routine: tokenize, batch-decode, pool, and L2-normalize.
    fn run_embed_sync(
        model: &LlamaModel,
        ctx: &mut llama_cpp_2::context::LlamaContext<'_>,
        input: &str,
        n_embd: usize,
        max_tokens: usize,
    ) -> Result<Vec<f32>> {
        // Each call is its own sequence — wipe the KV cache so previous
        // embeddings don't bleed into this one.
        ctx.clear_kv_cache();

        let mut tokens = model
            .str_to_token(input, AddBos::Always)
            .map_err(|e| Error::Inference(format!("Embed tokenization failed: {e}")))?;

        // Truncate inputs that exceed the context window. Embedding
        // models lose nothing critical by dropping trailing tokens —
        // the alternative is a llama.cpp panic on overflow.
        if tokens.len() > max_tokens {
            tokens.truncate(max_tokens);
        }

        if tokens.is_empty() {
            // The empty-string case. Return a zero vector rather than
            // erroring out — corpus-engine occasionally calls embed
            // with whitespace-only chunks during testing.
            return Ok(vec![0.0; n_embd]);
        }

        let mut batch = LlamaBatch::new(max_tokens, 1);
        for (i, &token) in tokens.iter().enumerate() {
            batch
                .add(token, i as i32, &[0], true)
                .map_err(|e| Error::Inference(format!("Embed batch add failed: {e}")))?;
        }

        ctx.decode(&mut batch)
            .map_err(|e| Error::Inference(format!("Embed decode failed: {e}")))?;

        // Sequence 0 has the pooled embedding (mean, last, or cls depending on
        // the context params set at load time).
        let raw = ctx
            .embeddings_seq_ith(0)
            .map_err(|e| Error::Inference(format!("Failed to read embeddings: {e}")))?;

        // L2-normalize so the resulting vectors are directly comparable
        // by cosine similarity (which is what corpus-engine's vector search assumes).
        // We always normalise in-process — NormalizationStrategy::Server only applies
        // in remote llama-server mode.
        let mut out = raw.to_vec();
        let norm: f32 = out.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in out.iter_mut() {
                *v /= norm;
            }
        }
        Ok(out)
    }

    /// Batch-embed multiple texts in a single decode pass.
    ///
    /// Each text gets its own sequence ID. All tokens are packed into one
    /// `LlamaBatch` and decoded together — llama.cpp processes them in
    /// parallel on the GPU. This amortises the decode overhead and can
    /// yield 5-10x throughput over sequential single-text embedding.
    ///
    /// If the combined token count exceeds `max_tokens`, the batch is
    /// split into sub-batches that each fit within the context window.
    /// Batch-embed: hold the context lock once and embed all texts
    /// sequentially. Avoids per-call mutex + spawn_blocking overhead.
    fn run_embed_batch_sync(
        slot: &EmbedSlot,
        inputs: &[String],
    ) -> Result<Vec<Vec<f32>>> {
        if inputs.is_empty() {
            return Ok(vec![]);
        }

        let batch_start = std::time::Instant::now();
        let mut ctx_lock = slot.contexts[0].blocking_lock();
        let mut results = Vec::with_capacity(inputs.len());

        for text in inputs {
            let emb = Self::embed_sync(
                &slot.model,
                &mut ctx_lock.ctx,
                text,
                slot.n_embd,
                slot.max_tokens,
                slot.embed_quirks.as_ref(),
            )?;
            results.push(emb);
        }

        let total_ms = batch_start.elapsed().as_millis();
        let seqs_per_sec = if total_ms > 0 {
            (inputs.len() as f64 / total_ms as f64) * 1000.0
        } else {
            0.0
        };
        tracing::info!(
            sequences = inputs.len(),
            total_ms,
            seqs_per_sec = format!("{seqs_per_sec:.1}"),
            "Batch embed complete"
        );

        Ok(results)
    }
}

// ─── EmbeddedLlamaCpp (triple-slot) ────────────────────────────

/// Configuration for loading the inference provider.
pub struct InferenceConfig {
    pub fast_model: PathBuf,
    pub primary_model: Option<PathBuf>,
    pub embed_model: Option<PathBuf>,
    pub context_size: u32,
    pub gpu_layers: Option<u32>,
    /// Model family for the fast slot. Drives thinking injection and
    /// sampling defaults. Defaults to `Unknown` (conservative no-thinking).
    pub fast_family: ModelFamily,
    /// Model family for the primary (thoughtful) slot.
    pub primary_family: ModelFamily,
    /// Model family for the embed slot. Must be `Qwen3Embedding` for
    /// last-token pooling and query instruction support.
    pub embed_family: ModelFamily,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            fast_model: PathBuf::from("models/fast.gguf"),
            primary_model: None,
            embed_model: None,
            context_size: 2048,
            gpu_layers: None,
            fast_family: ModelFamily::Unknown,
            primary_family: ModelFamily::Unknown,
            embed_family: ModelFamily::Unknown,
        }
    }
}

/// Triple-slot inference provider wrapping llama.cpp via FFI.
///
/// - **Fast slot**: always loaded, small model for classification and simple queries.
/// - **Primary slot**: loaded on-demand for deep reasoning, unloaded after idle timeout.
/// - **Embed slot**: optional dedicated embedding model for RAG / corpus indexing.
///   When absent, `embed()` returns a clear "no embed model configured" error so
///   callers can guide the user to configure one rather than silently producing
///   garbage vectors.
pub struct EmbeddedLlamaCpp {
    #[allow(dead_code)]
    backend: Arc<LlamaBackend>,
    fast: Arc<ModelSlot>,
    primary: Arc<Mutex<Option<ModelSlot>>>,
    primary_path: Option<PathBuf>,
    primary_ctx_size: u32,
    gpu_layers: u32,
    primary_backend: Arc<LlamaBackend>,
    last_primary_use: Arc<Mutex<Option<Instant>>>,
    embed_slot: Option<Arc<EmbedSlot>>,
    hardware: HardwareProfile,
    /// Quirks for the fast slot — controls thinking injection and sampling defaults.
    fast_quirks: ModelQuirks,
    /// Quirks for the primary (thoughtful) slot.
    primary_quirks: ModelQuirks,
}

impl EmbeddedLlamaCpp {
    /// Load with a single model (used for both fast and primary slots).
    /// This is the simple path for development and small deployments.
    pub fn load(model_path: &Path) -> Result<Self> {
        Self::load_dual(
            model_path,
            None, // No separate primary model.
            2048,
            None,
        )
    }

    /// Load with separate fast and primary models. No embed slot.
    pub fn load_dual(
        fast_model_path: &Path,
        primary_model_path: Option<&Path>,
        context_size: u32,
        gpu_layers: Option<u32>,
    ) -> Result<Self> {
        Self::load_full(
            fast_model_path,
            primary_model_path,
            None,
            context_size,
            gpu_layers,
        )
    }

    /// Load with separate fast, primary, and embed models. The embed
    /// slot is optional — when absent, `embed()` returns a clear
    /// "no embedding model configured" error.
    pub fn load_full(
        fast_model_path: &Path,
        primary_model_path: Option<&Path>,
        embed_model_path: Option<&Path>,
        context_size: u32,
        gpu_layers: Option<u32>,
    ) -> Result<Self> {
        Self::load_full_with_families(
            fast_model_path,
            primary_model_path,
            embed_model_path,
            context_size,
            gpu_layers,
            ModelFamily::Unknown,
            ModelFamily::Unknown,
            ModelFamily::Unknown,
        )
    }

    /// Load with separate fast, primary, and embed models, specifying the model
    /// family for each slot. The family drives thinking injection, sampling
    /// defaults, and embedding pooling/instruction behaviour.
    pub fn load_full_with_families(
        fast_model_path: &Path,
        primary_model_path: Option<&Path>,
        embed_model_path: Option<&Path>,
        context_size: u32,
        gpu_layers: Option<u32>,
        fast_family: ModelFamily,
        primary_family: ModelFamily,
        embed_family: ModelFamily,
    ) -> Result<Self> {
        let hardware = HardwareProfile::detect();
        let n_gpu_layers = gpu_layers.unwrap_or(hardware.recommended_gpu_layers);

        let fast_quirks = fast_family.default_quirks();
        let primary_quirks = primary_family.default_quirks();
        let embed_quirks = embed_family.default_quirks().embed;

        let mut backend = LlamaBackend::init()
            .map_err(|e| Error::Inference(format!("Failed to init llama backend: {e}")))?;
        backend.void_logs(); // suppress llama.cpp/ggml C-level stderr noise
        let backend = Arc::new(backend);

        tracing::info!(slot = "fast", family = ?fast_family, "loading slot");
        let fast = Arc::new(ModelSlot::load(
            &backend,
            fast_model_path,
            context_size,
            n_gpu_layers,
        )?);
        tracing::info!(slot = "fast", family = ?fast_family, "slot loaded");

        // Embedding models are usually small (~100-500 MB) and we want
        // them resident any time corpus ingestion or search runs, so we
        // load eagerly rather than lazily. Failure to load is logged
        // but not fatal — the rest of the runtime keeps working and
        // `embed()` will return a clear error.
        let embed_slot = match embed_model_path {
            Some(path) => {
                tracing::info!(
                    slot = "embed",
                    path = %path.display(),
                    family = ?embed_family,
                    "loading slot"
                );
                // Embedding contexts always run CPU-only (n_gpu_layers = 0).
                // The Metal scheduler mis-handles embedding tensor graphs in
                // several llama.cpp versions (GGML_ASSERT buf_src failure),
                // and embedding workloads are not compute-bound enough to
                // benefit from GPU offloading.
                match EmbedSlot::load(&backend, path, 0, embed_quirks) {
                    Ok(slot) => {
                        tracing::info!(slot = "embed", "slot loaded (CPU-only)");
                        Some(Arc::new(slot))
                    }
                    Err(e) => {
                        tracing::warn!(slot = "embed", error = %e, "slot load failed — embedding unavailable");
                        None
                    }
                }
            }
            None => None,
        };

        Ok(Self {
            backend: Arc::clone(&backend),
            fast,
            primary: Arc::new(Mutex::new(None)),
            primary_path: primary_model_path.map(|p| p.to_path_buf()),
            primary_ctx_size: context_size,
            gpu_layers: n_gpu_layers,
            primary_backend: backend,
            last_primary_use: Arc::new(Mutex::new(None)),
            embed_slot,
            hardware,
            fast_quirks,
            primary_quirks,
        })
    }

    /// Returns true if an embedding model is loaded and ready.
    pub fn has_embed_slot(&self) -> bool {
        self.embed_slot.is_some()
    }

    /// Start a background task that unloads the primary model after idle timeout.
    pub fn start_idle_monitor(self: &Arc<Self>, timeout_secs: u64) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

                let should_unload = {
                    let last_use = this.last_primary_use.lock().await;
                    match *last_use {
                        Some(t) => t.elapsed().as_secs() >= timeout_secs,
                        None => false,
                    }
                };

                if should_unload {
                    let mut primary = this.primary.lock().await;
                    if primary.is_some() {
                        tracing::info!(
                            slot = "primary",
                            idle_secs = timeout_secs,
                            "unloading slot (idle timeout)"
                        );
                        *primary = None;
                        *this.last_primary_use.lock().await = None;
                    }
                }
            }
        });
    }

    /// Select the appropriate slot for a request.
    fn select_slot_for_speed(&self, speed: Speed) -> bool {
        // Returns true if we should use the primary slot.
        match speed {
            Speed::Fast => false,
            Speed::Medium | Speed::Slow => self.primary_path.is_some(),
        }
    }
}

#[async_trait]
impl InferenceProvider for EmbeddedLlamaCpp {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        let use_primary = self.select_slot_for_speed(request.preferred_speed);
        let slot_name = if use_primary { "primary" } else { "fast" };
        let prompt_chars = request.prompt.len();
        let system_chars = request.system_message.as_ref().map(|s| s.len()).unwrap_or(0);

        tracing::debug!(
            slot = slot_name,
            speed = ?request.preferred_speed,
            prompt_chars,
            system_chars,
            max_tokens = ?request.max_tokens,
            think_budget = ?request.think_budget,
            temperature = ?request.temperature,
            "inference.complete: call"
        );

        if use_primary {
            // Load primary if needed.
            let primary_path = self.primary_path.clone().unwrap();
            let primary_lock = Arc::clone(&self.primary);
            let backend = Arc::clone(&self.primary_backend);
            let ctx_size = self.primary_ctx_size;
            let gpu_layers = self.gpu_layers;
            let last_use = Arc::clone(&self.last_primary_use);
            let request = request.clone();
            let quirks = self.primary_quirks.clone();

            let result: Result<CompletionResponse> = tokio::task::spawn_blocking(move || {
                let start = Instant::now();

                // Ensure primary is loaded.
                let mut primary = primary_lock.blocking_lock();
                if primary.is_none() {
                    tracing::info!(slot = "primary", "loading slot (first use)");
                    let slot = ModelSlot::load(&backend, &primary_path, ctx_size, gpu_layers)?;
                    *primary = Some(slot);
                }

                let slot = primary.as_ref().unwrap();
                let mut ctx_lock = slot.context.blocking_lock();

                // Catch panics from llama.cpp (e.g., context overflow assertions).
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    ModelSlot::generate_sync(&slot.model, &mut ctx_lock.ctx, &request, &quirks)
                }));

                let (text, tokens_used) = match result {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => {
                        tracing::warn!(slot = "primary", error = %e, "inference error");
                        return Err(e);
                    }
                    Err(_) => {
                        tracing::error!(slot = "primary", "inference panicked — likely context overflow");
                        return Err(Error::Inference(
                            "Model inference failed: prompt may exceed the model's context window. \
                             Try a shorter message or reduce conversation history.".to_string(),
                        ));
                    }
                };

                let latency_ms = start.elapsed().as_millis() as u64;

                *last_use.blocking_lock() = Some(Instant::now());

                Ok(CompletionResponse {
                    text,
                    tokens_used,
                    model_id: slot.model_id.clone(),
                    latency_ms,
                    oicp_meta: None,
                })
            })
            .await
            .map_err(|e| Error::Inference(format!("Inference task failed: {e}")))?;

            if let Ok(ref resp) = result {
                tracing::info!(
                    slot = "primary",
                    model = %resp.model_id,
                    latency_ms = resp.latency_ms,
                    tokens_used = resp.tokens_used,
                    response_chars = resp.text.len(),
                    "inference.complete: done"
                );
            }
            result
        } else {
            // Use fast slot.
            let slot = Arc::clone(&self.fast);
            let request = request.clone();
            let quirks = self.fast_quirks.clone();

            let result: Result<CompletionResponse> = tokio::task::spawn_blocking(move || {
                let start = Instant::now();
                let mut ctx_lock = slot.context.blocking_lock();

                // Catch panics from llama.cpp (e.g., context overflow assertions).
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    ModelSlot::generate_sync(&slot.model, &mut ctx_lock.ctx, &request, &quirks)
                }));

                let (text, tokens_used) = match result {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => {
                        tracing::warn!(slot = "fast", error = %e, "inference error");
                        return Err(e);
                    }
                    Err(_) => {
                        tracing::error!(slot = "fast", "inference panicked — likely context overflow");
                        return Err(Error::Inference(
                            "Model inference failed: prompt may exceed the model's context window. \
                             Try a shorter message or reduce conversation history.".to_string(),
                        ));
                    }
                };

                let latency_ms = start.elapsed().as_millis() as u64;

                Ok(CompletionResponse {
                    text,
                    tokens_used,
                    model_id: slot.model_id.clone(),
                    latency_ms,
                    oicp_meta: None,
                })
            })
            .await
            .map_err(|e| Error::Inference(format!("Inference task failed: {e}")))?;

            if let Ok(ref resp) = result {
                tracing::info!(
                    slot = "fast",
                    model = %resp.model_id,
                    latency_ms = resp.latency_ms,
                    tokens_used = resp.tokens_used,
                    response_chars = resp.text.len(),
                    "inference.complete: done"
                );
            }
            result
        }
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        let use_primary = self.select_slot_for_speed(request.preferred_speed);
        let slot_name = if use_primary { "primary" } else { "fast" };

        tracing::debug!(
            slot = slot_name,
            speed = ?request.preferred_speed,
            prompt_chars = request.prompt.len(),
            system_chars = request.system_message.as_ref().map(|s| s.len()).unwrap_or(0),
            max_tokens = ?request.max_tokens,
            "inference.complete_stream: call"
        );

        let request = request.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String>>(32);

        if use_primary {
            let primary_path = self.primary_path.clone().unwrap();
            let primary_lock = Arc::clone(&self.primary);
            let backend = Arc::clone(&self.primary_backend);
            let ctx_size = self.primary_ctx_size;
            let gpu_layers = self.gpu_layers;
            let last_use = Arc::clone(&self.last_primary_use);
            let quirks = self.primary_quirks.clone();

            tokio::task::spawn_blocking(move || {
                let start = Instant::now();
                let mut primary = primary_lock.blocking_lock();
                if primary.is_none() {
                    tracing::info!(slot = "primary", "loading slot (first use, streaming)");
                    match ModelSlot::load(&backend, &primary_path, ctx_size, gpu_layers) {
                        Ok(slot) => *primary = Some(slot),
                        Err(e) => {
                            tracing::error!(slot = "primary", error = %e, "slot load failed");
                            let _ = tx.blocking_send(Err(e));
                            return;
                        }
                    }
                }
                let slot = primary.as_ref().unwrap();
                let mut ctx_lock = slot.context.blocking_lock();
                *last_use.blocking_lock() = Some(Instant::now());
                if let Err(e) =
                    ModelSlot::generate_stream_sync(&slot.model, &mut ctx_lock.ctx, &request, &tx, &quirks)
                {
                    tracing::warn!(slot = "primary", error = %e, "stream error");
                    let _ = tx.blocking_send(Err(e));
                } else {
                    tracing::info!(
                        slot = "primary",
                        latency_ms = start.elapsed().as_millis() as u64,
                        "inference.complete_stream: done"
                    );
                }
            });
        } else {
            let slot = Arc::clone(&self.fast);
            let quirks = self.fast_quirks.clone();
            tokio::task::spawn_blocking(move || {
                let start = Instant::now();
                let mut ctx_lock = slot.context.blocking_lock();
                if let Err(e) =
                    ModelSlot::generate_stream_sync(&slot.model, &mut ctx_lock.ctx, &request, &tx, &quirks)
                {
                    tracing::warn!(slot = "fast", error = %e, "stream error");
                    let _ = tx.blocking_send(Err(e));
                } else {
                    tracing::info!(
                        slot = "fast",
                        latency_ms = start.elapsed().as_millis() as u64,
                        "inference.complete_stream: done"
                    );
                }
            });
        }

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let slot = self.embed_slot.as_ref().ok_or_else(|| {
            Error::Inference(
                "No embedding model is configured. Open Settings → Embedding model and \
                 select a GGUF embedding model (e.g. nomic-embed-text-v1.5.Q4_K_M.gguf), \
                 then retry.".to_string(),
            )
        })?;
        let slot = Arc::clone(slot);
        let text_len = text.len();
        let text = text.to_string();

        tracing::debug!(text_chars = text_len, "inference.embed: call");

        tokio::task::spawn_blocking(move || {
            let start = Instant::now();
            let mut ctx_lock = slot.contexts[0].blocking_lock();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                EmbedSlot::embed_sync(
                    &slot.model,
                    &mut ctx_lock.ctx,
                    &text,
                    slot.n_embd,
                    slot.max_tokens,
                    slot.embed_quirks.as_ref(),
                )
            }));
            match result {
                Ok(Ok(v)) => {
                    tracing::debug!(
                        dims = v.len(),
                        latency_ms = start.elapsed().as_millis() as u64,
                        "inference.embed: done"
                    );
                    Ok(v)
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "embed error");
                    Err(e)
                }
                Err(_) => {
                    tracing::error!("embed panicked — input may be malformed");
                    Err(Error::Inference(
                        "Embedding inference panicked — input may be malformed or \
                         the embed model is incompatible.".to_string(),
                    ))
                }
            }
        })
        .await
        .map_err(|e| Error::Inference(format!("Embed task failed: {e}")))?
    }

    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        let slot = self.embed_slot.as_ref().ok_or_else(|| {
            Error::Inference(
                "No embedding model is configured. Open Settings → Embedding model and \
                 select a GGUF embedding model (e.g. nomic-embed-text-v1.5.Q4_K_M.gguf), \
                 then retry.".to_string(),
            )
        })?;
        let slot = Arc::clone(slot);
        let query = query.to_string();

        tokio::task::spawn_blocking(move || {
            let mut ctx_lock = slot.contexts[0].blocking_lock();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                EmbedSlot::embed_query_sync(
                    &slot.model,
                    &mut ctx_lock.ctx,
                    &query,
                    slot.n_embd,
                    slot.max_tokens,
                    slot.embed_quirks.as_ref(),
                )
            }));
            match result {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(Error::Inference(
                    "Embedding inference panicked — input may be malformed or \
                     the embed model is incompatible.".to_string(),
                )),
            }
        })
        .await
        .map_err(|e| Error::Inference(format!("Embed task failed: {e}")))?
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let slot = self.embed_slot.as_ref().ok_or_else(|| {
            Error::Inference(
                "No embedding model is configured. Open Settings → Embedding model and \
                 select a GGUF embedding model (e.g. nomic-embed-text-v1.5.Q4_K_M.gguf), \
                 then retry.".to_string(),
            )
        })?;
        let slot = Arc::clone(slot);
        let texts = texts.to_vec();

        tokio::task::spawn_blocking(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                EmbedSlot::run_embed_batch_sync(&slot, &texts)
            }));
            match result {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(Error::Inference(
                    "Batch embedding inference panicked — one or more inputs may be \
                     malformed or the embed model is incompatible.".to_string(),
                )),
            }
        })
        .await
        .map_err(|e| Error::Inference(format!("Embed batch task failed: {e}")))?
    }

    fn model_id_for(&self, speed: Speed) -> String {
        let use_primary = matches!(speed, Speed::Slow | Speed::Medium);
        if use_primary {
            // Primary slot is lazy-loaded; derive from path without locking.
            self.primary_path
                .as_ref()
                .and_then(|p| p.file_stem())
                .and_then(|s| s.to_str())
                .unwrap_or(&self.fast.model_id)
                .to_string()
        } else {
            self.fast.model_id.clone()
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 2048,
            supports_structured_output: false,
            relative_speed: if self.hardware.gpu_available {
                Speed::Medium
            } else {
                Speed::Slow
            },
            relative_reasoning: Depth::Deep,
        }
    }
}

// ─── Shared helpers ────────────────────────────────────────────

fn format_prompt(model: &LlamaModel, request: &CompletionRequest, quirks: &ModelQuirks) -> Result<String> {
    // Inject thinking-mode token into the system message based on family quirks.
    // `think_budget == Some(0)` signals the caller wants thinking suppressed.
    // For SystemPromptToken families (Qwen3, Qwen3.5, SmolLM3): append /think or /no_think.
    // For AlwaysOn (Phi-4-reasoning): thinking cannot be disabled — don't inject.
    // For None (Gemma3, Llama3, Phi-4): no thinking tokens exist — don't inject.
    let base_system = request.system_message.as_deref().unwrap_or("");
    let system_with_thinking = match &quirks.thinking {
        ThinkingControl::SystemPromptToken { enable, disable } => {
            let token = if request.think_budget == Some(0) { disable } else { enable };
            if base_system.is_empty() {
                token.clone()
            } else {
                format!("{base_system}\n{token}")
            }
        }
        // AlwaysOn: thinking is structural — injecting /no_think degrades output.
        // None: family has no thinking tokens — nothing to inject.
        ThinkingControl::AlwaysOn | ThinkingControl::None => base_system.to_string(),
    };

    if let Ok(template) = model.chat_template(None) {
        let mut messages = Vec::new();
        if !system_with_thinking.is_empty() {
            messages.push(
                LlamaChatMessage::new("system".to_string(), system_with_thinking.clone())
                    .map_err(|e| Error::Inference(format!("Chat message error: {e}")))?,
            );
        }
        messages.push(
            LlamaChatMessage::new("user".to_string(), request.prompt.clone())
                .map_err(|e| Error::Inference(format!("Chat message error: {e}")))?,
        );
        if let Ok(formatted) = model.apply_chat_template(&template, &messages, true) {
            return Ok(formatted);
        }
    }

    Ok(if system_with_thinking.is_empty() {
        request.prompt.clone()
    } else {
        format!("{system_with_thinking}\n\n{}", request.prompt)
    })
}

/// Token budget for `<think>` blocks. After this many tokens inside a thinking
/// block the generation loop injects `</think>` into the KV cache and stream,
/// forcing the model to transition to its answer rather than spiral endlessly.
const THINK_BUDGET: usize = 512;

/// Build the sampler chain.
///
/// Technique rationale:
/// - **DRY** ("Don't Repeat Yourself"): penalises repeated *token sequences*
///   rather than individual tokens. This is the primary fix for the
///   "completely totally wholly fully …" cascade — those are all different
///   tokens, so standard `penalties` misses them, but DRY catches the
///   recurring suffix pattern they form.
/// - **min_p**: replaces top_p. When the model is confident (high-probability
///   next token) min_p becomes more selective, preventing runaway picks from
///   a collapsed distribution. When uncertain it relaxes, preserving diversity.
/// - **penalties**: kept as a backstop for single-token repetition that DRY's
///   `allowed_length = 2` intentionally ignores.
fn build_sampler(model: &LlamaModel, request: &CompletionRequest, quirks: &ModelQuirks) -> LlamaSampler {
    // Grammar-constrained decoding: if structured_output contains a JSON
    // schema, generate a GBNF grammar and use it to constrain the sampler.
    // DISABLED: the generated GBNF causes llama.cpp assertion failures
    // during sampling (empty parse stacks). The grammar generator needs
    // validation against llama.cpp's GBNF parser before we can enable this.
    // For now, log the grammar so we can debug it offline, and fall through
    // to the standard sampler.
    if let Some(ref schema) = request.structured_output {
        let grammar_str = json_schema_to_gbnf(schema);
        tracing::info!(
            grammar = %grammar_str,
            schema = %schema,
            "Grammar-constrained decoding requested (currently disabled — using standard sampler)"
        );
        // TODO: Re-enable once grammar is validated:
        // match LlamaSampler::grammar(model, &grammar_str, "root") { ... }
    }

    // Temperature: per-request override → family default.
    let temp = request.temperature.unwrap_or(quirks.default_temperature);
    // Top-k: per-request override → family default → hard fallback of 40.
    let top_k_val = request.top_k
        .or(quirks.default_top_k)
        .unwrap_or(40) as i32;
    // Sequence breakers tell DRY where one "thought unit" ends and another
    // begins — any of these tokens resets the repeated-suffix detector.
    let breakers: &[&[u8]] = &[b"\n", b".", b"?", b"!", b":", b"\"", b"*"];
    if temp < 0.01 {
        // Greedy decoding — DRY and penalties still applied to prevent
        // degenerate repetition loops even in greedy mode.
        LlamaSampler::chain_simple([
            LlamaSampler::dry(model, 0.8, 1.75, 2, -1, breakers.iter().copied()),
            LlamaSampler::penalties(128, 1.15, 0.1, 0.1),
            LlamaSampler::greedy(),
        ])
    } else {
        LlamaSampler::chain_simple([
            LlamaSampler::dry(model, 0.8, 1.75, 2, -1, breakers.iter().copied()),
            LlamaSampler::penalties(128, 1.15, 0.1, 0.1),
            LlamaSampler::top_k(top_k_val),
            LlamaSampler::min_p(0.05, 1),
            LlamaSampler::temp(temp),
            LlamaSampler::dist(rand_seed()),
        ])
    }
}

fn rand_seed() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos()
}

// ─── JSON Schema → GBNF Grammar ─────────────────────────────

/// Convert a JSON schema to a GBNF (GGML BNF) grammar string.
///
/// Handles the subset of JSON schema used by tool descriptors:
/// - `"type": "object"` with `"properties"` and `"required"`
/// - `"type": "string"` / `"integer"` / `"number"` / `"boolean"`
/// - Flat schemas only (no nested objects or arrays of objects)
///
/// The grammar constrains the token sampler so the model can only
/// produce valid JSON matching the schema. This eliminates malformed
/// JSON, missing required fields, and type errors.
pub fn json_schema_to_gbnf(schema: &serde_json::Value) -> String {
    let mut rules = Vec::new();

    // Primitive rules shared by all schemas.
    rules.push(r#"ws ::= [ \t\n]*"#.to_string());
    rules.push(r#"string ::= "\"" ([^"\\] | "\\" .)* "\""  "#.to_string());
    rules.push(r#"integer ::= "-"? [0-9]+"#.to_string());
    rules.push(r#"number ::= "-"? [0-9]+ ("." [0-9]+)?"#.to_string());
    rules.push(r#"boolean ::= "true" | "false""#.to_string());
    rules.push(r#"null ::= "null""#.to_string());

    match schema.get("type").and_then(|t| t.as_str()) {
        Some("object") => {
            let props = schema
                .get("properties")
                .and_then(|p| p.as_object())
                .cloned()
                .unwrap_or_default();
            let required: Vec<String> = schema
                .get("required")
                .and_then(|r| r.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();

            if props.is_empty() {
                // Any JSON object.
                rules.push(r#"root ::= "{" ws (string ws ":" ws value (ws "," ws string ws ":" ws value)*)? ws "}""#.to_string());
                rules.push(r#"value ::= string | number | integer | boolean | null"#.to_string());
            } else {
                // Build a rule that produces exactly the required fields in order,
                // then optionally the non-required fields. This is simpler than
                // allowing arbitrary order and covers our tool calling needs.
                let mut field_parts = Vec::new();

                // Required fields first, in schema order.
                for key in &required {
                    if let Some(prop_schema) = props.get(key) {
                        let type_rule = prop_type_rule(prop_schema);
                        field_parts.push(format!(
                            r#"ws "\"{}\"" ws ":" ws {}"#,
                            key, type_rule
                        ));
                    }
                }

                // Optional fields.
                for (key, prop_schema) in &props {
                    if required.contains(key) {
                        continue;
                    }
                    let type_rule = prop_type_rule(prop_schema);
                    field_parts.push(format!(
                        r#"(ws "," ws "\"{}\"" ws ":" ws {})?"#,
                        key, type_rule
                    ));
                }

                if field_parts.is_empty() {
                    rules.push(r#"root ::= "{" ws "}""#.to_string());
                } else {
                    let fields_joined = field_parts.join(r#" ws "," "#);
                    rules.push(format!(r#"root ::= "{{" {} ws "}}""#, fields_joined));
                }
            }
        }
        _ => {
            // Fallback: allow any JSON value.
            rules.push(r#"root ::= "{" ws (string ws ":" ws value (ws "," ws string ws ":" ws value)*)? ws "}""#.to_string());
            rules.push(r#"value ::= string | number | integer | boolean | null"#.to_string());
        }
    }

    rules.join("\n")
}

/// Get the GBNF rule name for a property's type.
fn prop_type_rule(schema: &serde_json::Value) -> &'static str {
    match schema.get("type").and_then(|t| t.as_str()) {
        Some("string") => "string",
        Some("integer") => "integer",
        Some("number") => "number",
        Some("boolean") => "boolean",
        _ => "string", // default to string for unknown types
    }
}
