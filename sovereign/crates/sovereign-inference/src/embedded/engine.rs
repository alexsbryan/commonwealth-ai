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
/// Default sequence-batch parameters for the FastShort companion slot.
/// Total `n_ctx` divided across `n_seq_max` gives 2048 tokens per
/// sequence, which fits 97.4% of wiki-tier2-500 Phase 1b calls. Set
/// `SOVEREIGN_FAST_SHORT_DISABLE=1` to skip building this slot (saves
/// ~750MB KV cache on 2B Q6_K models — useful on low-memory hosts).
pub(crate) const FAST_SHORT_N_SEQ_MAX: u32 = 8;
pub(crate) const FAST_SHORT_N_CTX: u32 = 16_384;
pub(crate) const FAST_SHORT_N_UBATCH: u32 = 2_048;

/// Conservative chars→tokens estimate the slot picker uses before
/// tokenization is available. The FastShort per-sequence KV budget
/// is `FAST_SHORT_N_CTX / FAST_SHORT_N_SEQ_MAX = 2048` tokens; any
/// request whose tokenized prompt exceeds that overflows the slot
/// and fails with `NoKvCacheSlot` at decode time. 4 chars/token is
/// conservative for Llama-family BPE vocabularies (real ratio is
/// typically 3.5–4 for English, lower for code-dense prompts) so a
/// 1500-token budget maps to ~6000 chars. Headroom for output cap
/// (≤512 tokens) is the remaining 500 tokens of slot budget. A
/// prompt above this threshold falls through to the Fast slot
/// (n_seq_max=1, full n_ctx) where it has the whole window. See
/// `pick_slot` for the gate. Forensic 2026-05-19 trace surfaced
/// `prompt_chars=9272, max_tokens=16` (a small-output Fast call
/// with a large conversation-context prompt) routing here and
/// blowing up — title/scope classifiers see full conversation
/// context even when they emit ~16 tokens.
pub(crate) const FAST_SHORT_MAX_INPUT_CHARS: usize = 6_000;

/// Time the coalescer waits between accepting the first request and
/// firing the batched decode. Concurrent enrichment-phase callers
/// (Phase 1b/3/5/6 dispatched via `buffer_unordered` or `try_join_all`)
/// arrive within microseconds of each other; 5ms is generous enough
/// to fill an 8-seq batch in practice while staying inside the OICP
/// v0.3 §2.2 "fast = hundreds of ms" TTFT envelope. A solo caller
/// pays at most this 5ms latency tax — then decodes immediately
/// (proven zero-overhead at K=1 in `bench_decode_batch.rs`).
const FAST_SHORT_COALESCE_WINDOW_MS: u64 = 5;

// ─── FastShort coalescer ───────────────────────────────────────
//
// Continuous-batched dispatch in front of the FastShort `LlamaContext`
// (`n_seq_max=8`). Concurrent callers each enqueue a request via
// `complete()` and `await` a `oneshot::Receiver`; a long-lived
// background task drains the queue, coalesces up to `n_seq_max` jobs
// inside `FAST_SHORT_COALESCE_WINDOW_MS`, then dispatches one
// `generate_sync_batched` call. The bench (`bench_decode_batch.rs`)
// proves the per-call wall-clock at K=8 lands at ~46% of the K=1
// wall-clock for the 900-tok/128-tok shape — a 2.1× speedup which
// projects to ~15min savings per wiki-tier2-500 Phase 1b pass.

struct CoalescerJob {
    pub(crate) request: CompletionRequest,
    pub(crate) response: tokio::sync::oneshot::Sender<Result<CompletionResponse>>,
}

struct FastShortCoalescer {
    pub(crate) enqueue: tokio::sync::mpsc::UnboundedSender<CoalescerJob>,
}

impl FastShortCoalescer {
    /// Spawn the long-lived drainer task and return a handle that
    /// callers use to enqueue requests via `complete`. The slot is
    /// `Arc`-shared with the caller so the existing `Arc::strong_count`
    /// idle-eviction logic keeps working unchanged.
    fn spawn(slot: Arc<ModelSlot>, quirks: ModelQuirks, n_seq_max: usize) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<CoalescerJob>();
        tokio::spawn(async move {
            loop {
                let Some(first) = rx.recv().await else {
                    tracing::debug!(
                        slot = "fast_short",
                        "coalescer drainer exiting (channel closed)"
                    );
                    return;
                };
                let mut batch = vec![first];

                // Coalesce window: try to fill the batch within the
                // latency budget. A solo caller (no concurrent demand)
                // exits this loop immediately on the first timeout.
                let deadline = tokio::time::Instant::now()
                    + tokio::time::Duration::from_millis(FAST_SHORT_COALESCE_WINDOW_MS);
                while batch.len() < n_seq_max {
                    let remaining = deadline
                        .saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    match tokio::time::timeout(remaining, rx.recv()).await {
                        Ok(Some(job)) => batch.push(job),
                        Ok(None) => break, // channel closed mid-coalesce
                        Err(_) => break,   // timeout → fire what we have
                    }
                }

                Self::dispatch_batch(Arc::clone(&slot), quirks.clone(), batch).await;
            }
        });
        Self { enqueue: tx }
    }

    /// Acquire the slot's inflight permit, then run
    /// `generate_sync_batched` against the collected jobs on a
    /// blocking thread. Each job's `oneshot::Sender` resolves with
    /// its individual `(text, tokens)` slice.
    async fn dispatch_batch(
        slot: Arc<ModelSlot>,
        quirks: ModelQuirks,
        batch: Vec<CoalescerJob>,
    ) {
        let permit = match slot.inflight.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => {
                for job in batch {
                    let _ = job
                        .response
                        .send(Err(Error::Inference(
                            "FastShort slot permit closed".into(),
                        )));
                }
                return;
            }
        };
        let batch_size = batch.len();
        let join = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let start = Instant::now();
            slot.last_used
                .store(now_millis(), std::sync::atomic::Ordering::Relaxed);
            let mut ctx_lock = slot.context.blocking_lock();

            let model_id = slot.model_id.clone();
            let requests_refs: Vec<&CompletionRequest> =
                batch.iter().map(|j| &j.request).collect();

            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                ModelSlot::generate_sync_batched(
                    &slot.model,
                    &model_id,
                    ctx_lock.ctx_mut(),
                    &requests_refs,
                    &quirks,
                )
            }));
            drop(requests_refs);

            match result {
                Ok(Ok(per_seq_results)) => {
                    let latency_ms = start.elapsed().as_millis() as u64;
                    tracing::debug!(
                        slot = "fast_short",
                        batch_size,
                        latency_ms,
                        "FastShort batched inference complete"
                    );
                    for (job, (text, prompt_tokens, completion_tokens)) in
                        batch.into_iter().zip(per_seq_results)
                    {
                        let finish_reason =
                            finish_reason_from_counts(&job.request, completion_tokens);
                        let resp = CompletionResponse {
                            text,
                            tokens_used: prompt_tokens + completion_tokens,
                            prompt_tokens,
                            model_id: model_id.clone(),
                            latency_ms,
                            oicp_meta: None,
                            finish_reason: Some(finish_reason),
                            completion_tokens: Some(completion_tokens as u32),
                        };
                        let _ = job.response.send(Ok(resp));
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!(
                        slot = "fast_short",
                        error = %e,
                        batch_size,
                        "FastShort batched inference returned error — failing all batch members"
                    );
                    let msg = e.to_string();
                    for job in batch {
                        let _ = job
                            .response
                            .send(Err(Error::Inference(msg.clone())));
                    }
                }
                Err(_) => {
                    tracing::error!(
                        slot = "fast_short",
                        batch_size,
                        "FastShort batched inference panicked — likely context overflow"
                    );
                    for job in batch {
                        let _ = job.response.send(Err(Error::Inference(
                            "FastShort batched inference panicked — \
                             a batch member likely exceeded the per-seq context window"
                                .to_string(),
                        )));
                    }
                }
            }
        });
        if let Err(e) = join.await {
            tracing::error!(error = %e, "FastShort dispatch task join failed");
        }
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.enqueue
            .send(CoalescerJob {
                request,
                response: tx,
            })
            .map_err(|_| {
                Error::Inference("FastShort coalescer is shut down".into())
            })?;
        rx.await.map_err(|_| {
            Error::Inference("FastShort coalescer dropped response".into())
        })?
    }
}

pub struct EmbeddedLlamaCpp {
    #[allow(dead_code)]
    backend: Arc<LlamaBackend>,
    fast: Arc<ModelSlot>,
    /// Continuous-batched companion to `fast`, sharing the same loaded
    /// `LlamaModel` via `Arc` but standing up its own `LlamaContext`
    /// with `n_seq_max=8`. Short-call enrichment phases (Phase 1b
    /// coverage, Phase 3 cluster naming, Phase 5/6) route here when
    /// the request's OICP `max_output_tokens` is small enough to fit
    /// the FastShort claim's hard gate (per OICP-v0.3 §2.4).
    ///
    /// `None` when:
    ///   - `SOVEREIGN_FAST_SHORT_DISABLE=1` opts out (memory escape
    ///     hatch),
    ///   - the second `new_context` call failed (logged at warn-level;
    ///     the daemon stays operational by routing all callers to
    ///     `fast` as before).
    ///
    /// The coalescer routes solo-vs-batched dispatch (see
    /// `fast_short_coalescer`). Held as `Arc` so the inflight
    /// semaphore + KV cache stay shared across in-flight requests.
    fast_short: Option<Arc<ModelSlot>>,
    /// Continuous-batching dispatcher in front of `fast_short`. Receives
    /// `complete` requests, drains them in batches of up to
    /// `n_seq_max` jobs inside a 5ms coalesce window, and delivers each
    /// caller its own `CompletionResponse`. `None` whenever
    /// `fast_short` is `None` — the two fields are constructed and
    /// discarded as a unit.
    fast_short_coalescer: Option<Arc<FastShortCoalescer>>,
    /// The lazy chat slot. When `code_path` is set, this slot
    /// hot-swaps between the Main responder and the Code specialist
    /// based on the incoming request's `capability_hint`. Keeping
    /// one on-demand slot (instead of two held-warm slots) trades
    /// a ~5–30s reload on hint-switch for a smaller memory
    /// footprint on mid-range hardware — a fair default for solo
    /// users. Peers running a collective can dedicate a separate
    /// code node; the mesh scheduler will route `code`-hinted
    /// requests there before the local hot-swap kicks in.
    primary: Arc<Mutex<Option<ModelSlot>>>,
    primary_path: Option<PathBuf>,
    primary_ctx_size: u32,
    gpu_layers: u32,
    primary_backend: Arc<LlamaBackend>,
    last_primary_use: Arc<Mutex<Option<Instant>>>,
    /// PR-E2: Path whose GGUF currently occupies the lazy slot.
    /// `None` → slot unloaded. Used to decide whether an incoming
    /// request requires a hot-swap (different path than the one
    /// already loaded) vs. can reuse the resident model.
    primary_loaded_path: Arc<Mutex<Option<PathBuf>>>,
    /// PR-E2: Optional Code specialist GGUF. When set, a request
    /// whose OICP envelope carries `capability_hint = "code"` is
    /// served from this path (loaded into the lazy slot, swapping
    /// out the Main responder if needed). `None` = no code slot,
    /// all substantive work goes to the Main responder.
    code_path: Option<PathBuf>,
    /// Quirks to apply when the Code specialist is loaded.
    /// Different from `primary_quirks` because code models can
    /// have different chat templates / thinking control than
    /// general models (e.g., DeepSeek Coder vs Qwen3.5).
    code_quirks: ModelQuirks,
    embed_slot: Option<Arc<EmbedSlot>>,
    /// Optional cross-encoder reranker slot. Behind a Mutex<Option<…>>
    /// rather than `Option<Arc<…>>` so it can be installed lazily
    /// after the inference provider is constructed — the daemon
    /// reads its path from `models.toml` or
    /// `SOVEREIGN_RERANK_MODEL_PATH` and calls
    /// `install_rerank_slot()`. Inference reads via a brief lock,
    /// clones the inner Arc, and releases.
    rerank_slot: std::sync::Mutex<Option<Arc<RerankSlot>>>,
    hardware: HardwareProfile,
    /// Quirks for the fast slot — controls thinking injection and sampling defaults.
    fast_quirks: ModelQuirks,
    /// Quirks for the primary (thoughtful) slot.
    primary_quirks: ModelQuirks,

    /// Operator-declared additional chat slots. The map itself is
    /// behind an `RwLock` so the runtime `/internal/models/load`
    /// + `/internal/models/unload` endpoints can mutate the lineup
    /// without daemon restart. Inference (the hot path) takes a
    /// read lock, clones the `Arc<ModelSlot>` for the matched slot,
    /// and releases — so concurrent mutations don't block in-flight
    /// requests, and dropped slots stay alive until their last
    /// in-flight request finishes (RAII via `Arc`).
    extras: Arc<std::sync::RwLock<ExtrasState>>,
    /// Inflight gate for the lazy slot (Primary or Code). Single
    /// permit. The fast / extras slots have their own per-slot
    /// `inflight` semaphores on `ModelSlot`; the lazy path can't use
    /// those since the slot is `None` until first use, so the
    /// `EmbeddedLlamaCpp` owns a parallel semaphore that covers both
    /// hot-swap and generation. See `ModelSlot::inflight` for the
    /// rationale.
    lazy_inflight: Arc<tokio::sync::Semaphore>,
    /// Optional eager-loaded sibling pool for `SlotTarget::Primary`
    /// non-streaming completion. `None` (the default) leaves
    /// dispatch on the single-context lazy path — behaviour is
    /// byte-identical to pre-sibling builds. `Some(pool)` short-
    /// circuits the lazy path: each call picks a sibling round-
    /// robin and acquires that sibling's own per-slot `inflight`
    /// permit, so two callers can be in `generate_sync` at once.
    /// Built only when `SOVEREIGN_PRIMARY_SIBLINGS=N` (N≥2) is set
    /// at process start. See `primary_siblings_env` for the
    /// rationale and incompatibility with `code_path`.
    primary_pool: Option<Arc<PrimarySiblingPool>>,
}

/// Internal state for the operator-declared extras lineup. Held
/// behind a single RwLock in `EmbeddedLlamaCpp.extras` so all three
/// fields stay in sync — a partial mutation (slot loaded but
/// `by_model_id` not updated) would route requests to the wrong
/// place. Keeping them in one struct guarantees lock acquisition
/// covers the whole record.
struct ExtrasState {
    /// slot_name → loaded chat slot. Each slot is held resident
    /// until the operator unloads it OR the LRU policy evicts it.
    pub(crate) slots: HashMap<String, Arc<ModelSlot>>,
    /// Index from advertised model_id (gguf file stem) →
    /// `slots` key. Lookup is the hot path for
    /// `select_slot_for_request`, so we keep it pre-resolved.
    pub(crate) by_model_id: HashMap<String, String>,
    /// Per-slot quirks. Keys mirror `slots`. All entries today
    /// carry `ModelFamily::Unknown` defaults — runtime adapts to
    /// the gguf header. Reserved as its own field so a future
    /// op-flag could pin a specific family per slot.
    pub(crate) quirks: HashMap<String, ModelQuirks>,
    /// Optional total-memory budget in bytes for the extras
    /// lineup. When `Some(b)`, a `load_extra` that would push
    /// `(sum of size_bytes) + new_size` above `b` triggers LRU
    /// eviction of the coldest non-busy slots. `None` (default)
    /// disables eviction. Wired from
    /// `SetupConfig::ModelsSection.max_extras_memory_gb`.
    pub(crate) budget_bytes: Option<u64>,
}

impl ExtrasState {
    fn new() -> Self {
        Self {
            slots: HashMap::new(),
            by_model_id: HashMap::new(),
            quirks: HashMap::new(),
            budget_bytes: None,
        }
    }
}

/// Which slot to dispatch a request to. Computed from
/// `(request.preferred_speed, request.oicp.capability_hint,
/// request.model_id, configured slots)` — see
/// [`EmbeddedLlamaCpp::select_slot_for_request`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SlotTarget {
    Fast,
    /// Continuous-batched companion to the Fast slot. Selected for
    /// short-call enrichment phases (Phase 1b/3/5/6) and any other
    /// request whose `max_tokens` fits the FastShort claim's hard
    /// gate. Routed through `fast_short_coalescer` which collects
    /// concurrent arrivals into a single `generate_sync_batched`
    /// call against the n_seq_max=8 context.
    FastShort,
    Primary,
    Code,
    /// Operator-declared additional eagerly-loaded chat slot. The
    /// inner string is the key in `EmbeddedLlamaCpp.extras` —
    /// looked up via `extras_by_model_id` from
    /// `request.model_id`. See `pick_slot` for the precedence rules.
    Extra(String),
}

/// Pure slot-selection policy extracted from
/// [`EmbeddedLlamaCpp::select_slot_for_request`] so the rules can
/// be unit-tested without loading real GGUF weights. Inputs are
/// intentionally scalar: the request + "is the primary configured?"
/// + "is the code slot configured?" + an optional pre-resolved
/// extras key (the model_id → extras-slot lookup). The hot-swap
/// logic in `complete` / `complete_stream` depends on this decision
/// being deterministic for a fixed (request, slot-configuration)
/// pair.
///
/// Precedence (in order):
/// 1. **Extras match on `request.model_id`.** Operator-declared
///    routing wins over heuristic Speed/code-hint selection. This
///    is what makes per-phase model recruitment work end-to-end.
/// 2. Code-hint + Code slot configured.
/// 3. Speed-based: Fast → fast slot; Medium/Slow → primary if
///    configured else fast.
pub(crate) fn pick_slot(
    request: &CompletionRequest,
    has_primary: bool,
    has_code: bool,
    has_fast_short: bool,
    extras_match: Option<String>,
) -> SlotTarget {
    if let Some(name) = extras_match {
        return SlotTarget::Extra(name);
    }
    let wants_code = request
        .oicp
        .as_ref()
        .and_then(|o| o.capability_hint.as_ref())
        .map(|h| h.as_str() == sovereign_core::oicp::CapabilityHint::CODE)
        .unwrap_or(false);
    if wants_code && has_code {
        return SlotTarget::Code;
    }

    // FastShort routing: when the FastShort companion slot is built
    // and the request fits its envelope (small output budget, fast
    // latency, prompt fits the per-seq KV slice), route there. The
    // OICP scheduler at the daemon edge already does the analogous
    // selection between FastShort / FastLong claims; this is the
    // local-dispatch mirror so a sovereign daemon serving its own
    // request can also benefit without the round-trip through the
    // mesh router.
    //
    // Three gates, all required:
    //   1. `max_tokens ≤ 512` — matches the FastShort claim's
    //      `max_output` (see `synthesize_slot_claims`). Phase 1b/3/5/6
    //      composers set ≤512 (some at 256/1024; 1024 falls through to
    //      Fast, which is correct: those phases prefer per-call
    //      window over batching).
    //   2. `preferred_speed == Fast` — the slot exists to soak up
    //      Fast traffic concurrently; Medium/Slow want the Primary
    //      pool.
    //   3. `prompt_chars ≤ FAST_SHORT_MAX_INPUT_CHARS` — each
    //      FastShort sequence's KV slice is `n_ctx/n_seq_max = 2048`
    //      tokens, so a large prompt overflows even when the output
    //      cap is tiny. Title/scope/coarse classifiers issue Fast +
    //      `max_tokens=16` calls but see full conversation context
    //      in the prompt (~9k chars at session-restoration time).
    //      Without this gate they land on FastShort and decode fails
    //      with `NoKvCacheSlot` mid-prefill. Forensic 2026-05-19.
    let max_out = request.max_tokens.unwrap_or(usize::MAX);
    let prompt_chars = request.prompt.chars().count()
        + request
            .system_message
            .as_ref()
            .map_or(0, |s| s.chars().count());
    let fits_fast_short = max_out <= 512
        && request.preferred_speed == Speed::Fast
        && prompt_chars <= FAST_SHORT_MAX_INPUT_CHARS;
    if has_fast_short && fits_fast_short {
        return SlotTarget::FastShort;
    }

    match request.preferred_speed {
        Speed::Fast => SlotTarget::Fast,
        Speed::Medium | Speed::Slow => {
            if has_primary {
                SlotTarget::Primary
            } else {
                SlotTarget::Fast
            }
        }
    }
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
            None,
            context_size,
            gpu_layers,
            ModelFamily::Unknown,
            ModelFamily::Unknown,
            ModelFamily::Unknown,
            ModelFamily::Unknown,
        )
    }

    /// Load with separate fast, primary, embed, and optional code
    /// models, specifying the model family for each slot. The family
    /// drives thinking injection, sampling defaults, and
    /// embedding pooling/instruction behaviour.
    ///
    /// The `code` slot is a PR-E2 addition: when configured, code-
    /// hinted inference requests are routed to the Code specialist
    /// GGUF (via hot-swap into the shared lazy slot) instead of the
    /// Main responder. Passing `None` preserves the pre-E2 two-slot
    /// behaviour — all substantive work goes to the primary.
    #[allow(clippy::too_many_arguments)]
    pub fn load_full_with_families(
        fast_model_path: &Path,
        primary_model_path: Option<&Path>,
        embed_model_path: Option<&Path>,
        code_model_path: Option<&Path>,
        context_size: u32,
        gpu_layers: Option<u32>,
        fast_family: ModelFamily,
        primary_family: ModelFamily,
        embed_family: ModelFamily,
        code_family: ModelFamily,
    ) -> Result<Self> {
        let hardware = HardwareProfile::detect();
        let n_gpu_layers = gpu_layers.unwrap_or(hardware.recommended_gpu_layers);

        let fast_quirks = fast_family.default_quirks();
        let primary_quirks = primary_family.default_quirks();
        let code_quirks = code_family.default_quirks();
        let embed_quirks = embed_family.default_quirks().embed;

        let mut backend = LlamaBackend::init()
            .map_err(|e| Error::Inference(format!("Failed to init llama backend: {e}")))?;
        // Suppress llama.cpp/ggml C-level stderr noise by default. Set
        // SOVEREIGN_LLAMA_LOGS=1 when diagnosing a model-load failure
        // (e.g. an unsupported quant or arch returning a "null result")
        // so the underlying libllama error reaches daemon.err.
        if std::env::var("SOVEREIGN_LLAMA_LOGS").ok().as_deref() != Some("1") {
            backend.void_logs();
        }
        let backend = Arc::new(backend);

        tracing::info!(slot = "fast", family = ?fast_family, "loading slot");
        let fast = Arc::new(ModelSlot::load(
            &backend,
            fast_model_path,
            context_size,
            n_gpu_layers,
        )?);
        tracing::info!(slot = "fast", family = ?fast_family, "slot loaded");

        // FastShort companion: a second LlamaContext on the same Fast
        // model with n_seq_max=8 for continuous-batched short calls.
        // Sharing weights via Arc<LlamaModel> means the only marginal
        // cost is ~750MB KV cache (Strix Halo unified memory makes
        // this a non-issue; tighter hosts can opt out). Failure is
        // logged but non-fatal — the daemon still serves all callers
        // through `fast` as before.
        //
        // **Recurrent-arch gate (2026-05-24).** Models whose gguf
        // architecture is recurrent (qwen*moe / mamba / deltanet /
        // rwkv / ssm) need `with_n_rs_seq(>= n_seq_max)` on every
        // context that issues a batched decode — the recurrent
        // layers carry per-sequence state and a low `n_rs_seq`
        // makes `ctx.decode(batch)` fail with `Decode Error -3:
        // unknown` the first time a continuous-batched call lands.
        // FastShort's `from_existing_model` constructor does not
        // wire `n_rs_seq` (only the lazy primary slot's
        // `ModelSlot::load` does), so we'd build a ctx that
        // crashes on its first real request. Symptom observed on
        // Qwen3.6-35B-A3B-Claude-4.7-Opus-Reasoning-Distilled-APEX-I-
        // Compact (`qwen3moe` arch) in inner-work first turn,
        // 2026-05-24. Refuse to construct FastShort for recurrent
        // models — all callers route through `fast` (n_seq_max=1)
        // and the speedup is forfeited rather than the daemon
        // crashing the first time the model is touched.
        //
        // The right long-term fix is propagating `n_rs_seq` into
        // `from_existing_model` so FastShort can serve recurrent
        // models too; deferred until there's a bench fixture that
        // covers this combo. Track at [[invariant_fast_short_recurrent_arch]].
        let fast_short_disabled = std::env::var("SOVEREIGN_FAST_SHORT_DISABLE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let fast_arch = read_gguf_arch(&fast.model);
        // FastShort's `from_existing_model` cannot propagate `n_rs_seq`,
        // so its ctx crashes (Decode Error -3) on any model whose decode
        // path provisions recurrent-state seq slots: a genuinely
        // recurrent arch, OR an MTP model (the speculative draft/verify
        // needs n_rs_seq — same gate as `is_mtp_model` at slot load,
        // which is `mtp_by_name || mtp_by_arch`). The prior check only
        // covered the recurrent-arch half, so an MTP-by-NAME model whose
        // arch isn't classified recurrent (e.g. Qwopus*-MTP on a qwen3
        // arch) slipped through and crashed every continuous-batched
        // call. Detect MTP by name here too; a `SOVEREIGN_MTP_DISABLE`d
        // model runs single-token, so FastShort is safe for it. Skip for
        // both unsafe cases — callers route to `fast` (n_seq_max=1),
        // forfeiting the speedup rather than crashing.
        // [[invariant_fast_short_recurrent_arch]]
        let mtp_disabled_at_load = std::env::var("SOVEREIGN_MTP_DISABLE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let fast_short_unsafe = is_recurrent_arch(&fast_arch)
            || (fast.model_id.to_lowercase().contains("mtp") && !mtp_disabled_at_load);
        let (fast_short, fast_short_coalescer) = if fast_short_disabled {
            tracing::info!(
                slot = "fast_short",
                "skipped (SOVEREIGN_FAST_SHORT_DISABLE=1)"
            );
            (None, None)
        } else if fast_short_unsafe {
            tracing::warn!(
                slot = "fast_short",
                arch = %fast_arch,
                model_id = %fast.model_id,
                "skipped — fast slot model is MTP or recurrent; FastShort \
                 ctx would crash on first decode (Decode Error -3: n_rs_seq \
                 not propagated through from_existing_model). All callers \
                 route to `fast`."
            );
            (None, None)
        } else {
            match ModelSlot::from_existing_model(
                &backend,
                Arc::clone(&fast.model),
                fast.model_id.clone(),
                fast.size_bytes,
                FAST_SHORT_N_CTX,
                FAST_SHORT_N_SEQ_MAX,
                FAST_SHORT_N_UBATCH,
            ) {
                Ok(slot) => {
                    let slot = Arc::new(slot);
                    let coalescer = Arc::new(FastShortCoalescer::spawn(
                        Arc::clone(&slot),
                        fast_quirks.clone(),
                        FAST_SHORT_N_SEQ_MAX as usize,
                    ));
                    (Some(slot), Some(coalescer))
                }
                Err(e) => {
                    tracing::warn!(
                        slot = "fast_short",
                        error = %e,
                        "failed to build continuous-batched companion; \
                         all callers will route to `fast` as before"
                    );
                    (None, None)
                }
            }
        };

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
                // Embedding offload mirrors the chat slot: pass the
                // hardware-derived n_gpu_layers and let `EmbedSlot::load`
                // try-GPU-then-fallback. The old comment here said
                // "embedding workloads are not compute-bound enough to
                // benefit from GPU offloading" — measured to be wrong
                // on Strix Halo Vulkan (ingest was stuck at ~4 chunks/s
                // CPU-only; GPU embed is ~10× faster) and on Apple
                // Silicon Metal (33 vs 2.8 seq/sec, 12×). The Metal
                // GGML_ASSERT(buf_src) crash referenced in the old
                // comment was fixed in llama-cpp-2 0.1.141.
                match EmbedSlot::load(&backend, path, n_gpu_layers, embed_quirks) {
                    Ok(slot) => {
                        tracing::info!(slot = "embed", "slot loaded");
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

        if let Some(p) = code_model_path {
            tracing::info!(
                slot = "code",
                path = %p.display(),
                family = ?code_family,
                "code specialist configured — will load on first code-hinted request"
            );
        }

        // Optional primary-slot sibling pool — see
        // `primary_siblings_env` for the rationale. Build at most
        // once, eagerly, before assembling the struct so any failure
        // surfaces during daemon startup rather than on first
        // chat-completion.
        let primary_pool = if let Some(n) = primary_siblings_env() {
            let n = n.get() as usize;
            // Guard rails: siblings only make sense with a real
            // primary GGUF, and they don't compose with the Code
            // specialist's hot-swap (which expects to mutate one
            // resident model).
            let primary_path = primary_model_path.ok_or_else(|| {
                Error::Inference(
                    "SOVEREIGN_PRIMARY_SIBLINGS set but no primary model configured \
                     — siblings pool requires a primary GGUF to share weights from."
                        .into(),
                )
            })?;
            if code_model_path.is_some() {
                return Err(Error::Inference(
                    "SOVEREIGN_PRIMARY_SIBLINGS is incompatible with a configured \
                     code specialist (the code slot relies on hot-swapping the lazy \
                     primary slot, which a sibling pool can't satisfy without \
                     swapping every sibling at once). Unset one of the two."
                        .into(),
                ));
            }
            tracing::info!(
                slot = "primary",
                siblings = n,
                path = %primary_path.display(),
                "building primary sibling pool — loading weights once + N contexts"
            );
            let primary_0 = ModelSlot::load(
                &backend,
                primary_path,
                context_size,
                n_gpu_layers,
            )?;
            let shared_model = Arc::clone(&primary_0.model);
            let shared_id = primary_0.model_id.clone();
            let shared_size = primary_0.size_bytes;
            let mut slots: Vec<Arc<ModelSlot>> = Vec::with_capacity(n);
            slots.push(Arc::new(primary_0));
            for i in 1..n {
                let sibling = ModelSlot::from_existing_model(
                    &backend,
                    Arc::clone(&shared_model),
                    shared_id.clone(),
                    shared_size,
                    context_size,
                    /* n_seq_max */ 1,
                    /* n_ubatch */ 512,
                )?;
                tracing::info!(
                    slot = "primary",
                    sibling_idx = i,
                    "primary sibling context ready"
                );
                slots.push(Arc::new(sibling));
            }
            Some(Arc::new(PrimarySiblingPool {
                slots,
                next: std::sync::atomic::AtomicUsize::new(0),
            }))
        } else {
            None
        };

        // ─── Primary-slot alias when fast_path == primary_path ───────
        // sovereign-core's ModelsSection::fast_path() returns the
        // primary GGUF when no distinct fast is configured. That
        // means the loader receives the same `&Path` for both
        // arguments, the fast slot is eagerly loaded above, and a
        // naive lazy-primary path would re-load the same weights
        // into VRAM on the first /v1/chat/completions to primary —
        // doubling the model's resident footprint and breaking the
        // VRAM-tight pods that asked for the subsume in the first
        // place.
        //
        // When the alias is detected and a primary-sibling pool is
        // NOT configured (the pool builds its own primary slots
        // from primary_path and would conflict with this
        // pre-population), build a primary ModelSlot from the
        // already-loaded fast model via `from_existing_model`. That
        // call shares the `Arc<LlamaModel>` (one VRAM weights
        // allocation across both slots) and allocates only a fresh
        // context — a separate KV cache so primary and fast don't
        // contend on the same llama_decode state.
        let primary_is_alias = matches!(
            primary_model_path,
            Some(p) if p == fast_model_path
        ) && primary_pool.is_none();
        let (primary_preloaded, primary_preloaded_path): (Option<ModelSlot>, Option<PathBuf>) =
            if primary_is_alias {
                let primary_p = primary_model_path.unwrap();
                tracing::info!(
                    slot = "primary",
                    path = %primary_p.display(),
                    "fast and primary share a GGUF; aliasing primary slot to fast's loaded weights (one VRAM copy, separate KV)"
                );
                match ModelSlot::from_existing_model(
                    &backend,
                    Arc::clone(&fast.model),
                    fast.model_id.clone(),
                    fast.size_bytes,
                    context_size,
                    /* n_seq_max */ 1,
                    /* n_ubatch */ 512,
                ) {
                    Ok(slot) => (Some(slot), Some(primary_p.to_path_buf())),
                    Err(e) => {
                        // Fall back to lazy-load. Logged so operators
                        // notice the missed optimisation, but the
                        // daemon still works — just with the extra
                        // VRAM cost on first primary use.
                        tracing::warn!(
                            slot = "primary",
                            error = %e,
                            "fast/primary alias setup failed; primary will lazy-load and double VRAM"
                        );
                        (None, None)
                    }
                }
            } else {
                (None, None)
            };

        Ok(Self {
            backend: Arc::clone(&backend),
            fast,
            fast_short,
            fast_short_coalescer,
            primary: Arc::new(Mutex::new(primary_preloaded)),
            primary_path: primary_model_path.map(|p| p.to_path_buf()),
            primary_ctx_size: context_size,
            gpu_layers: n_gpu_layers,
            primary_backend: backend,
            last_primary_use: Arc::new(Mutex::new(None)),
            primary_loaded_path: Arc::new(Mutex::new(primary_preloaded_path)),
            code_path: code_model_path.map(|p| p.to_path_buf()),
            code_quirks,
            embed_slot,
            rerank_slot: std::sync::Mutex::new(None),
            hardware,
            fast_quirks,
            primary_quirks,
            extras: Arc::new(std::sync::RwLock::new(ExtrasState::new())),
            lazy_inflight: Arc::new(tokio::sync::Semaphore::new(1)),
            primary_pool,
        })
    }

    /// Install operator-declared additional chat slots after
    /// construction. Each `(slot_name, gguf_path)` entry is
    /// eagerly loaded and held resident until the operator
    /// unloads it (via `unload_extra` or by replacing the lineup
    /// through another `install_extras` call).
    ///
    /// The advertised `model_id` (gguf file stem) is the routing
    /// key — when an inbound request carries `model_id` matching
    /// one of these stems, the slot picker returns
    /// [`SlotTarget::Extra`] before falling through to
    /// Speed-based routing.
    ///
    /// Loading is sequential (each call to `ModelSlot::load` opens
    /// the gguf and uploads weights). On success the slot becomes
    /// part of the routing table; on failure the slot is skipped
    /// with a `warn!` log and the remaining slots still load. This
    /// matches the embed-slot policy — partial slot inventories are
    /// preferable to a hard fail at boot, which would prevent the
    /// daemon from serving _any_ requests.
    ///
    /// **Concurrency**: holds a write lock on `extras` for the full
    /// load duration. In-flight requests on existing extras slots
    /// are unaffected — they hold a clone of the `Arc<ModelSlot>`
    /// and don't need the outer lock. New requests during the load
    /// briefly block on the read lock acquisition, then proceed
    /// against the post-load lineup.
    ///
    /// Re-calling this method **replaces** the previous extras
    /// lineup. Slots dropped from the new lineup release their
    /// `Arc<ModelSlot>` once their last in-flight request
    /// completes (RAII). Used by the daemon at startup with the
    /// `[models.extra]` config table.
    pub fn install_extras(
        &self,
        extras: BTreeMap<String, PathBuf>,
        context_size: u32,
    ) -> Result<()> {
        // Preserve any operator-set budget across reloads. The
        // budget is set via `set_extras_memory_budget` and lives
        // alongside the slot map; reinstalling slots shouldn't
        // implicitly reset eviction behaviour.
        let preserved_budget = self
            .extras
            .read()
            .map(|g| g.budget_bytes)
            .unwrap_or(None);
        let default_quirks = ModelFamily::Unknown.default_quirks();
        let mut state = ExtrasState::new();
        state.budget_bytes = preserved_budget;
        for (slot_name, path) in extras {
            tracing::info!(
                slot = %slot_name,
                path = %path.display(),
                "loading extras slot"
            );
            match ModelSlot::load(
                &self.primary_backend,
                &path,
                context_size,
                self.gpu_layers,
            ) {
                Ok(slot) => {
                    let model_id = slot.model_id.clone();
                    let arc = Arc::new(slot);
                    state.slots.insert(slot_name.clone(), Arc::clone(&arc));
                    state.by_model_id.insert(model_id.clone(), slot_name.clone());
                    state.quirks.insert(slot_name.clone(), default_quirks.clone());
                    tracing::info!(
                        slot = %slot_name,
                        model_id = %model_id,
                        "extras slot loaded"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        slot = %slot_name,
                        path = %path.display(),
                        error = %e,
                        "extras slot load failed — skipping; remaining extras still load"
                    );
                }
            }
        }
        let mut guard = self
            .extras
            .write()
            .map_err(|e| Error::Inference(format!("extras lock poisoned: {e}")))?;
        *guard = state;
        Ok(())
    }

    /// Install (or replace) the cross-encoder reranker slot at
    /// runtime. Loads the GGUF eagerly so the first rerank call
    /// doesn't pay the model-load latency. Family defaults to
    /// `ModelFamily::Reranker` quirks (8192 ctx, max_batch 50);
    /// pass `Some(_)` to override per-deployment.
    ///
    /// Returns the model_id (file stem) of the installed reranker,
    /// matching the pattern other slot installers use.
    ///
    /// Replacing an in-use reranker is safe: the old `Arc<RerankSlot>`
    /// drops when the last in-flight rerank call finishes (RAII via
    /// Arc cloning in the `rerank_batch` hot path).
    pub fn install_rerank_slot(
        &self,
        path: PathBuf,
        family: ModelFamily,
    ) -> Result<String> {
        let rerank_quirks = family.default_quirks().rerank;
        tracing::info!(
            slot = "rerank",
            path = %path.display(),
            family = ?family,
            "installing rerank slot"
        );
        let slot = RerankSlot::load(
            &self.primary_backend,
            &path,
            self.gpu_layers,
            rerank_quirks,
        )?;
        let model_id = slot.model_id.clone();
        let arc = Arc::new(slot);
        let mut guard = self
            .rerank_slot
            .lock()
            .map_err(|e| Error::Inference(format!("rerank slot lock poisoned: {e}")))?;
        *guard = Some(arc);
        tracing::info!(slot = "rerank", model_id = %model_id, "rerank slot installed");
        Ok(model_id)
    }

    /// Returns the model_id of the currently installed reranker, if any.
    pub fn rerank_model_id(&self) -> Option<String> {
        self.rerank_slot
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| s.model_id.clone()))
    }

    /// Add (or replace) a single extras slot at runtime. Returns
    /// the `model_id` (gguf file stem) advertised for this slot,
    /// which is what callers send in `request.model` / `model_id`
    /// to land here.
    ///
    /// If a slot with `slot_name` already exists, its
    /// `Arc<ModelSlot>` is replaced and the `model_id → slot_name`
    /// index updated. The previous slot's Arc drops once the last
    /// in-flight request finishes; if no requests are in flight,
    /// the model unloads immediately. Use [`unload_extra`] to drop
    /// without replacement.
    ///
    /// LRU eviction is **not** applied — this entry point assumes
    /// the caller has already accounted for memory. For
    /// budget-aware loading, use [`load_extra_with_budget`].
    pub fn load_extra(
        &self,
        slot_name: String,
        path: PathBuf,
        context_size: u32,
    ) -> Result<String> {
        self.load_extra_with_budget(slot_name, path, context_size, None)
    }

    /// Budget-aware variant of [`load_extra`]. When `budget_bytes`
    /// is `Some(B)`, this method first peeks at the new gguf's
    /// on-disk size; if `(sum of currently-loaded extras' sizes) +
    /// (new size) > B`, it evicts cold (least-recently-used)
    /// slots in ascending `last_used` order until the new slot
    /// fits. Slots currently serving a request — detected via
    /// `Arc::strong_count` > 1 — are skipped to avoid yanking the
    /// rug from under in-flight inference. If after walking every
    /// non-busy slot the new load still doesn't fit, the call
    /// returns an error with the budget shortfall.
    ///
    /// `None` skips eviction entirely (matches
    /// [`load_extra`] historical behaviour).
    pub fn load_extra_with_budget(
        &self,
        slot_name: String,
        path: PathBuf,
        context_size: u32,
        budget_bytes: Option<u64>,
    ) -> Result<String> {
        tracing::info!(
            slot = %slot_name,
            path = %path.display(),
            "load_extra: preparing slot"
        );

        // The pre-load eviction check uses the gguf file size as an
        // estimate of the upcoming slot's footprint. After load, the
        // ModelSlot stores `LlamaModel::size()` instead — usually
        // very close, but the file-size estimate is what's available
        // BEFORE we've called `ModelSlot::load`, and that's the
        // gating moment for "should we make room first?". Pre-load
        // and post-load can diverge by a few percent; the budget is
        // a soft cap by design.
        let new_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

        // LRU eviction pass under the write lock. Done BEFORE
        // ModelSlot::load so we don't briefly OOM by holding the
        // old + new slot's weights simultaneously. Eviction drops
        // the slot from the map; the actual model unload happens
        // when the last `Arc<ModelSlot>` reference releases (in-
        // flight requests still hold theirs and finish naturally).
        if let Some(budget) = budget_bytes {
            self.evict_extras_for_new_load(&slot_name, new_size, budget)?;
        }

        let slot = ModelSlot::load(
            &self.primary_backend,
            &path,
            context_size,
            self.gpu_layers,
        )?;
        let model_id = slot.model_id.clone();
        let arc = Arc::new(slot);

        let mut guard = self
            .extras
            .write()
            .map_err(|e| Error::Inference(format!("extras lock poisoned: {e}")))?;
        // If this slot_name was previously bound to a different
        // model_id, drop the stale `by_model_id` entry first to
        // keep the index injective.
        if let Some(prev_arc) = guard.slots.remove(&slot_name) {
            let prev_model_id = &prev_arc.model_id;
            guard.by_model_id.remove(prev_model_id);
        }
        guard.slots.insert(slot_name.clone(), Arc::clone(&arc));
        guard.by_model_id.insert(model_id.clone(), slot_name.clone());
        guard
            .quirks
            .insert(slot_name.clone(), ModelFamily::Unknown.default_quirks());
        tracing::info!(slot = %slot_name, model_id = %model_id, "load_extra: slot loaded");
        Ok(model_id)
    }

    /// Walk the extras lineup in ascending `last_used` order,
    /// dropping cold slots until `(remaining sum) + new_size <=
    /// budget`. Skips any slot whose `Arc<ModelSlot>` strong count
    /// exceeds 1 (in-flight request) and any slot that matches
    /// `incoming_slot_name` (we'll replace it shortly anyway).
    /// Returns `Err` when not enough cold capacity is available
    /// to make room — caller surfaces the message verbatim.
    ///
    /// The pure selection step lives in [`pick_evictions`] for
    /// unit testability; this method handles the impure parts
    /// (snapshot under lock, mutate state, log evictions).
    fn evict_extras_for_new_load(
        &self,
        incoming_slot_name: &str,
        new_size: u64,
        budget_bytes: u64,
    ) -> Result<()> {
        let mut guard = self
            .extras
            .write()
            .map_err(|e| Error::Inference(format!("extras lock poisoned: {e}")))?;
        // Snapshot non-busy candidates and the current footprint
        // (excluding any slot we're about to replace) into pure
        // values so the selection step can be unit-tested.
        let current: u64 = guard
            .slots
            .iter()
            .filter(|(name, _)| name.as_str() != incoming_slot_name)
            .map(|(_, slot)| slot.size_bytes)
            .sum();
        let candidates: Vec<EvictionCandidate> = guard
            .slots
            .iter()
            .filter(|(name, _)| name.as_str() != incoming_slot_name)
            .filter_map(|(name, arc)| {
                if Arc::strong_count(arc) > 1 {
                    None // in-flight; preserve
                } else {
                    Some(EvictionCandidate {
                        slot_name: name.clone(),
                        last_used_ms: arc
                            .last_used
                            .load(std::sync::atomic::Ordering::Relaxed),
                        size_bytes: arc.size_bytes,
                    })
                }
            })
            .collect();

        let plan = pick_evictions(&candidates, current, new_size, budget_bytes);
        match plan {
            EvictionPlan::Fits => Ok(()),
            EvictionPlan::Evict(slot_names) => {
                let mut evicted = Vec::with_capacity(slot_names.len());
                for slot_name in slot_names {
                    if let Some(arc) = guard.slots.remove(&slot_name) {
                        let model_id = arc.model_id.clone();
                        guard.by_model_id.remove(&model_id);
                        guard.quirks.remove(&slot_name);
                        evicted.push((slot_name, model_id));
                    }
                }
                drop(guard);
                for (slot_name, model_id) in evicted {
                    tracing::info!(
                        slot = %slot_name,
                        model_id = %model_id,
                        reason = "lru_eviction",
                        "extras slot dropped to free memory for incoming load"
                    );
                }
                Ok(())
            }
            EvictionPlan::Insufficient {
                need_to_free,
                cold_total,
            } => {
                tracing::warn!(
                    budget_gb = budget_bytes as f64 / (1u64 << 30) as f64,
                    cold_total_mb = cold_total / (1024 * 1024),
                    need_to_free_mb = need_to_free / (1024 * 1024),
                    "load_extra rejected: insufficient cold capacity"
                );
                Err(Error::Inference(format!(
                    "load_extra rejected: cannot free enough memory under \
                     max_extras_memory_gb budget. Cold (non-busy) extras \
                     total {} MB but need to free {} MB. Either raise the \
                     budget, unload an in-flight slot manually, or wait \
                     for current inference to drain.",
                    cold_total / (1024 * 1024),
                    need_to_free / (1024 * 1024),
                )))
            }
        }
    }
}

/// Eviction-candidate snapshot. Pulled out of the live RwLock so
/// the selection algorithm in [`pick_evictions`] is a pure
/// function over scalar inputs — testable without loading any
/// real GGUF.
#[derive(Debug, Clone)]
pub(crate) struct EvictionCandidate {
    pub(crate) slot_name: String,
    pub(crate) last_used_ms: u64,
    pub(crate) size_bytes: u64,
}

/// Outcome of [`pick_evictions`]. Three states map to three
/// caller actions: nothing to do, evict these names in order, or
/// fail loud because the cold lineup can't free enough.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvictionPlan {
    /// Already within budget — caller proceeds without mutating.
    Fits,
    /// Drop these slot names in order. The slice is sorted by
    /// ascending `last_used` so the coldest slots go first.
    Evict(Vec<String>),
    /// Cold capacity isn't enough. Caller surfaces an error to
    /// the operator; busy slots are preserved.
    Insufficient {
        /// How many bytes the caller still needs to free after
        /// evicting every cold candidate.
        need_to_free: u64,
        /// Sum of `size_bytes` across the cold candidates. Useful
        /// for the operator-facing error message.
        cold_total: u64,
    },
}

/// Pure LRU selection. Given a list of non-busy candidates plus
/// the current footprint and incoming load size, returns which
/// slots to evict so the new load fits under `budget_bytes`.
/// Sorts candidates ascending by `last_used_ms` and consumes them
/// until `freed >= need_to_free`.
///
/// Keeping this pure (no `&self`, no lock) means the routing-
/// regression suite can lock-step LRU behaviour without depending
/// on a real llama.cpp load.
pub(crate) fn pick_evictions(
    candidates: &[EvictionCandidate],
    current_total: u64,
    incoming_size: u64,
    budget_bytes: u64,
) -> EvictionPlan {
    if current_total.saturating_add(incoming_size) <= budget_bytes {
        return EvictionPlan::Fits;
    }
    let need_to_free = current_total
        .saturating_add(incoming_size)
        .saturating_sub(budget_bytes);
    let mut sorted: Vec<&EvictionCandidate> = candidates.iter().collect();
    sorted.sort_by_key(|c| c.last_used_ms);
    let mut freed: u64 = 0;
    let mut to_evict: Vec<String> = Vec::new();
    for c in &sorted {
        if freed >= need_to_free {
            break;
        }
        freed = freed.saturating_add(c.size_bytes);
        to_evict.push(c.slot_name.clone());
    }
    if freed >= need_to_free {
        EvictionPlan::Evict(to_evict)
    } else {
        EvictionPlan::Insufficient {
            need_to_free,
            cold_total: sorted.iter().map(|c| c.size_bytes).sum(),
        }
    }
}

// Reopen the impl block so subsequent methods on EmbeddedLlamaCpp
// keep their existing namespace.
impl EmbeddedLlamaCpp {

    /// Drop an extras slot. Returns the `model_id` that was bound
    /// to that slot, or `None` if the slot wasn't loaded. The slot's
    /// `Arc<ModelSlot>` releases once any in-flight request on it
    /// completes — concurrent inference is not interrupted.
    pub fn unload_extra(&self, slot_name: &str) -> Result<Option<String>> {
        let mut guard = self
            .extras
            .write()
            .map_err(|e| Error::Inference(format!("extras lock poisoned: {e}")))?;
        let Some(arc) = guard.slots.remove(slot_name) else {
            return Ok(None);
        };
        let model_id = arc.model_id.clone();
        guard.by_model_id.remove(&model_id);
        guard.quirks.remove(slot_name);
        drop(guard);
        tracing::info!(slot = %slot_name, model_id = %model_id, "unload_extra: slot dropped");
        Ok(Some(model_id))
    }

    /// Install (or clear) the extras memory budget. `Some(bytes)`
    /// turns LRU eviction on; subsequent `load_extra` calls evict
    /// cold slots when adding the new slot would exceed the budget.
    /// `None` disables eviction (the historical default — slots are
    /// held until unloaded).
    ///
    /// The daemon calls this once at startup with
    /// `cfg.models.max_extras_memory_bytes()`. Future runtime
    /// adjustments (operator wants to tighten the budget without
    /// restart) re-call this entry point.
    pub fn set_extras_memory_budget(&self, budget_bytes: Option<u64>) -> Result<()> {
        let mut guard = self
            .extras
            .write()
            .map_err(|e| Error::Inference(format!("extras lock poisoned: {e}")))?;
        guard.budget_bytes = budget_bytes;
        match budget_bytes {
            Some(b) => tracing::info!(
                budget_gb = b as f64 / (1u64 << 30) as f64,
                "extras memory budget set; LRU eviction active"
            ),
            None => tracing::info!("extras memory budget cleared; eviction disabled"),
        }
        Ok(())
    }

    /// Read the currently-configured extras memory budget. `None`
    /// means eviction is disabled.
    pub fn extras_memory_budget(&self) -> Option<u64> {
        self.extras.read().ok().and_then(|g| g.budget_bytes)
    }

    /// Inspect the currently-loaded extras lineup. Returns
    /// `(slot_name, model_id)` pairs in deterministic
    /// (slot_name-sorted) order. Used by `/v1/models` registration
    /// + the `/internal/models/inventory` endpoint.
    pub fn extras_inventory(&self) -> Vec<(String, String)> {
        let guard = match self.extras.read() {
            Ok(g) => g,
            Err(e) => {
                tracing::error!(error = %e, "extras lock poisoned during inventory read");
                return Vec::new();
            }
        };
        let mut out: Vec<(String, String)> = guard
            .by_model_id
            .iter()
            .map(|(model_id, slot_name)| (slot_name.clone(), model_id.clone()))
            .collect();
        out.sort();
        out
    }

    /// True when a Code specialist GGUF has been configured.
    /// Exposed on the concrete type (tests + direct constructors);
    /// trait-level consumers use `InferenceProvider::code_model_id()`.
    pub fn has_code_slot(&self) -> bool {
        self.code_path.is_some()
    }

    /// Pick which slot should serve this request.
    ///
    /// Rules:
    /// - `request.model_id` matches an extras slot → `Extra(name)`.
    ///   Operator-declared routing wins over heuristics.
    /// - `code` hint + code slot configured → `Code` (hot-swaps
    ///   the lazy slot to load the code GGUF).
    /// - `Speed::Fast` → `Fast` (always loaded).
    /// - Everything else → `Primary` if a primary GGUF is
    ///   configured, otherwise `Fast` (degraded fallback).
    fn select_slot_for_request(&self, request: &CompletionRequest) -> SlotTarget {
        let extras_match = request.model_id.as_deref().and_then(|mid| {
            // Hot-path read: clone the matched slot_name out of the
            // index and release the lock immediately. Concurrent
            // load/unload mutators block briefly but inference dispatch
            // never holds the lock across the actual generate_sync.
            let guard = self.extras.read().ok()?;
            guard.by_model_id.get(mid).cloned()
        });

        // Named-slot match. The fast / primary / code slots are
        // configured at startup, so they don't appear in the extras
        // index — but a client can still address them by model_id
        // (the GGUF file stem) via the OpenAI `model` field. Without
        // this check, such a request would fall through to OICP-based
        // slot picking, which defaults to `Slow → Primary` and
        // silently routes the user to a different model than they
        // asked for.
        if extras_match.is_none() {
            if let Some(mid) = request.model_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                if mid == self.fast.model_id {
                    return SlotTarget::Fast;
                }
                if let Some(pid) = self
                    .primary_path
                    .as_ref()
                    .and_then(|p| p.file_stem())
                    .and_then(|s| s.to_str())
                {
                    if mid == pid {
                        return SlotTarget::Primary;
                    }
                }
                if let Some(cid) = self
                    .code_path
                    .as_ref()
                    .and_then(|p| p.file_stem())
                    .and_then(|s| s.to_str())
                {
                    if mid == cid {
                        return SlotTarget::Code;
                    }
                }
            }
        }

        pick_slot(
            request,
            self.primary_path.is_some(),
            self.code_path.is_some(),
            self.fast_short.is_some(),
            extras_match,
        )
    }

    /// Resolve a slot_name in `extras` to its `(Arc<ModelSlot>,
    /// ModelQuirks)` pair, cloning out of the lock so the dispatch
    /// path doesn't hold the read lock across inference. Returns
    /// `None` if the slot was unloaded between
    /// `select_slot_for_request` and dispatch (operator unloaded
    /// mid-flight) — caller surfaces a clear error.
    fn extras_lookup(&self, slot_name: &str) -> Option<(Arc<ModelSlot>, ModelQuirks)> {
        let guard = self.extras.read().ok()?;
        let slot = guard.slots.get(slot_name)?.clone();
        let quirks = guard
            .quirks
            .get(slot_name)
            .cloned()
            .unwrap_or_else(|| ModelFamily::Unknown.default_quirks());
        Some((slot, quirks))
    }

    /// Returns true if an embedding model is loaded and ready.
    pub fn has_embed_slot(&self) -> bool {
        self.embed_slot.is_some()
    }

    /// Start a background task that unloads cold extras slots
    /// once they pass the configured idle threshold.
    ///
    /// Polling cadence: every 10 seconds. A slot is dropped when:
    /// - its `last_used` timestamp is older than `idle_secs` ago, AND
    /// - its `Arc<ModelSlot>` strong count is exactly 1 (no
    ///   in-flight request is holding it).
    ///
    /// `idle_secs == 0` is "disabled" — the task isn't spawned.
    /// This keeps the historical "extras stay loaded forever"
    /// default for operators who haven't opted in. Set
    /// `[daemon].extras_idle_secs = 1800` (30 min) for a balance
    /// between reload tax (full reload on next use) and VRAM
    /// reclaim on a shared host.
    ///
    /// Independent from `start_idle_monitor` (which manages the
    /// lazy primary slot) — extras and primary have different
    /// load semantics so they need separate idle policies.
    pub fn start_extras_idle_monitor(self: &Arc<Self>, idle_secs: u64) {
        if idle_secs == 0 {
            // Disabled — don't spawn the task.
            tracing::debug!("extras idle monitor disabled (idle_secs=0)");
            return;
        }
        let this = Arc::clone(self);
        tokio::spawn(async move {
            tracing::info!(idle_secs, "extras idle monitor started");
            let threshold_ms = idle_secs.saturating_mul(1000);
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                let now = now_millis();
                // Snapshot stale slot names under read lock, then
                // call unload_extra (which takes the write lock)
                // outside the read scope to avoid lock-upgrade
                // deadlocks.
                let stale_names: Vec<String> = {
                    let guard = match this.extras.read() {
                        Ok(g) => g,
                        Err(e) => {
                            tracing::error!(
                                error = %e,
                                "extras lock poisoned in idle monitor — exiting"
                            );
                            return;
                        }
                    };
                    guard
                        .slots
                        .iter()
                        .filter_map(|(name, arc)| {
                            // strong_count == 1 means only the map
                            // holds this slot — safe to drop.
                            if Arc::strong_count(arc) > 1 {
                                return None;
                            }
                            let last_used = arc
                                .last_used
                                .load(std::sync::atomic::Ordering::Relaxed);
                            if now.saturating_sub(last_used) >= threshold_ms {
                                Some(name.clone())
                            } else {
                                None
                            }
                        })
                        .collect()
                };
                for name in stale_names {
                    match this.unload_extra(&name) {
                        Ok(Some(model_id)) => {
                            tracing::info!(
                                slot = %name,
                                model_id = %model_id,
                                idle_secs,
                                "extras: idle-unload"
                            );
                        }
                        Ok(None) => {
                            // Slot vanished between snapshot and
                            // unload — race with operator unload.
                            // Benign.
                        }
                        Err(e) => {
                            tracing::warn!(
                                slot = %name,
                                error = %e,
                                "extras idle-unload failed"
                            );
                        }
                    }
                }
            }
        });
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
                        // Report which model was in the slot at unload
                        // time — "primary" or "code" — so operators can
                        // correlate idle-unloads with hot-swap activity.
                        let slot_label = {
                            let loaded = this.primary_loaded_path.lock().await;
                            match loaded.as_ref() {
                                Some(p) if Some(p.as_path()) == this.code_path.as_deref() => "code",
                                _ => "primary",
                            }
                        };
                        tracing::info!(
                            slot = slot_label,
                            idle_secs = timeout_secs,
                            "unloading slot (idle timeout)"
                        );
                        *primary = None;
                        *this.primary_loaded_path.lock().await = None;
                        *this.last_primary_use.lock().await = None;
                    }
                }
            }
        });
    }

}

#[async_trait]
impl InferenceProvider for EmbeddedLlamaCpp {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        let target = self.select_slot_for_request(request);
        let slot_name: String = match &target {
            SlotTarget::Fast => "fast".into(),
            SlotTarget::FastShort => "fast_short".into(),
            SlotTarget::Primary => "primary".into(),
            SlotTarget::Code => "code".into(),
            SlotTarget::Extra(name) => format!("extras:{name}"),
        };
        let prompt_chars = request.prompt.len();
        let system_chars = request.system_message.as_ref().map(|s| s.len()).unwrap_or(0);

        tracing::debug!(
            slot = %slot_name,
            speed = ?request.preferred_speed,
            prompt_chars,
            system_chars,
            max_tokens = ?request.max_tokens,
            think_budget = ?request.think_budget,
            temperature = ?request.temperature,
            "inference.complete: call"
        );

        // Extras slot: eagerly loaded, no hot-swap. Run it through
        // the same generate_sync path as the fast slot, just with a
        // different ModelSlot reference.
        if let SlotTarget::Extra(name) = &target {
            let Some((slot, quirks)) = self.extras_lookup(name) else {
                // Slot was unloaded between selection and dispatch
                // (concurrent /internal/models/unload). Fail fast
                // with a clear error rather than silently routing
                // to a different slot.
                return Err(Error::Inference(format!(
                    "extras slot {name:?} was unloaded between selection \
                     and dispatch — retry the request, or pick a different \
                     model_id"
                )));
            };
            // Acquire the slot's inflight permit BEFORE spawn_blocking
            // so concurrent callers queue at the async layer, not in
            // the blocking pool. See ModelSlot.inflight docs.
            let _permit = slot
                .inflight
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| Error::Inference(format!("slot permit closed: {e}")))?;
            let request = request.clone();
            let slot_label_owned = slot_name.clone();
            let result: Result<CompletionResponse> = tokio::task::spawn_blocking(move || {
                let start = Instant::now();
                // Stamp last_used at dispatch start. Using start
                // (rather than completion) makes the LRU eviction
                // policy treat a long-running request as still-warm
                // throughout — preferred behaviour for batch atlas
                // pipelines that fire 5-10min Phase 1 calls.
                slot.last_used
                    .store(now_millis(), std::sync::atomic::Ordering::Relaxed);
                let mut ctx_lock = slot.context.blocking_lock();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    ModelSlot::generate_sync(&slot.model, &slot.model_id, &mut ctx_lock, &request, &quirks)
                }));
                let outcome: GenerationOutcome = match result {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => {
                        tracing::warn!(slot = %slot_label_owned, error = %e, "inference error");
                        return Err(e);
                    }
                    Err(_) => {
                        tracing::error!(
                            slot = %slot_label_owned,
                            "inference panicked — likely context overflow"
                        );
                        return Err(Error::Inference(
                            "Model inference failed: prompt may exceed the model's context window. \
                             Try a shorter message or reduce conversation history."
                                .to_string(),
                        ));
                    }
                };
                let latency_ms = start.elapsed().as_millis() as u64;
                Ok(CompletionResponse {
                    text: outcome.text,
                    tokens_used: outcome.prompt_tokens + outcome.completion_tokens,
                    prompt_tokens: outcome.prompt_tokens,
                    model_id: slot.model_id.clone(),
                    latency_ms,
                    oicp_meta: None,
                    finish_reason: Some(outcome.finish_reason),
                    completion_tokens: Some(outcome.completion_tokens as u32),
                })
            })
            .await
            .map_err(|e| Error::Inference(format!("Inference task failed: {e}")))?;
            if let Ok(ref resp) = result {
                tracing::info!(
                    slot = %slot_name,
                    model = %resp.model_id,
                    latency_ms = resp.latency_ms,
                    tokens_used = resp.tokens_used,
                    response_chars = resp.text.len(),
                    "inference.complete: done"
                );
            }
            return result;
        }

        // FastShort: continuous-batched dispatch via the coalescer.
        // The coalescer batches concurrent arrivals into a single
        // `generate_sync_batched` call against the n_seq_max=8 ctx;
        // a solo caller pays at most a 5ms coalesce-window wait then
        // decodes immediately. `pick_slot` only selects FastShort
        // when both `fast_short` slot and `fast_short_coalescer` are
        // present, so the unwraps below match construction in
        // `load_full_with_families`.
        if matches!(target, SlotTarget::FastShort) {
            let coalescer = self
                .fast_short_coalescer
                .as_ref()
                .expect("FastShort selected without coalescer present")
                .clone();
            let result = coalescer.complete(request.clone()).await;
            if let Ok(ref resp) = result {
                tracing::info!(
                    slot = "fast_short",
                    model = %resp.model_id,
                    latency_ms = resp.latency_ms,
                    tokens_used = resp.tokens_used,
                    response_chars = resp.text.len(),
                    "inference.complete: done"
                );
            }
            return result;
        }

        // Primary sibling pool — opt-in via `SOVEREIGN_PRIMARY_SIBLINGS`.
        // When present, short-circuits the lazy slot entirely for
        // `SlotTarget::Primary`: each call picks a sibling round-
        // robin and runs against its own `Mutex<SlotContext>`, so
        // `pool.len()` calls can be in `generate_sync` at once.
        // Construction guarantees the pool is `None` whenever
        // `code_path` is configured (see `load_full_with_families`),
        // so `SlotTarget::Code` is never observed on this branch.
        if matches!(target, SlotTarget::Primary) {
            if let Some(pool) = self.primary_pool.as_ref() {
                let (sibling_idx, slot) = pool.pick();
                let pool_size = pool.len();
                let quirks = self.primary_quirks.clone();
                tracing::debug!(
                    slot = "primary",
                    sibling_idx,
                    pool_size,
                    "dispatching to primary sibling"
                );
                let _permit = slot
                    .inflight
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|e| {
                        Error::Inference(format!("primary sibling permit closed: {e}"))
                    })?;
                let request = request.clone();
                let result: Result<CompletionResponse> = tokio::task::spawn_blocking(move || {
                    let _permit = _permit;
                    let start = Instant::now();
                    slot.last_used
                        .store(now_millis(), std::sync::atomic::Ordering::Relaxed);
                    let mut ctx_lock = slot.context.blocking_lock();
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        ModelSlot::generate_sync(
                            &slot.model,
                            &slot.model_id,
                            &mut ctx_lock,
                            &request,
                            &quirks,
                        )
                    }));
                    let outcome: GenerationOutcome = match result {
                        Ok(Ok(r)) => r,
                        Ok(Err(e)) => {
                            tracing::warn!(
                                slot = "primary",
                                sibling_idx,
                                error = %e,
                                "inference error"
                            );
                            return Err(e);
                        }
                        Err(_) => {
                            tracing::error!(
                                slot = "primary",
                                sibling_idx,
                                "inference panicked — likely context overflow"
                            );
                            return Err(Error::Inference(
                                "Model inference failed: prompt may exceed the model's context window. \
                                 Try a shorter message or reduce conversation history."
                                    .to_string(),
                            ));
                        }
                    };
                    let latency_ms = start.elapsed().as_millis() as u64;
                    Ok(CompletionResponse {
                        text: outcome.text,
                        tokens_used: outcome.prompt_tokens + outcome.completion_tokens,
                        prompt_tokens: outcome.prompt_tokens,
                        model_id: slot.model_id.clone(),
                        latency_ms,
                        oicp_meta: None,
                        finish_reason: Some(outcome.finish_reason),
                        completion_tokens: Some(outcome.completion_tokens as u32),
                    })
                })
                .await
                .map_err(|e| Error::Inference(format!("Inference task failed: {e}")))?;
                if let Ok(ref resp) = result {
                    tracing::info!(
                        slot = "primary",
                        sibling_idx,
                        pool_size,
                        model = %resp.model_id,
                        latency_ms = resp.latency_ms,
                        tokens_used = resp.tokens_used,
                        response_chars = resp.text.len(),
                        "inference.complete: done"
                    );
                }
                return result;
            }
        }

        // Primary + Code share the lazy slot (hot-swap). Fast uses
        // the always-resident slot. Pick path + quirks up front so
        // the blocking task has a single load-or-reuse code path.
        let lazy_target = match target {
            SlotTarget::Fast => None,
            SlotTarget::FastShort => unreachable!("handled above"),
            SlotTarget::Primary => Some((
                self.primary_path.clone().unwrap(),
                self.primary_quirks.clone(),
                "primary",
            )),
            SlotTarget::Code => Some((
                self.code_path.clone().unwrap(),
                self.code_quirks.clone(),
                "code",
            )),
            SlotTarget::Extra(_) => unreachable!("handled above"),
        };

        if let Some((target_path, quirks, slot_label)) = lazy_target {
            // Throttle lazy-slot dispatch to 1 inflight at the async
            // layer; otherwise concurrent callers stack up blocking
            // threads waiting on `primary.blocking_lock()`. See the
            // `lazy_inflight` field doc.
            let _permit = self
                .lazy_inflight
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| Error::Inference(format!("lazy slot permit closed: {e}")))?;
            let primary_lock = Arc::clone(&self.primary);
            let backend = Arc::clone(&self.primary_backend);
            let ctx_size = self.primary_ctx_size;
            let gpu_layers = self.gpu_layers;
            let last_use = Arc::clone(&self.last_primary_use);
            let loaded_path = Arc::clone(&self.primary_loaded_path);
            let request = request.clone();

            let result: Result<(CompletionResponse, &'static str)> = tokio::task::spawn_blocking(move || {
                let start = Instant::now();

                // Hot-swap check: if the lazy slot is holding a
                // different model than we need, unload + reload.
                let mut primary = primary_lock.blocking_lock();
                let mut loaded = loaded_path.blocking_lock();
                let needs_swap = loaded.as_deref() != Some(target_path.as_path());
                if needs_swap {
                    if primary.is_some() {
                        tracing::info!(
                            slot = slot_label,
                            from = ?loaded.as_ref().map(|p| p.display().to_string()),
                            to = %target_path.display(),
                            "hot-swapping lazy slot"
                        );
                    } else {
                        tracing::info!(
                            slot = slot_label,
                            path = %target_path.display(),
                            "loading lazy slot (first use)"
                        );
                    }
                    *primary = None;
                    let s = ModelSlot::load(&backend, &target_path, ctx_size, gpu_layers)?;
                    *primary = Some(s);
                    *loaded = Some(target_path.clone());
                }
                drop(loaded);

                let slot = primary.as_ref().unwrap();
                let model_id = slot.model_id.clone();
                let mut ctx_lock = slot.context.blocking_lock();

                // Catch panics from llama.cpp (e.g., context overflow assertions).
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    ModelSlot::generate_sync(&slot.model, &slot.model_id, &mut ctx_lock, &request, &quirks)
                }));

                let outcome: GenerationOutcome = match result {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => {
                        tracing::warn!(slot = slot_label, error = %e, "inference error");
                        return Err(e);
                    }
                    Err(_) => {
                        tracing::error!(slot = slot_label, "inference panicked — likely context overflow");
                        return Err(Error::Inference(
                            "Model inference failed: prompt may exceed the model's context window. \
                             Try a shorter message or reduce conversation history.".to_string(),
                        ));
                    }
                };

                let latency_ms = start.elapsed().as_millis() as u64;

                *last_use.blocking_lock() = Some(Instant::now());

                Ok((CompletionResponse {
                    text: outcome.text,
                    tokens_used: outcome.prompt_tokens + outcome.completion_tokens,
                    prompt_tokens: outcome.prompt_tokens,
                    // `slot` may have been dropped on the fresh-ctx
                    // path; use the model_id cloned before the
                    // branch.
                    model_id: model_id.clone(),
                    latency_ms,
                    oicp_meta: None,
                    finish_reason: Some(outcome.finish_reason),
                    completion_tokens: Some(outcome.completion_tokens as u32),
                }, slot_label))
            })
            .await
            .map_err(|e| Error::Inference(format!("Inference task failed: {e}")))?;

            match result {
                Ok((resp, slot_label)) => {
                    tracing::info!(
                        slot = slot_label,
                        model = %resp.model_id,
                        latency_ms = resp.latency_ms,
                        tokens_used = resp.tokens_used,
                        response_chars = resp.text.len(),
                        "inference.complete: done"
                    );
                    Ok(resp)
                }
                Err(e) => Err(e),
            }
        } else {
            // Use fast slot.
            let slot = Arc::clone(&self.fast);
            // Throttle fast-slot dispatch to 1 inflight at the async
            // layer. Same rationale as the extras path. KV enrichment
            // Phase 2 fans out 4+ parallel calls on this slot — without
            // the gate they all park blocking-pool threads and clone
            // 8 KB prompts; with it, only one proceeds and the rest
            // queue cheaply as async tasks.
            let _permit = slot
                .inflight
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| Error::Inference(format!("fast slot permit closed: {e}")))?;
            let request = request.clone();
            let quirks = self.fast_quirks.clone();

            let result: Result<CompletionResponse> = tokio::task::spawn_blocking(move || {
                let start = Instant::now();
                let mut ctx_lock = slot.context.blocking_lock();

                // Catch panics from llama.cpp (e.g., context overflow assertions).
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    ModelSlot::generate_sync(&slot.model, &slot.model_id, &mut ctx_lock, &request, &quirks)
                }));

                let outcome: GenerationOutcome = match result {
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
                    text: outcome.text,
                    tokens_used: outcome.prompt_tokens + outcome.completion_tokens,
                    prompt_tokens: outcome.prompt_tokens,
                    model_id: slot.model_id.clone(),
                    latency_ms,
                    oicp_meta: None,
                    finish_reason: Some(outcome.finish_reason),
                    completion_tokens: Some(outcome.completion_tokens as u32),
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
        // FastShort doesn't support streaming yet — the coalescer
        // returns whole responses, not token-by-token chunks. Coerce
        // streaming requests that would route to FastShort back onto
        // the Fast slot. Phase 1b/3/5/6 enrichment never streams (they
        // call `complete`), so this fallback only fires for callers
        // misconfigured to ask for streaming with a small max_output.
        let raw_target = self.select_slot_for_request(request);
        let target = if matches!(raw_target, SlotTarget::FastShort) {
            tracing::debug!(
                "FastShort selected for streaming request; falling back to Fast \
                 (FastShort doesn't expose a streaming API)"
            );
            SlotTarget::Fast
        } else {
            raw_target
        };
        let slot_name: String = match &target {
            SlotTarget::Fast => "fast".into(),
            SlotTarget::FastShort => "fast_short".into(),
            SlotTarget::Primary => "primary".into(),
            SlotTarget::Code => "code".into(),
            SlotTarget::Extra(name) => format!("extras:{name}"),
        };

        tracing::debug!(
            slot = %slot_name,
            speed = ?request.preferred_speed,
            prompt_chars = request.prompt.len(),
            system_chars = request.system_message.as_ref().map(|s| s.len()).unwrap_or(0),
            max_tokens = ?request.max_tokens,
            "inference.complete_stream: call"
        );

        let request = request.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String>>(32);

        // Extras slot: eagerly loaded, mirrors the fast-slot streaming
        // path with a different ModelSlot reference.
        if let SlotTarget::Extra(name) = &target {
            let Some((slot, quirks)) = self.extras_lookup(name) else {
                return Err(Error::Inference(format!(
                    "extras slot {name:?} was unloaded between selection \
                     and dispatch — retry the request"
                )));
            };
            let _permit = slot
                .inflight
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| Error::Inference(format!("slot permit closed: {e}")))?;
            let slot_label_owned = slot_name.clone();
            tokio::task::spawn_blocking(move || {
                // Hold the permit for the streaming task's lifetime.
                let _permit = _permit;
                let start = Instant::now();
                slot.last_used
                    .store(now_millis(), std::sync::atomic::Ordering::Relaxed);
                let mut ctx_lock = slot.context.blocking_lock();
                if let Err(e) = ModelSlot::generate_stream_sync(
                    &slot.model,
                    &slot.model_id,
                    ctx_lock.ctx_mut(),
                    &request,
                    &tx,
                    &quirks,
                    None,
                ) {
                    tracing::warn!(slot = %slot_label_owned, error = %e, "stream error");
                    let _ = tx.blocking_send(Err(e));
                } else {
                    tracing::info!(
                        slot = %slot_label_owned,
                        latency_ms = start.elapsed().as_millis() as u64,
                        "inference.complete_stream: done"
                    );
                }
            });
            return Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)));
        }

        // Primary + Code share the lazy slot (hot-swap). Fast uses the
        // always-resident slot. Resolve path + quirks + slot-label up
        // front so the blocking task has one code path.
        let lazy_target: Option<(PathBuf, ModelQuirks, &'static str)> = match target {
            SlotTarget::Fast => None,
            // FastShort is handled before this match (or coerced to
            // Fast in the streaming path), so this arm never fires.
            SlotTarget::FastShort => unreachable!("FastShort handled above"),
            SlotTarget::Primary => Some((
                self.primary_path.clone().unwrap(),
                self.primary_quirks.clone(),
                "primary",
            )),
            SlotTarget::Code => Some((
                self.code_path.clone().unwrap(),
                self.code_quirks.clone(),
                "code",
            )),
            SlotTarget::Extra(_) => unreachable!("handled above"),
        };

        if let Some((target_path, quirks, slot_label)) = lazy_target {
            let _permit = self
                .lazy_inflight
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| Error::Inference(format!("lazy slot permit closed: {e}")))?;
            let primary_lock = Arc::clone(&self.primary);
            let backend = Arc::clone(&self.primary_backend);
            let ctx_size = self.primary_ctx_size;
            let gpu_layers = self.gpu_layers;
            let last_use = Arc::clone(&self.last_primary_use);
            let loaded_path = Arc::clone(&self.primary_loaded_path);

            tokio::task::spawn_blocking(move || {
                // Hold the permit for the streaming task's lifetime.
                let _permit = _permit;
                let start = Instant::now();

                // Hot-swap check: if the lazy slot is holding a different
                // model than we need, unload + reload. For streaming we
                // report the swap on the error channel only if the load
                // actually fails — the swap itself is internal plumbing.
                let mut primary = primary_lock.blocking_lock();
                let mut loaded = loaded_path.blocking_lock();
                let needs_swap = loaded.as_deref() != Some(target_path.as_path());
                if needs_swap {
                    if primary.is_some() {
                        tracing::info!(
                            slot = slot_label,
                            from = ?loaded.as_ref().map(|p| p.display().to_string()),
                            to = %target_path.display(),
                            "hot-swapping lazy slot (streaming)"
                        );
                    } else {
                        tracing::info!(
                            slot = slot_label,
                            path = %target_path.display(),
                            "loading lazy slot (first use, streaming)"
                        );
                    }
                    *primary = None;
                    *loaded = None;
                    match ModelSlot::load(&backend, &target_path, ctx_size, gpu_layers) {
                        Ok(slot) => {
                            *primary = Some(slot);
                            *loaded = Some(target_path.clone());
                        }
                        Err(e) => {
                            tracing::error!(slot = slot_label, error = %e, "slot load failed");
                            let _ = tx.blocking_send(Err(e));
                            return;
                        }
                    }
                }
                drop(loaded);

                let slot = primary.as_ref().unwrap();
                let mut ctx_lock = slot.context.blocking_lock();
                *last_use.blocking_lock() = Some(Instant::now());
                if let Err(e) =
                    ModelSlot::generate_stream_sync(&slot.model, &slot.model_id, ctx_lock.ctx_mut(), &request, &tx, &quirks, None)
                {
                    tracing::warn!(slot = slot_label, error = %e, "stream error");
                    let _ = tx.blocking_send(Err(e));
                } else {
                    tracing::info!(
                        slot = slot_label,
                        latency_ms = start.elapsed().as_millis() as u64,
                        "inference.complete_stream: done"
                    );
                }
            });
        } else {
            let slot = Arc::clone(&self.fast);
            let _permit = slot
                .inflight
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| Error::Inference(format!("fast slot permit closed: {e}")))?;
            let quirks = self.fast_quirks.clone();
            tokio::task::spawn_blocking(move || {
                // Hold the permit for the streaming task's lifetime.
                let _permit = _permit;
                let start = Instant::now();
                let mut ctx_lock = slot.context.blocking_lock();
                if let Err(e) =
                    ModelSlot::generate_stream_sync(&slot.model, &slot.model_id, ctx_lock.ctx_mut(), &request, &tx, &quirks, None)
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

    /// Typed-frame override: yield [`StreamFrame`] with an accurate
    /// terminal [`FinishReason`]. Mirrors [`complete_stream`]'s
    /// slot-routing dance so per-slot mutex / hot-swap semantics stay
    /// identical — only the channel item type and the inner generate
    /// helper change.
    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        let target = self.select_slot_for_request(request);
        let slot_name: String = match &target {
            SlotTarget::Fast => "fast".into(),
            SlotTarget::FastShort => "fast_short".into(),
            SlotTarget::Primary => "primary".into(),
            SlotTarget::Code => "code".into(),
            SlotTarget::Extra(name) => format!("extras:{name}"),
        };

        tracing::debug!(
            slot = %slot_name,
            speed = ?request.preferred_speed,
            prompt_chars = request.prompt.len(),
            system_chars = request.system_message.as_ref().map(|s| s.len()).unwrap_or(0),
            max_tokens = ?request.max_tokens,
            "inference.complete_stream_with_finish: call"
        );

        let request = request.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamFrame>(32);

        if let SlotTarget::Extra(name) = &target {
            let Some((slot, quirks)) = self.extras_lookup(name) else {
                return Err(Error::Inference(format!(
                    "extras slot {name:?} was unloaded between selection \
                     and dispatch — retry the request"
                )));
            };
            let _permit = slot
                .inflight
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| Error::Inference(format!("slot permit closed: {e}")))?;
            let slot_label_owned = slot_name.clone();
            tokio::task::spawn_blocking(move || {
                let _permit = _permit;
                let start = Instant::now();
                slot.last_used
                    .store(now_millis(), std::sync::atomic::Ordering::Relaxed);
                let mut ctx_lock = slot.context.blocking_lock();
                if let Err(e) = ModelSlot::generate_stream_sync_with_finish(
                    &slot.model,
                    &slot.model_id,
                    ctx_lock.ctx_mut(),
                    &request,
                    &tx,
                    &quirks,
                    None,
                ) {
                    tracing::warn!(slot = %slot_label_owned, error = %e, "stream error");
                    let _ = tx.blocking_send(StreamFrame::Finish {
                        reason: FinishReason::Error(format!("{e}")),
                        usage: None,
                    });
                } else {
                    tracing::info!(
                        slot = %slot_label_owned,
                        latency_ms = start.elapsed().as_millis() as u64,
                        "inference.complete_stream_with_finish: done"
                    );
                }
            });
            return Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)));
        }

        let lazy_target: Option<(PathBuf, ModelQuirks, &'static str)> = match target {
            SlotTarget::Fast => None,
            // FastShort is handled before this match (or coerced to
            // Fast in the streaming path), so this arm never fires.
            SlotTarget::FastShort => unreachable!("FastShort handled above"),
            SlotTarget::Primary => Some((
                self.primary_path.clone().unwrap(),
                self.primary_quirks.clone(),
                "primary",
            )),
            SlotTarget::Code => Some((
                self.code_path.clone().unwrap(),
                self.code_quirks.clone(),
                "code",
            )),
            SlotTarget::Extra(_) => unreachable!("handled above"),
        };

        if let Some((target_path, quirks, slot_label)) = lazy_target {
            let _permit = self
                .lazy_inflight
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| Error::Inference(format!("lazy slot permit closed: {e}")))?;
            let primary_lock = Arc::clone(&self.primary);
            let backend = Arc::clone(&self.primary_backend);
            let ctx_size = self.primary_ctx_size;
            let gpu_layers = self.gpu_layers;
            let last_use = Arc::clone(&self.last_primary_use);
            let loaded_path = Arc::clone(&self.primary_loaded_path);

            tokio::task::spawn_blocking(move || {
                let _permit = _permit;
                let start = Instant::now();

                let mut primary = primary_lock.blocking_lock();
                let mut loaded = loaded_path.blocking_lock();
                let needs_swap = loaded.as_deref() != Some(target_path.as_path());
                if needs_swap {
                    if primary.is_some() {
                        tracing::info!(
                            slot = slot_label,
                            from = ?loaded.as_ref().map(|p| p.display().to_string()),
                            to = %target_path.display(),
                            "hot-swapping lazy slot (streaming, typed)"
                        );
                    } else {
                        tracing::info!(
                            slot = slot_label,
                            path = %target_path.display(),
                            "loading lazy slot (first use, streaming, typed)"
                        );
                    }
                    *primary = None;
                    *loaded = None;
                    match ModelSlot::load(&backend, &target_path, ctx_size, gpu_layers) {
                        Ok(slot) => {
                            *primary = Some(slot);
                            *loaded = Some(target_path.clone());
                        }
                        Err(e) => {
                            tracing::error!(slot = slot_label, error = %e, "slot load failed");
                            let _ = tx.blocking_send(StreamFrame::Finish {
                                reason: FinishReason::Error(format!("{e}")),
                                usage: None,
                            });
                            return;
                        }
                    }
                }
                drop(loaded);

                let slot = primary.as_ref().unwrap();
                let mut ctx_lock = slot.context.blocking_lock();
                *last_use.blocking_lock() = Some(Instant::now());
                if let Err(e) = ModelSlot::generate_stream_sync_with_finish(
                    &slot.model,
                    &slot.model_id,
                    ctx_lock.ctx_mut(),
                    &request,
                    &tx,
                    &quirks,
                    None,
                ) {
                    tracing::warn!(slot = slot_label, error = %e, "stream error");
                    let _ = tx.blocking_send(StreamFrame::Finish {
                        reason: FinishReason::Error(format!("{e}")),
                        usage: None,
                    });
                } else {
                    tracing::info!(
                        slot = slot_label,
                        latency_ms = start.elapsed().as_millis() as u64,
                        "inference.complete_stream_with_finish: done"
                    );
                }
            });
        } else {
            let slot = Arc::clone(&self.fast);
            let _permit = slot
                .inflight
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| Error::Inference(format!("fast slot permit closed: {e}")))?;
            let quirks = self.fast_quirks.clone();
            tokio::task::spawn_blocking(move || {
                let _permit = _permit;
                let start = Instant::now();
                let mut ctx_lock = slot.context.blocking_lock();
                if let Err(e) = ModelSlot::generate_stream_sync_with_finish(
                    &slot.model,
                    &slot.model_id,
                    ctx_lock.ctx_mut(),
                    &request,
                    &tx,
                    &quirks,
                    None,
                ) {
                    tracing::warn!(slot = "fast", error = %e, "stream error");
                    let _ = tx.blocking_send(StreamFrame::Finish {
                        reason: FinishReason::Error(format!("{e}")),
                        usage: None,
                    });
                } else {
                    tracing::info!(
                        slot = "fast",
                        latency_ms = start.elapsed().as_millis() as u64,
                        "inference.complete_stream_with_finish: done"
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

    /// Reranker override. Routes to the lazily-installed `RerankSlot`
    /// when present; returns `Error::NotImplemented` otherwise so the
    /// caller (corpus-engine `search_with_rerank`) falls back to the
    /// un-reranked fusion result without surfacing a hard error.
    async fn rerank_batch(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        let slot = {
            let guard = self
                .rerank_slot
                .lock()
                .map_err(|e| Error::Inference(format!("rerank slot lock poisoned: {e}")))?;
            guard.as_ref().map(Arc::clone)
        };
        let slot = slot.ok_or_else(|| {
            Error::NotImplemented(
                "No reranker model is installed. Set rerank.path in models.toml \
                 (or SOVEREIGN_RERANK_MODEL_PATH) and restart the daemon."
                    .to_string(),
            )
        })?;
        let query = query.to_string();
        let docs = docs.to_vec();
        tokio::task::spawn_blocking(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                slot.score_batch(&query, &docs)
            }));
            match result {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(Error::Inference(
                    "Rerank inference panicked — model may be incompatible with \
                     pooling=rank, or an input pair exceeded its context."
                        .to_string(),
                )),
            }
        })
        .await
        .map_err(|e| Error::Inference(format!("Rerank task failed: {e}")))?
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

    /// Configured primary-slot context size. This is the value passed
    /// to `LlamaContextParams::with_n_ctx` at slot construction —
    /// llama.cpp may pad upward to the nearest multiple of 256, so the
    /// runtime-observed `ctx.n_ctx()` can differ by a tail-end ~255
    /// tokens. For pre-flight budgeting that's well within the
    /// `reserved_output` safety margin; surfacing the configured value
    /// keeps this method sync (no `Mutex` lock) and matches the value
    /// the user sees in Settings.
    fn effective_context_size(&self) -> Option<u32> {
        Some(self.primary_ctx_size)
    }

    /// The fast slot's `LlamaModel` is held by `Arc` (always loaded,
    /// no lock needed). `n_ctx_train` comes from gguf metadata and is
    /// shared across the family, so reading it from fast is the right
    /// answer for both primary and fast slots when they're the same
    /// gguf (subsume case). When primary points at a different gguf,
    /// this returns the fast slot's value — close enough for the
    /// Settings UI ceiling hint; the upper bound is shape-similar
    /// across the same model family.
    fn n_ctx_train_for_primary(&self) -> Option<u32> {
        Some(self.fast.model.n_ctx_train())
    }

    /// Override the trait default heuristic with the fast slot's
    /// actual BPE tokenizer. `LlamaModel::str_to_token` is what the
    /// inference path calls at decode time, so this returns the same
    /// count the runtime budget needs to predict. Reads from the
    /// always-resident `Arc<LlamaModel>` — no lock, safe to call in
    /// the chat-context-management hot path.
    ///
    /// When `str_to_token` fails (rare — invalid UTF-8 or empty
    /// tokenizer vocab), falls back to the project-wide ~4 chars/token
    /// heuristic so the caller still gets a usable number. The
    /// heuristic over-estimates compared to the real tokenizer on
    /// dense text, which is the safe direction for budgeting (we'd
    /// rather over-compact than blow the slot).
    fn count_tokens(&self, text: &str) -> u32 {
        match self.fast.model.str_to_token(text, AddBos::Never) {
            Ok(tokens) => tokens.len() as u32,
            Err(_) => (text.chars().count() / 4) as u32,
        }
    }

    /// PR-E2: filename stem of the configured code GGUF, or `None` if
    /// the user skipped configuring a Code specialist. Mirrors
    /// `model_id_for` — derived from the path without locking.
    fn code_model_id(&self) -> Option<String> {
        self.code_path
            .as_ref()
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
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

    fn load_extra_slot(
        &self,
        slot_name: String,
        path: std::path::PathBuf,
        context_size: u32,
    ) -> Result<String> {
        // Honour the operator-set budget — when present, LRU
        // eviction kicks in if the new slot's size would push the
        // lineup past the cap. When absent, behaviour matches the
        // historical "hold forever" semantics.
        let budget = self.extras_memory_budget();
        self.load_extra_with_budget(slot_name, path, context_size, budget)
    }

    fn unload_extra_slot(&self, slot_name: &str) -> Result<Option<String>> {
        self.unload_extra(slot_name)
    }

    fn extras_inventory(&self) -> Vec<(String, String)> {
        // Delegate to the inherent method to avoid the recursive
        // call that `EmbeddedLlamaCpp::extras_inventory` would
        // produce if we just wrote `self.extras_inventory()` here.
        Self::extras_inventory(self)
    }

    async fn warmup_primary(&self) -> Result<()> {
        // No primary configured (single-model BYOM setup) — nothing
        // to warm; return success so callers don't have to special-
        // case the topology.
        let Some(target_path) = self.primary_path.clone() else {
            return Ok(());
        };

        // Acquire the same lazy-slot inflight permit `complete()` /
        // `complete_stream()` use, so a warmup can't race a hot-swap
        // out from under an in-flight request.
        let _permit = self
            .lazy_inflight
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| Error::Inference(format!("lazy slot permit closed: {e}")))?;
        let primary_lock = Arc::clone(&self.primary);
        let backend = Arc::clone(&self.primary_backend);
        let ctx_size = self.primary_ctx_size;
        let gpu_layers = self.gpu_layers;
        let loaded_path = Arc::clone(&self.primary_loaded_path);

        // Load on a blocking thread — `ModelSlot::load` is
        // synchronous llama.cpp work and can take 10–90s on a 35B.
        // Mirrors the dispatch path's blocking section.
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut primary = primary_lock.blocking_lock();
            let mut loaded = loaded_path.blocking_lock();
            // Already warm with the right model? Idempotent fast
            // path — no log spam, no work.
            if loaded.as_deref() == Some(target_path.as_path()) && primary.is_some() {
                return Ok(());
            }
            let prior = loaded.clone();
            tracing::info!(
                slot = "primary",
                from = ?prior.as_ref().map(|p| p.display().to_string()),
                to = %target_path.display(),
                "warmup_primary: loading lazy slot ahead of first request"
            );
            // If a different model is currently resident (e.g. a
            // Code-slot hot-swap left it loaded), drop it before
            // loading the primary — same eviction step the dispatch
            // path runs on hot-swap.
            *primary = None;
            let started = Instant::now();
            let s = ModelSlot::load(&backend, &target_path, ctx_size, gpu_layers)?;
            *primary = Some(s);
            *loaded = Some(target_path.clone());
            tracing::info!(
                slot = "primary",
                latency_ms = started.elapsed().as_millis() as u64,
                "warmup_primary: lazy slot ready"
            );
            Ok(())
        })
        .await
        .map_err(|e| Error::Inference(format!("warmup join failed: {e}")))?
    }
}

