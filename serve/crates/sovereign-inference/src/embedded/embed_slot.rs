// SPDX-License-Identifier: AGPL-3.0-or-later
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
use crate::llama::cpp::llama_backend::LlamaBackend;
use crate::llama::cpp::llama_batch::LlamaBatch;
use crate::llama::cpp::model::params::LlamaModelParams;
use crate::llama::cpp::model::{AddBos, LlamaChatMessage, LlamaModel};
use crate::llama::cpp::mtp::MtpSession;
use crate::llama::cpp::sampling::LlamaSampler;
use crate::llama::cpp::token::LlamaToken;
use crate::llama::{LlamaContextExt, LlamaModelExt};

use oicp_types::RequestPriority;
use sovereign_contracts::error::Error;
use sovereign_contracts::model_family::{
    EmbedQuirks, ModelFamily, ModelQuirks, PoolingStrategy, RerankQuirks, ThinkingControl,
};
use sovereign_contracts::request_priority;
use sovereign_contracts::traits::InferenceProvider;
use sovereign_contracts::types::*;
use sovereign_contracts::Result;

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
    /// Family-specific embedding configuration: instruction prefixes + EOS
    /// appending (pooling itself is gguf-driven — see `load`). `None` = an
    /// unknown/BYOM embedder with no family card: no prefix, no EOS. NB this is
    /// NOT Qwen3-Embedding — that family carries Last pooling + a query
    /// instruction-prefix + EOS (`model_family::Qwen3Embedding`), which `load`
    /// auto-detects from the gguf when the caller passes `None`.
    pub(crate) embed_quirks: Option<EmbedQuirks>,
    /// The writer's context, when `contexts[0]` computes on the GPU: see
    /// [`WriterContext`].
    pub(crate) writer: Option<WriterContext>,
}

/// A CPU context over a CPU copy of the model, for an interactive request's
/// embeds while `contexts[0]` computes on the GPU (OICP v0.6 §2.17, PV3d).
/// The GPU is shared with the primary: on 2026-10-17 a writer's query embed
/// took p50 353 ms beside the primary's compute against 78 ms without it
/// (zoracite a181013), and the 0.6B embedder read a 32-token batch in 47 ms
/// on 16 CPU threads with the primary computing (llama-bench, op offload
/// off). Its vectors score within 0.0051 of the GPU's against GPU-embedded
/// sentences.
pub(crate) struct WriterContext {
    pub(crate) ctx: Mutex<EmbedSlotContext>,
}

/// The writer's context holds two sequences of `max_input_tokens` each: a
/// sentence search embeds its query and its probe together.
const WRITER_SEQ_MAX: u32 = 2;
const WRITER_CTX_TOKENS: u32 = 2048;

/// Whether a slot keeps a writer's context. Only an engine that serves the
/// writer beside the primary on one device wants one: an embed-only engine's
/// calls carry no priority, so its writer's context would never be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Writer {
    OnCpu,
    None,
}

/// Whether a request that asked `priority` embeds on the writer's context.
fn on_writers_context(priority: Option<RequestPriority>) -> bool {
    priority == Some(RequestPriority::Interactive)
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
        writer: Writer,
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
        let gpu_default_available =
            cfg!(all(target_os = "macos", target_arch = "aarch64")) || cfg!(target_os = "linux");
        let requested_gpu_layers = if gpu_default_available && n_gpu_layers == 0 {
            999 // every layer on GPU
        } else {
            n_gpu_layers
        };
        let model_params = LlamaModelParams::default().with_n_gpu_layers(requested_gpu_layers);

        let model =
            LlamaModel::load_from_file(backend, model_path, &model_params).map_err(|e| {
                Error::Inference(crate::llama::describe_model_load_failure(
                    "embed", model_path, e,
                ))
            })?;

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

        // ── Embed-family auto-detect (gguf-driven) ──────────────────────────
        // The caller resolves `embed_quirks` from the model's family, which the
        // manifest derives by FILENAME (`embed_family_for_file`). A prescribed
        // Qwen3-Embedding gguf stored under a non-default name (e.g.
        // `qwen-embedding-0.6b.gguf`) fails that match → family=Unknown →
        // `embed_quirks=None`, which silently DROPS Qwen3-Embedding's query
        // instruction-prefix + EOS. That degrades retrieval AND makes the
        // embeddings diverge from the pre-built router cache (the sentinel probe
        // then rejects it → a multi-minute re-embed). Recover from the gguf
        // itself: a `qwen3`-family embedding model gets Qwen3-Embedding's quirks
        // regardless of how the file is named. Only runs when the caller didn't
        // already resolve a family, so it's free on the common path.
        let embed_quirks = embed_quirks.or_else(|| {
            let arch = super::prompt_helpers::read_gguf_arch(&model);
            if arch.to_ascii_lowercase().starts_with("qwen3") {
                tracing::info!(
                    arch = %arch,
                    "embed family auto-detected from gguf: Qwen3Embedding \
                     (filename did not match a manifest embed slot)"
                );
                ModelFamily::Qwen3Embedding.default_quirks().embed
            } else {
                None
            }
        });

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
                let model_ref: &'static LlamaModel = &*(Arc::as_ptr(&model));
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
                    let model_ref: &'static LlamaModel = &*(Arc::as_ptr(&model));
                    model_ref
                        .new_context(backend, build_params(false))
                        .map_err(|e| {
                            Error::Inference(format!("Failed to create embed context: {e}"))
                        })?
                };
                (ctx, false)
            }
        };

        let quirks_pooling = match embed_quirks.as_ref().map(|q| &q.pooling) {
            Some(PoolingStrategy::Last) => "last-token",
            Some(PoolingStrategy::Cls) => "cls",
            _ => "mean",
        };
        // libllama-reported pooling type at runtime — sourced from the
        // gguf's `<arch>.pooling_type` after the UNSPECIFIED → hparams
        // resolution in `llama_init_from_model`. Surfacing it next to
        // the family-declared quirks pooling lets one log line catch
        // the case where the gguf author and the family card disagree
        // (which silently bakes the wrong embedding into chunks.lance
        // without any other signal).
        let runtime_pooling = format!("{:?}", ctx.pooling_type());
        let compute_backend = compute_backend_label(used_gpu);
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

        let writer = if used_gpu && writer == Writer::OnCpu {
            Self::load_writer(backend, model_path, n_threads, max_input_tokens)
        } else {
            None
        };

        Ok(Self {
            model: model.clone(),
            contexts: vec![Mutex::new(EmbedSlotContext { ctx, _model: model })],
            n_embd,
            max_input_tokens,
            ctx_tokens: ctx_tokens as usize,
            n_seq_max: n_seq_max as usize,
            embed_quirks,
            writer,
        })
    }

    /// The writer's context: the model loaded again with no layer on the GPU,
    /// and a context that offloads no op, so its compute stays on the CPU. A
    /// failure is said and leaves the writer's embeds on `contexts[0]`.
    fn load_writer(
        backend: &Arc<LlamaBackend>,
        model_path: &Path,
        n_threads: usize,
        max_input_tokens: usize,
    ) -> Option<WriterContext> {
        debug_assert!(max_input_tokens <= (WRITER_CTX_TOKENS / WRITER_SEQ_MAX) as usize);
        let params = LlamaModelParams::default().with_n_gpu_layers(0);
        let model = match LlamaModel::load_from_file(backend, model_path, &params) {
            Ok(m) => Arc::new(m),
            Err(e) => {
                tracing::warn!(error = %e, "EmbedSlot::load — the writer's CPU model did not load; interactive embeds stay on the GPU context");
                return None;
            }
        };
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(WRITER_CTX_TOKENS))
            .with_n_batch(WRITER_CTX_TOKENS)
            .with_n_ubatch(WRITER_CTX_TOKENS)
            .with_n_seq_max(WRITER_SEQ_MAX)
            .with_n_threads(n_threads as i32)
            .with_n_threads_batch(n_threads as i32)
            .with_embeddings(true)
            .with_offload_kqv(false)
            .with_op_offload(false);
        let ctx = unsafe {
            let model_ref: &'static LlamaModel = &*(Arc::as_ptr(&model));
            model_ref.new_context(backend, ctx_params)
        };
        match ctx {
            Ok(ctx) => {
                tracing::info!(
                    n_ctx = WRITER_CTX_TOKENS,
                    n_seq_max = WRITER_SEQ_MAX,
                    n_threads,
                    size_mb = model.size() / (1024 * 1024),
                    "EmbedSlot::load — the writer's CPU context ready"
                );
                Some(WriterContext {
                    ctx: Mutex::new(EmbedSlotContext { ctx, _model: model }),
                })
            }
            Err(e) => {
                tracing::warn!(error = ?e, "EmbedSlot::load — the writer's CPU context failed; interactive embeds stay on the GPU context");
                None
            }
        }
    }

    /// The context an embed for the running request computes on, with the
    /// tokens and sequences one decode of it holds: the writer's when the
    /// request asked as interactive and the slot has one, else `contexts[0]`.
    pub(crate) fn context_for_request(&self) -> (&Mutex<EmbedSlotContext>, usize, usize) {
        let asked = request_priority::current().map(|a| a.priority());
        match &self.writer {
            Some(w) if on_writers_context(asked) => {
                tracing::debug!(target: "priority", "priority: an interactive embed on the writer's CPU context");
                (&w.ctx, WRITER_CTX_TOKENS as usize, WRITER_SEQ_MAX as usize)
            }
            _ => (&self.contexts[0], self.ctx_tokens, self.n_seq_max),
        }
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
        // The one table assembles the input; this slot never formats a
        // prefix or an EOS literal of its own (ARCH §10.6 — the same
        // `prepare_document` runs in `corpus-mcp` against a bare endpoint,
        // which is the only reason the two produce the same vector space).
        let prepared = match embed_quirks {
            Some(eq) => eq.prepare_document(text),
            None => text.to_string(),
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
        let prepared = match embed_quirks {
            Some(eq) => eq.prepare_query(query),
            None => query.to_string(),
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
        super::ffi_trace::record(super::ffi_trace::FfiCall::SetEmbeddings);
        ctx.set_embeddings(true);
        ctx.decode(&mut batch)
            .map_err(|e| Error::Inference(format!("Embed decode failed: {e}")))?;
        super::ffi_trace::record(super::ffi_trace::FfiCall::EmbedDecode);

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
    ///      (a) total tokens ≤ the context's `ctx_tokens`
    ///      (b) sequence count ≤ the context's `n_seq_max`
    /// 3. For each sub-batch: clear KV cache, add tokens with
    ///    distinct `seq_id`s, `decode()`, then read one pooled
    ///    embedding per sequence.
    /// 4. Splice results back in input order — bin-packing is
    ///    order-preserving so this is just concatenation.
    ///
    /// A pathological input longer than the context's `ctx_tokens` is
    /// truncated to `slot.max_input_tokens` up front, so single
    /// oversized chunks always fit in their own sub-batch.
    pub(crate) fn run_embed_batch_sync(
        slot: &EmbedSlot,
        inputs: &[String],
        step: &mut crate::priority_gate::Step,
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
                Some(eq) => eq.prepare_document(text),
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
        let mut results: Vec<Vec<f32>> = Vec::with_capacity(inputs.len());

        let mut sub_batches = 0usize;
        let mut cursor = 0usize;
        // Reuse one `LlamaBatch` allocation across sub-batches;
        // `clear()` resets n_tokens without freeing the backing
        // memory. Matches the context's n_batch capacity.
        let (context, ctx_tokens, n_seq_max) = slot.context_for_request();
        let mut batch = LlamaBatch::new(ctx_tokens, n_seq_max as i32);

        // Pack to ~90% of the nominal ctx budget. llama.cpp uses
        // some headroom inside the unified KV cache for scheduler
        // bookkeeping; packing to exactly `ctx_tokens` has tripped
        // `NoKvCacheSlot` at decode time on real-world corpora.
        let pack_budget = ctx_tokens.saturating_sub(ctx_tokens / 10);

        // A background request's sub-batches each take the context and let it
        // go after, so a writer's embed queued at it goes between them (PV3c);
        // the KV cache is cleared before every decode, so nothing carries over.
        let gate = crate::priority_gate::PriorityGate::process();
        gate.parts_under(
            "embed_batch",
            || {
                let held = context.blocking_lock();
                step.entered();
                held
            },
            |ctx_lock| {
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
                            || seqs_in_batch + 1 > n_seq_max)
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
                            batch.add(tok, pos as i32, &[seq_id], true).map_err(|e| {
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
                    super::ffi_trace::record(super::ffi_trace::FfiCall::SetEmbeddings);
                    ctx_lock.ctx.set_embeddings(true);
                    ctx_lock
                        .ctx
                        .decode(&mut batch)
                        .map_err(|e| Error::Inference(format!("Embed decode failed: {e}")))?;
                    super::ffi_trace::record(super::ffi_trace::FfiCall::EmbedDecode);
                }

                // Walk the inputs that belonged to this sub-batch and
                // emit one embedding per input, mapping local seq_id
                // back to the original input slot. Empty inputs get
                // zero vectors of the right length. Non-empty inputs
                // read the libllama-pooled vector via `embeddings_seq_ith`.
                let mut local_seq: i32 = 0;
                for prepared_item in &prepared[sub_start..cursor] {
                    if prepared_item.is_empty() {
                        results.push(vec![0.0; slot.n_embd]);
                    } else {
                        let raw = ctx_lock.ctx.embeddings_seq_ith(local_seq).map_err(|e| {
                            Error::Inference(format!("Failed to read embedding: {e}"))
                        })?;
                        results.push(l2_normalize(raw.to_vec()));
                        local_seq += 1;
                    }
                }
                sub_batches += 1;
                Ok::<_, Error>(cursor < prepared.len())
            },
        )?;

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

#[cfg(test)]
#[path = "embed_slot_tests.rs"]
mod tests;
