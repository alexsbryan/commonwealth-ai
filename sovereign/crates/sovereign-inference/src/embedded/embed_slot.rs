//! Auto-split from the former 9669-line `embedded.rs` (PR5b). One slot /
//! concern per file; re-exported flat through `embedded/mod.rs` so every
//! `crate::embedded::<Item>` path stays valid.
#![allow(unused_imports)]
use super::*;
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use futures::Stream;
use tokio::sync::Mutex;

use crate::llama::cpp::context::params::{LlamaContextParams, LlamaContextType};
use crate::llama::cpp::mtp::MtpSession;
use crate::llama::cpp::llama_backend::LlamaBackend;
use crate::llama::cpp::llama_batch::LlamaBatch;
use crate::llama::cpp::model::params::LlamaModelParams;
use crate::llama::cpp::model::{AddBos, LlamaChatMessage, LlamaModel};
use crate::llama::cpp::sampling::LlamaSampler;
use crate::llama::cpp::token::LlamaToken;
use crate::llama::{LlamaContextExt, LlamaModelExt};

use sovereign_core::error::Error;
use sovereign_core::model_family::{EmbedQuirks, ModelFamily, ModelQuirks, PoolingStrategy, RerankQuirks, ThinkingControl};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::*;
use sovereign_core::Result;

use crate::hardware::HardwareProfile;

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
pub(crate) struct EmbedSlot {
    pub(crate) model: Arc<LlamaModel>,
    /// Pool of contexts for parallel batch embedding. contexts[0] is
    /// the primary context used by single `embed()` calls. All contexts
    /// share the same model weights (via Arc) — only the KV cache is
    /// per-context (~50MB each for a 2048-token embedding model).
    pub(crate) contexts: Vec<Mutex<EmbedSlotContext>>,
    /// Embedding dimensionality reported by the model. Cached so the
    /// hot path doesn't have to call `model.n_embd()` on every embed.
    pub(crate) n_embd: usize,
    /// Per-input truncation limit. Any single text longer than this
    /// is clipped before decode. Named separately from the context
    /// size so bin-packing logic doesn't conflate "how big one seq
    /// can be" with "how big a packed sub-batch can be".
    pub(crate) max_input_tokens: usize,
    /// Total tokens the KV cache can hold across all active
    /// sequences in one packed decode. Governs how many chunks we
    /// can pack into a single sub-batch in `run_embed_batch_sync`.
    pub(crate) ctx_tokens: usize,
    /// Upper bound on distinct sequences per packed batch, matching
    /// the value passed to `LlamaContextParams::with_n_seq_max`.
    pub(crate) n_seq_max: usize,
    /// Family-specific embedding configuration: pooling strategy,
    /// instruction prefixes, EOS appending. None = Qwen3-Embedding and similar
    /// defaults (mean pooling, no instructions, no EOS).
    pub(crate) embed_quirks: Option<EmbedQuirks>,
}

pub(crate) struct EmbedSlotContext {
    pub(crate) ctx: crate::llama::cpp::context::LlamaContext<'static>,
    pub(crate) _model: Arc<LlamaModel>,
}

unsafe impl Send for EmbedSlotContext {}
unsafe impl Sync for EmbedSlotContext {}

impl EmbedSlot {
    pub(crate) fn load(
        backend: &Arc<LlamaBackend>,
        model_path: &Path,
        n_gpu_layers: u32,
        embed_quirks: Option<EmbedQuirks>,
    ) -> Result<Self> {
        // Force full-GPU offload when a GPU is present but the caller
        // passed 0. On Apple Silicon, Metal embed runs at ~33 seq/sec
        // vs ~2.8 seq/sec CPU (12×). On Strix Halo Vulkan it's ~40+
        // seq/sec vs 3.7-4.9 chunks/s CPU (see docs/TOOLBOX_SETUP.md
        // §9). Intel Macs stay excluded: the Metal scheduler crashes
        // in `ggml_metal_synchronize` there (hardware OOM / command-
        // buffer timeout), which is why the macOS arm is gated on
        // aarch64 rather than the broader "macos" predicate.
        // The old `GGML_ASSERT(buf_src)` crash was fixed in
        // llama-cpp-2 0.1.141; the fallback-to-CPU path below still
        // guards against context-creation failures on any platform.
        let gpu_default_available = cfg!(all(target_os = "macos", target_arch = "aarch64"))
            || cfg!(target_os = "linux");
        let requested_gpu_layers = if gpu_default_available && n_gpu_layers == 0 {
            999 // every layer on GPU
        } else {
            n_gpu_layers
        };
        let model_params = LlamaModelParams::default().with_n_gpu_layers(requested_gpu_layers);

        let model = LlamaModel::load_from_file(backend, model_path, &model_params)
            .map_err(|e| Error::Inference(format!("Failed to load embed model: {e}")))?;

        let n_embd = model.n_embd() as usize;
        // Per-input truncation: any single text longer than this is
        // clipped. Must satisfy max_input_tokens ≤ ctx_tokens / n_seq_max
        // so a full-length chunk always fits in one KV cache slot.
        let max_input_tokens = 1024;
        // Aggregate KV cache capacity. 16 slots × 1024 tokens = 16384.
        // On CPU (Intel Mac or a GPU-fallback path) this is pure RAM —
        // ~3.7 GB for a 28-layer/1024-dim model, well within 16 GB+.
        // On Apple Silicon Metal or Linux ROCm/Vulkan the same 16384
        // tokens work fine (30 GB+ unified on macOS; 124 GiB GTT on
        // Strix Halo). Intel Mac never reaches the GPU path because
        // the aarch64 gate above keeps requested_gpu_layers=0 there,
        // so wants_gpu=false and we always take the CPU context branch.
        let ctx_tokens: u32 = 16384;
        // 16 parallel sequence slots. Drives 256-seq batches through
        // 256/16=16 decode calls instead of 256/4=64, giving ~4× the
        // throughput. n_seq_max was temporarily reduced to 4 while
        // debugging Metal OOM on Intel Macs; now that Intel never
        // enters the Metal path (aarch64-only gate above) there is no
        // GPU memory pressure and we can restore the original value.
        let n_seq_max: u32 = 16;
        let model = Arc::new(model);

        // **Pooling: gguf-driven.** We deliberately do NOT call
        // `with_pooling_type(...)` here. The context default is
        // `LLAMA_POOLING_TYPE_UNSPECIFIED`, which libllama interprets
        // as "use the model's intrinsic pooling from gguf metadata"
        // (`<arch>.pooling_type`, e.g. `qwen3.pooling_type=3` for
        // Qwen3-Embedding → Last). See llama-context.cpp:158-164.
        //
        // History: an earlier attempt forced `LlamaPoolingType::Last`
        // explicitly to match the gguf, which provoked a `ggml_abort`
        // in `llama_init_from_model` on the bundled 0.2.x libllama
        // — apparently the explicit-set path on top of `embeddings=true`
        // hits a code path the bundled tree miscompiles. A second
        // attempt forced `LlamaPoolingType::None` and tried to do
        // Mean/Last/Cls in Rust against `embeddings_ith` reads, but
        // 0.2.x `embeddings_ith` returns null for the matching index
        // when the embeddings buffer is laid out for a pooled run,
        // so that path can't read what it needs either.
        //
        // Letting libllama do the pooling sidesteps both: it reads
        // the gguf's `pooling_type` (the author's stated intent), and
        // `embeddings_seq_ith(seq_id)` returns the pooled vector
        // exactly as it did on the previous binding. Family-specific
        // text prep — instruction prefix, EOS append, normalisation
        // — stays in `EmbedQuirks`, where it belongs; libllama only
        // owns the math.
        let n_threads = llama_threads_for_host();
        let wants_gpu =
            cfg!(any(target_os = "macos", target_os = "linux")) && requested_gpu_layers > 0;
        let build_params = |gpu: bool| {
            LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(ctx_tokens))
                .with_n_batch(ctx_tokens)
                .with_n_ubatch(2048)
                // EmbedSlot genuinely batches across sequences. The
                // vendored llama-cpp-4 (workspace `[patch.crates-io]`)
                // re-introduces `with_n_seq_max` so seq_id > 0 doesn't
                // hit "Decode Error -1: n_tokens == 0".
                .with_n_seq_max(16)
                .with_n_threads(n_threads as i32)
                .with_n_threads_batch(n_threads as i32)
                .with_embeddings(true)
                // GPU offload toggles. `with_offload_kqv(true)` puts
                // the KV cache in GPU memory; `with_op_offload(true)`
                // lets the GGML scheduler route compute ops to the GPU.
                // When both are true (the default when GPU works), the
                // decode runs entirely on the GPU. The CPU-only fallback
                // disables both explicitly so the scheduler keeps every
                // tensor in main memory.
                .with_offload_kqv(gpu)
                // MIGRATION 2026-05-17: .with_op_offload(...) retired in llama-cpp-4 0.2.x — see crate::llama
        };

        // Try GPU first if compiled in; fall back to CPU on any
        // context-creation error. The common failure mode on older
        // llama.cpp builds was Metal's `GGML_ASSERT(buf_src)` — ugly
        // from the crash's perspective, but if it happens before
        // `new_context` returns we just see `Err` here and retry.
        let (ctx, used_gpu) = match if wants_gpu {
            unsafe {
                let model_ref: &'static LlamaModel =
                    &*(Arc::as_ptr(&model) as *const LlamaModel);
                model_ref
                    .new_context(backend, build_params(true))
                    .map(|c| (c, true))
            }
        } else {
            Err(crate::llama::cpp::LlamaContextLoadError::NullReturn)
        } {
            Ok(pair) => pair,
            Err(e) => {
                if wants_gpu {
                    tracing::warn!(
                        error = ?e,
                        "GPU embed context failed, falling back to CPU"
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

        let quirks_pooling = match embed_quirks.as_ref().map(|q| &q.pooling) {
            Some(PoolingStrategy::Last) => "last-token",
            Some(PoolingStrategy::Cls)  => "cls",
            _                           => "mean",
        };
        // libllama-reported pooling type at runtime — sourced from the
        // gguf's `<arch>.pooling_type` after the UNSPECIFIED → hparams
        // resolution in `llama_init_from_model`. Surfacing it next to
        // the family-declared quirks pooling lets one log line catch
        // the case where the gguf author and the family card disagree
        // (which silently bakes the wrong embedding into chunks.lance
        // without any other signal).
        let runtime_pooling = format!("{:?}", ctx.pooling_type());
        let compute_backend = if used_gpu {
            gpu_backend_label()
        } else {
            embed_compute_backend_label()
        };
        tracing::info!(
            dims = n_embd,
            layers = model.n_layer(),
            size_mb = model.size() / (1024 * 1024),
            quirks_pooling,
            runtime_pooling = %runtime_pooling,
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
    pub(crate) fn embed_sync(
        model: &LlamaModel,
        ctx: &mut crate::llama::cpp::context::LlamaContext<'_>,
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
    pub(crate) fn embed_query_sync(
        model: &LlamaModel,
        ctx: &mut crate::llama::cpp::context::LlamaContext<'_>,
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
        ctx: &mut crate::llama::cpp::context::LlamaContext<'_>,
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
        // **Per-decode `set_embeddings(true)` is load-bearing.** The
        // `with_embeddings(true)` on context params *should* persist
        // for the lifetime of the context, but on the bundled
        // llama-cpp-4 0.2.x libllama the `cparams.embeddings` flag
        // ends up `false` by the time `output_reserve` runs at
        // decode — `ctx->embd.data` never gets allocated and every
        // read (`embeddings_seq_ith`, `embeddings_ith`,
        // `get_embeddings`) returns null. Calling
        // `llama_set_embeddings(true)` immediately before `decode`
        // mirrors what llama-server does per request and is what
        // unblocks the pooled-vector read. Don't remove without
        // re-running the embed probe.
        ctx.set_embeddings(true);
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
    pub(crate) fn run_embed_batch_sync(
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
                // See run_embed_sync for why this per-decode toggle
                // is load-bearing on llama-cpp-4 0.2.x.
                ctx_lock.ctx.set_embeddings(true);
                ctx_lock
                    .ctx
                    .decode(&mut batch)
                    .map_err(|e| Error::Inference(format!("Embed decode failed: {e}")))?;
            }

            // Walk the inputs that belonged to this sub-batch and
            // emit one embedding per input, mapping local seq_id
            // back to the original input slot. Empty inputs get
            // zero vectors of the right length. Non-empty inputs
            // read the libllama-pooled vector via `embeddings_seq_ith`.
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

