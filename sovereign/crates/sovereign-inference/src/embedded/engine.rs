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
use sovereign_core::setup_config::{fim_defaults, EditSection};
use sovereign_core::traits::{frames_to_text_stream, InferenceProvider, ResidentSlot};
use sovereign_core::types::*;
use sovereign_core::Result;

use crate::hardware::HardwareProfile;

/// Reserved extras-slot name for the dedicated code-editing model
/// (`sovereign/docs/NEXT_EDIT.md`, `INLINE_COMPLETION.md`). The idle
/// monitor and the LRU eviction pass both skip this name (decision D2
/// — the editing slot is pinned: a reload tax on the keystroke path is
/// disqualifying), while an explicit operator `unload_extra("edit")`
/// stays allowed.
pub const EDIT_SLOT_NAME: &str = "edit";

/// The pre-rename reserved name for the same slot. Still pinned: an
/// upgrade must not silently make an editing slot evictable just
/// because the reserved name moved, and un-pinning is invisible until
/// the slot is dropped mid-keystroke — the exact failure the pin exists
/// to prevent.
pub const LEGACY_EDIT_SLOT_NAME: &str = "fim";

/// Reserved slot names the idle monitor and LRU eviction pass must
/// never drop (decision D2).
const PINNED_SLOT_NAMES: [&str; 2] = [EDIT_SLOT_NAME, LEGACY_EDIT_SLOT_NAME];

/// Pin predicate for the extras lineup (decision D2). The editing slot
/// is the only pinned entry today: its bytes still count toward the
/// extras memory budget (a pinned 1.5–3B coder is ~1–2 GB the
/// operator opted into), but neither the idle monitor nor LRU
/// eviction may drop it.
pub fn extras_slot_is_evictable(slot_name: &str) -> bool {
    !PINNED_SLOT_NAMES.contains(&slot_name)
}

/// Decide which lanes an editing slot can serve.
///
/// The single answer to that question. All three install paths (alias,
/// dedicated, fallback) route through here so none of them can decide
/// it differently — two deciders for one capability is exactly the
/// drift ARCH §10.6 exists to stop.
///
/// - **Next-edit** needs nothing but a prompt dialect, so it is always
///   available once a model is loaded.
/// - **FIM** needs marker tokens in the vocabulary. No probe hit, no
///   lane — and callers must refuse rather than guess a convention,
///   because a wrong marker set yields confident garbage, not an error.
///
/// `section` is `None` for the automatic fallback, which has no
/// `[models.edit]` to read and takes the shared defaults.
fn edit_lanes(
    style: Option<FimStyle>,
    format: NextEditFormat,
    section: Option<&EditSection>,
) -> (Option<NextEditLane>, Option<FimLane>) {
    let fim = style.map(|style| match section {
        Some(s) => FimLane {
            style,
            max_tokens: s.effective_max_tokens(),
            temperature: s.effective_temperature(),
            max_prefix_chars: s.effective_max_prefix_chars(),
            max_suffix_chars: s.effective_max_suffix_chars(),
        },
        None => FimLane {
            style,
            max_tokens: fim_defaults::MAX_TOKENS,
            temperature: fim_defaults::TEMPERATURE,
            max_prefix_chars: fim_defaults::MAX_PREFIX_CHARS,
            max_suffix_chars: fim_defaults::MAX_SUFFIX_CHARS,
        },
    });
    (Some(NextEditLane { format }), fim)
}

/// Announce an editing-slot install, naming both lanes.
///
/// One renderer for all three install paths so the operator reads the
/// same shape whichever route ran, and so "which lanes came up" is
/// never left to be inferred from the absence of a log line (ARCH §9.1).
/// A withheld FIM lane logs at `warn` because `/v1/completions` will
/// 503 and the user deserves to learn that at boot, not at first use.
fn log_edit_slot_install(info: &EditSlotInfo, what: &str) {
    let fim_lane = info
        .fim
        .as_ref()
        .map(|f| f.style.as_str())
        .unwrap_or("none");
    let next_edit_lane = info
        .next_edit
        .as_ref()
        .map(|n| n.format.as_str())
        .unwrap_or("none");
    if info.fim.is_some() {
        tracing::info!(
            target: "edit_slot",
            slot = %info.slot,
            model_id = %info.model_id,
            fim_lane,
            next_edit_lane,
            degraded = info.degraded,
            "edit slot {what}"
        );
    } else {
        tracing::warn!(
            target: "edit_slot",
            slot = %info.slot,
            model_id = %info.model_id,
            fim_lane,
            next_edit_lane,
            degraded = info.degraded,
            "edit slot {what} — no FIM markers in this model's vocab, so \
             /v1/completions will 503; next-edit (/v1/edit_predictions) serves \
             normally. For FIM, point [models.edit].path at a coder GGUF \
             (Mellum2, Qwen2.5-Coder)"
        );
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

/// The window each slot is built with — one value, named per slot.
///
/// # Why this is a struct and not a scalar
///
/// It WAS a scalar. `context_size: u32` was threaded from config into every
/// `ModelSlot::load` in this file, so the fast slot, the primary, the primary
/// sibling pool and the fast/primary alias all got the same number. KV cache
/// is linear in `n_ctx`, so a 4B fast model carried a 27B primary's window and
/// paid a 27B primary's cache for it.
///
/// The fix is not a second scalar parameter — that reintroduces the same
/// question one slot later ("and what about embed?"). It is a value whose
/// fields are the slots, so adding a slot means adding a field and the
/// compiler asks every construction site what that slot's window should be.
/// Same move as `LaneSources` and `RuntimeParts`: totality over a surface a
/// caller could otherwise forget half of.
///
/// `FastShort` is deliberately absent. It already owns `FAST_SHORT_N_CTX` next
/// to `FAST_SHORT_N_SEQ_MAX`, because its window is not a free choice — the
/// two divide to give the per-sequence budget `pick_slot` gates on, so they
/// have to move together and belong in one place.
#[derive(Debug, Clone, Copy)]
pub struct SlotWindows {
    /// The primary (deep-reasoning) slot, and the default for embed / code /
    /// extras.
    pub primary: u32,
    /// The fast slot. Note this is the OVERFLOW path — `pick_slot` sends any
    /// prompt too large for FastShort here — so it must cover the largest
    /// prompt that lands on it, not the typical one.
    pub fast: u32,
}

impl SlotWindows {
    /// Every slot gets the same window. This is what the scalar did, kept as a
    /// NAMED constructor so the call sites that genuinely mean it (a compute
    /// child, where one slot is the only slot) say so, and the ones that were
    /// merely inheriting a global stop being indistinguishable from them.
    pub fn uniform(n_ctx: u32) -> Self {
        Self {
            primary: n_ctx,
            fast: n_ctx,
        }
    }

    /// Resolve from config: the primary's window, and the fast slot's own if
    /// `[models].fast_context_size` is set.
    pub fn from_models(models: &sovereign_core::setup_config::ModelsSection) -> Self {
        Self {
            primary: models.effective_context_size(),
            fast: models.effective_fast_context_size(),
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

/// How many FastShort requests may be waiting for the drainer before
/// new arrivals are shed.
///
/// WHY A BOUND EXISTS AT ALL. The coalescer channel was
/// `unbounded_channel`, and the FastShort dispatch path takes its
/// inflight permit inside `dispatch_batch` — i.e. AFTER the queue —
/// so the backlog was invisible to `queue.depth()` and the slot's
/// predicted-wait shed could never fire on this path. Not "the shed
/// rarely fires": it *cannot*
/// (`MESH_SCALE_100_USERS_1000_CORPORA.md` §7.2). An enrichment or
/// pipeline pass that outruns decode therefore grew the queue until
/// the process died, with every caller still parked on its oneshot.
///
/// The default is eight full batches at `n_seq_max = 8`. A queue
/// deeper than that is not latency, it is a memory leak with a
/// completion handler.
const FAST_SHORT_QUEUE_CAP_DEFAULT: usize = 64;

fn fast_short_queue_cap() -> usize {
    std::env::var("SOVEREIGN_FAST_SHORT_QUEUE_CAP")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(FAST_SHORT_QUEUE_CAP_DEFAULT)
}

struct FastShortCoalescer {
    pub(crate) enqueue: tokio::sync::mpsc::Sender<CoalescerJob>,
    /// The bound `enqueue` was built with. Held because
    /// `Sender::capacity()` reports REMAINING permits; depth is
    /// `cap - capacity`, and depth is what a shed has to report.
    cap: usize,
    /// Batch size the drainer coalesces up to — how many queued jobs
    /// one dispatch retires.
    n_seq_max: usize,
    /// EWMA of observed batch wall-clock, in ms. The predicted wait a
    /// shed reports is derived from a MEASURED dispatch cost, not a
    /// guessed constant; `0` means nothing has dispatched yet.
    dispatch_ms: Arc<std::sync::atomic::AtomicU64>,
}

impl FastShortCoalescer {
    /// Spawn the long-lived drainer task and return a handle that
    /// callers use to enqueue requests via `complete`. The slot is
    /// `Arc`-shared with the caller so the existing `Arc::strong_count`
    /// idle-eviction logic keeps working unchanged.
    fn spawn(slot: Arc<ModelSlot>, quirks: ModelQuirks, n_seq_max: usize) -> Self {
        let cap = fast_short_queue_cap();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<CoalescerJob>(cap);
        let dispatch_ms = Arc::new(std::sync::atomic::AtomicU64::new(0));
        tracing::info!(
            slot = "fast_short",
            queue_cap = cap,
            n_seq_max,
            "coalescer armed with a bounded queue"
        );
        let drain_dispatch_ms = Arc::clone(&dispatch_ms);
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
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    match tokio::time::timeout(remaining, rx.recv()).await {
                        Ok(Some(job)) => batch.push(job),
                        Ok(None) => break, // channel closed mid-coalesce
                        Err(_) => break,   // timeout → fire what we have
                    }
                }

                let batch_started = Instant::now();
                let dispatched = batch.len();
                Self::dispatch_batch(Arc::clone(&slot), quirks.clone(), batch).await;
                // Feed the shed's predictor with what dispatch ACTUALLY
                // cost. EWMA rather than last-value: one dispatch is not
                // a measurement (§18.5), and a single cold first batch
                // would otherwise set the retry hint for every caller
                // shed after it.
                let observed = batch_started
                    .elapsed()
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64;
                let prior = drain_dispatch_ms.load(std::sync::atomic::Ordering::Relaxed);
                let next = if prior == 0 {
                    observed
                } else {
                    (prior * 3 + observed) / 4
                };
                drain_dispatch_ms.store(next, std::sync::atomic::Ordering::Relaxed);
                tracing::debug!(
                    slot = "fast_short",
                    dispatched,
                    observed_ms = observed,
                    ewma_ms = next,
                    "coalescer dispatch cost updated"
                );
            }
        });
        Self {
            enqueue: tx,
            cap,
            n_seq_max,
            dispatch_ms,
        }
    }

    /// Build a handle around an already-made sender, WITHOUT spawning a
    /// drainer. The seam the queue-bound regression test uses: it holds
    /// the receiver and never reads it, which is the only way to
    /// actually fill the coalescer and watch the shed fire.
    #[cfg(test)]
    fn with_sender(
        enqueue: tokio::sync::mpsc::Sender<CoalescerJob>,
        cap: usize,
        n_seq_max: usize,
        dispatch_ms_seed: u64,
    ) -> Self {
        Self {
            enqueue,
            cap,
            n_seq_max,
            dispatch_ms: Arc::new(std::sync::atomic::AtomicU64::new(dispatch_ms_seed)),
        }
    }

    /// Acquire the slot's inflight permit, then run
    /// `generate_sync_batched` against the collected jobs on a
    /// blocking thread. Each job's `oneshot::Sender` resolves with
    /// its individual `(text, tokens)` slice.
    async fn dispatch_batch(slot: Arc<ModelSlot>, quirks: ModelQuirks, batch: Vec<CoalescerJob>) {
        let permit = match ModelSlot::acquire_inflight(&slot, "fast_short_batch").await {
            Ok(p) => p,
            Err(e) => {
                let msg = e.to_string();
                for job in batch {
                    let _ = job.response.send(Err(Error::Inference(msg.clone())));
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
            let requests_refs: Vec<&CompletionRequest> = batch.iter().map(|j| &j.request).collect();

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
                    // Glassbox batch receipt (INFO — this is the only surface
                    // that exposes batch cardinality + the head-of-line coupling
                    // inherent to lockstep batched decode; promoted from debug
                    // 2026-07-14 after a K=8 investigation had to reconstruct
                    // `batch_size` from completion-line timestamps). `completion_*`
                    // is per-sequence output length; when max ≫ min the short
                    // members waited for the longest one to finish the batch, and
                    // `aggregate_tok_per_s` is the whole batch's decode throughput
                    // (all sequences) — compare against the MTP `fast` slot's
                    // per-request `tok_per_s` to see when batching actually wins.
                    let completions: Vec<usize> =
                        per_seq_results.iter().map(|(_, _, c)| *c).collect();
                    let total_completion: usize = completions.iter().sum();
                    let completion_min = completions.iter().copied().min().unwrap_or(0);
                    let completion_max = completions.iter().copied().max().unwrap_or(0);
                    let aggregate_tok_per_s = if latency_ms > 0 {
                        total_completion as f64 * 1000.0 / latency_ms as f64
                    } else {
                        0.0
                    };
                    tracing::info!(
                        slot = "fast_short",
                        batch_size,
                        latency_ms,
                        total_completion_tokens = total_completion,
                        aggregate_tok_per_s = format!("{aggregate_tok_per_s:.1}"),
                        completion_min,
                        completion_max,
                        head_of_line_ratio = format!(
                            "{:.1}",
                            completion_max as f64 / completion_min.max(1) as f64
                        ),
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
                        let _ = job.response.send(Err(Error::Inference(msg.clone())));
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

    /// Admit a request to the queue, or shed. Returns the receiver the
    /// caller awaits.
    ///
    /// `try_send`, never `send`: an awaiting `send` on a bounded
    /// channel converts the unbounded MEMORY growth into unbounded
    /// WAITING, which is the same failure wearing a different hat — the
    /// caller still never learns the host is saturated and still cannot
    /// route elsewhere. A shed is the only outcome a peer load balancer
    /// or an HTTP client can act on.
    fn enqueue_job(
        &self,
        request: CompletionRequest,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<CompletionResponse>>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        match self.enqueue.try_send(CoalescerJob {
            request,
            response: tx,
        }) {
            Ok(()) => Ok(rx),
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                Err(Error::Inference("FastShort coalescer is shut down".into()))
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                // Depth from the sender's remaining permits — the queue
                // itself is the authority, so the number in the shed is
                // the real one rather than an assumption that "full"
                // means exactly `cap`.
                let depth = self.cap.saturating_sub(self.enqueue.capacity());
                let position = (depth + 1).min(u32::MAX as usize) as u32;
                // Batches ahead of this caller × measured cost per
                // batch. `dispatch_ms == 0` means nothing has dispatched
                // yet, so there is no measurement to report — fall back
                // to the coalesce window, which is the only interval we
                // can honestly name at that point.
                let per_batch_ms = match self.dispatch_ms.load(std::sync::atomic::Ordering::Relaxed)
                {
                    0 => FAST_SHORT_COALESCE_WINDOW_MS,
                    ms => ms,
                };
                let batches_ahead = position.div_ceil(self.n_seq_max.max(1) as u32) as u64;
                let predicted_wait_ms = batches_ahead.saturating_mul(per_batch_ms);
                let err = Error::queue_shed(position, predicted_wait_ms);
                tracing::info!(
                    slot = "fast_short",
                    position,
                    depth,
                    queue_cap = self.cap,
                    predicted_wait_ms,
                    per_batch_ms,
                    "inference.queue: SHED — FastShort coalescer queue is full"
                );
                Err(err)
            }
        }
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        let rx = self.enqueue_job(request)?;
        rx.await
            .map_err(|_| Error::Inference("FastShort coalescer dropped response".into()))?
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
    /// Advertised id of the loaded embed model — the gguf file stem,
    /// the same convention `/v1/models` advertises and
    /// `SplitInferenceProvider` reports, so persisted embeddings
    /// written through either surface vouch under the same id.
    /// `None` when no embed slot is configured.
    embed_model_id: Option<String>,
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
    /// Live code-editing serving arrangement (`sovereign/docs/NEXT_EDIT.md`,
    /// `INLINE_COMPLETION.md`). `Some` after `install_edit_slot` or
    /// `install_fallback_next_edit_slot`; `None` only when there is no
    /// editing model at all. `edit_slot_info()` surfaces it to
    /// `/status`, `/v1/completions` and `/v1/edit_predictions`.
    ///
    /// A failed marker probe no longer empties this — it withholds the
    /// FIM lane, leaving next-edit served. Behind an RwLock (not
    /// set-once) so a future operator reload path can re-probe without
    /// a daemon restart.
    edit_slot: Arc<std::sync::RwLock<Option<EditSlotInfo>>>,
    /// Inflight gate for the lazy slot (Primary or Code). Single
    /// permit. The fast / extras slots have their own per-slot
    /// `inflight` semaphores on `ModelSlot`; the lazy path can't use
    /// those since the slot is `None` until first use, so the
    /// `EmbeddedLlamaCpp` owns a parallel semaphore that covers both
    /// hot-swap and generation. See `ModelSlot::inflight` for the
    /// rationale.
    /// Generation gate for the LAZY slot (Primary or Code): permit, depth
    /// gauge, turn EWMA and shed bound. The fast/extras slots own their own
    /// `SlotQueue` on `ModelSlot`; the lazy path cannot use those because the
    /// slot is `None` until first use, so the engine owns a parallel one that
    /// covers both hot-swap and generation.
    ///
    /// THIS is the queue that speaks to MESH_N4_TOPOLOGY M5. The configured
    /// PRIMARY model is served from the lazy slot, so every concurrent chat
    /// turn against the big model serialises HERE — measured 2026-08-06 at a
    /// 90.7 s worst wait with nine distinct-context callers.
    lazy_queue: Arc<super::model_slot::SlotQueue>,
    /// Optional eager-loaded sibling pool for `SlotTarget::Primary`
    /// completion — non-streaming AND streaming (order
    /// mesh-scale-t1-streampool). `None` (the default) leaves
    /// dispatch on the single-context lazy path — behaviour is
    /// byte-identical to pre-sibling builds. `Some(pool)` short-
    /// circuits the lazy path: each call picks the least-loaded
    /// sibling and acquires that sibling's own per-slot `inflight`
    /// permit, so two callers can be generating at once.
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
/// The FastShort eligibility gate: a short-output, Fast-latency request whose
/// prompt fits a single FastShort KV slice (`n_ctx/n_seq_max`). Shared by
/// `pick_slot` AND the named-fast-slot fast-path in `select_slot_for_request`,
/// so a request that addresses the fast model by `model_id` still rides the
/// continuous-batched companion instead of serializing on the single Fast slot
/// (the bug that made the code-intel enrich / atlas phases run serially).
fn fits_fast_short(request: &CompletionRequest) -> bool {
    let max_out = request.max_tokens.unwrap_or(usize::MAX);
    let prompt_chars = request.prompt.chars().count()
        + request
            .system_message
            .as_ref()
            .map_or(0, |s| s.chars().count());
    max_out <= 512
        && request.preferred_speed == Speed::Fast
        && prompt_chars <= FAST_SHORT_MAX_INPUT_CHARS
}

/// Whether a FastShort-eligible request should actually engage the
/// continuous-batched companion. **FastShort is the batched OVERFLOW
/// lane, not the default for Fast traffic.**
///
/// Single-token batching only pays off when there is concurrent demand
/// to coalesce. A request arriving while the MTP-capable `fast` slot is
/// IDLE is a latency-bound singleton: it has nothing to batch with, so
/// routing it to FastShort forfeits speculative (MTP) decode, pays the
/// coalesce-window tax, and exposes it to head-of-line blocking — all
/// for zero batching benefit. Such a request belongs on `fast`.
///
/// Measured 2026-07-14 (Qwen3.5-9B-MTP, Strix Halo Vulkan, `scripts`-free
/// A/B against the live daemon):
/// - Solo (K=1): MTP `fast` decodes ~29% faster than single-token
///   FastShort (29.3 vs 22.7 tok/s regressed; MTP self-reports 32.7 at
///   0.57 draft-accept) — ~1.0s saved on a 100-token answer.
/// - Concurrent (K≈8): FastShort's batch throughput is only ~1.4× over
///   serial single-token here (the code's 2.1–2.8× claim was M2 Max /
///   Metal; decode on unified LPDDR5 is bandwidth-bound), which roughly
///   *ties* serialized MTP — while head-of-line-blocking a 73-token
///   reply for 40s behind a 512-token batch member.
///
/// So: engage FastShort only under contention (`fast_slot_busy`) — a
/// pipeline fan-out keeps the `fast` slot busy, which is exactly when
/// batching the remaining arrivals is the right call. The streaming
/// path already coerces FastShort→Fast unconditionally (FastShort has
/// no streaming API); this generalises that demotion to solo
/// non-streaming calls, for the latency reason rather than the API one.
/// See [[project_fast_short_overflow_routing]].
pub(crate) fn should_batch_fast_short(
    has_fast_short: bool,
    fast_slot_busy: bool,
    request: &CompletionRequest,
) -> bool {
    has_fast_short && fast_slot_busy && fits_fast_short(request)
}

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

    // Forced-choice sentinel backstop (SLOT_POLICY §6). A forced-choice
    // request carries `max_tokens=1` and may be labelled `Speed::Fast`,
    // which would satisfy the FastShort gate below — but the calibrated
    // logprob elicitation reads the PRIMARY model's next-token
    // distribution, not the small companion's, so it must land on
    // Primary. This MUST precede the FastShort gate for exactly that
    // reason. On a primary-less host it falls through to the normal
    // gates (best-effort) rather than failing.
    if request.forced_choice_candidates().is_some() && has_primary {
        if request.preferred_speed == Speed::Fast {
            tracing::warn!(
                "forced_choice sentinel declared Speed::Fast — overriding to the \
                 Primary slot for calibrated logprobs (SLOT_POLICY §6)"
            );
        }
        return SlotTarget::Primary;
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
    if has_fast_short && fits_fast_short(request) {
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
            model_path, None, // No separate primary model.
            2048, None,
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
            // A dual load has no separate fast budget to spend: both slots are
            // this one model. `uniform` says that, where the bare scalar used
            // to make it indistinguishable from a slot inheriting a global.
            SlotWindows::uniform(context_size),
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
        windows: SlotWindows,
        gpu_layers: Option<u32>,
    ) -> Result<Self> {
        Self::load_full_with_families(
            fast_model_path,
            primary_model_path,
            embed_model_path,
            None,
            windows,
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
        windows: SlotWindows,
        gpu_layers: Option<u32>,
        fast_family: ModelFamily,
        primary_family: ModelFamily,
        embed_family: ModelFamily,
        code_family: ModelFamily,
    ) -> Result<Self> {
        Self::load_full_inner(
            fast_model_path,
            primary_model_path,
            embed_model_path,
            code_model_path,
            windows,
            gpu_layers,
            fast_family,
            primary_family,
            embed_family,
            code_family,
            /* only_slot_distributable */ false,
        )
    }

    /// Load ONE model as this process's only slot, marked **distributable** —
    /// the shape a `sovereign-compute` child takes when it hosts the mesh's
    /// distributed primary.
    ///
    /// Two things make this different from [`load_dual`]:
    ///
    /// 1. **Distributable.** Every other slot is hardcoded non-distributable
    ///    (only the daemon's primary distributes, which keeps fast/embed/code
    ///    off the RPC path). A child that IS the primary needs its one slot to
    ///    take the distributed placement path, or it would load ~91 GB locally.
    /// 2. **No primary slot.** With `primary_path: None`, `Speed::Slow` routes
    ///    to this slot too (`select_slot`), so the single distributed model
    ///    answers every request — one weight copy, one KV cache.
    ///
    /// The load itself goes through the same `resolve_placement` the daemon
    /// uses, so the local-fit gate, the quorum gate, and the `-ot` override path
    /// all behave identically here. Pin the daemon's shard plan with
    /// [`pin_shard_plan`](crate::embedded::pin_shard_plan) BEFORE calling this,
    /// or the child will re-plan against post-warm VRAM and miss every cache.
    pub fn load_single_distributed(
        model_path: &Path,
        context_size: u32,
        gpu_layers: Option<u32>,
        family: ModelFamily,
    ) -> Result<Self> {
        Self::load_full_inner(
            model_path,
            None,
            None,
            None,
            // A compute child's one slot IS the only slot — there is no second
            // window to size, and `uniform` is the honest name for that rather
            // than an omission.
            SlotWindows::uniform(context_size),
            gpu_layers,
            family,
            ModelFamily::Unknown,
            ModelFamily::Unknown,
            ModelFamily::Unknown,
            /* only_slot_distributable */ true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn load_full_inner(
        fast_model_path: &Path,
        primary_model_path: Option<&Path>,
        embed_model_path: Option<&Path>,
        code_model_path: Option<&Path>,
        windows: SlotWindows,
        gpu_layers: Option<u32>,
        fast_family: ModelFamily,
        primary_family: ModelFamily,
        embed_family: ModelFamily,
        code_family: ModelFamily,
        only_slot_distributable: bool,
    ) -> Result<Self> {
        // Unpacked once, at the top, so every `ModelSlot::load` below names
        // WHICH window it is spending rather than reading one ambient scalar.
        let SlotWindows {
            primary: context_size,
            fast: fast_context_size,
        } = windows;
        let hardware = HardwareProfile::detect();
        // GPU offload layer count. Precedence: explicit config > env override
        // > hardware default (999 when a GPU is detected, else 0).
        //
        // `SOVEREIGN_GPU_LAYERS=<n>` (or `off`/`cpu` = 0) is the escape hatch
        // for machines where a GPU is detected but can't actually load the
        // model at full offload — e.g. some Vulkan/iGPU setups where 999
        // layers make `load_from_file` return a null result. Without it, a
        // detected-but-unusable GPU forces 999 with no way to fall back to CPU.
        let n_gpu_layers = gpu_layers
            .or_else(gpu_layers_from_env)
            .unwrap_or(hardware.recommended_gpu_layers);
        if let Some(env_n) = gpu_layers_from_env() {
            if gpu_layers.is_none() {
                tracing::info!(
                    n_gpu_layers = env_n,
                    "gpu layers overridden by SOVEREIGN_GPU_LAYERS"
                );
            }
        }

        let fast_quirks = fast_family.default_quirks();
        let primary_quirks = primary_family.default_quirks();
        let code_quirks = code_family.default_quirks();
        let embed_quirks = embed_family.default_quirks().embed;

        let mut backend = LlamaBackend::init()
            .map_err(|e| Error::Inference(format!("Failed to init llama backend: {e}")))?;
        // Route llama.cpp/ggml logs to `tracing`. By DEFAULT we surface
        // WARN/ERROR (so a model-load failure explains itself instead of
        // reaching the operator as a bare "null result from llama cpp")
        // while demoting INFO/DEBUG chatter to TRACE to stay quiet. The
        // old default was `void_logs()`, which silenced the failure reason
        // too — the exact trap behind field reports of an opaque null.
        //   SOVEREIGN_LLAMA_LOGS=1 → full ggml verbosity (deep debugging)
        //   SOVEREIGN_LLAMA_LOGS=0 → silence llama.cpp entirely (old behavior)
        //
        // `GGML_RPC_DEBUG` implies full verbosity. That is llama.cpp's OWN
        // documented knob for debugging an RPC worker, and on its own it only
        // opens the FIRST of three gates: it makes `LOG_DBG` in ggml-rpc.cpp
        // call `GGML_LOG_DEBUG`, but errors-only mode below then demotes
        // DEBUG to TRACE, and the daemon's target allowlist drops it again.
        // An operator who sets it and gets an empty log reads that as "the
        // worker received no traffic" — a false negative from a disconnected
        // instrument. Honour the var so it means what upstream says it means.
        // An explicit `SOVEREIGN_LLAMA_LOGS=0` still wins: asking for silence
        // is unambiguous, and should not be overridden by a debug var.
        crate::llama_logs::LlamaLogs::from_env().install(&mut backend);
        let backend = Arc::new(backend);

        // Distributable mesh worker: if SOVEREIGN_RPC_SERVE is set, expose this
        // node's local GPU to peers via an in-process RPC server (no separate
        // rpc-server binary to build/run). Safe here — the ggml device registry
        // is populated now that LlamaBackend is initialized.
        super::rpc_distribution::serve_rpc_worker_if_configured();

        // ─── P0.2 boot guard: refuse an aliased HUGE primary ─────────
        // With no distinct `fast`, fast_path == primary_path and the fast
        // slot below eagerly loads the FULL model 100% local — for a
        // can't-fit-one-box model that is a silent OOM at boot (observed:
        // 123GB peak on a 125GB box), and because fast holds the weights
        // `Arc`, the later distributed reload cannot free them. Refuse
        // with the fix in the message instead of OOMing.
        if matches!(primary_model_path, Some(p) if p == fast_model_path) {
            let model_bytes = super::model_slot::total_model_bytes(fast_model_path);
            if fast_alias_too_large(model_bytes, hardware.system_ram_bytes, alias_ceiling_env()) {
                return Err(Error::Inference(format!(
                    "models.primary ({}, {:.1} GB) is too large to double-load: with no \
                     distinct `models.fast` configured, the fast slot would eagerly load \
                     the full model locally at boot. Set `models.fast` to a small model \
                     (e.g. a 4B GGUF) — the primary then loads lazily and can distribute \
                     across the mesh. (Override ceiling: SOVEREIGN_FAST_ALIAS_MAX_MB.)",
                    fast_model_path.display(),
                    model_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                )));
            }
        }

        tracing::info!(slot = "fast", family = ?fast_family, "loading slot");
        let fast = Arc::new(ModelSlot::load(
            "fast",
            &backend,
            fast_model_path,
            // The fast slot's OWN window. Identical to the primary's unless
            // `[models].fast_context_size` says otherwise — see `SlotWindows`.
            fast_context_size,
            n_gpu_layers,
            // The fast slot stays local — never distributed across the mesh —
            // EXCEPT in a compute child loaded via `load_single_distributed`,
            // where this one slot IS the mesh's distributed primary.
            only_slot_distributable,
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
        let fast_arch = read_gguf_arch(&fast.model);
        // FastShort construction gate — decision matrix + history in
        // `gates::fast_short_gate` (tested weight-free). NARROWED
        // 2026-06-11 (qwen-MoE + MTP-by-name vetoes removed); qwen-MoE
        // RE-VETOED 2026-06-16 after the predicted `Decode Error -3`
        // bite-back on a ~10k-prefill + sustained-FastShort workload
        // (see the gate's doc). MTP-by-name stays cleared; recurrent
        // families (mamba/rwkv/deltanet/ssm) + qwen*moe are vetoed.
        // SOVEREIGN_FAST_SHORT_DISABLE=1 is the catch-all mitigation.
        // [[invariant_fast_short_recurrent_arch]]
        let gate = fast_short_gate(&fast_arch, only_slot_distributable, |k| {
            std::env::var(k).ok()
        });
        let (fast_short, fast_short_coalescer) = if gate == FastShortGate::DistributedChild {
            tracing::info!(
                slot = "fast_short",
                "skipped — distributed compute child: this slot IS the sharded \
                 primary and serves a single stream, so the contention FastShort \
                 exists for cannot occur; building it anyway cost 14.5 GB of GTT \
                 (measured 2026-08-02)"
            );
            (None, None)
        } else if gate == FastShortGate::Disabled {
            tracing::info!(
                slot = "fast_short",
                "skipped (SOVEREIGN_FAST_SHORT_DISABLE=1)"
            );
            (None, None)
        } else if gate == FastShortGate::UnsafeRecurrent {
            tracing::warn!(
                slot = "fast_short",
                arch = %fast_arch,
                model_id = %fast.model_id,
                "skipped — fast slot arch is an uncleared recurrent family \
                 (mamba/rwkv/deltanet/ssm); the FastShort burst repro has \
                 never run against it. All callers route to `fast`. To \
                 clear it: ./scripts/gate-repros.sh --fastshort <gguf> \
                 (see gates::fast_short_gate doc)."
            );
            (None, None)
        } else if gate == FastShortGate::UnsafeQwenMoeBiteback {
            tracing::warn!(
                slot = "fast_short",
                arch = %fast_arch,
                model_id = %fast.model_id,
                "skipped — qwen*moe FastShort `Decode Error -3` bite-back \
                 (re-vetoed 2026-06-16: a ~10k-token prefill + a background \
                 FastShort heartbeat corrupted shared decode state on APEX \
                 qwen35moe). All callers route to `fast` (n_seq_max=1) — \
                 forfeits the batched speedup, never crashes. Diagnostic \
                 override: SOVEREIGN_FAST_SHORT_FORCE=1 (see gates doc)."
            );
            (None, None)
        } else {
            if gate == FastShortGate::ForcedSafe {
                tracing::warn!(
                    slot = "fast_short",
                    arch = %fast_arch,
                    model_id = %fast.model_id,
                    "SOVEREIGN_FAST_SHORT_FORCE overrode an unsafe verdict — \
                     DIAGNOSTIC ONLY; expect Decode Error -3 on the first \
                     continuous-batched call unless upstream fixed n_rs_seq \
                     propagation"
                );
            }
            match ModelSlot::from_existing_model(
                "fast_short",
                &backend,
                Arc::clone(&fast.model),
                fast.model_id.clone(),
                fast.size_bytes,
                FAST_SHORT_N_CTX,
                FAST_SHORT_N_SEQ_MAX,
                FAST_SHORT_N_UBATCH,
                n_gpu_layers,
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
            // The sibling pool is a LOCAL weight-sharing optimization (one
            // resident copy, N contexts) — incoherent with distribution, which
            // splits the weights across remote workers. Keep it local.
            let primary_0 = ModelSlot::load(
                "primary",
                &backend,
                primary_path,
                context_size,
                n_gpu_layers,
                false,
            )?;
            let shared_model = Arc::clone(&primary_0.model);
            let shared_id = primary_0.model_id.clone();
            let shared_size = primary_0.size_bytes;
            let mut slots: Vec<Arc<ModelSlot>> = Vec::with_capacity(n);
            slots.push(Arc::new(primary_0));
            for i in 1..n {
                let sibling = ModelSlot::from_existing_model(
                    "primary_sibling",
                    &backend,
                    Arc::clone(&shared_model),
                    shared_id.clone(),
                    shared_size,
                    context_size,
                    /* n_seq_max */ 1,
                    /* n_ubatch */ 512,
                    n_gpu_layers,
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
                    "primary_alias",
                    &backend,
                    Arc::clone(&fast.model),
                    fast.model_id.clone(),
                    fast.size_bytes,
                    context_size,
                    /* n_seq_max */ 1,
                    /* n_ubatch */ 512,
                    n_gpu_layers,
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
            embed_model_id: embed_model_path
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().into_owned()),
            rerank_slot: std::sync::Mutex::new(None),
            hardware,
            fast_quirks,
            primary_quirks,
            extras: Arc::new(std::sync::RwLock::new(ExtrasState::new())),
            // Two independent renames landed on these two lines at once:
            // `fim_info` became `edit_slot` with the lane split, and the
            // `lazy_inflight` semaphore became a queue-gauged `SlotQueue`.
            // Both struct fields are declared above; take one from each.
            edit_slot: Arc::new(std::sync::RwLock::new(None)),
            lazy_queue: Arc::new(super::model_slot::SlotQueue::new("lazy", 1_000)),
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
        let preserved_budget = self.extras.read().map(|g| g.budget_bytes).unwrap_or(None);
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
                "primary",
                &self.primary_backend,
                &path,
                context_size,
                self.gpu_layers,
                false,
            ) {
                Ok(slot) => {
                    let model_id = slot.model_id.clone();
                    let arc = Arc::new(slot);
                    state.slots.insert(slot_name.clone(), Arc::clone(&arc));
                    state
                        .by_model_id
                        .insert(model_id.clone(), slot_name.clone());
                    state
                        .quirks
                        .insert(slot_name.clone(), default_quirks.clone());
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

    /// Install the dedicated code-editing slot (`[models.edit]` —
    /// `sovereign/docs/NEXT_EDIT.md`, `INLINE_COMPLETION.md`). Two
    /// modes (decision D8):
    ///
    /// - **Alias mode** — `section.path` resolves to the same GGUF the
    ///   always-resident fast slot already loaded (the daemon passes
    ///   `config.models.fast_path()` as `fast_path`, so "same file" is
    ///   decided against the *resolved* fast path, primary-fallback
    ///   included). No duplicate load: probe the fast slot's own model
    ///   for FIM markers and record an `EditSlotInfo` pointing at it.
    ///   Requests route via the named-slot match in
    ///   `select_slot_for_request` (`model_id == fast.model_id`).
    /// - **Dedicated mode** — any other path loads a pinned extras
    ///   slot under the reserved name [`EDIT_SLOT_NAME`] (never
    ///   idle-unloaded, never LRU-evicted — `extras_slot_is_evictable`).
    ///
    /// Marker detection is a vocab probe (plan correction #5):
    /// `ModelFamily` is `Unknown` on every production slot, so family
    /// tables can't decide the marker convention — the tokenizer can.
    ///
    /// **A failed probe no longer discards the slot.** It withholds the
    /// FIM lane only: the model still serves next-edit off its ordinary
    /// prompt surface, `/v1/completions` 503s, and the warn names the
    /// fix. Before this split, a marker-less model meant *no* editing
    /// assistance at all — the vocab probe silently gated a lane that
    /// never needed markers.
    pub fn install_edit_slot(&self, section: &EditSection, fast_path: &Path) -> Result<()> {
        // Path comparison tolerant to symlink/spelling differences:
        // direct equality first, canonicalized equality as fallback.
        let aliases_fast = section.path == fast_path
            || match (
                std::fs::canonicalize(&section.path),
                std::fs::canonicalize(fast_path),
            ) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            };

        let format = section.effective_next_edit_format();

        if aliases_fast {
            let style = crate::fim::detect_fim_style(&self.fast.model);
            let (next_edit, fim) = edit_lanes(style, format, Some(section));
            let info = EditSlotInfo {
                slot: "fast".to_string(),
                model_id: self.fast.model_id.clone(),
                aliased_to_fast: true,
                // Operator-chosen: they pointed `[models.edit].path`
                // here. Provenance, not capability — see `degraded`.
                degraded: false,
                next_edit,
                fim,
            };
            log_edit_slot_install(
                &info,
                "aliased to resident fast slot — no duplicate model load",
            );
            self.store_edit_slot_info(Some(info));
            return Ok(());
        }

        // Dedicated mode: load through the budget-aware extras path so
        // the edit slot's bytes count toward `max_extras_memory_gb`
        // (decision D2: pinned, but accounted).
        let budget = self.extras.read().map(|g| g.budget_bytes).unwrap_or(None);
        let ctx = section.context_size.unwrap_or(4096);
        let model_id = self.load_extra_with_budget(
            EDIT_SLOT_NAME.to_string(),
            section.path.clone(),
            ctx,
            budget,
        )?;

        let model = {
            let guard = self
                .extras
                .read()
                .map_err(|e| Error::Inference(format!("extras lock poisoned: {e}")))?;
            guard
                .slots
                .get(EDIT_SLOT_NAME)
                .map(|s| Arc::clone(&s.model))
        };
        let style = model.as_deref().and_then(crate::fim::detect_fim_style);
        let (next_edit, fim) = edit_lanes(style, format, Some(section));
        let info = EditSlotInfo {
            slot: EDIT_SLOT_NAME.to_string(),
            model_id: model_id.clone(),
            aliased_to_fast: false,
            degraded: false,
            next_edit,
            fim,
        };
        log_edit_slot_install(&info, "installed (dedicated, pinned)");
        self.store_edit_slot_info(Some(info));
        Ok(())
    }

    /// Install the automatic next-edit fallback when the operator
    /// configured no `[models.edit]` at all.
    ///
    /// This is the graceful-degradation route: a user who never picked
    /// an edit model still gets next-edit suggestions off weights that
    /// are already resident, for zero extra GB and zero download. The
    /// alternative for that user is not a worse suggestion — it is no
    /// feature. Measured on the 60-case gen bank (2026-08-07), a chat
    /// primary on `region_instruct` with thinking off scored 21/30
    /// useful with 0 wrong edits, against a 1.5B specialist's 19/30;
    /// the specialist's win is latency (~0.8 s vs ~2.6 s p95), which is
    /// exactly what the nudge tells the user they'd be buying.
    ///
    /// **Targets the fast slot, which IS the bubble-up.**
    /// `ModelsSection::fast_path()` returns the explicit `[models].fast`
    /// GGUF when set and the primary otherwise, and the engine loads
    /// *that* path into the always-resident fast slot at boot (aliasing
    /// primary's weights when the two coincide, so one VRAM copy). So
    /// this route is fast-when-there-is-a-fast,
    /// primary-when-there-is-not, and in **both** cases the weights are
    /// already resident. That is why there is no warmup call here and
    /// why an editing keystroke can never trigger a model load — a cold
    /// 35B primary would stall the editor ~10–20 s.
    ///
    /// FIM style is probed anyway rather than assumed absent: a user
    /// whose fast model happens to be a coder GGUF gets
    /// `/v1/completions` too, for free.
    ///
    /// Never overwrites an existing arrangement — `install_edit_slot`
    /// wins, because an operator-chosen model beats a fallback.
    pub fn install_fallback_next_edit_slot(&self, format: NextEditFormat) -> Result<()> {
        if self.edit_slot_info().is_some() {
            return Ok(());
        }
        let style = crate::fim::detect_fim_style(&self.fast.model);
        // No `[models.edit]` exists to read, so the shared defaults
        // stand in — see `setup_config::fim_defaults`.
        let (next_edit, fim) = edit_lanes(style, format, None);
        let info = EditSlotInfo {
            slot: "fast".to_string(),
            model_id: self.fast.model_id.clone(),
            aliased_to_fast: true,
            // Nobody chose this model for editing. This flag is the
            // whole basis of the nudge on /status.inference.edit.
            degraded: true,
            next_edit,
            fim,
        };
        log_edit_slot_install(
            &info,
            "no [models.edit] configured — next-edit falls back to the resident chat \
             model (degraded). Suggestions work; a dedicated edit model would be \
             faster. Set [models.edit].path to specialise.",
        );
        self.store_edit_slot_info(Some(info));
        Ok(())
    }

    /// Current code-editing serving arrangement, if any.
    pub fn edit_slot_info(&self) -> Option<EditSlotInfo> {
        self.edit_slot.read().ok().and_then(|g| g.clone())
    }

    fn store_edit_slot_info(&self, info: Option<EditSlotInfo>) {
        match self.edit_slot.write() {
            Ok(mut g) => *g = info,
            Err(e) => {
                tracing::warn!(target: "edit_slot", error = %e, "edit_slot lock poisoned")
            }
        }
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
    pub fn install_rerank_slot(&self, path: PathBuf, family: ModelFamily) -> Result<String> {
        let rerank_quirks = family.default_quirks().rerank;
        tracing::info!(
            slot = "rerank",
            path = %path.display(),
            family = ?family,
            "installing rerank slot"
        );
        let slot = RerankSlot::load(&self.primary_backend, &path, self.gpu_layers, rerank_quirks)?;
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
            "extra",
            &self.primary_backend,
            &path,
            context_size,
            self.gpu_layers,
            false,
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
        guard
            .by_model_id
            .insert(model_id.clone(), slot_name.clone());
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
            // Pinned slots (the FIM slot, decision D2) count toward
            // the budget but are never eviction candidates.
            .filter(|(name, _)| extras_slot_is_evictable(name))
            .filter_map(|(name, arc)| {
                if Arc::strong_count(arc) > 1 {
                    None // in-flight; preserve
                } else {
                    Some(EvictionCandidate {
                        slot_name: name.clone(),
                        last_used_ms: arc.last_used.load(std::sync::atomic::Ordering::Relaxed),
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

    /// Report the real in-memory residency of every slot — the ground
    /// truth behind `/status`'s `loaded` flag (the `ollama ps` analog),
    /// distinct from what is *configured* or *advertised*.
    ///
    /// Non-blocking by construction: the lazy Primary/Code slot is
    /// inspected via `try_lock` on the lightweight `primary_loaded_path`
    /// marker — never a blocking lock on the heavy `primary` weights
    /// mutex — so this NEVER forces a load and NEVER blocks a hot-swap.
    /// A contended marker yields `transitioning: true` instead of a
    /// guessed residency.
    pub fn resident_slots(&self) -> Vec<ResidentSlot> {
        fn stem(p: &std::path::Path) -> String {
            p.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string()
        }
        let mut out: Vec<ResidentSlot> = Vec::new();

        // ── fast (eager, always resident) ──
        out.push(ResidentSlot {
            role: "fast".to_string(),
            model_id: self.fast.model_id.clone(),
            resident: true,
            size_bytes: Some(self.fast.size_bytes),
            transitioning: false,
            placement: None,
        });

        // ── primary / code: ONE lazy hot-swap slot shared between the
        // Main responder and the Code specialist. Read the lightweight
        // loaded-path marker (not the weights mutex); at most one of the
        // two roles is resident at any instant. ──
        let (loaded_path, contended) = match self.primary_loaded_path.try_lock() {
            Ok(g) => (g.clone(), false),
            Err(_) => (None, true),
        };
        // Best-effort resident byte size of the current occupant —
        // non-blocking; `None` when the weights lock is momentarily busy.
        let occupant_bytes = self
            .primary
            .try_lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| s.size_bytes));
        for (role, configured) in [
            ("primary", self.primary_path.as_deref()),
            ("code", self.code_path.as_deref()),
        ] {
            if let Some(path) = configured {
                let resident = !contended && loaded_path.as_deref() == Some(path);
                out.push(ResidentSlot {
                    role: role.to_string(),
                    model_id: stem(path),
                    resident,
                    size_bytes: if resident { occupant_bytes } else { None },
                    transitioning: contended,
                    // The primary is the only distributable slot — surface its
                    // live placement (distributed vs local + the split) so
                    // `/status` states it outright.
                    placement: if role == "primary" && resident {
                        crate::embedded::rpc_distribution::last_primary_placement()
                    } else {
                        None
                    },
                });
            }
        }

        // ── embed (eager when configured) ──
        if let (Some(_), Some(id)) = (&self.embed_slot, &self.embed_model_id) {
            out.push(ResidentSlot {
                role: "embed".to_string(),
                model_id: id.clone(),
                resident: true,
                size_bytes: None, // EmbedSlot doesn't surface a byte count today
                transitioning: false,
                placement: None,
            });
        }

        // ── rerank (lazily installed; std Mutex) ──
        if let Ok(guard) = self.rerank_slot.try_lock() {
            if let Some(slot) = guard.as_ref() {
                out.push(ResidentSlot {
                    role: "rerank".to_string(),
                    model_id: slot.model_id.clone(),
                    resident: true,
                    size_bytes: None,
                    transitioning: false,
                    placement: None,
                });
            }
        }

        // ── extras (operator-declared, resident until unload/evict) ──
        if let Ok(guard) = self.extras.read() {
            let mut extras: Vec<ResidentSlot> = guard
                .slots
                .iter()
                .map(|(slot_name, slot)| ResidentSlot {
                    role: format!("extra:{slot_name}"),
                    model_id: slot.model_id.clone(),
                    resident: true,
                    size_bytes: Some(slot.size_bytes),
                    transitioning: false,
                    placement: None,
                })
                .collect();
            extras.sort_by(|a, b| a.role.cmp(&b.role));
            out.append(&mut extras);
        }

        // ── primary sibling pool (eager N copies of the primary) ──
        if self.primary_pool.is_some() {
            if let Some(pp) = self.primary_path.as_deref() {
                out.push(ResidentSlot {
                    role: "primary_pool".to_string(),
                    model_id: stem(pp),
                    resident: true,
                    size_bytes: None,
                    transitioning: false,
                    placement: None,
                });
            }
        }

        out
    }

    /// True when a Code specialist GGUF has been configured.
    /// Exposed on the concrete type (tests + direct constructors);
    /// trait-level consumers use `InferenceProvider::code_model_id()`.
    pub fn has_code_slot(&self) -> bool {
        self.code_path.is_some()
    }

    /// The MTP-capable `fast` slot is currently serving a request — its
    /// single inflight permit is taken (acquired for the whole decode,
    /// see the fast-slot dispatch in `complete`). A taken permit is the
    /// liveness signal that there is concurrent Fast demand a FastShort
    /// batch could coalesce; an available permit means the incoming
    /// request is a solo, latency-bound singleton. Drives
    /// [`should_batch_fast_short`] — the FastShort-as-overflow-lane rule.
    fn fast_slot_busy(&self) -> bool {
        self.fast.queue.available_permits() == 0
    }

    /// Acquire the lazy slot's single permit, reporting the wait.
    ///
    /// THE ONE PLACE a caller may take `lazy_inflight` (§10.6). It was
    /// taken at five sites, each a bare `.acquire_owned().await` with
    /// no timing, no depth and no event — and because the configured
    /// primary model is served from this slot, that made the queue
    /// every concurrent big-model chat turn actually waits in
    /// completely invisible host-side.
    ///
    /// `slot_label` distinguishes primary from the code model, which
    /// hot-swaps through the same slot; `phase` names the entry point.
    async fn acquire_lazy(&self, phase: &'static str) -> Result<super::model_slot::SlotPermit> {
        super::model_slot::acquire_with_queue_gauge(&self.lazy_queue, phase).await
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
            if let Some(mid) = request
                .model_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                if mid == self.fast.model_id {
                    // A short/Fast request addressed to the fast model by name rides
                    // the FastShort continuous-batched companion ONLY under contention
                    // (`fast` slot busy) — that's when a bulk fan-out (code-intel
                    // enrich, atlas phase 1b/3/5/6) has concurrent arrivals worth
                    // batching. A solo call goes to `fast` for speculative (MTP)
                    // decode instead: ~29% faster and no head-of-line stall (measured
                    // 2026-07-14). See `should_batch_fast_short`.
                    if should_batch_fast_short(
                        self.fast_short.is_some(),
                        self.fast_slot_busy(),
                        request,
                    ) {
                        return SlotTarget::FastShort;
                    }
                    if self.fast_short.is_some() && fits_fast_short(request) {
                        tracing::debug!(
                            "FastShort-eligible but `fast` slot idle — routing to Fast \
                             (MTP) for lower solo latency; FastShort is the batched \
                             overflow lane, engaged only under concurrent Fast demand"
                        );
                    }
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
            // FastShort is the batched overflow lane: only offer it to the
            // picker when the `fast` slot is already busy (concurrent Fast
            // demand). Idle → the request is a latency-bound singleton and
            // pick_slot routes it to `fast` (MTP). See `should_batch_fast_short`.
            self.fast_short.is_some() && self.fast_slot_busy(),
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
                        .filter(|(name, _)| extras_slot_is_evictable(name))
                        .filter_map(|(name, arc)| {
                            // strong_count == 1 means only the map
                            // holds this slot — safe to drop.
                            if Arc::strong_count(arc) > 1 {
                                return None;
                            }
                            let last_used =
                                arc.last_used.load(std::sync::atomic::Ordering::Relaxed);
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

    /// Force-reload the primary slot using the existing backend, even when the
    /// same model is already resident. Used by mesh RPC-worker auto-discovery:
    /// registering a newly-discovered worker device only affects the *next*
    /// load, so we drop + reload the primary so its layers redistribute across
    /// the updated device set (local GPU + all registered RPC workers). No-op
    /// when no primary is configured. Holds the lazy-slot permit so it can't
    /// race an in-flight request.
    pub async fn reload_primary(&self) -> Result<()> {
        let Some(target_path) = self.primary_path.clone() else {
            return Ok(());
        };
        // This path always targets the configured primary, so it is distributable.
        let distributable = true;
        let _permit = self.acquire_lazy("reload_primary").await?;
        let primary_lock = Arc::clone(&self.primary);
        let backend = Arc::clone(&self.primary_backend);
        let ctx_size = self.primary_ctx_size;
        let gpu_layers = self.gpu_layers;
        let loaded_path = Arc::clone(&self.primary_loaded_path);

        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut primary = primary_lock.blocking_lock();
            let mut loaded = loaded_path.blocking_lock();
            tracing::info!(
                slot = "primary",
                to = %target_path.display(),
                "reload_primary: redistributing across updated RPC device set"
            );
            // Drop the resident slot before reloading so the new load's device
            // enumeration includes the just-registered RPC worker(s).
            *primary = None;
            let started = Instant::now();
            let s = ModelSlot::load(
                "primary",
                &backend,
                &target_path,
                ctx_size,
                gpu_layers,
                distributable,
            )?;
            *primary = Some(s);
            *loaded = Some(target_path.clone());
            tracing::info!(
                slot = "primary",
                latency_ms = started.elapsed().as_millis() as u64,
                "reload_primary: slot reloaded across device set"
            );
            Ok(())
        })
        .await
        .map_err(|e| Error::Inference(format!("reload join failed: {e}")))?
    }
}

/// P0.2 boot-guard policy, pure for testability: an aliased fast+primary
/// model above the ceiling would double-book RAM at boot. Default ceiling
/// is 40% of system RAM — a model larger than that cannot afford to be
/// resident twice (eager fast copy + the distributed reload's local share).
/// `override_mb` (SOVEREIGN_FAST_ALIAS_MAX_MB) replaces the default when set.
fn fast_alias_too_large(model_bytes: u64, system_ram_bytes: u64, override_mb: Option<u64>) -> bool {
    let ceiling = match override_mb {
        Some(mb) => mb.saturating_mul(1024 * 1024),
        None => system_ram_bytes / 10 * 4,
    };
    model_bytes > ceiling
}

fn alias_ceiling_env() -> Option<u64> {
    std::env::var("SOVEREIGN_FAST_ALIAS_MAX_MB")
        .ok()
        .and_then(|v| v.trim().parse().ok())
}

/// Operator override for GPU offload layers: `SOVEREIGN_GPU_LAYERS=<n>`, or
/// `off`/`cpu` for pure-CPU inference. `None` when unset or unparseable, so
/// the caller falls back to the hardware default. The CPU escape hatch for
/// detected-but-unusable GPUs (see the call site in the engine constructor).
fn gpu_layers_from_env() -> Option<u32> {
    let raw = std::env::var("SOVEREIGN_GPU_LAYERS").ok()?;
    let v = raw.trim();
    if v.eq_ignore_ascii_case("off") || v.eq_ignore_ascii_case("cpu") {
        return Some(0);
    }
    v.parse::<u32>().ok()
}

#[cfg(test)]
mod fast_alias_guard_tests {
    use super::fast_alias_too_large;

    const GB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn ceiling_is_forty_percent_of_ram() {
        // 125GB box: 88GB model (the live 122B OOM case) refused, 35GB allowed.
        assert!(fast_alias_too_large(88 * GB, 125 * GB, None));
        assert!(!fast_alias_too_large(35 * GB, 125 * GB, None));
        // Boundary: exactly at the ceiling is allowed; just over is not.
        let ram = 100 * GB;
        assert!(!fast_alias_too_large(ram / 10 * 4, ram, None));
        assert!(fast_alias_too_large(ram / 10 * 4 + 1, ram, None));
    }

    #[test]
    fn override_replaces_the_default() {
        // Operator raises the ceiling: 88GB passes under a 100_000MB override.
        assert!(!fast_alias_too_large(88 * GB, 125 * GB, Some(100_000)));
        // Or tightens it: a 10GB model refused under a 4096MB override.
        assert!(fast_alias_too_large(10 * GB, 125 * GB, Some(4096)));
    }
}

#[cfg(test)]
mod edit_lane_tests {
    use super::edit_lanes;
    use sovereign_core::setup_config::fim_defaults;
    use sovereign_core::types::{FimStyle, NextEditFormat};

    /// Next-edit needs only a prompt dialect, so it is available on any
    /// loaded model. This is the whole basis of graceful degradation:
    /// before the lane split, a marker-less vocab meant no editing
    /// assistance at all.
    #[test]
    fn next_edit_lane_is_always_available() {
        for style in [None, Some(FimStyle::QwenCoder)] {
            let (next_edit, _) = edit_lanes(style, NextEditFormat::RegionInstruct, None);
            let lane = next_edit.expect("next-edit serves regardless of FIM markers");
            assert_eq!(lane.format, NextEditFormat::RegionInstruct);
        }
    }

    /// No markers, no FIM lane — and callers must therefore refuse
    /// rather than guess a convention (ARCH §18.3).
    #[test]
    fn fim_lane_is_withheld_without_markers() {
        let (_, fim) = edit_lanes(None, NextEditFormat::RegionInstruct, None);
        assert!(fim.is_none());
    }

    #[test]
    fn fim_lane_carries_the_probed_style() {
        let (_, fim) = edit_lanes(Some(FimStyle::Mellum), NextEditFormat::RegionInstruct, None);
        assert_eq!(fim.expect("markers found").style, FimStyle::Mellum);
    }

    /// With no `[models.edit]` to read, the fallback takes the SHARED
    /// defaults rather than a second copy of the same numbers — one
    /// decider for one policy (ARCH §10.6).
    #[test]
    fn fallback_fim_sampling_comes_from_the_shared_defaults() {
        let (_, fim) = edit_lanes(
            Some(FimStyle::QwenCoder),
            NextEditFormat::RegionInstruct,
            None,
        );
        let lane = fim.expect("markers found");
        assert_eq!(lane.max_tokens, fim_defaults::MAX_TOKENS);
        assert_eq!(lane.temperature, fim_defaults::TEMPERATURE);
        assert_eq!(lane.max_prefix_chars, fim_defaults::MAX_PREFIX_CHARS);
        assert_eq!(lane.max_suffix_chars, fim_defaults::MAX_SUFFIX_CHARS);
    }

    /// The dialect is carried through untouched — it is explicit
    /// config, never sniffed, because a wrong guess feeds a model a
    /// register it was never trained on.
    #[test]
    fn next_edit_format_is_carried_verbatim() {
        for format in [
            NextEditFormat::RegionInstruct,
            NextEditFormat::Zeta2,
            NextEditFormat::Sweep,
        ] {
            let (next_edit, _) = edit_lanes(None, format, None);
            assert_eq!(next_edit.expect("lane present").format, format);
        }
    }

    /// Only the chat-template dialect suppresses thinking; the raw
    /// dialects ride completion fine-tunes with no thinking phase.
    /// Pinned here because the consult lane reads this to decide a knob
    /// worth 21/30 vs 0/30.
    #[test]
    fn only_region_instruct_uses_the_chat_template() {
        assert!(NextEditFormat::RegionInstruct.uses_chat_template());
        assert!(!NextEditFormat::Zeta2.uses_chat_template());
        assert!(!NextEditFormat::Sweep.uses_chat_template());
    }
}

#[cfg(test)]
mod edit_slot_pin_tests {
    use super::{extras_slot_is_evictable, EDIT_SLOT_NAME, LEGACY_EDIT_SLOT_NAME};

    #[test]
    fn edit_slot_is_pinned() {
        assert!(!extras_slot_is_evictable(EDIT_SLOT_NAME));
        assert_eq!(EDIT_SLOT_NAME, "edit");
    }

    /// The rename from `"fim"` to `"edit"` must not un-pin the slot an
    /// already-running deployment installed under the old name — an
    /// evictable editing slot reloads mid-keystroke, which is the
    /// disqualifying behaviour decision D2 exists to prevent.
    #[test]
    fn legacy_edit_slot_name_stays_pinned() {
        assert!(!extras_slot_is_evictable(LEGACY_EDIT_SLOT_NAME));
        assert_eq!(LEGACY_EDIT_SLOT_NAME, "fim");
    }

    #[test]
    fn ordinary_extras_stay_evictable() {
        assert!(extras_slot_is_evictable("reasoning"));
        assert!(extras_slot_is_evictable("bulk"));
        // Near-miss spellings must not be caught by the pin.
        assert!(extras_slot_is_evictable("fim2"));
        assert!(extras_slot_is_evictable("FIM"));
        assert!(extras_slot_is_evictable("edit2"));
        assert!(extras_slot_is_evictable("Edit"));
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
        let system_chars = request
            .system_message
            .as_ref()
            .map(|s| s.len())
            .unwrap_or(0);

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
            let _permit = ModelSlot::acquire_inflight(&slot, "complete/extras").await?;
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
        // `SlotTarget::Primary`: each call picks the least-loaded
        // sibling and runs against its own `Mutex<SlotContext>`, so
        // `pool.len()` calls can be in `generate_sync` at once.
        // Eligibility is `pool_dispatch` — THE decider shared with the
        // streaming paths. Construction guarantees the pool is `None`
        // whenever `code_path` is configured (see
        // `load_full_with_families`); observing Code + pool anyway is
        // an invariant breach and refuses loudly instead of silently
        // hot-swapping the lazy slot.
        match pool_dispatch(self.primary_pool.is_some(), &target) {
            PoolDispatch::NoPool => {}
            PoolDispatch::NotPrimaryClass => {
                tracing::debug!(
                    target = ?target,
                    "sibling pool present but request is not primary-class — \
                     its own slot serves it"
                );
            }
            PoolDispatch::RefuseCodeIncompatible => {
                tracing::error!(slot = "code", pool = true, "{}", POOL_CODE_REFUSAL);
                return Err(Error::Inference(POOL_CODE_REFUSAL.to_string()));
            }
            PoolDispatch::Sibling => {
                let pool = self
                    .primary_pool
                    .as_ref()
                    .expect("PoolDispatch::Sibling implies a pool");
                let (sibling_idx, slot) = pool.pick();
                let pool_size = pool.len();
                let quirks = self.primary_quirks.clone();
                tracing::debug!(
                    slot = "primary",
                    sibling_idx,
                    pool_size,
                    "dispatching to primary sibling"
                );
                let _permit = ModelSlot::acquire_inflight(&slot, "complete/primary").await?;
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
        // (fall through: no pool, or the request's own slot serves it)

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
            // Distribute only when this lazy load targets the configured PRIMARY
            // model — the shared lazy slot also hot-swaps in the code model, which
            // must stay local (distributing a non-primary slot is the §3 crash).
            let distributable = self.primary_path.as_deref() == Some(target_path.as_path());
            // Throttle lazy-slot dispatch to 1 inflight at the async
            // layer; otherwise concurrent callers stack up blocking
            // threads waiting on `primary.blocking_lock()`. See the
            // `lazy_inflight` field doc.
            let _permit = self.acquire_lazy("complete/lazy").await?;
            let primary_lock = Arc::clone(&self.primary);
            let backend = Arc::clone(&self.primary_backend);
            let ctx_size = self.primary_ctx_size;
            let gpu_layers = self.gpu_layers;
            let last_use = Arc::clone(&self.last_primary_use);
            let loaded_path = Arc::clone(&self.primary_loaded_path);
            let request = request.clone();

            let result: Result<(CompletionResponse, &'static str)> =
                tokio::task::spawn_blocking(move || {
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
                        let s = ModelSlot::load(
                            "primary",
                            &backend,
                            &target_path,
                            ctx_size,
                            gpu_layers,
                            distributable,
                        )?;
                        *primary = Some(s);
                        *loaded = Some(target_path.clone());
                    }
                    drop(loaded);

                    let slot = primary.as_ref().unwrap();
                    let model_id = slot.model_id.clone();
                    let mut ctx_lock = slot.context.blocking_lock();

                    // Catch panics from llama.cpp (e.g., context overflow assertions).
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
                            tracing::warn!(slot = slot_label, error = %e, "inference error");
                            return Err(e);
                        }
                        Err(_) => {
                            tracing::error!(
                                slot = slot_label,
                                "inference panicked — likely context overflow"
                            );
                            return Err(Error::Inference(
                            "Model inference failed: prompt may exceed the model's context window. \
                             Try a shorter message or reduce conversation history.".to_string(),
                        ));
                        }
                    };

                    let latency_ms = start.elapsed().as_millis() as u64;

                    *last_use.blocking_lock() = Some(Instant::now());

                    Ok((
                        CompletionResponse {
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
                        },
                        slot_label,
                    ))
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
            let _permit = ModelSlot::acquire_inflight(&slot, "complete/fast").await?;
            let request = request.clone();
            let quirks = self.fast_quirks.clone();

            let result: Result<CompletionResponse> = tokio::task::spawn_blocking(move || {
                let start = Instant::now();
                let mut ctx_lock = slot.context.blocking_lock();

                // Catch panics from llama.cpp (e.g., context overflow assertions).
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
                        tracing::warn!(slot = "fast", error = %e, "inference error");
                        return Err(e);
                    }
                    Err(_) => {
                        tracing::error!(
                            slot = "fast",
                            "inference panicked — likely context overflow"
                        );
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
        // ONE routing dance, not two. Until noun-convergence rung nc-17 this
        // method was a ~285-line mirror of `complete_stream_with_finish`
        // below — same FastShort coercion, same extras/sibling-pool/lazy/fast
        // slot selection, same hot-swap and inflight-permit sequence — and
        // that copy's own doc comment said so. The pair had DRIFTED on the
        // branch that matters: the typed twin grew the Raw/FIM
        // `generate_stream_sync_fim` fork (INLINE_COMPLETION.md §4/D8) on its
        // extras and fast branches; this copy never did, so an inline-
        // completion request that arrived here took the templated full-clear
        // path and re-prefilled the whole window on every keystroke instead
        // of the typing delta. Delegating closes that by construction: there
        // is now one body, so a fork can no longer land in half of it.
        tracing::debug!(
            "inference.complete_stream: delegating to the typed path, adapting frames to text"
        );
        let frames = self.complete_stream_with_finish(request).await?;
        Ok(frames_to_text_stream(frames))
    }

    /// The one embedded streaming implementation: slot selection,
    /// hot-swap, inflight permits, and typed [`StreamFrame`]s carrying an
    /// accurate terminal [`FinishReason`].
    ///
    /// [`complete_stream`][Self::complete_stream] is a thin adapter over
    /// this. It used to be a second copy of everything below; nc-17 deleted
    /// that copy after finding the two had diverged on the Raw/FIM fork.
    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        // FastShort doesn't support streaming — the coalescer returns whole
        // responses, not token-by-token chunks, so coerce back onto Fast.
        // Without it, a Fast-speed request with small max_tokens and a
        // short prompt panics on the FastShort `unreachable!` arm
        // below and the stream never terminates (caught 2026-06-09 by
        // the desktop real-mode e2e smoke: the chat turn hung at
        // "Searching your knowledge…" while the worker thread died).
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
            let _permit =
                ModelSlot::acquire_inflight(&slot, "complete_stream_with_finish/extras").await?;
            let slot_label_owned = slot_name.clone();
            tokio::task::spawn_blocking(move || {
                let _permit = _permit;
                let start = Instant::now();
                slot.last_used
                    .store(now_millis(), std::sync::atomic::Ordering::Relaxed);
                let mut ctx_lock = slot.context.blocking_lock();
                // Raw (FIM) prompts take the LCP partial-keep sibling —
                // steady-state keystrokes re-prefill only the typing
                // delta instead of the full window (INLINE_COMPLETION.md
                // §4). Templated requests keep the legacy full-clear path.
                let gen_result = if matches!(request.prompt_shape, Some(PromptShape::Raw)) {
                    ModelSlot::generate_stream_sync_fim(
                        &slot.model,
                        &slot.model_id,
                        &mut ctx_lock,
                        &request,
                        &tx,
                        &quirks,
                        None,
                    )
                } else {
                    ModelSlot::generate_stream_dispatch(
                        &slot.model,
                        &slot.model_id,
                        &mut ctx_lock,
                        &request,
                        &tx,
                        &quirks,
                        None,
                    )
                };
                if let Err(e) = gen_result {
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

        // Primary sibling pool — the streaming twin of `complete()`'s pool
        // branch (order mesh-scale-t1-streampool; red §8.3.5: the lever moved
        // non-streaming 1.00→1.27 while streaming stayed at 1.02 because this
        // branch did not exist). Same eligibility decider as the
        // non-streaming path; the sibling's own context + inflight permit is
        // what lets a second stream's first token arrive while the first
        // stream is still decoding.
        match pool_dispatch(self.primary_pool.is_some(), &target) {
            PoolDispatch::NoPool => {}
            PoolDispatch::NotPrimaryClass => {
                tracing::debug!(
                    target = ?target,
                    "sibling pool present but request is not primary-class — \
                     its own slot serves it"
                );
            }
            PoolDispatch::RefuseCodeIncompatible => {
                tracing::error!(slot = "code", pool = true, "{}", POOL_CODE_REFUSAL);
                return Err(Error::Inference(POOL_CODE_REFUSAL.to_string()));
            }
            PoolDispatch::Sibling => {
                let pool = self
                    .primary_pool
                    .as_ref()
                    .expect("PoolDispatch::Sibling implies a pool");
                let (sibling_idx, slot) = pool.pick();
                let pool_size = pool.len();
                tracing::debug!(
                    slot = "primary",
                    sibling_idx,
                    pool_size,
                    streaming = true,
                    "dispatching to primary sibling"
                );
                let _permit =
                    ModelSlot::acquire_inflight(&slot, "complete_stream_with_finish/primary_pool")
                        .await?;
                let quirks = self.primary_quirks.clone();
                tokio::task::spawn_blocking(move || {
                    // Hold the permit for the streaming task's lifetime.
                    let _permit = _permit;
                    let start = Instant::now();
                    slot.last_used
                        .store(now_millis(), std::sync::atomic::Ordering::Relaxed);
                    let mut ctx_lock = slot.context.blocking_lock();
                    // No Raw/FIM fork here: FIM rides the fast/alias slot
                    // (INLINE_COMPLETION.md §4/D8), mirroring the lazy
                    // primary branch below which also dispatches
                    // unconditionally.
                    if let Err(e) = ModelSlot::generate_stream_dispatch(
                        &slot.model,
                        &slot.model_id,
                        &mut ctx_lock,
                        &request,
                        &tx,
                        &quirks,
                        None,
                    ) {
                        tracing::warn!(slot = "primary", sibling_idx, error = %e, "stream error");
                        let _ = tx.blocking_send(StreamFrame::Finish {
                            reason: FinishReason::Error(format!("{e}")),
                            usage: None,
                        });
                    } else {
                        tracing::info!(
                            slot = "primary",
                            sibling_idx,
                            pool_size,
                            latency_ms = start.elapsed().as_millis() as u64,
                            "inference.complete_stream_with_finish: done"
                        );
                    }
                });
                return Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)));
            }
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
            // Distribute only when this lazy load targets the configured PRIMARY
            // model — the shared lazy slot also hot-swaps in the code model, which
            // must stay local (distributing a non-primary slot is the §3 crash).
            let distributable = self.primary_path.as_deref() == Some(target_path.as_path());
            let _permit = self
                .acquire_lazy("complete_stream_with_finish/lazy")
                .await?;
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
                    match ModelSlot::load(
                        "primary",
                        &backend,
                        &target_path,
                        ctx_size,
                        gpu_layers,
                        distributable,
                    ) {
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
                if let Err(e) = ModelSlot::generate_stream_dispatch(
                    &slot.model,
                    &slot.model_id,
                    &mut ctx_lock,
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
            let _permit =
                ModelSlot::acquire_inflight(&slot, "complete_stream_with_finish/fast").await?;
            let quirks = self.fast_quirks.clone();
            tokio::task::spawn_blocking(move || {
                let _permit = _permit;
                let start = Instant::now();
                let mut ctx_lock = slot.context.blocking_lock();
                // Raw (FIM) prompts take the LCP partial-keep sibling —
                // the alias-mode FIM slot IS this fast slot
                // (INLINE_COMPLETION.md §4/D8).
                let gen_result = if matches!(request.prompt_shape, Some(PromptShape::Raw)) {
                    ModelSlot::generate_stream_sync_fim(
                        &slot.model,
                        &slot.model_id,
                        &mut ctx_lock,
                        &request,
                        &tx,
                        &quirks,
                        None,
                    )
                } else {
                    ModelSlot::generate_stream_dispatch(
                        &slot.model,
                        &slot.model_id,
                        &mut ctx_lock,
                        &request,
                        &tx,
                        &quirks,
                        None,
                    )
                };
                if let Err(e) = gen_result {
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
                 then retry."
                    .to_string(),
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
                         the embed model is incompatible."
                            .to_string(),
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
                 then retry."
                    .to_string(),
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
                     the embed model is incompatible."
                        .to_string(),
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
                 then retry."
                    .to_string(),
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
                     malformed or the embed model is incompatible."
                        .to_string(),
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

    fn embed_model_id(&self) -> String {
        self.embed_model_id
            .clone()
            .unwrap_or_else(|| "unknown".to_string())
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

    /// Live code-editing arrangement — see `install_edit_slot` and
    /// `install_fallback_next_edit_slot`. `None` only when there is no
    /// editing model at all; a present value may still withhold either
    /// lane.
    fn edit_slot_info(&self) -> Option<EditSlotInfo> {
        EmbeddedLlamaCpp::edit_slot_info(self)
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

    fn resident_slots(&self) -> Vec<ResidentSlot> {
        // Delegate to the inherent method (same disambiguation
        // rationale as `extras_inventory` above).
        Self::resident_slots(self)
    }

    async fn warmup_primary(&self) -> Result<()> {
        // No primary configured (single-model BYOM setup) — nothing
        // to warm; return success so callers don't have to special-
        // case the topology.
        let Some(target_path) = self.primary_path.clone() else {
            return Ok(());
        };
        // This path always targets the configured primary, so it is distributable.
        let distributable = true;

        // Acquire the same lazy-slot inflight permit `complete()` /
        // `complete_stream()` use, so a warmup can't race a hot-swap
        // out from under an in-flight request.
        let _permit = self.acquire_lazy("warmup_primary").await?;
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
            let s = ModelSlot::load(
                "primary",
                &backend,
                &target_path,
                ctx_size,
                gpu_layers,
                distributable,
            )?;
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

#[cfg(test)]
mod fast_short_queue_bound_tests {
    use super::*;

    /// A coalescer whose receiver is held and never read — the only way
    /// to genuinely FILL the queue without a resident model. Returned
    /// alongside the receiver so the channel stays open (dropping it
    /// would turn every `try_send` into `Closed`, which is a different
    /// error and would let this test pass for the wrong reason).
    fn full_coalescer(
        cap: usize,
        n_seq_max: usize,
    ) -> (
        FastShortCoalescer,
        tokio::sync::mpsc::Receiver<CoalescerJob>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel::<CoalescerJob>(cap);
        (FastShortCoalescer::with_sender(tx, cap, n_seq_max, 120), rx)
    }

    /// RED-FIRST (order mesh-scale-t0, item 4): "fill the coalescer,
    /// assert a shed". On the pre-fix `unbounded_channel` there was no
    /// cap to reach and `send` never refused, so no shed could EVER
    /// fire on this path — the assertion at the end had nothing that
    /// could produce it.
    #[test]
    fn a_full_coalescer_sheds_instead_of_growing() {
        let cap = 4;
        let (coalescer, _rx_held_open) = full_coalescer(cap, 2);

        // Fill it exactly to the bound. Every one of these is admitted;
        // `enqueue_job` hands back a receiver we deliberately keep so
        // the jobs stay queued.
        let mut admitted = Vec::new();
        for i in 0..cap {
            admitted.push(
                coalescer
                    .enqueue_job(CompletionRequest::default())
                    .unwrap_or_else(|e| panic!("request {i} should be admitted under cap: {e}")),
            );
        }
        assert_eq!(admitted.len(), cap);

        // One more. This is the request that used to disappear into an
        // unbounded queue.
        let err = coalescer
            .enqueue_job(CompletionRequest::default())
            .expect_err("a queue at its bound must shed, not grow");

        match err {
            Error::QueueShed {
                position,
                predicted_wait_ms,
                retry_after_secs,
            } => {
                assert_eq!(
                    position,
                    cap as u32 + 1,
                    "the shed must report the caller's real place in line"
                );
                // 5 in line at n_seq_max=2 → 3 batches ahead × 120ms.
                assert_eq!(predicted_wait_ms, 360);
                assert!(
                    retry_after_secs >= 1,
                    "Retry-After: 0 invites a hot loop against the host that just refused"
                );
            }
            other => panic!("expected the existing QueueShed shape, got: {other:?}"),
        }
    }

    /// Draining one job must make room again — a shed that latched would
    /// be worse than the leak it replaced.
    #[test]
    fn the_queue_reopens_once_the_drainer_takes_one() {
        let cap = 2;
        let (coalescer, mut rx) = full_coalescer(cap, 8);
        let _a = coalescer.enqueue_job(CompletionRequest::default()).unwrap();
        let _b = coalescer.enqueue_job(CompletionRequest::default()).unwrap();
        assert!(coalescer.enqueue_job(CompletionRequest::default()).is_err());

        let taken = rx.try_recv().expect("drainer takes one");
        drop(taken);
        assert!(
            coalescer.enqueue_job(CompletionRequest::default()).is_ok(),
            "the bound must be a moving window, not a latch"
        );
    }

    /// The cap is an operator dial, and a nonsense value must not
    /// silently produce a zero-capacity channel (which would shed
    /// every request forever).
    #[test]
    fn a_bad_cap_env_falls_back_to_the_default() {
        // Not touching the process env — the parse/filter policy is the
        // thing under test and it is expressed here exactly as
        // `fast_short_queue_cap` expresses it.
        let parse = |s: &str| {
            s.parse::<usize>()
                .ok()
                .filter(|n| *n > 0)
                .unwrap_or(FAST_SHORT_QUEUE_CAP_DEFAULT)
        };
        assert_eq!(parse("0"), FAST_SHORT_QUEUE_CAP_DEFAULT);
        assert_eq!(parse("banana"), FAST_SHORT_QUEUE_CAP_DEFAULT);
        assert_eq!(parse("128"), 128);
    }
}
