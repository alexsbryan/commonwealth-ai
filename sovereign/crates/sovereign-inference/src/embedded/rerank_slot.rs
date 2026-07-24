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
    /// How this model expresses a relevance score — auto-detected at
    /// load from the tokenizer (see [`RerankProtocol`]).
    pub(crate) protocol: RerankProtocol,
    /// Pre-tokenised chat-template prefix (everything up to where
    /// the query gets spliced in). Built once at load time so the
    /// hot path skips re-tokenising the boilerplate.
    pub(crate) prefix_tokens: Vec<LlamaToken>,
    /// Pre-tokenised separator between query and document (the
    /// `<|im_end|>\n<doc>` glue in the Qwen3 chat template).
    pub(crate) middle_tokens: Vec<LlamaToken>,
    /// Pre-tokenised suffix after the document (protocol-specific
    /// closing glue).
    pub(crate) suffix_tokens: Vec<LlamaToken>,
    /// File stem of the loaded reranker — surfaced in provenance +
    /// telemetry so a user can tell which reranker scored a given
    /// result. Format matches `ModelSlot.model_id`.
    pub(crate) model_id: String,
}

/// Score-extraction protocol, auto-detected from the GGUF tokenizer.
///
/// - `JinaScoreToken`: the jina-reranker-v3 scheme — prompt ends with
///   `<|rerank_token|>` and the score is the `<|score_token|>` logit
///   at the last position. NOTE: measured 2026-07-09, the publicly
///   converted jina-v3 GGUF DROPS the model's scoring/projection head
///   (no `output.weight`, no head tensors), so this protocol reads a
///   tied-embedding dot product — relevance noise. It is kept for a
///   future correctly-converted GGUF; prefer a yes/no-family reranker
///   until one exists.
/// - `YesNoLogit`: the Qwen3-Reranker family scheme (incl. fine-tunes
///   like harrier-oss-v1) — a chat prompt asks whether the document
///   answers the query, and the score is `logit("yes") − logit("no")`
///   at the last position. Survives vanilla GGUF conversion because
///   Qwen3-0.6B ties `lm_head` to `token_embd` — the scoring surface
///   IS the trained next-token head, nothing to drop.
#[derive(Debug, Clone, Copy)]
pub(crate) enum RerankProtocol {
    JinaScoreToken {
        score_token_id: i32,
        rerank_token_id: i32,
    },
    YesNoLogit {
        yes_token_id: i32,
        no_token_id: i32,
    },
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

/// Prompt fragments for the Qwen3-Reranker yes/no protocol (the
/// published Qwen3-Reranker chat scaffold, empty think block included
/// — the family is trained with thinking disabled for scoring).
/// `SOVEREIGN_RERANK_INSTRUCT` overrides the instruct line; the
/// default is the family's own generic retrieval instruction.
fn yes_no_prompt_fragments() -> (String, String, String) {
    let instruct = std::env::var("SOVEREIGN_RERANK_INSTRUCT").unwrap_or_else(|_| {
        "Given a web search query, retrieve relevant passages that answer the query".to_string()
    });
    let prefix = format!(
        "<|im_start|>system\nJudge whether the Document meets the requirements based on the \
         Query and the Instruct provided. Note that the answer can only be \"yes\" or \
         \"no\".<|im_end|>\n<|im_start|>user\n<Instruct>: {instruct}\n<Query>: "
    );
    let middle = "\n<Document>: ".to_string();
    let suffix = "<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n".to_string();
    (prefix, middle, suffix)
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

        let model =
            LlamaModel::load_from_file(backend, model_path, &model_params).map_err(|e| {
                Error::Inference(crate::llama::describe_model_load_failure(
                    "reranker", model_path, e,
                ))
            })?;

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
                // Multi-sequence rerank: size the KV for RERANK_BATCH_SEQS
                // concurrent sequences (1 shared prefix + doc tails) so
                // `score_batch` can fan a decoded prefix out to doc
                // sequences and score a whole wave in one decode.
                // (with_n_seq_max restored in llama-cpp-4 0.4.2 — absent
                // in the 0.2.x binding.)
                .with_n_seq_max(RERANK_BATCH_SEQS)
                // UNIFIED cache = one stream shared across sequences. This
                // is load-bearing for the fanout: only in unified mode is
                // a partial `copy_kv_cache_seq(prefix range)` the cheap
                // same-stream cell-tag update the fanout assumes. The
                // llama.cpp default (per-sequence streams) makes that a
                // cross-stream copy that ABORTS ("seq_cp() is only
                // supported for full KV buffers", llama-kv-cache.cpp).
                .with_kv_unified(true)
                .with_n_ubatch(ctx_tokens.min(2048))
                .with_n_threads(n_threads as i32)
                .with_n_threads_batch(n_threads as i32)
                .with_offload_kqv(gpu)
            // MIGRATION 2026-05-17: .with_op_offload(...) retired in llama-cpp-4 0.2.x — see crate::llama
        };

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
                        "GPU rerank context failed, falling back to CPU"
                    );
                }
                let ctx = unsafe {
                    let model_ref: &'static LlamaModel = &*(Arc::as_ptr(&model));
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

        // Protocol auto-detection from the tokenizer. Jina's special
        // tokens present → jina score-token protocol; otherwise fall
        // back to the Qwen3-Reranker yes/no protocol (plain "yes"/"no"
        // must each resolve to a single token — true for the whole
        // qwen vocab family). Either way a failed resolution fails the
        // load up front — never garbage scores at query time.
        let protocol = match (
            resolve_special_token(&model, "<|score_token|>"),
            resolve_special_token(&model, "<|rerank_token|>"),
        ) {
            (Some(score_token_id), Some(rerank_token_id)) => RerankProtocol::JinaScoreToken {
                score_token_id,
                rerank_token_id,
            },
            _ => {
                let yes_token_id = resolve_special_token(&model, "yes").ok_or_else(|| {
                    Error::Inference(
                        "Reranker GGUF lacks jina special tokens AND `yes` is not a \
                         single token — unknown reranker protocol"
                            .to_string(),
                    )
                })?;
                let no_token_id = resolve_special_token(&model, "no").ok_or_else(|| {
                    Error::Inference(
                        "Reranker GGUF lacks jina special tokens AND `no` is not a \
                         single token — unknown reranker protocol"
                            .to_string(),
                    )
                })?;
                RerankProtocol::YesNoLogit {
                    yes_token_id,
                    no_token_id,
                }
            }
        };

        // Pre-tokenise the static chat-template fragments. The hot
        // path stitches: prefix + query_tokens + middle + doc_tokens
        // + suffix (+ rerank token on the jina protocol). Boilerplate
        // is non-trivial (~40 tokens) so caching saves a meaningful
        // slice of per-pair CPU.
        let (prefix_str, middle_str, suffix_str) = match protocol {
            RerankProtocol::JinaScoreToken { .. } => jina_v3_prompt_fragments(),
            RerankProtocol::YesNoLogit { .. } => yes_no_prompt_fragments(),
        };
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
            protocol = ?protocol,
            prefix_tokens_n = prefix_tokens.len(),
            middle_tokens_n = middle_tokens.len(),
            suffix_tokens_n = suffix_tokens.len(),
            "RerankSlot::load — slot ready"
        );

        Ok(Self {
            model: model.clone(),
            ctx: std::sync::Mutex::new(RerankSlotContext { ctx, _model: model }),
            max_input_tokens,
            protocol,
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
    /// Read the protocol's relevance score off a logit slice (the
    /// last decoded position): the `<|score_token|>` logit (jina) or
    /// `logit(yes) − logit(no)` (Qwen3-Reranker family). Higher =
    /// more relevant.
    fn read_score(protocol: RerankProtocol, logits: &[f32]) -> Result<f32> {
        let read = |id: i32| -> Result<f32> {
            let idx = id as usize;
            if idx >= logits.len() {
                return Err(Error::Inference(format!(
                    "token id {id} out of vocab ({} tokens)",
                    logits.len()
                )));
            }
            Ok(logits[idx])
        };
        match protocol {
            RerankProtocol::JinaScoreToken { score_token_id, .. } => read(score_token_id),
            RerankProtocol::YesNoLogit {
                yes_token_id,
                no_token_id,
            } => Ok(read(yes_token_id)? - read(no_token_id)?),
        }
    }

    /// Split the context budget for one rerank call. Fixed overhead =
    /// prefix + middle + suffix + 1 (the trailing rerank token).
    /// Whatever's left is split: query keeps its full length up to a
    /// ceiling (queries are usually short), doc takes the rest. When
    /// even the query won't fit, the query is truncated too. Returns
    /// the (untruncated) query tokens plus the two budgets. Shared by
    /// `score_batch` and `score_sequential` so a pair can never be
    /// truncated one way in production and another in the oracle.
    fn split_budget(&self, query: &str) -> Result<(Vec<LlamaToken>, usize, usize)> {
        let query_tokens = self
            .model
            .str_to_token(query, AddBos::Never)
            .map_err(|e| Error::Inference(format!("Rerank query tokenise: {e}")))?;
        let fixed =
            self.prefix_tokens.len() + self.middle_tokens.len() + self.suffix_tokens.len() + 1;
        let dynamic_budget = self.max_input_tokens.saturating_sub(fixed);
        if dynamic_budget == 0 {
            return Err(Error::Inference(
                "Rerank context too small for prompt scaffold".to_string(),
            ));
        }
        const QUERY_HARD_CAP: usize = 256;
        let query_budget = query_tokens.len().min(QUERY_HARD_CAP).min(dynamic_budget);
        let doc_budget = dynamic_budget.saturating_sub(query_budget);
        Ok((query_tokens, query_budget, doc_budget))
    }

    /// Build one doc's tail tokens: `doc[..doc_budget] + suffix (+
    /// rerank token on the jina protocol)`. This is everything that
    /// rides AFTER the shared `prefix + query + middle` prefix. Shared
    /// by `score_batch` and `score_sequential`.
    fn build_tail(&self, doc: &str, doc_budget: usize) -> Result<Vec<LlamaToken>> {
        let doc_tokens = self
            .model
            .str_to_token(doc, AddBos::Never)
            .map_err(|e| Error::Inference(format!("Rerank doc tokenise: {e}")))?;
        let mut tail: Vec<LlamaToken> =
            Vec::with_capacity(doc_budget.min(doc_tokens.len()) + self.suffix_tokens.len() + 1);
        tail.extend(doc_tokens.iter().take(doc_budget).copied());
        tail.extend_from_slice(&self.suffix_tokens);
        if let RerankProtocol::JinaScoreToken {
            rerank_token_id, ..
        } = self.protocol
        {
            tail.push(LlamaToken(rerank_token_id));
        }
        Ok(tail)
    }

    /// Reference scorer: each (query, doc) pair is decoded as ONE fully
    /// independent sequence — whole-KV clear between pairs, scaffold +
    /// query re-decoded every time, no cross-pair state. Slower than
    /// [`score_batch`] (no shared-prefix reuse, no multi-sequence
    /// batching) but it depends on NONE of the KV-fanout / wave
    /// machinery, which makes it two things at once:
    ///   1. the ground-truth oracle the `rerank_batch_check` example
    ///      holds `score_batch` against — a correct batched decode of
    ///      independent sequences MUST reproduce these logits; and
    ///   2. a safe fallback mode for callers that would rather pay
    ///      latency than trust the batched path on an unproven backend.
    /// Uses the same `split_budget` / `build_tail` helpers as
    /// `score_batch`, so identical truncation is guaranteed.
    pub fn score_sequential(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        let mut guard = self
            .ctx
            .lock()
            .map_err(|e| Error::Inference(format!("rerank lock poisoned: {e}")))?;
        let ctx = &mut guard.ctx;

        let (query_tokens, query_budget, doc_budget) = self.split_budget(query)?;
        let mut head: Vec<LlamaToken> =
            Vec::with_capacity(self.prefix_tokens.len() + query_budget + self.middle_tokens.len());
        head.extend_from_slice(&self.prefix_tokens);
        head.extend(query_tokens.iter().take(query_budget).copied());
        head.extend_from_slice(&self.middle_tokens);

        let mut out = Vec::with_capacity(docs.len());
        for doc in docs {
            let tail = self.build_tail(doc, doc_budget)?;
            // Fully independent: nothing from the previous pair survives.
            ctx.clear_kv_cache();
            let total = head.len() + tail.len();
            let last = total - 1;
            let mut batch = LlamaBatch::new(total, 1);
            for (i, &tok) in head.iter().chain(tail.iter()).enumerate() {
                batch
                    .add(tok, i as i32, &[0], i == last)
                    .map_err(|e| Error::Inference(format!("Rerank seq batch add: {e}")))?;
            }
            ctx.decode(&mut batch)
                .map_err(|e| Error::Inference(format!("Rerank seq decode: {e}")))?;
            out.push(Self::read_score(
                self.protocol,
                ctx.get_logits_ith(last as i32),
            )?);
        }
        Ok(out)
    }

    /// Score multiple docs against one query with MULTI-SEQUENCE
    /// batching on top of shared-prefix KV reuse: the shared
    /// `scaffold + query + middle` prefix decodes ONCE on seq 0, is
    /// fanned out to doc sequences via `copy_kv_cache_seq` (a KV-cell
    /// tag update in the unified cache — no data copy), and every
    /// doc's tail rides ONE `decode` call per wave with per-doc
    /// logits harvested by batch index. Measured motivation
    /// (2026-07-17): per-DECODE-CALL overhead (~15-35ms Vulkan
    /// dispatch) dominated per-pair cost regardless of token count —
    /// prefix reuse alone left gate/prerank latency unchanged. Waves
    /// are packed under both the sequence cap and the KV token
    /// budget; a failed wave falls back to sequential scoring so a
    /// batching regression can never zero the gate.
    pub fn score_batch(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        // A/B valve + rollback hatch: the batched decode changes rerank
        // numerics at FP scale vs a fully independent decode (batched
        // GEMM ≠ single GEMM on a quantized model). `SOVEREIGN_RERANK_
        // SEQUENTIAL=1` routes to the machinery-free path so a scoreboard
        // A/B can isolate the batched decode's effect, and an operator
        // can fall back instantly if a backend ever misbehaves.
        if rerank_sequential_forced() {
            return self.score_sequential(query, docs);
        }
        let mut guard = self
            .ctx
            .lock()
            .map_err(|e| Error::Inference(format!("rerank lock poisoned: {e}")))?;
        let ctx = &mut guard.ctx;
        ctx.clear_kv_cache();

        // Truncation budget shared verbatim with `score_sequential`
        // (see `split_budget`), so the batched and reference scorers
        // can never truncate a pair differently.
        let (query_tokens, query_budget, doc_budget) = self.split_budget(query)?;

        // Decode the shared prefix once: scaffold + query + middle.
        // The last prefix token requests logits purely as decode
        // insurance (some backends reject zero-logit batches); the
        // value is never read.
        let mut shared: Vec<LlamaToken> =
            Vec::with_capacity(self.prefix_tokens.len() + query_budget + self.middle_tokens.len());
        shared.extend_from_slice(&self.prefix_tokens);
        shared.extend(query_tokens.iter().take(query_budget).copied());
        shared.extend_from_slice(&self.middle_tokens);
        let shared_len = shared.len();
        let mut batch = LlamaBatch::new(shared_len, 1);
        for (i, &tok) in shared.iter().enumerate() {
            batch
                .add(tok, i as i32, &[0], i + 1 == shared_len)
                .map_err(|e| Error::Inference(format!("Rerank prefix batch add: {e}")))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| Error::Inference(format!("Rerank prefix decode: {e}")))?;

        // Tokenise all tails up front (cheap, CPU-side).
        let mut tails: Vec<Vec<LlamaToken>> = Vec::with_capacity(docs.len());
        for doc in docs {
            tails.push(self.build_tail(doc, doc_budget)?);
        }

        // Fan the decoded prefix out to the doc sequences once (tag
        // update in the unified KV — cheap and idempotent).
        let doc_seqs = (RERANK_BATCH_SEQS as usize).saturating_sub(1).max(1);
        for s in 1..=doc_seqs as i32 {
            ctx.copy_kv_cache_seq(0, s, Some(0), Some(shared_len as u32))
                .map_err(|e| Error::Inference(format!("Rerank prefix fanout: {e}")))?;
        }

        // Wave packing: bounded by doc-sequence count AND the KV token
        // budget beyond the shared prefix.
        let wave_token_budget = (self.max_input_tokens - shared_len).saturating_sub(64);
        let mut out: Vec<f32> = Vec::with_capacity(docs.len());
        let mut di = 0usize;
        while di < tails.len() {
            let mut wave_end = di;
            let mut wave_tokens = 0usize;
            while wave_end < tails.len()
                && (wave_end - di) < doc_seqs
                && wave_tokens + tails[wave_end].len() <= wave_token_budget
            {
                wave_tokens += tails[wave_end].len();
                wave_end += 1;
            }
            if wave_end == di {
                // Single over-budget tail: run it alone (its length is
                // already capped by doc_budget ≤ the context).
                wave_end = di + 1;
                wave_tokens = tails[di].len();
            }
            let wave = &tails[di..wave_end];

            // Clear the doc sequences' PREVIOUS tails (positions past
            // the shared prefix); the prefix tags survive.
            for s in 1..=wave.len() as i32 {
                let _ = ctx.clear_kv_cache_seq(Some(s as u32), Some(shared_len as u32), None);
            }

            let mut batch = LlamaBatch::new(wave_tokens, 1);
            let mut last_indices: Vec<i32> = Vec::with_capacity(wave.len());
            let mut batch_pos = 0i32;
            for (w, tail) in wave.iter().enumerate() {
                let seq = (w + 1) as i32;
                let last = tail.len() - 1;
                for (i, &tok) in tail.iter().enumerate() {
                    batch
                        .add(tok, (shared_len + i) as i32, &[seq], i == last)
                        .map_err(|e| Error::Inference(format!("Rerank batch add: {e}")))?;
                    if i == last {
                        last_indices.push(batch_pos);
                    }
                    batch_pos += 1;
                }
            }
            match ctx.decode(&mut batch) {
                Ok(()) => {
                    for &idx in &last_indices {
                        let logits = ctx.get_logits_ith(idx);
                        out.push(Self::read_score(self.protocol, logits)?);
                    }
                }
                Err(e) => {
                    // Fallback: score this wave sequentially on seq 0
                    // (prefix intact) so one bad batch cannot zero the
                    // whole call.
                    tracing::warn!(
                        error = %e,
                        wave = wave.len(),
                        "rerank wave decode failed — sequential fallback"
                    );
                    for tail in wave {
                        let _ = ctx.clear_kv_cache_seq(Some(0), Some(shared_len as u32), None);
                        let last = tail.len() - 1;
                        let mut b = LlamaBatch::new(tail.len(), 1);
                        for (i, &tok) in tail.iter().enumerate() {
                            b.add(tok, (shared_len + i) as i32, &[0], i == last)
                                .map_err(|e| Error::Inference(format!("Rerank batch add: {e}")))?;
                        }
                        ctx.decode(&mut b)
                            .map_err(|e| Error::Inference(format!("Rerank decode: {e}")))?;
                        out.push(Self::read_score(
                            self.protocol,
                            ctx.get_logits_ith(last as i32),
                        )?);
                    }
                }
            }
            di = wave_end;
        }
        Ok(out)
    }
}

/// Doc sequences per wave + 1 for the shared prefix. 16 keeps KV
/// bookkeeping small while collapsing a 48-title prerank from 48
/// decode calls to 4.
const RERANK_BATCH_SEQS: u32 = 16;

/// `SOVEREIGN_RERANK_SEQUENTIAL=1` forces `score_batch` down the
/// fully-independent `score_sequential` path — the A/B valve and
/// rollback hatch for the batched decode's FP-scale numeric change.
fn rerank_sequential_forced() -> bool {
    matches!(
        std::env::var("SOVEREIGN_RERANK_SEQUENTIAL").ok().as_deref(),
        Some("1" | "true" | "on" | "yes")
    )
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
    // Windows backend is feature-selected (see sovereign-inference/Cargo.toml):
    // the matrix builds CPU / vulkan / cuda variants, and those features ARE
    // visible as rustc cfgs here, so the label can name the actual backend.
    #[cfg(all(target_os = "windows", feature = "windows-cuda"))]
    {
        "gpu+cuda"
    }
    #[cfg(all(
        target_os = "windows",
        feature = "windows-vulkan",
        not(feature = "windows-cuda")
    ))]
    {
        "gpu+vulkan"
    }
    #[cfg(all(
        target_os = "windows",
        not(feature = "windows-vulkan"),
        not(feature = "windows-cuda")
    ))]
    {
        "cpu"
    }
    // Linux: vulkan is selected at the workspace level and not re-exposed as a
    // cfg here, so we just say "gpu".
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        "gpu"
    }
}
