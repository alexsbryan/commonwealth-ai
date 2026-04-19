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
use llama_cpp_2::token::LlamaToken;

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

        let n_threads = llama_threads_for_host();
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(context_size))
            // n_batch = context_size so the full context window is available
            // for prompt processing. llama.cpp automatically splits the batch
            // into n_ubatch=512 micro-batches for GPU/CPU kernel calls, so
            // memory pressure is unchanged — only the Rust-side assertion limit
            // is relaxed.
            .with_n_batch(context_size)
            .with_n_ubatch(512)
            // llama-cpp-2 defaults both thread counts to 4, which
            // caps matmul at four cores regardless of machine
            // size. Embedding prefill and chat prompt processing
            // are both dominated by `n_threads_batch`; `n_threads`
            // governs single-token decode. Set both to the
            // platform's parallelism so the CPU backend actually
            // uses all the cores (BLAS on macOS, plain ggml
            // elsewhere). The default-of-4 behaviour was the
            // reason a 600M embed model ran at 2 seq/s while only
            // 400% of the CPU was busy.
            .with_n_threads(n_threads as i32)
            .with_n_threads_batch(n_threads as i32);

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

        let n_batch = ctx.n_batch() as usize;
        let n_ctx = ctx.n_ctx() as usize;
        let max_tokens = clamp_max_tokens(
            request.max_tokens,
            tokens.len(),
            n_ctx,
        )?;

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

        let n_ctx = ctx.n_ctx() as usize;
        let max_tokens = clamp_max_tokens(
            request.max_tokens,
            tokens.len(),
            n_ctx,
        )?;

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
/// `Qwen3-Embedding-0.6B-Q4_K_M.gguf`) with `embeddings = true` and
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
    /// Per-input truncation limit. Any single text longer than this
    /// is clipped before decode. Named separately from the context
    /// size so bin-packing logic doesn't conflate "how big one seq
    /// can be" with "how big a packed sub-batch can be".
    max_input_tokens: usize,
    /// Total tokens the KV cache can hold across all active
    /// sequences in one packed decode. Governs how many chunks we
    /// can pack into a single sub-batch in `run_embed_batch_sync`.
    ctx_tokens: usize,
    /// Upper bound on distinct sequences per packed batch, matching
    /// the value passed to `LlamaContextParams::with_n_seq_max`.
    n_seq_max: usize,
    /// Family-specific embedding configuration: pooling strategy,
    /// instruction prefixes, EOS appending. None = Qwen3-Embedding and similar
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
        // Force Metal offload on platforms where the feature was
        // compiled in. Benchmarked on M2 Max with Qwen3-Embedding-
        // 0.6B: CPU peaks at ~2.8 seq/sec @ 8 threads; Metal hits
        // 33 seq/sec — a 12× speedup that directly unblocks
        // Wikipedia-scale ingestion. The old `GGML_ASSERT(buf_src)`
        // crash that forced CPU-only mode was fixed in llama-cpp-2
        // 0.1.141; if it comes back for a given quant or platform,
        // the fallback-to-CPU path below catches it.
        let requested_gpu_layers = if cfg!(target_os = "macos") && n_gpu_layers == 0 {
            999 // every layer on GPU
        } else {
            n_gpu_layers
        };
        let model_params = LlamaModelParams::default().with_n_gpu_layers(requested_gpu_layers);

        let model = LlamaModel::load_from_file(backend, model_path, &model_params)
            .map_err(|e| Error::Inference(format!("Failed to load embed model: {e}")))?;

        let n_embd = model.n_embd() as usize;
        // Per-input truncation: any single text longer than this
        // is clipped. Set to match the per-slot KV budget below
        // (ctx_tokens / n_seq_max) so a full-length chunk is
        // guaranteed to fit in one of the packed slots — llama.cpp
        // rejects a decode with `NoKvCacheSlot` if any single
        // sequence exceeds its partition of the unified cache.
        let max_input_tokens = 1024;
        // Aggregate KV cache: `n_ctx` bounds the sum of tokens
        // across all active sequences in a packed decode. 16384
        // gives us 1024 tokens × 16 parallel slots, which covers
        // the p99 chunk length for Wikipedia without forcing a
        // second sub-batch per chunk.
        let ctx_tokens: u32 = 16384;
        // Parallel slot count. The scheduler reserves
        // `ctx_tokens / n_seq_max` tokens per slot, so raising
        // this above 16 shrinks per-slot budget below what our
        // chunker emits and trips `NoKvCacheSlot` at decode time.
        // 16 gives a 16× throughput multiplier over the single-
        // sequence path while staying comfortably inside
        // llama.cpp's unified-cache semantics.
        let n_seq_max: u32 = 16;
        let model = Arc::new(model);

        let pooling_type = match embed_quirks.as_ref().map(|q| &q.pooling) {
            Some(PoolingStrategy::Last) => LlamaPoolingType::Last,
            Some(PoolingStrategy::Cls)  => LlamaPoolingType::Cls,
            _                           => LlamaPoolingType::Mean,
        };

        let n_threads = llama_threads_for_host();
        let wants_metal = cfg!(target_os = "macos") && requested_gpu_layers > 0;
        let build_params = |metal: bool| {
            LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(ctx_tokens))
                .with_n_batch(ctx_tokens)
                .with_n_ubatch(2048)
                .with_n_seq_max(n_seq_max)
                .with_n_threads(n_threads as i32)
                .with_n_threads_batch(n_threads as i32)
                .with_embeddings(true)
                .with_pooling_type(pooling_type)
                // Metal offload toggles. `with_offload_kqv(true)` puts
                // the KV cache in GPU memory; `with_op_offload(true)`
                // lets the GGML scheduler route compute ops to Metal.
                // When both are true (the default when Metal works),
                // the decode runs entirely on the GPU. The CPU-only
                // fallback disables both explicitly so the scheduler
                // keeps every tensor in main memory.
                .with_offload_kqv(metal)
                .with_op_offload(metal)
        };

        // Try Metal first if compiled in; fall back to CPU on any
        // context-creation error. The common failure mode on older
        // llama.cpp builds was `GGML_ASSERT(buf_src)` — ugly from
        // the crash's perspective, but if it happens before
        // `new_context` returns we just see `Err` here and retry.
        let (ctx, used_metal) = match if wants_metal {
            unsafe {
                let model_ref: &'static LlamaModel =
                    &*(Arc::as_ptr(&model) as *const LlamaModel);
                model_ref
                    .new_context(backend, build_params(true))
                    .map(|c| (c, true))
            }
        } else {
            Err(llama_cpp_2::LlamaContextLoadError::NullReturn)
        } {
            Ok(pair) => pair,
            Err(e) => {
                if wants_metal {
                    tracing::warn!(
                        error = ?e,
                        "Metal embed context failed, falling back to CPU"
                    );
                }
                let ctx = unsafe {
                    let model_ref: &'static LlamaModel =
                        &*(Arc::as_ptr(&model) as *const LlamaModel);
                    model_ref
                        .new_context(backend, build_params(false))
                        .map_err(|e| {
                            Error::Inference(format!(
                                "Failed to create embed context: {e}"
                            ))
                        })?
                };
                (ctx, false)
            }
        };

        let pooling_name = match embed_quirks.as_ref().map(|q| &q.pooling) {
            Some(PoolingStrategy::Last) => "last-token",
            Some(PoolingStrategy::Cls)  => "cls",
            _                           => "mean",
        };
        // Report the actual backend so logs show whether Metal
        // took (33 seq/sec on M2 Max, measured) or we fell back
        // to CPU (2.8 seq/sec peak). Key diagnostic — a silent
        // CPU fallback is the exact class of regression that
        // triggered the benchmark-driven investigation of this
        // path.
        let compute_backend = if used_metal {
            "gpu+metal"
        } else {
            embed_compute_backend_label()
        };
        tracing::info!(
            dims = n_embd,
            layers = model.n_layer(),
            size_mb = model.size() / (1024 * 1024),
            pooling = pooling_name,
            n_ctx = ctx_tokens,
            n_seq_max,
            max_input_tokens,
            n_threads,
            compute_backend,
            "EmbedSlot::load — slot ready"
        );

        Ok(Self {
            model: model.clone(),
            contexts: vec![Mutex::new(EmbedSlotContext {
                ctx,
                _model: model,
            })],
            n_embd,
            max_input_tokens,
            ctx_tokens: ctx_tokens as usize,
            n_seq_max: n_seq_max as usize,
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
        max_input_tokens: usize,
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
        Self::run_embed_sync(model, ctx, &prepared, n_embd, max_input_tokens)
    }

    /// Embed a query string, applying the query-side instruction prefix
    /// and EOS token from the embed quirks (when configured).
    fn embed_query_sync(
        model: &LlamaModel,
        ctx: &mut llama_cpp_2::context::LlamaContext<'_>,
        query: &str,
        n_embd: usize,
        max_input_tokens: usize,
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
        Self::run_embed_sync(model, ctx, &prepared, n_embd, max_input_tokens)
    }

    /// Single-sequence embedding routine. Kept alongside the
    /// multi-sequence [`run_embed_batch_sync`] for single-query
    /// paths (e.g. `embed_query`) where building a packed batch
    /// would be pure overhead.
    fn run_embed_sync(
        model: &LlamaModel,
        ctx: &mut llama_cpp_2::context::LlamaContext<'_>,
        input: &str,
        n_embd: usize,
        max_input_tokens: usize,
    ) -> Result<Vec<f32>> {
        ctx.clear_kv_cache();

        let mut tokens = model
            .str_to_token(input, AddBos::Always)
            .map_err(|e| Error::Inference(format!("Embed tokenization failed: {e}")))?;
        if tokens.len() > max_input_tokens {
            tokens.truncate(max_input_tokens);
        }
        if tokens.is_empty() {
            return Ok(vec![0.0; n_embd]);
        }

        let mut batch = LlamaBatch::new(max_input_tokens, 1);
        for (i, &token) in tokens.iter().enumerate() {
            batch
                .add(token, i as i32, &[0], true)
                .map_err(|e| Error::Inference(format!("Embed batch add failed: {e}")))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| Error::Inference(format!("Embed decode failed: {e}")))?;

        let raw = ctx
            .embeddings_seq_ith(0)
            .map_err(|e| Error::Inference(format!("Failed to read embeddings: {e}")))?;
        Ok(l2_normalize(raw.to_vec()))
    }

    /// Batch-embed multiple texts by packing distinct sequences
    /// into a single `LlamaBatch` and calling `ctx.decode` once per
    /// sub-batch.
    ///
    /// ## Why this exists
    ///
    /// The prior implementation was a sequential loop over
    /// `embed_sync`. Each iteration reset the KV cache and ran a
    /// full decode pass for one sequence, so a 256-input call
    /// performed 256 single-sequence prefills. On Apple-Silicon
    /// CPU with Accelerate (Metal is disabled for embedding
    /// contexts due to a llama.cpp `GGML_ASSERT(buf_src)` crash —
    /// see the `EmbedSlot::load` comment) the per-sequence decode
    /// is dominated by the attention + FFN matmuls. Accelerate's
    /// SGEMM amortises dispatch + cache misses across the batch
    /// dimension, so packing N sequences into one decode delivers
    /// close to N× the FLOPs with ~1× the wall time until we
    /// saturate the vector engine.
    ///
    /// ## Strategy
    ///
    /// 1. Tokenize every input up front (no llama.cpp state, so
    ///    we can do this outside the context lock).
    /// 2. Bin-pack tokenized inputs into sub-batches subject to:
    ///      (a) total tokens ≤ `slot.ctx_tokens`
    ///      (b) sequence count ≤ `slot.n_seq_max`
    /// 3. For each sub-batch: clear KV cache, add tokens with
    ///    distinct `seq_id`s, `decode()`, then read one pooled
    ///    embedding per sequence.
    /// 4. Splice results back in input order — bin-packing is
    ///    order-preserving so this is just concatenation.
    ///
    /// A pathological input longer than `slot.ctx_tokens` is
    /// truncated to `slot.max_input_tokens` up front, so single
    /// oversized chunks always fit in their own sub-batch.
    fn run_embed_batch_sync(
        slot: &EmbedSlot,
        inputs: &[String],
    ) -> Result<Vec<Vec<f32>>> {
        if inputs.is_empty() {
            return Ok(vec![]);
        }

        // Tokenize every input without holding the context lock.
        // Tokenization is pure (model vocab is immutable after
        // load); keeping it outside the lock shortens the
        // critical section and lets a future caller tokenize in
        // parallel if we ever move that behind a thread pool.
        let mut prepared: Vec<Vec<LlamaToken>> = Vec::with_capacity(inputs.len());
        for text in inputs {
            let prefixed = match slot.embed_quirks.as_ref() {
                Some(eq) => {
                    let mut s = format!("{}{}", eq.document_instruction, text);
                    if eq.append_eos_token {
                        s.push_str("<|endoftext|>");
                    }
                    s
                }
                None => text.to_string(),
            };
            let mut toks = slot
                .model
                .str_to_token(&prefixed, AddBos::Always)
                .map_err(|e| Error::Inference(format!("Embed tokenization failed: {e}")))?;
            if toks.len() > slot.max_input_tokens {
                toks.truncate(slot.max_input_tokens);
            }
            prepared.push(toks);
        }

        let batch_start = std::time::Instant::now();
        let mut ctx_lock = slot.contexts[0].blocking_lock();
        let mut results: Vec<Vec<f32>> = Vec::with_capacity(inputs.len());

        let mut sub_batches = 0usize;
        let mut cursor = 0usize;
        // Reuse one `LlamaBatch` allocation across sub-batches;
        // `clear()` resets n_tokens without freeing the backing
        // memory. Matches the context's n_batch capacity.
        let mut batch = LlamaBatch::new(slot.ctx_tokens, slot.n_seq_max as i32);

        // Pack to ~90% of the nominal ctx budget. llama.cpp uses
        // some headroom inside the unified KV cache for scheduler
        // bookkeeping; packing to exactly `ctx_tokens` has tripped
        // `NoKvCacheSlot` at decode time on real-world corpora.
        let pack_budget = slot.ctx_tokens.saturating_sub(slot.ctx_tokens / 10);

        while cursor < prepared.len() {
            // Greedily accumulate sequences into `batch` until the
            // next one would overflow either bound. `take >= 1`
            // always — a single input exceeding ctx_tokens already
            // had its tokens truncated to max_input_tokens, and
            // max_input_tokens ≤ ctx_tokens / n_seq_max by
            // construction at `EmbedSlot::load`.
            batch.clear();
            let mut tokens_in_batch = 0usize;
            let mut seqs_in_batch = 0usize;
            let sub_start = cursor;

            while cursor < prepared.len() {
                let next_len = prepared[cursor].len().max(1);
                if seqs_in_batch > 0
                    && (tokens_in_batch + next_len > pack_budget
                        || seqs_in_batch + 1 > slot.n_seq_max)
                {
                    break;
                }
                let seq_id = seqs_in_batch as i32;
                let toks = &prepared[cursor];
                if toks.is_empty() {
                    // Zero-token input: record a sentinel so we
                    // can inject a zero vector without running it
                    // through llama.cpp (adding zero tokens to a
                    // batch is a no-op that then has no seq to
                    // read from).
                    // We handle this by NOT advancing seqs_in_batch
                    // for this input; instead we account for it
                    // after the decode. Track its global index.
                    // Simpler: fabricate a synthetic token? Cheaper
                    // to skip. We'll handle below by post-filling.
                } else {
                    for (pos, &tok) in toks.iter().enumerate() {
                        batch
                            .add(tok, pos as i32, &[seq_id], true)
                            .map_err(|e| {
                                Error::Inference(format!("Embed batch add failed: {e}"))
                            })?;
                    }
                    tokens_in_batch += next_len;
                    seqs_in_batch += 1;
                }
                cursor += 1;
            }

            // Decode whatever sequences were packed. If every
            // input in the range was empty, `seqs_in_batch == 0`
            // and we skip the decode entirely.
            if seqs_in_batch > 0 {
                ctx_lock.ctx.clear_kv_cache();
                ctx_lock
                    .ctx
                    .decode(&mut batch)
                    .map_err(|e| Error::Inference(format!("Embed decode failed: {e}")))?;
            }

            // Walk the inputs that belonged to this sub-batch and
            // emit one embedding per input, mapping local seq_id
            // back to the original input slot. Empty inputs get
            // zero vectors of the right length.
            let mut local_seq: i32 = 0;
            for local_idx in sub_start..cursor {
                if prepared[local_idx].is_empty() {
                    results.push(vec![0.0; slot.n_embd]);
                } else {
                    let raw = ctx_lock
                        .ctx
                        .embeddings_seq_ith(local_seq)
                        .map_err(|e| {
                            Error::Inference(format!("Failed to read embedding: {e}"))
                        })?;
                    results.push(l2_normalize(raw.to_vec()));
                    local_seq += 1;
                }
            }
            sub_batches += 1;
        }

        let total_ms = batch_start.elapsed().as_millis();
        let seqs_per_sec = if total_ms > 0 {
            (inputs.len() as f64 / total_ms as f64) * 1000.0
        } else {
            0.0
        };
        tracing::info!(
            sequences = inputs.len(),
            sub_batches,
            total_ms,
            seqs_per_sec = format!("{seqs_per_sec:.1}"),
            "Batch embed complete"
        );

        Ok(results)
    }
}

/// L2-normalize in place, preserving direction; all-zero vectors
/// pass through unchanged so downstream cosine-similarity search
/// treats them consistently.
fn l2_normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
    v
}

/// Describe which compute backend the embed slot effectively
/// uses. Logged at slot load so the operator can tell at a glance
/// whether matmul is hitting Accelerate, OpenBLAS, or plain CPU.
/// The embed context is always CPU-pinned (see `EmbedSlot::load`)
/// so the only question is which CPU math library GGML picked.
/// How many CPU threads to hand llama.cpp for a single context.
///
/// Benchmarked on M2 Max (8P + 4E) with Qwen3-Embedding-0.6B:
///
///   n_threads=1  → 0.44 seq/sec    (baseline)
///   n_threads=4  → 1.63 seq/sec    (3.7× — scales)
///   n_threads=8  → 2.83 seq/sec    (6.4× — peak)
///   n_threads=10 → 2.76 seq/sec    (6.3× — plateau)
///   n_threads=12 → 2.41 seq/sec    (5.5× — REGRESSION)
///
/// Peak is at the performance-core count. Going higher puts work
/// on the efficiency cores, which are slower per-thread AND drag
/// the P-cores down by forcing cross-cluster synchronisation. The
/// crate's default of 4 under-uses any modern machine; our
/// earlier fix of "use everything available" over-used them on
/// big.LITTLE chips.
///
/// Clamp to `[2, 8]`: 8 is the P-core count on current Apple
/// Silicon flagships and roughly the elbow on x86 too (SMT
/// threads behave like E-cores here — they don't help SGEMM).
fn llama_threads_for_host() -> usize {
    let raw = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    raw.clamp(2, 8)
}

fn embed_compute_backend_label() -> &'static str {
    // Embed context is CPU-pinned (see `EmbedSlot::load`'s
    // `with_offload_kqv(false).with_op_offload(false)`). On
    // Apple Silicon, GGML's CPU backend links Accelerate's
    // SGEMM — that's where real throughput comes from. Other
    // platforms fall back to plain llama.cpp CPU kernels.
    #[cfg(target_os = "macos")]
    {
        "cpu+accelerate"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "cpu"
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
                 select a GGUF embedding model (e.g. Qwen3-Embedding-0.6B-Q4_K_M.gguf), \
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
                    slot.max_input_tokens,
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
                 select a GGUF embedding model (e.g. Qwen3-Embedding-0.6B-Q4_K_M.gguf), \
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
                    slot.max_input_tokens,
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
                 select a GGUF embedding model (e.g. Qwen3-Embedding-0.6B-Q4_K_M.gguf), \
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

/// Resolve the actual `max_tokens` budget for a generation, given the
/// caller's request, the prompt length, and the loaded context window.
///
/// Three behaviours we want to preserve, in order of importance:
///   1. **Never exceed the context window.** llama.cpp's KV cache
///      asserts on overflow and crashes the daemon. The decode loop
///      treats this cap as the upper bound for `n_generated`.
///   2. **Never reject a request that could partially succeed.** If a
///      caller asks for max_tokens=4096 on a 32k context but their
///      prompt is 30k tokens, we'd rather emit ~2k tokens than 503
///      with "Prompt too long" — opencode and aider don't recover
///      from that gracefully and the user just sees a dead chat.
///   3. **Default generously when the caller omits max_tokens.** A
///      1024-token cap (the previous default) clipped most coding
///      replies mid-thought. We hand back the entire remaining
///      context window in that case; the EOG sampler still terminates
///      naturally for short answers.
///
/// Errors only when the prompt itself doesn't fit. That's a real
/// "your input is too big" — no clamping can recover.
pub(crate) fn clamp_max_tokens(
    requested: Option<usize>,
    prompt_tokens: usize,
    n_ctx: usize,
) -> Result<usize> {
    if prompt_tokens >= n_ctx {
        return Err(Error::Inference(format!(
            "Prompt too long: {prompt_tokens} tokens already meets or exceeds \
             the context window of {n_ctx}. Shorten the conversation."
        )));
    }
    let headroom = n_ctx - prompt_tokens;
    let resolved = match requested {
        Some(asked) if asked > headroom => {
            tracing::warn!(
                requested = asked,
                clamped_to = headroom,
                prompt_tokens,
                n_ctx,
                "max_tokens exceeded context headroom; clamping to fit instead of rejecting"
            );
            headroom
        }
        Some(asked) => asked,
        // No explicit cap — give the model the whole remaining
        // window. Generation still stops at EOG for short replies.
        None => headroom,
    };
    Ok(resolved)
}

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

#[cfg(test)]
mod clamp_tests {
    use super::clamp_max_tokens;

    #[test]
    fn defaults_to_remaining_context_when_unspecified() {
        // Caller didn't ask for a cap — give them the whole window.
        assert_eq!(clamp_max_tokens(None, 1000, 8192).unwrap(), 7192);
    }

    #[test]
    fn passes_through_when_request_fits() {
        assert_eq!(
            clamp_max_tokens(Some(2000), 1000, 8192).unwrap(),
            2000
        );
    }

    #[test]
    fn clamps_when_request_exceeds_headroom() {
        // Coding-agent worst case: opencode asks for 4096 on a tight
        // window. Pre-fix: returned Err. Post-fix: emits as much as
        // fits.
        assert_eq!(
            clamp_max_tokens(Some(4096), 30000, 32768).unwrap(),
            2768
        );
    }

    #[test]
    fn errs_only_when_prompt_alone_exhausts_context() {
        let err = clamp_max_tokens(Some(100), 8192, 8192).unwrap_err();
        // The wording is the user-visible error string; lock the
        // hint phrasing so it stays actionable.
        let msg = format!("{err}");
        assert!(msg.contains("Prompt too long"), "{msg}");
    }

    #[test]
    fn errs_when_prompt_is_strictly_oversized() {
        assert!(clamp_max_tokens(None, 9000, 8192).is_err());
    }
}
