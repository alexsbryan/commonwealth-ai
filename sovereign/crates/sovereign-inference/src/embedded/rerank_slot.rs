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

use sovereign_core::error::Error;
use sovereign_core::model_family::{
    EmbedQuirks, ModelFamily, ModelQuirks, PoolingStrategy, RerankQuirks, ThinkingControl,
};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::*;
use sovereign_core::Result;

use crate::hardware::HardwareProfile;

// ─── RerankSlot ───────────────────────────────────────────────

/// Dedicated cross-encoder reranker slot. Loads a reranker GGUF
/// (e.g. `jina-reranker-v3-Q6_K.gguf`, `bge-reranker-v2-m3-Q8_0.gguf`)
/// with `embeddings = true` and `pooling_type = LLAMA_POOLING_TYPE_RANK`
/// so each decode pass yields a single scalar relevance logit per
/// (query, doc) pair.
///
/// The slot is lazy-loaded on first rerank call and held resident
/// thereafter — at ~500 MB on disk for a 0.6B Q6_K model, idle
/// cost is acceptable on `default`/`high`/`very_high` profiles. We
/// clear the KV cache between calls so each pair is an independent
/// sequence.
///
/// Public so `StandaloneReranker` (see `reranker_standalone.rs`) can
/// hold one directly without going through `EmbeddedLlamaCpp` —
/// useful for eval processes that don't want to pay the cost of
/// loading host chat slots they'll never call.
pub struct RerankSlot {
    pub(crate) model: Arc<LlamaModel>,
    // std::sync::Mutex (not tokio::sync) because score_batch runs
    // inside spawn_blocking — the lock is held across a blocking
    // llama.cpp decode, never across an `.await`.
    pub(crate) ctx: std::sync::Mutex<RerankSlotContext>,
    /// Hard cap on the full prompt (chat-template wrapping + query +
    /// doc + trailing rerank token). Pairs longer than this are
    /// truncated on the doc side.
    pub(crate) max_input_tokens: usize,
    /// Token-ID of `<|score_token|>` — read once at load time. The
    /// reranker emits this token's logit at the last-input position
    /// and that logit IS the relevance score.
    pub(crate) score_token_id: i32,
    /// Token-ID of `<|rerank_token|>` — placed at the end of the
    /// prompt so the model conditions on "now produce a score".
    pub(crate) rerank_token_id: i32,
    /// Pre-tokenised chat-template prefix (everything up to where
    /// the query gets spliced in). Built once at load time so the
    /// hot path skips re-tokenising the boilerplate.
    pub(crate) prefix_tokens: Vec<LlamaToken>,
    /// Pre-tokenised separator between query and document (the
    /// `<|im_end|>\n<doc>` glue in the Qwen3 chat template).
    pub(crate) middle_tokens: Vec<LlamaToken>,
    /// Pre-tokenised suffix between document and `<|rerank_token|>`
    /// (the `<|im_end|>\n<|im_start|>assistant\n` glue).
    pub(crate) suffix_tokens: Vec<LlamaToken>,
    /// File stem of the loaded reranker — surfaced in provenance +
    /// telemetry so a user can tell which reranker scored a given
    /// result. Format matches `ModelSlot.model_id`.
    pub(crate) model_id: String,
}

/// Build the Qwen3-style chat-template fragments used to wrap a
/// (query, doc) pair into a jina-reranker-v3 prompt.
///
/// The protocol: a chat-template prompt containing the query +
/// document, ending with the special `<|rerank_token|>` so the
/// model conditions on "produce a score next". The relevance
/// score is then read off as the logit of `<|score_token|>` at
/// the last-input position.
///
/// `SOVEREIGN_RERANK_PROMPT_VARIANT` selects between templates so
/// we can A/B without recompiling. Values:
///   - `verbose` (default-historic) — full system message + XML tags
///   - `lean` — no system message, `Query: ... Document: ...` body
fn jina_v3_prompt_fragments() -> (String, String, String) {
    let variant =
        std::env::var("SOVEREIGN_RERANK_PROMPT_VARIANT").unwrap_or_else(|_| "verbose".to_string());
    match variant.as_str() {
        "lean" => {
            let prefix = "<|im_start|>user\nQuery: ".to_string();
            let middle = "\n\nDocument: ".to_string();
            let suffix = "\n<|im_end|>\n<|im_start|>assistant\n".to_string();
            (prefix, middle, suffix)
        }
        _ => {
            let prefix = "<|im_start|>system\nYou are a search relevance assessor. \
                Read the query and document, then judge how relevant the document \
                is to the query.\n<|im_end|>\n<|im_start|>user\n<query>\n"
                .to_string();
            let middle = "\n</query>\n<document>\n".to_string();
            let suffix = "\n</document>\n<|im_end|>\n<|im_start|>assistant\n".to_string();
            (prefix, middle, suffix)
        }
    }
}

pub(crate) struct RerankSlotContext {
    pub(crate) ctx: crate::llama::cpp::context::LlamaContext<'static>,
    pub(crate) _model: Arc<LlamaModel>,
}

unsafe impl Send for RerankSlotContext {}
unsafe impl Sync for RerankSlotContext {}

impl RerankSlot {
    pub fn load(
        backend: &Arc<LlamaBackend>,
        model_path: &Path,
        n_gpu_layers: u32,
        rerank_quirks: Option<RerankQuirks>,
    ) -> Result<Self> {
        // Same GPU-first / CPU-fallback policy as EmbedSlot.
        let gpu_default_available =
            cfg!(all(target_os = "macos", target_arch = "aarch64")) || cfg!(target_os = "linux");
        let requested_gpu_layers = if gpu_default_available && n_gpu_layers == 0 {
            999
        } else {
            n_gpu_layers
        };
        let model_params = LlamaModelParams::default().with_n_gpu_layers(requested_gpu_layers);

        let model = LlamaModel::load_from_file(backend, model_path, &model_params)
            .map_err(|e| Error::Inference(format!("Failed to load reranker model: {e}")))?;

        // The reranker emits one scalar per sequence — n_embd is
        // typically 1 in this mode but we don't read it; we just
        // grab the first float of the rank output.
        let max_input_tokens = rerank_quirks
            .as_ref()
            .map(|q| q.max_context)
            .unwrap_or(8192);
        let ctx_tokens: u32 = max_input_tokens as u32;
        let model = Arc::new(model);

        let n_threads = llama_threads_for_host();
        let wants_gpu =
            cfg!(any(target_os = "macos", target_os = "linux")) && requested_gpu_layers > 0;
        // Generative mode: pooling=None, embeddings=false. The
        // reranker is a Qwen3 LLM whose relevance score lives in
        // the logit for `<|score_token|>` at the position right
        // after a trailing `<|rerank_token|>` — there is no BERT-
        // style pooled rank head to read from. Setting
        // pooling=Rank on this model silently returns zeros (the
        // mode jina-reranker-v3 doesn't implement).
        let build_params = |gpu: bool| {
            LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(ctx_tokens))
                .with_n_batch(ctx_tokens)
                .with_n_ubatch(ctx_tokens.min(2048))
                // MIGRATION 2026-05-17: .with_n_seq_max(...) retired in llama-cpp-4 0.2.x — see crate::llama
                .with_n_threads(n_threads as i32)
                .with_n_threads_batch(n_threads as i32)
                .with_offload_kqv(gpu)
            // MIGRATION 2026-05-17: .with_op_offload(...) retired in llama-cpp-4 0.2.x — see crate::llama
        };

        let (ctx, used_gpu) = match if wants_gpu {
            unsafe {
                let model_ref: &'static LlamaModel = &*(Arc::as_ptr(&model) as *const LlamaModel);
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
                        "GPU rerank context failed, falling back to CPU"
                    );
                }
                let ctx = unsafe {
                    let model_ref: &'static LlamaModel =
                        &*(Arc::as_ptr(&model) as *const LlamaModel);
                    model_ref
                        .new_context(backend, build_params(false))
                        .map_err(|e| {
                            Error::Inference(format!("Failed to create rerank context: {e}"))
                        })?
                };
                (ctx, false)
            }
        };

        let model_id = model_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("reranker")
            .to_string();

        // Resolve the two special tokens by name. They're added by
        // Jina's fine-tune and embedded in the GGUF tokenizer.
        // `str_to_token` returns them when the string matches a
        // special token exactly. If either lookup fails, the load
        // fails up front — we never want to silently fall through to
        // garbage scores at query time.
        let score_token_id = resolve_special_token(&model, "<|score_token|>").ok_or_else(|| {
            Error::Inference(
                "Reranker GGUF lacks `<|score_token|>` — does this model \
                     follow the jina-reranker-v3 protocol?"
                    .to_string(),
            )
        })?;
        let rerank_token_id =
            resolve_special_token(&model, "<|rerank_token|>").ok_or_else(|| {
                Error::Inference(
                    "Reranker GGUF lacks `<|rerank_token|>` — does this model \
                     follow the jina-reranker-v3 protocol?"
                        .to_string(),
                )
            })?;

        // Pre-tokenise the static chat-template fragments. The hot
        // path stitches: prefix + query_tokens + middle + doc_tokens
        // + suffix + rerank_token. Boilerplate is non-trivial (~40
        // tokens) so caching saves a meaningful slice of per-pair
        // CPU.
        let (prefix_str, middle_str, suffix_str) = jina_v3_prompt_fragments();
        let prefix_tokens = model
            .str_to_token(&prefix_str, AddBos::Never)
            .map_err(|e| Error::Inference(format!("tokenise rerank prefix: {e}")))?;
        let middle_tokens = model
            .str_to_token(&middle_str, AddBos::Never)
            .map_err(|e| Error::Inference(format!("tokenise rerank middle: {e}")))?;
        let suffix_tokens = model
            .str_to_token(&suffix_str, AddBos::Never)
            .map_err(|e| Error::Inference(format!("tokenise rerank suffix: {e}")))?;

        let compute_backend = if used_gpu {
            gpu_backend_label()
        } else {
            embed_compute_backend_label()
        };
        tracing::info!(
            slot = "rerank",
            model_id = %model_id,
            layers = model.n_layer(),
            size_mb = model.size() / (1024 * 1024),
            n_ctx = ctx_tokens,
            max_input_tokens,
            n_threads,
            compute_backend,
            score_token = score_token_id,
            rerank_token = rerank_token_id,
            prefix_tokens_n = prefix_tokens.len(),
            middle_tokens_n = middle_tokens.len(),
            suffix_tokens_n = suffix_tokens.len(),
            "RerankSlot::load — slot ready"
        );

        Ok(Self {
            model: model.clone(),
            ctx: std::sync::Mutex::new(RerankSlotContext { ctx, _model: model }),
            max_input_tokens,
            score_token_id,
            rerank_token_id,
            prefix_tokens,
            middle_tokens,
            suffix_tokens,
            model_id,
        })
    }
}

/// Resolve a special token by its string form. Returns `None` when
/// the tokenizer doesn't carry the token (which means the model
/// doesn't speak the jina-reranker-v3 protocol — caller surfaces
/// a hard error).
fn resolve_special_token(model: &LlamaModel, name: &str) -> Option<i32> {
    // Special tokens are added by `str_to_token` when the string
    // matches the literal token form. `AddBos::Never` prevents
    // smuggling a BOS in front of the lookup.
    let toks = model.str_to_token(name, AddBos::Never).ok()?;
    // The lookup is well-formed only when it produces exactly one
    // token — otherwise the string didn't match a single special
    // entry and we treat the model as incompatible.
    if toks.len() == 1 {
        Some(toks[0].0)
    } else {
        None
    }
}

// Continuation of the RerankSlot impl block (split for the helper above).
impl RerankSlot {
    /// Score a single (query, doc) pair via the jina-reranker-v3
    /// protocol: build a Qwen3 chat-template prompt that wraps the
    /// pair, end with `<|rerank_token|>`, run one forward pass, and
    /// read the logit of `<|score_token|>` at the last input
    /// position. Higher = more relevant.
    #[allow(clippy::too_many_arguments)]
    fn score_pair_sync(
        model: &LlamaModel,
        ctx: &mut crate::llama::cpp::context::LlamaContext<'_>,
        query: &str,
        doc: &str,
        max_input_tokens: usize,
        score_token_id: i32,
        rerank_token_id: i32,
        prefix_tokens: &[LlamaToken],
        middle_tokens: &[LlamaToken],
        suffix_tokens: &[LlamaToken],
    ) -> Result<f32> {
        ctx.clear_kv_cache();

        // Tokenise the dynamic parts.
        let query_tokens = model
            .str_to_token(query, AddBos::Never)
            .map_err(|e| Error::Inference(format!("Rerank query tokenise: {e}")))?;
        let doc_tokens = model
            .str_to_token(doc, AddBos::Never)
            .map_err(|e| Error::Inference(format!("Rerank doc tokenise: {e}")))?;

        // Truncation budget. Fixed overhead = prefix + middle +
        // suffix + 1 (the trailing rerank token). Whatever's left
        // is split: query keeps its full length up to a ceiling
        // (queries are usually short), and doc takes everything
        // else. When even the query won't fit, truncate query too.
        let fixed = prefix_tokens.len() + middle_tokens.len() + suffix_tokens.len() + 1;
        let dynamic_budget = max_input_tokens.saturating_sub(fixed);
        const QUERY_HARD_CAP: usize = 256;
        let query_budget = query_tokens.len().min(QUERY_HARD_CAP).min(dynamic_budget);
        let doc_budget = dynamic_budget.saturating_sub(query_budget);
        if dynamic_budget == 0 {
            return Err(Error::Inference(
                "Rerank context too small for prompt scaffold".to_string(),
            ));
        }

        let mut tokens: Vec<LlamaToken> = Vec::with_capacity(
            prefix_tokens.len()
                + query_budget
                + middle_tokens.len()
                + doc_budget
                + suffix_tokens.len()
                + 1,
        );
        tokens.extend_from_slice(prefix_tokens);
        tokens.extend(query_tokens.iter().take(query_budget).copied());
        tokens.extend_from_slice(middle_tokens);
        tokens.extend(doc_tokens.iter().take(doc_budget).copied());
        tokens.extend_from_slice(suffix_tokens);
        tokens.push(LlamaToken(rerank_token_id));

        let mut batch = LlamaBatch::new(tokens.len(), 1);
        let last = tokens.len() - 1;
        for (i, &tok) in tokens.iter().enumerate() {
            // We only need logits at the LAST position — that's
            // where the model predicts the token after
            // `<|rerank_token|>` and the score-token logit lives.
            // Marking every position would waste memory and decode
            // time; marking only the last position is exactly what
            // the protocol needs.
            batch
                .add(tok, i as i32, &[0], i == last)
                .map_err(|e| Error::Inference(format!("Rerank batch add: {e}")))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| Error::Inference(format!("Rerank decode: {e}")))?;

        // Read the score-token logit at the last position. This is
        // the model's pre-softmax confidence that the next token
        // SHOULD be `<|score_token|>` — calibrated by fine-tuning
        // to be high for relevant pairs and low for irrelevant
        // ones.
        let logits = ctx.get_logits_ith(last as i32);
        let idx = score_token_id as usize;
        if idx >= logits.len() {
            return Err(Error::Inference(format!(
                "score_token_id {score_token_id} out of vocab ({} tokens)",
                logits.len()
            )));
        }
        Ok(logits[idx])
    }

    /// Score multiple docs against one query. Sequential under the
    /// slot mutex — generative decode of ~50-100 tokens per pair is
    /// fast on a 0.6B model (Vulkan/ROCm: ~10ms/pair), so a 50-doc
    /// rerank pass costs ~0.5s. True intra-sequence batching is
    /// possible but the engineering cost only pays off above ~200
    /// pairs/call, well beyond what corpus search produces.
    pub fn score_batch(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        let mut out = Vec::with_capacity(docs.len());
        let mut guard = self
            .ctx
            .lock()
            .map_err(|e| Error::Inference(format!("rerank lock poisoned: {e}")))?;
        for doc in docs {
            let score = Self::score_pair_sync(
                &self.model,
                &mut guard.ctx,
                query,
                doc,
                self.max_input_tokens,
                self.score_token_id,
                self.rerank_token_id,
                &self.prefix_tokens,
                &self.middle_tokens,
                &self.suffix_tokens,
            )?;
            out.push(score);
        }
        Ok(out)
    }
}

/// L2-normalize in place, preserving direction; all-zero vectors
/// pass through unchanged so downstream cosine-similarity search
/// treats them consistently.
pub(crate) fn l2_normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
    v
}

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
pub(crate) fn llama_threads_for_host() -> usize {
    let raw = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    raw.clamp(2, 8)
}

/// Label emitted in slot-load logs when the GPU path wasn't taken
/// (GPU context creation failed, or caller didn't request it).
/// Tells the operator which CPU math backend GGML is using.
pub(crate) fn embed_compute_backend_label() -> &'static str {
    // On Apple Silicon, GGML's CPU backend links Accelerate's SGEMM
    // — that's where real CPU-path throughput comes from. Other
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

/// Label emitted in slot-load logs when `wants_gpu` succeeded.
/// The specific backend (metal / rocm / vulkan) is still visible
/// in the nearby `ggml_*_init` llama.cpp output at startup.
pub(crate) fn gpu_backend_label() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "gpu+metal"
    }
    #[cfg(not(target_os = "macos"))]
    {
        // llama-cpp-2's `rocm` vs `vulkan` feature is selected at
        // the workspace level (see crates/sovereign-inference/Cargo.toml)
        // and not re-exposed as a rustc cfg here, so we just say "gpu".
        "gpu"
    }
}
