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
// Shim-restored 0.1.x-compatible method names. `LlamaModelExt` brings
// back `token_to_piece`, `token_to_piece_bytes`, `size`; the free
// functions cover the retired `chat_template` / `apply_chat_template_oaicompat`
// surface. See `crate::llama` module docs for the rollback story.
use crate::llama::{LlamaContextExt, LlamaModelExt};

use sovereign_core::error::Error;
use sovereign_core::model_family::{EmbedQuirks, ModelFamily, ModelQuirks, PoolingStrategy, RerankQuirks, ThinkingControl};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::*;
use sovereign_core::Result;

use crate::hardware::HardwareProfile;

// ─── ModelSlot ─────────────────────────────────────────────────

/// Per-slot inference mode. A sum type so the illegal hybrid state
/// the daemon hit on 2026-05-20 (`mtp_session = None` while the target
/// ctx still carries the `set_embeddings_pre_norm(true)` hook from a
/// prior `common_speculative_init`) is **structurally impossible**.
///
/// `MtpSession::new` mutates the target context — there is no
/// `common_speculative_uninit` upstream — so we cannot demote a
/// `Speculative` slot by nulling the session field; the target must be
/// rebuilt from scratch. Carrying that "must rebuild" invariant in the
/// type system means future maintainers can't reintroduce the half-
/// quarantined state. See [[invariant_mtp_demote_rebuilds_target]] and
/// `SlotContext::demote_to_single_token`.
///
/// Crate-private — exposed neither to other crates nor as a field on
/// `ModelSlot`. External callers continue to interact with `ModelSlot`
/// via `complete()` / `complete_stream()`.
enum SlotInferenceMode {
    /// Plain attention-only context. The default; used by every slot
    /// the daemon serves except for MTP-bearing ggufs that pass the
    /// upgrade probe.
    SingleToken {
        ctx: crate::llama::cpp::context::LlamaContext<'static>,
    },
    /// **MTP speculative decoding.** Two contexts (target + draft)
    /// backed by the same `Arc<LlamaModel>` plus a live `MtpSession`.
    ///
    /// Both contexts borrow from the same `LlamaModel` (the MTP gguf
    /// packs base layers + MTP-head tensors into one file) and both
    /// require `with_n_rs_seq(>= n_draft_max)` so partial KV rollback
    /// after rejected drafts succeeds — without it the target's
    /// recurrent layers refuse `seq_rm` and the next verify batch
    /// fails. See `generate_sync_mtp` for the loop, and
    /// [[project_mtp_invariants]] for the precise sequence of API
    /// calls the loop must make.
    ///
    /// **`session` invariant:** built at slot load via
    /// `MtpSession::new(&target, &draft, ...)`, which calls upstream's
    /// `common_speculative_init`. The constructor installs
    /// `set_embeddings_pre_norm(true)` on BOTH contexts so subsequent
    /// decodes compute the pre-norm hidden state that the draft side
    /// reads in `session.process(batch)`. Per-request,
    /// `generate_sync_mtp` rebuilds the session in place (see
    /// `rebuild_session_in_place`) because the session's internal
    /// draft-scheduler state persists across calls and pollutes
    /// subsequent generations — but the contexts are reused.
    Speculative {
        target_ctx: crate::llama::cpp::context::LlamaContext<'static>,
        draft_ctx: crate::llama::cpp::context::LlamaContext<'static>,
        session: MtpSession,
        n_draft_max: i32,
    },
}

/// Scalars + handles needed to rebuild a target context after a
/// speculative-mode failure. Stored on `SlotContext` so
/// `demote_to_single_token` can reconstruct the target without
/// re-plumbing them from the caller.
struct MtpRebuildParams {
    backend: Arc<LlamaBackend>,
    context_size: u32,
    n_threads: i32,
    n_batch: u32,
    n_ubatch: u32,
    /// Set iff the gguf is genuinely recurrent. The `with_n_rs_seq`
    /// call has to stay on the target ctx after demotion — recurrent
    /// architectures need it regardless of whether speculative
    /// decoding is active — or `seq_rm` on the recurrent layers fails
    /// at decode time.
    n_rs_seq: Option<u32>,
    /// Was the original target built on GPU? Preserved so a rebuild
    /// after demotion stays on the same backend; mismatching here
    /// would force a CPU↔GPU transition mid-life and almost certainly
    /// confuse the operator (silent perf cliff).
    used_gpu: bool,
}

struct SlotContext {
    mode: SlotInferenceMode,
    _model: Arc<LlamaModel>,
    /// **Prefix-cache bookkeeping.** The full token sequence
    /// currently resident in the target context's KV cache (positions
    /// 0..len). Used by `generate_sync` to compute the longest common
    /// prefix with each new request's tokenized prompt — when the
    /// prefix matches, we keep that portion of the KV cache and only
    /// decode the new tail (positions `lcp..new_len`).
    ///
    /// Empty when the cache is known to be clean (post-clear), or
    /// when the previous call cleared on error. Always updated AFTER
    /// a successful prompt decode so a mid-decode failure rewinds to
    /// "cache state unknown → clear next time" rather than leaving a
    /// stale fingerprint.
    ///
    /// Hoisted out of the mode-specific variants because both
    /// `SingleToken` and a future-prefix-cached `Speculative` path
    /// would use it identically, and a mode transition (demote)
    /// clears it unconditionally — single ownership for that reset.
    ///
    /// 2026-05-17 SEP-pipeline tuning: Phase 3 (cluster naming) has
    /// 4-7 calls per article sharing ~100% of a 4500-token prompt,
    /// and Phase 1 has 10 calls per article sharing the ~3000-token
    /// preamble + schema + exemplars (per-section text appended).
    /// Prefix-keep saves the prefill cost on every call after the
    /// first in each phase.
    cached_tokens: Vec<crate::llama::cpp::token::LlamaToken>,
    /// Architecture string from the gguf's `general.architecture`
    /// metadata key, read once at slot construction. Drives the
    /// `prefix_cache_safe` gate in `generate_sync` — recurrent-layer
    /// architectures cannot use the partial-keep prefix cache because
    /// `clear_kv_cache_seq` doesn't rewind their recurrent state.
    /// Empty string when metadata read failed (defensive — treat as
    /// "unknown" and fall back to the name-pattern heuristic).
    arch: String,
    /// Params used to rebuild the target ctx on demote. `Some` iff
    /// the slot was originally constructed as an MTP candidate (the
    /// only case where demote is possible); `None` on plain slots
    /// built via `from_existing_model` (FastShort) — those start in
    /// SingleToken and never transition.
    mtp_rebuild: Option<MtpRebuildParams>,
}

unsafe impl Send for SlotContext {}
unsafe impl Sync for SlotContext {}

impl SlotContext {
    /// Mutable handle to the target context — the one every decode
    /// (single-token or MTP-verify) runs against. The single
    /// accessor replaces direct `slot_ctx.ctx` field access that the
    /// pre-2026-05-20 code used.
    fn ctx_mut(&mut self) -> &mut crate::llama::cpp::context::LlamaContext<'static> {
        match &mut self.mode {
            SlotInferenceMode::SingleToken { ctx } => ctx,
            SlotInferenceMode::Speculative { target_ctx, .. } => target_ctx,
        }
    }

    /// True iff this slot was built (and probed) as speculative and
    /// is still in that mode. Replaces `mtp_session.is_some()` checks
    /// in dispatch.
    fn is_speculative(&self) -> bool {
        matches!(self.mode, SlotInferenceMode::Speculative { .. })
    }

    /// Rebuild the `MtpSession` in place on a `Speculative` slot.
    /// Used by `generate_sync_mtp` at every request boundary —
    /// `mtp_session_new` is cheap (no allocation, no model touch)
    /// and re-running `common_speculative_init` resets the session's
    /// internal draft-scheduler state, which otherwise desynchronises
    /// with the fresh prompt after ~1-2 sequential requests (observed
    /// 2026-05-17 as "Python sorting algorithm" responses to unrelated
    /// questions once the search-gym ran with `--replays > 1`).
    ///
    /// No-op + Err when called on `SingleToken` (dispatcher contract
    /// violation).
    fn rebuild_session_in_place(&mut self, model_id: &str) -> Result<()> {
        let SlotInferenceMode::Speculative {
            target_ctx,
            draft_ctx,
            session,
            n_draft_max,
        } = &mut self.mode
        else {
            return Err(Error::Inference(
                "rebuild_session_in_place: slot is in SingleToken mode — dispatcher contract violated"
                    .into(),
            ));
        };
        let rebuilt = MtpSession::new(&*target_ctx, &*draft_ctx, 1, *n_draft_max).map_err(|e| {
            Error::Inference(format!("MTP session rebuild failed: {e:?}"))
        })?;
        // Drop the old session *after* the rebuild succeeds. Holding
        // two sessions pointing at the same contexts simultaneously
        // would be UB inside `common_speculative_*`, so we use
        // `std::mem::replace` to swap atomically rather than `=
        // None; = Some(rebuilt)`.
        let _old = std::mem::replace(session, rebuilt);
        drop(_old);
        tracing::debug!(
            model_id = %model_id,
            n_draft_max = %*n_draft_max,
            "mtp: rebuilt session at request boundary"
        );
        Ok(())
    }

    /// **Demote a Speculative slot to SingleToken mode.** Replaces
    /// the pre-2026-05-20 `slot_ctx.mtp_session = None` quarantine
    /// path, which left the target context's
    /// `set_embeddings_pre_norm(true)` hook installed but disconnected
    /// from any draft-side consumer — every subsequent `ctx.decode`
    /// returned `Decode Error -3: unknown` for the rest of the
    /// slot's lifetime.
    ///
    /// Because `common_speculative_init` has no upstream inverse, the
    /// only safe demotion is to **drop both contexts and the session,
    /// then rebuild the target from scratch** using the params
    /// captured at slot construction. Cached prefix tokens are
    /// invalidated — the new ctx has an empty KV cache, so the next
    /// request pays one cold prefill (acceptable; recovery completes
    /// in under a second on any reasonable hardware).
    ///
    /// No-op + Err when invoked on `SingleToken` (defensive — the
    /// quarantine substring match in `generate_sync` only fires for
    /// MTP-side failures, so this branch shouldn't reach a non-MTP
    /// slot).
    fn demote_to_single_token(&mut self, model_id: &str) -> Result<()> {
        let rebuild = self
            .mtp_rebuild
            .as_ref()
            .ok_or_else(|| {
                Error::Inference(
                    "demote_to_single_token: slot has no MtpRebuildParams (was it ever built \
                     as speculative?)"
                        .into(),
                )
            })?;
        // Materialise the new target FIRST so a rebuild failure
        // surfaces before we discard the old (mutated, but still
        // serving) state. If `build_target_ctx_for_slot` fails the
        // slot is wedged anyway, but at least the error is loud and
        // diagnosable rather than silent.
        let new_target = build_target_ctx_for_slot(&self._model, rebuild)
            .map_err(|e| {
                Error::Inference(format!(
                    "demote_to_single_token: failed to rebuild target ctx: {e}"
                ))
            })?;
        // Replace the mode in one step. Dropping the old
        // `Speculative` variant runs the destructors for
        // `target_ctx`, `draft_ctx`, and `session` in declaration
        // order — session first (releases its C++-side hooks on the
        // target/draft), then draft, then target. That order matches
        // upstream's `mtp_session_free` contract: free the session
        // before its referenced contexts.
        let old_mode = std::mem::replace(
            &mut self.mode,
            SlotInferenceMode::SingleToken { ctx: new_target },
        );
        drop(old_mode);
        // Prefix cache is invalid — different ctx, different KV.
        self.cached_tokens.clear();
        tracing::warn!(
            model_id = %model_id,
            "slot demoted: Speculative → SingleToken (target ctx rebuilt). \
             Subsequent requests served via single-token decode for the rest \
             of this slot's lifetime; restart the daemon to retry MTP."
        );
        Ok(())
    }
}

/// Build a target `LlamaContext<'static>` for a slot given the
/// rebuild params captured at construction. Used both at initial
/// load AND by `demote_to_single_token` to replace a target whose
/// `set_embeddings_pre_norm` hook is poisoned after a failed
/// speculative upgrade.
///
/// SAFETY: the returned context borrows from `model` via a transmute
/// to `'static`. The caller MUST keep `model` alive (in practice via
/// the `_model: Arc<LlamaModel>` field on `SlotContext`) for the
/// lifetime of the returned context.
fn build_target_ctx_for_slot(
    model: &Arc<LlamaModel>,
    params: &MtpRebuildParams,
) -> Result<crate::llama::cpp::context::LlamaContext<'static>> {
    let mut ctx_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(params.context_size))
        .with_n_batch(params.n_batch)
        .with_n_ubatch(params.n_ubatch)
        .with_n_threads(params.n_threads)
        .with_n_threads_batch(params.n_threads)
        .with_offload_kqv(params.used_gpu);
    if let Some(n_rs_seq) = params.n_rs_seq {
        ctx_params = ctx_params.with_n_rs_seq(n_rs_seq);
    }
    unsafe {
        let model_ref: &'static LlamaModel =
            &*(Arc::as_ptr(model) as *const LlamaModel);
        model_ref
            .new_context(&params.backend, ctx_params)
            .map_err(|e| Error::Inference(format!("Failed to create target context: {e}")))
    }
}

/// Probe the speculative path with a 1-token synthetic prefill.
/// Catches the failure mode where `MtpSession::new` succeeded (so the
/// gguf appeared MTP-shaped at the C++ level) but the first real
/// `target.decode → session.process` round-trip fails — most often
/// because the gguf was MTP-named but lacks the actual head tensors,
/// or because an upstream `common_speculative_init` partially mutates
/// the target before erroring.
///
/// Without this probe (the pre-2026-05-20 behaviour), the failure
/// would only manifest on the first real request, at which point the
/// quarantine path would already have to deal with a mutated target.
/// Running the probe at construction shifts that failure to a
/// recoverable moment (caller drops the bad target + rebuilds fresh).
///
/// ~10-50ms cost on a warm slot. Acceptable; the alternative is a
/// silently-broken slot serving traffic.
///
/// Leaves both KV caches empty on success so the first real request
/// starts from a clean state.
fn probe_mtp_roundtrip(
    target_ctx: &mut crate::llama::cpp::context::LlamaContext<'_>,
    draft_ctx: &mut crate::llama::cpp::context::LlamaContext<'_>,
    session: &mut MtpSession,
    model: &LlamaModel,
) -> std::result::Result<(), String> {
    target_ctx.clear_kv_cache();
    draft_ctx.clear_kv_cache();
    let bos = model.token_bos();
    let prompt = [bos];
    let mut batch = LlamaBatch::new(1, 1);
    batch
        .add(bos, 0, &[0], true)
        .map_err(|e| format!("probe: batch add failed: {e}"))?;
    target_ctx
        .decode(&mut batch)
        .map_err(|e| format!("probe: target decode failed: {e}"))?;
    session
        .process(&batch)
        .map_err(|e| format!("probe: session.process failed: {e:?}"))?;
    session
        .begin(0, &prompt)
        .map_err(|e| format!("probe: session.begin failed: {e:?}"))?;
    target_ctx.clear_kv_cache();
    draft_ctx.clear_kv_cache();
    Ok(())
}

/// Attempt to upgrade a freshly-built target context into a
/// `Speculative` slot mode. On failure, hand the (now-mutated) target
/// back to the caller alongside the failure cause — the type signature
/// encodes the "target is no longer reusable as a SingleToken ctx"
/// invariant.
///
/// Contract: caller MUST drop the returned target on the `Err` path
/// and rebuild a fresh one for `SingleToken`. The pre-2026-05-20 code
/// silently reused the mutated target after nulling the session +
/// draft fields, which left the `set_embeddings_pre_norm(true)` hook
/// installed and orphaned — every subsequent decode returned -3.
fn try_upgrade_to_speculative(
    target: crate::llama::cpp::context::LlamaContext<'static>,
    model: &Arc<LlamaModel>,
    backend: &LlamaBackend,
    draft_params: LlamaContextParams,
    n_draft_max: i32,
) -> std::result::Result<
    SlotInferenceMode,
    (
        crate::llama::cpp::context::LlamaContext<'static>,
        Error,
    ),
> {
    let mut target_ctx = target;
    let draft_ctx_res = unsafe {
        let model_ref: &'static LlamaModel = &*(Arc::as_ptr(model) as *const LlamaModel);
        model_ref.new_context(backend, draft_params)
    };
    let mut draft_ctx = match draft_ctx_res {
        Ok(c) => c,
        Err(e) => {
            // Draft never built → target was never passed into
            // `common_speculative_init`, so it's still pristine. But
            // the contract says we hand it back regardless; callers
            // are expected to drop and rebuild on Err for safety.
            return Err((
                target_ctx,
                Error::Inference(format!("MTP draft context build failed: {e}")),
            ));
        }
    };
    let mut session = match MtpSession::new(&target_ctx, &draft_ctx, 1, n_draft_max) {
        Ok(s) => s,
        Err(e) => {
            return Err((
                target_ctx,
                Error::Inference(format!("MtpSession::new failed: {e:?}")),
            ));
        }
    };
    if let Err(probe_err) =
        probe_mtp_roundtrip(&mut target_ctx, &mut draft_ctx, &mut session, model)
    {
        return Err((
            target_ctx,
            Error::Inference(format!("MTP probe roundtrip failed: {probe_err}")),
        ));
    }
    Ok(SlotInferenceMode::Speculative {
        target_ctx,
        draft_ctx,
        session,
        n_draft_max,
    })
}

struct ModelSlot {
    model: Arc<LlamaModel>,
    context: Mutex<SlotContext>,
    model_id: String,
    /// In-memory footprint of the loaded model, in bytes — captured
    /// at load time. Sourced from `LlamaModel::size()` (the
    /// backend's own accounting of the buffers it allocated) with a
    /// gguf file-size fallback when `size()` reports zero. Used by
    /// the LRU eviction policy to decide which slots to drop when a
    /// new load would exceed the operator's `max_extras_memory_gb`
    /// budget.
    size_bytes: u64,
    /// Last time this slot was used to serve a request, expressed
    /// as milliseconds since UNIX epoch. Updated by `complete()` /
    /// `complete_stream()` after dispatch. The eviction policy
    /// orders slots by ascending `last_used` and drops the coldest
    /// first. `AtomicU64` so updates don't need a write lock on
    /// `ExtrasState`.
    last_used: std::sync::atomic::AtomicU64,
    /// Per-slot inflight gate — single permit. The slot's
    /// `Mutex<SlotContext>` already serialises generation, but the
    /// gate is acquired BEFORE `spawn_blocking` so concurrent
    /// callers don't each park a tokio blocking-pool thread (and
    /// each clone an 8 KB prompt into the closure) waiting their
    /// turn. With the gate, only one caller proceeds into the
    /// blocking task; the rest queue cheaply at the async layer.
    ///
    /// Observed motivator: KnowledgeView Phase 2 entity extraction
    /// fans out 4+ parallel `complete()` calls per chunk batch. All
    /// 4 used to land in `blocking_lock` on the slot mutex with
    /// 4 blocking threads held, while the user's first chat request
    /// queued behind them. With the permit, the queue forms in the
    /// async runtime, threads stay free.
    inflight: Arc<tokio::sync::Semaphore>,
}

/// Current millis-since-epoch as `u64`. Saturates at 0 if the
/// system clock is set before 1970 (impossible in practice;
/// guarded so we never panic).
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Hard wall-clock deadline for any single chat inference, in
/// seconds. Defends against pathological JSON-Schema mask states
/// where the in-house constraint enforcer's per-token mask
/// computation degrades from ~25 ms/token to 100s of ms/token —
/// the slot's `Mutex<SlotContext>` is held for the entire
/// generate_sync / generate_stream_sync call, so a stuck inference
/// blocks every other request to that slot until the deadline
/// fires and `ctx.clear_kv_cache()` runs.
///
/// Tunable via `SOVEREIGN_INFERENCE_TIMEOUT_SECS`. Default 300s —
/// generous enough that any legitimate Slow-slot Phase-1 call
/// (~60-160s observed worst-case under heavy grammar) finishes
/// with margin, short enough that a runaway mask-state is killed
/// before it pins the daemon for ~30 min and starves the mesh.
fn inference_deadline_secs() -> u64 {
    use std::sync::OnceLock;
    static DEADLINE: OnceLock<u64> = OnceLock::new();
    *DEADLINE.get_or_init(|| {
        std::env::var("SOVEREIGN_INFERENCE_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(300)
    })
}

/// Operator-declared primary-slot sibling count.
///
/// Set `SOVEREIGN_PRIMARY_SIBLINGS=N` with N≥2 to eager-load N
/// independent primary `LlamaContext`s sharing one `Arc<LlamaModel>`
/// (weights are not duplicated; each sibling pays only its own KV
/// cache). Non-streaming chat-completion dispatch then round-robins
/// across siblings so two callers can actually generate in parallel
/// instead of serialising on the lazy slot's single `Mutex<Context>`.
///
/// Returns `None` for absent / unparseable / `0` / `1` — in any of
/// those cases the daemon keeps its single-context lazy behaviour
/// and the change is a no-op.
///
/// Scope (intentional, experimental):
///   - Sibling pool covers `SlotTarget::Primary` non-streaming
///     completion only. Streaming, embed, rerank, and the lazy
///     warm-load helper continue to use the single lazy slot.
///   - Incompatible with `code_path`: the Code specialist relies on
///     hot-swapping the lazy slot's one resident model. Mixing the
///     two would require hot-swapping every sibling on a hint
///     transition. Startup fails with a clear message when both are
///     configured.
fn parse_primary_siblings(raw: Option<&str>) -> Option<std::num::NonZeroU32> {
    let n: u32 = raw?.trim().parse().ok()?;
    if n <= 1 {
        return None;
    }
    std::num::NonZeroU32::new(n)
}

fn primary_siblings_env() -> Option<std::num::NonZeroU32> {
    parse_primary_siblings(std::env::var("SOVEREIGN_PRIMARY_SIBLINGS").ok().as_deref())
}

/// Eager-loaded pool of primary contexts. All siblings share one
/// `Arc<LlamaModel>` (no weight duplication); each owns its own
/// `LlamaContext` + `Mutex<SlotContext>` + `inflight` permit, so
/// `pool.len()` callers can be in `generate_sync` concurrently.
///
/// Memory cost is therefore additive in KV cache only — for a
/// 35B-A3B Q4 at `n_ctx=32768`, that's a few GB per sibling on top
/// of the one-time ~22 GB weight load. On Strix Halo Vulkan
/// (124 GiB GTT) 2–4 siblings is comfortable; tighter hardware
/// should keep N low and watch resident-size in `daemon.out`.
///
/// Dispatch picks siblings round-robin. A more sophisticated
/// "least-loaded" policy could read each sibling's
/// `inflight.available_permits()`, but for the SEP-ingest workload
/// (uniform per-call cost) round-robin is already optimal and the
/// atomic counter is wait-free.
struct PrimarySiblingPool {
    slots: Vec<Arc<ModelSlot>>,
    next: std::sync::atomic::AtomicUsize,
}

impl PrimarySiblingPool {
    fn pick(&self) -> (usize, Arc<ModelSlot>) {
        let i = self
            .next
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            % self.slots.len();
        (i, Arc::clone(&self.slots[i]))
    }

    fn len(&self) -> usize {
        self.slots.len()
    }
}

impl ModelSlot {
    fn load(
        backend: &Arc<LlamaBackend>,
        model_path: &Path,
        context_size: u32,
        n_gpu_layers: u32,
    ) -> Result<Self> {
        // Operator escape hatch: `SOVEREIGN_FORCE_CPU_CHAT=1` forces
        // the chat slot fully onto CPU. The known-safe CPU config (per
        // the embed-slot invariant note) is the combination of:
        //   1. `n_gpu_layers = 0` on the model — weights stay in RAM.
        //   2. `with_offload_kqv(false).with_op_offload(false)` on the
        //      context — KV cache + ops also run on CPU.
        // Earlier versions of this gate set only (2), which left the
        // model weights on the GPU and still required Metal compute
        // pipelines for matmul. That hybrid path is what causes
        // `ggml_metal_encoder_set_pipeline` null-derefs to bypass the
        // gate entirely. Both must be set.
        //
        // Use this when chat decodes are crashing in ggml's Metal /
        // Vulkan backend on a particular quant/architecture combo and
        // you want to confirm whether the fault is in the GPU path or
        // upstream of it. Restart the desktop / daemon after setting.
        let force_cpu = std::env::var("SOVEREIGN_FORCE_CPU_CHAT")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let effective_gpu_layers = if force_cpu { 0 } else { n_gpu_layers };

        let model_params =
            LlamaModelParams::default().with_n_gpu_layers(effective_gpu_layers);

        let model = LlamaModel::load_from_file(backend, model_path, &model_params)
            .map_err(|e| Error::Inference(format!("Failed to load model: {e}")))?;

        let model_id = model_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let model = Arc::new(model);

        let n_threads = llama_threads_for_host();

        // GPU offload for chat slots. `with_n_gpu_layers(999)` alone
        // puts the model weights on the GPU but leaves the KV cache in
        // CPU memory and may route some compute ops back to CPU.
        // Setting `offload_kqv(true)` + `op_offload(true)` pins the
        // whole forward pass on the GPU.
        //
        // Measured impact (M2 Max, Qwen3.5-9B.Q8_0): decode tok/sec
        // ~49 without these flags, ~70-80 with them — the last 5s
        // of the Joan Robinson fix. Strix Halo Vulkan shows a similar
        // gap (embed ingest was 3.7 chunks/s CPU-only with weights on
        // GPU but KQV in RAM; see docs/TOOLBOX_SETUP.md §9).
        //
        // Try-GPU-then-CPU-fallback: a future llama.cpp upgrade could
        // reintroduce a crash on some quant/backend combo, and silent
        // fallback to CPU is what made the original embed regression
        // invisible. Always log the actual backend so operators can
        // spot a silent fallback.
        let wants_gpu = !force_cpu
            && cfg!(any(target_os = "macos", target_os = "linux"))
            && n_gpu_layers > 0;
        if force_cpu {
            tracing::warn!(
                model_id = %model_id,
                "SOVEREIGN_FORCE_CPU_CHAT set — chat slot loaded with n_gpu_layers=0; \
                 model weights and ops stay on CPU"
            );
        }
        // **MTP detection.** The Unsloth + community MTP-augmented
        // ggufs (e.g. Qwen3.6-A3B-MTP-Q6_K) pack base layers + MTP
        // prediction heads into a single file. Detect by `MTP`
        // substring in the gguf filename — matches the community
        // naming convention. False positives are non-fatal: the
        // draft-context constructor returns an error when the gguf
        // lacks MTP heads, we log + leave `draft_ctx = None`, and the
        // slot falls back to single-token decode for every request.
        //
        // The target context needs `with_n_rs_seq(>= n_draft_max)`
        // alongside the draft, otherwise partial KV rollback after
        // rejected drafts fails inside the target's recurrent layers
        // (Qwen3.6-A3B is hybrid Mamba/attention) and the next verify
        // batch hits an M-RoPE position assert. n_draft_max=3 is the
        // sweet spot per upstream; we set n_rs_seq=4 (one slot of
        // headroom) on both contexts.
        // MTP candidacy combines two signals:
        //
        // 1. Filename substring "MTP" — community naming convention
        //    for Unsloth + similar ggufs that pack draft heads into
        //    the file. Stable across architectures, no metadata
        //    required, but misses MTP-bearing ggufs the operator
        //    renamed or sourced from a vendor that doesn't follow
        //    the convention.
        //
        // 2. Architecture signal — `general.architecture` set to a
        //    recurrent-bearing family (qwen*moe / mamba / deltanet /
        //    rwkv / ssm). These are the families that ship with MTP
        //    draft heads in practice. Reading the arch metadata is
        //    cheap (one llama_model_meta_val_str call); the result
        //    also feeds `SlotContext.arch` for the prefix-cache gate.
        //
        // Either signal qualifies the slot as an MTP candidate. The
        // draft-context build below is the definitive probe: it
        // succeeds only if the model genuinely has MTP heads. False
        // positives are graceful (logged + fall back to single-token
        // decode), so we err on the side of attempting.
        let slot_arch = read_gguf_arch(&model);
        let mtp_by_name = model_id.to_lowercase().contains("mtp");
        let mtp_by_arch = is_recurrent_arch(&slot_arch);
        // `SOVEREIGN_MTP_DISABLE=1` short-circuits MTP at slot
        // construction (not just at dispatch). The runtime check at
        // `generate_sync` lower down also reads the same env var, but
        // skipping it here too means `common_speculative_init` never
        // runs — i.e. the `set_embeddings_pre_norm(true)` hook
        // doesn't get installed on the target ctx in the first place.
        // Pre-2026-05-20 the env var was dispatch-only, which left
        // arch-mis-detected slots stuck with the hook on the target
        // and dispatch routing around the (still-broken) draft path —
        // result: every target decode failed `rc=-3` because the
        // pre-norm consumer the hook expects was never wired up.
        // Slot-time gate is the correct boundary.
        let mtp_disabled_at_load = std::env::var("SOVEREIGN_MTP_DISABLE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let is_mtp_model = !mtp_disabled_at_load && (mtp_by_name || mtp_by_arch);
        tracing::info!(
            model_id = %model_id,
            arch = %slot_arch,
            mtp_by_name,
            mtp_by_arch,
            mtp_disabled_at_load,
            is_mtp_candidate = is_mtp_model,
            "MTP candidacy decided — draft-ctx build will confirm or fall back"
        );
        let mtp_n_rs_seq: u32 = 4;
        let build_ctx_params = |gpu: bool| {
            let mut p = LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(context_size))
                // Chat slots are serial: a Mutex<SlotContext> guards the
                // single context and generate_sync builds LlamaBatch with
                // n_seq=1. The llama-cpp-2 default n_seq_max=16 would
                // split n_ctx into 16 slots (e.g. 16384 → 1024 per seq)
                // and waste 15/16ths of the configured window. Pin to 1
                // so the full context_size is the usable per-request
                // window. (EmbedSlot keeps its own n_seq_max=16 — it
                // genuinely batches.)
                // MIGRATION 2026-05-17: .with_n_seq_max(...) retired in llama-cpp-4 0.2.x — see crate::llama
                // n_batch = context_size so the full context window is available
                // for prompt processing. llama.cpp automatically splits the batch
                // into n_ubatch=512 micro-batches for GPU/CPU kernel calls, so
                // memory pressure is unchanged — only the Rust-side assertion limit
                // is relaxed.
                .with_n_batch(context_size)
                .with_n_ubatch(512);
            if is_mtp_model {
                // Only the target context type stays Default here; the
                // draft context flips to Mtp below. `n_rs_seq` is what
                // enables partial seq_rm on recurrent layers — see
                // [[project_mtp_invariants]].
                p = p.with_n_rs_seq(mtp_n_rs_seq);
            }
            p
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
                .with_n_threads_batch(n_threads as i32)
                .with_offload_kqv(gpu)
                // MIGRATION 2026-05-17: .with_op_offload(...) retired in llama-cpp-4 0.2.x — see crate::llama
                // **2026-05-17 KV-Q8 experiment (REVERTED).** Tried
                // `with_type_k(Q8_0)` + `with_type_v(Q8_0)` to halve
                // KV bandwidth. SEP profile: total wall -5.6% (within
                // run variance), Phase 1 -9% throughput regression
                // (the dominant phase). Net negative on the workload
                // that matters. Likely the per-token quant/dequant
                // overhead on Vulkan eats the bandwidth savings.
                // Reverted to F16 KV. See git history for the
                // experiment.
        };

        let (ctx, used_gpu) = match if wants_gpu {
            unsafe {
                let model_ref: &'static LlamaModel =
                    &*(Arc::as_ptr(&model) as *const LlamaModel);
                model_ref
                    .new_context(backend, build_ctx_params(true))
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
                        model_id = %model_id,
                        "GPU chat-slot context failed, falling back to CPU"
                    );
                }
                let ctx = unsafe {
                    let model_ref: &'static LlamaModel =
                        &*(Arc::as_ptr(&model) as *const LlamaModel);
                    model_ref
                        .new_context(backend, build_ctx_params(false))
                        .map_err(|e| {
                            Error::Inference(format!("Failed to create context: {e}"))
                        })?
                };
                (ctx, false)
            }
        };

        let compute_backend = if used_gpu {
            gpu_backend_label()
        } else {
            embed_compute_backend_label()
        };

        // **Speculative-mode upgrade — probed at construction.**
        //
        // For an MTP-candidate slot, we try to upgrade the freshly-
        // built target into a `Speculative` mode by attaching a draft
        // context + `MtpSession` AND verifying the path actually
        // round-trips end-to-end. The probe (a 1-token synthetic
        // prefill through `target.decode → session.process →
        // session.begin`) is what catches "gguf is MTP-named but
        // lacks head tensors" — pre-2026-05-20 that case slipped
        // through `MtpSession::new` and only blew up at first real
        // request, leaving the target ctx with an orphaned
        // `set_embeddings_pre_norm(true)` hook and `Decode Error -3`
        // for the rest of the slot's life.
        //
        // On any Err path inside `try_upgrade_to_speculative`, the
        // contract is: the (now-possibly-mutated) target ctx is
        // returned via the Err payload, the caller drops it, and we
        // rebuild a fresh target for `SingleToken` mode. That's the
        // structural fix for the "no upstream
        // `common_speculative_uninit`" problem — we never reuse a
        // ctx that may have been touched by a failed
        // `common_speculative_init`.
        //
        // n_draft_max = 3 matches upstream's Qwen3.6-A3B-MTP sweet
        // spot. Overridable via `SOVEREIGN_MTP_DRAFT_MAX` for
        // measurement passes; n_rs_seq=4 on both contexts gives one
        // slot of headroom for partial KV rollback.
        let n_draft_max: i32 = std::env::var("SOVEREIGN_MTP_DRAFT_MAX")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|n: &i32| *n >= 1 && *n < (mtp_n_rs_seq as i32))
            .unwrap_or(3);
        let rebuild_params = MtpRebuildParams {
            backend: Arc::clone(backend),
            context_size,
            n_threads: n_threads as i32,
            n_batch: context_size,
            n_ubatch: 512,
            n_rs_seq: if is_mtp_model { Some(mtp_n_rs_seq) } else { None },
            used_gpu,
        };
        let mode: SlotInferenceMode = if is_mtp_model {
            let draft_params = LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(context_size))
                .with_ctx_type(LlamaContextType::Mtp)
                .with_n_rs_seq(mtp_n_rs_seq)
                .with_n_batch(context_size)
                .with_n_ubatch(512)
                .with_n_threads(n_threads as i32)
                .with_n_threads_batch(n_threads as i32)
                .with_offload_kqv(used_gpu);
            match try_upgrade_to_speculative(
                ctx,
                &model,
                backend,
                draft_params,
                n_draft_max,
            ) {
                Ok(mode) => {
                    tracing::info!(
                        model_id = %model_id,
                        n_rs_seq = mtp_n_rs_seq,
                        n_draft_max,
                        "MTP speculative mode active — draft ctx + session built and probe \
                         round-trip succeeded"
                    );
                    mode
                }
                Err((bad_target, e)) => {
                    // Drop the (possibly mutated) target. The
                    // type-system contract on `try_upgrade_to_speculative`
                    // is what makes this branch the only safe place
                    // to do so — the bad target cannot leak into a
                    // SingleToken slot.
                    drop(bad_target);
                    tracing::warn!(
                        model_id = %model_id,
                        error = %e,
                        "MTP upgrade probe failed — rebuilding a fresh target ctx and \
                         falling back to single-token decode for this slot. (gguf likely \
                         lacks MTP heads, or upstream rejected the pairing at \
                         common_speculative_init.)"
                    );
                    let fresh = build_target_ctx_for_slot(&model, &rebuild_params)
                        .map_err(|e| {
                            Error::Inference(format!(
                                "Failed to rebuild target ctx after MTP upgrade failure: {e}"
                            ))
                        })?;
                    SlotInferenceMode::SingleToken { ctx: fresh }
                }
            }
        } else {
            SlotInferenceMode::SingleToken { ctx }
        };

        tracing::info!(
            model_id = %model_id,
            params = model.n_params(),
            layers = model.n_layer(),
            size_mb = model.size() / (1024 * 1024),
            n_threads,
            compute_backend,
            mtp = matches!(&mode, SlotInferenceMode::Speculative { .. }),
            "ModelSlot::load — slot ready"
        );

        // Slot footprint for the LRU eviction policy. Prefer
        // `LlamaModel::size()` — that's the in-memory size the
        // backend actually allocated (covers GPU offload + any
        // backend-specific padding) — and fall back to the gguf
        // file size when the backend reports zero. For most quants
        // these track within ~1%, but on Vulkan with split offload
        // the loaded size can differ enough to swing budget
        // accounting on tight hardware.
        let loaded_size = model.size();
        let file_size = std::fs::metadata(model_path)
            .map(|m| m.len())
            .unwrap_or(0);
        let size_bytes = if loaded_size > 0 {
            loaded_size
        } else {
            file_size
        };

        Ok(Self {
            model: model.clone(),
            context: Mutex::new(SlotContext {
                mode,
                _model: model,
                cached_tokens: Vec::new(),
                arch: slot_arch,
                mtp_rebuild: if is_mtp_model {
                    Some(rebuild_params)
                } else {
                    None
                },
            }),
            model_id,
            size_bytes,
            last_used: std::sync::atomic::AtomicU64::new(now_millis()),
            inflight: Arc::new(tokio::sync::Semaphore::new(1)),
        })
    }

    /// Build a sibling slot that shares another slot's loaded model
    /// but stands up its own `LlamaContext` with different sequence
    /// parameters. Used for the FastShort companion to the Fast slot
    /// — same weights, but `n_seq_max=8 / n_ctx=16384 / n_ubatch=2048`
    /// so continuous-batched short calls (Phase 1b coverage, Phase 3
    /// cluster naming, Phase 5 positions, Phase 6 tensions) get the
    /// 2.1–2.8× wall-clock speedup measured in
    /// `bench_decode_batch.rs`. The original Fast slot keeps
    /// `n_seq_max=1` so long-context Phase 1 chapter ingestion still
    /// has the full window per call.
    ///
    /// Each FastShort sequence's KV cache slice is `n_ctx /
    /// n_seq_max` tokens (i.e. 2048 for the default config). 97.4%
    /// of wiki-tier2-500 sections fit inside that budget after
    /// Phase 1b prompt overhead; the overflow tail falls through to
    /// the FastLong claim via OICP-v0.3 §2.4 hard gates.
    fn from_existing_model(
        backend: &Arc<LlamaBackend>,
        model: Arc<LlamaModel>,
        model_id: String,
        size_bytes: u64,
        context_size: u32,
        n_seq_max: u32,
        n_ubatch: u32,
    ) -> Result<Self> {
        let force_cpu = std::env::var("SOVEREIGN_FORCE_CPU_CHAT")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let wants_gpu = !force_cpu && cfg!(any(target_os = "macos", target_os = "linux"));

        let n_threads = llama_threads_for_host();

        let build_ctx_params = |gpu: bool| {
            LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(context_size))
                // Re-introduced in the sovereign vendor of llama-cpp-4
                // (see workspace `[patch.crates-io]`). Without this the
                // C-side context defaults to n_seq_max=1 and the
                // continuous-batched coalescer fails with "Decode
                // Error -1: n_tokens == 0" the moment a batch carries
                // a second sequence — forensic trace 2026-05-18.
                .with_n_seq_max(n_seq_max)
                .with_n_batch(context_size)
                .with_n_ubatch(n_ubatch)
                .with_n_threads(n_threads as i32)
                .with_n_threads_batch(n_threads as i32)
                .with_offload_kqv(gpu)
                // MIGRATION 2026-05-17: .with_op_offload(...) retired in llama-cpp-4 0.2.x — see crate::llama
        };

        let (ctx, used_gpu) = match if wants_gpu {
            unsafe {
                let model_ref: &'static LlamaModel =
                    &*(Arc::as_ptr(&model) as *const LlamaModel);
                model_ref
                    .new_context(backend, build_ctx_params(true))
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
                        model_id = %model_id,
                        "FastShort GPU context failed, falling back to CPU"
                    );
                }
                let ctx = unsafe {
                    let model_ref: &'static LlamaModel =
                        &*(Arc::as_ptr(&model) as *const LlamaModel);
                    model_ref
                        .new_context(backend, build_ctx_params(false))
                        .map_err(|e| {
                            Error::Inference(format!(
                                "FastShort context creation failed: {e}"
                            ))
                        })?
                };
                (ctx, false)
            }
        };

        tracing::info!(
            model_id = %model_id,
            n_seq_max,
            n_ctx = context_size,
            n_ubatch,
            used_gpu,
            "FastShort slot ready — continuous-batched short-call companion to Fast"
        );

        let arch = read_gguf_arch(&model);
        Ok(Self {
            model: Arc::clone(&model),
            context: Mutex::new(SlotContext {
                // Sibling slots (FastShort) are built off the primary
                // model with different `n_seq_max`/`n_ubatch` for
                // continuous-batched short calls. MTP is gguf-specific
                // and only meaningful for serial decode on the primary
                // slot, so the sibling stays vanilla — fixed at
                // `SingleToken` for life, no `mtp_rebuild` params
                // captured because demotion can never fire on this
                // shape.
                mode: SlotInferenceMode::SingleToken { ctx },
                _model: model,
                cached_tokens: Vec::new(),
                arch,
                mtp_rebuild: None,
            }),
            model_id,
            size_bytes,
            last_used: std::sync::atomic::AtomicU64::new(now_millis()),
            inflight: Arc::new(tokio::sync::Semaphore::new(1)),
        })
    }

    fn generate_sync(
        model: &LlamaModel,
        model_id: &str,
        slot_ctx: &mut SlotContext,
        request: &CompletionRequest,
        quirks: &ModelQuirks,
    ) -> Result<(String, usize, usize)> {
        // **MTP dispatch (Phase A widened 2026-05-17).** Route to
        // MTP when the slot has a draft context AND the request has
        // no tools. Tools stay gated out because the tool-call
        // JSON-depth tracker that stops generation at the close
        // brace lives in the single-token path; MTP would emit
        // unbounded text.
        //
        // `structured_output` is intentionally NOT gated. The
        // `ConstrainedSampler::sample()` applies the JsonConstraint
        // mask at every verify position, and the FSM state machine
        // advances in lockstep with what we emit (accepted drafts +
        // bonus next_token, never rejected drafts). The likely cost
        // is reduced acceptance rate — the MTP head proposes drafts
        // without seeing the schema, so masked-out drafts get
        // rejected at the target's masked sample step. That's a
        // graceful degradation we want to MEASURE, not avoid.
        // See `has_schema` / `has_tools` fields on the MTP
        // end-of-generation log to decompose acceptance rate by
        // request shape.
        //
        // Disable via SOVEREIGN_MTP_DISABLE=1 for A/B measurement.
        let mtp_disabled = std::env::var("SOVEREIGN_MTP_DISABLE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if !mtp_disabled
            && slot_ctx.is_speculative()
            && request.tools.as_ref().is_none_or(|t| t.is_empty())
        {
            // Try MTP. On prefill-decode failure, quarantine MTP for
            // this slot's lifetime and fall through to single-token
            // decode. The fallback is self-healing without bench-side
            // intervention: chat traffic auto-recovers, batched
            // workloads (entity_extraction, atlas Phase 1b) finish
            // their run on the non-MTP path.
            //
            // Failure mode this catches: observed 2026-05-20 on
            // conversations-anthropic ingest, the Qwopus3.5-9B-MTP
            // slot failed every Phase 1b batch after the first with
            // `MTP prefill decode failed: Decode Error -3: unknown`.
            // First call succeeded; all subsequent calls failed at
            // prefill. The MtpSession rebuild + common_speculative_init
            // cycle isn't idempotent across reused contexts under
            // repeated short-prompt calls — upstream-side bug we
            // can't reach from Rust. Quarantining the slot's MTP
            // turns the 99.96% loss into a non-issue (single-token
            // decode is ~10% slower than MTP, still serves traffic).
            //
            // The quarantine is per-slot, per-process: operator
            // restart restores MTP. Add the env var
            // `SOVEREIGN_MTP_QUARANTINE_DISABLE=1` to opt out
            // (debugging MTP-stability work).
            match Self::generate_sync_mtp(model, model_id, slot_ctx, request, quirks) {
                Ok(r) => return Ok(r),
                Err(e) => {
                    let msg = e.to_string();
                    // The three error sites that signal "MTP draft side
                    // is broken on this slot, single-token decode still
                    // works": target prefill decode, prefill batch add,
                    // and `session.process(&prefill)` (the post-prefill
                    // hook into the draft side). The original two were
                    // caught here on 2026-05-17; `process(prefill)` was
                    // missed and caused the 2026-05-20 conversations-
                    // anthropic Phase 2b cluster-labeling cascade —
                    // every cluster failed, the loop logged + retried
                    // every 2-3s for hours, no surfacing to the
                    // operator. Add it to the set so the slot
                    // quarantines on the first failure instead.
                    let is_prefill_error = msg.contains("MTP prefill decode failed")
                        || msg.contains("MTP prefill batch add failed")
                        || msg.contains("MTP process(prefill) failed");
                    let quarantine_disabled = std::env::var("SOVEREIGN_MTP_QUARANTINE_DISABLE")
                        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                        .unwrap_or(false);
                    if is_prefill_error && !quarantine_disabled {
                        tracing::warn!(
                            model_id = %model_id,
                            error = %msg,
                            "MTP prefill failed — quarantining MTP on this slot and \
                             falling through to single-token decode. Restart the daemon \
                             to re-enable; set SOVEREIGN_MTP_QUARANTINE_DISABLE=1 to \
                             surface the error instead."
                        );
                        // Demote structurally: drop session + draft +
                        // mutated-target and rebuild a fresh target
                        // for SingleToken mode. The pre-2026-05-20
                        // path just did `mtp_session = None` and left
                        // the target ctx in place — but
                        // `common_speculative_init` had already
                        // installed `set_embeddings_pre_norm(true)`
                        // on it, with no upstream uninit, so every
                        // subsequent decode returned `Decode Error -3`
                        // for the rest of the slot's lifetime.
                        // `demote_to_single_token` is the only safe
                        // transition. Defence-in-depth: probe at
                        // construction catches most cases, this
                        // catches the surprise mid-life ones.
                        slot_ctx.demote_to_single_token(model_id)?;
                        // Fall through to the non-MTP path below.
                    } else {
                        return Err(e);
                    }
                }
            }
        }

        // Capability gate: the prefix-cache partial-keep path
        // (clear_kv_cache_seq(lcp, None) + tail-prefill) does not
        // work on hybrid Mamba/SSM models. `clear_kv_cache_seq`
        // succeeds against the attention layers but doesn't rewind
        // the recurrent layers' position-by-position hidden state,
        // so the subsequent tail decode is rejected upstream
        // (llama_decode returns -1; the Rust binding statically
        // labels this "n_tokens == 0" even though the batch has
        // tokens — verified 2026-05-19 with batch_n_tokens=1 and
        // batch_n_tokens=50 both failing identically). The bug is
        // general to any partial-keep on this model class, not an
        // edge case of small tails.
        //
        // Detector: a successfully-built MtpSession on the slot.
        // MTP requires `with_n_rs_seq > 1`, which means the model
        // has recurrent layers; the inverse is also true in our
        // build (mtp_session is Some iff the slot was constructed
        // with the MTP gguf and the speculative init succeeded).
        // Pure-attention slots (mtp_session is None) keep the
        // partial-keep path — they were the original target of the
        // 2026-05-17 SEP-pipeline tuning and the bug doesn't affect
        // them.
        //
        // Non-MTP hybrid families (Qwen3-MoE Gated DeltaNet,
        // Mamba+MoE variants without MTP draft heads) hit the same
        // recurrent-state bug — their gguf has recurrent layers but
        // the slot was constructed without an MTP draft, so the MTP
        // dispatch above doesn't catch them. Confirmed 2026-05-20 on
        // Qwen3.6-35B-A3B-Claude-4.7-Opus-Reasoning-Distilled-APEX:
        // every 2nd request failed `Decode Error -1: n_tokens == 0`
        // at the post-cache tail decode; disabling partial-keep
        // restored throughput.
        //
        // Detection ladder (data first, ARCH §6):
        // 1. The slot's gguf `general.architecture` metadata, read
        //    once at construction and stored on `SlotContext.arch`.
        // 2. `ModelQuirks::has_recurrent_layers` — the per-family
        //    declaration in `model_family.rs`. Used when arch
        //    metadata is empty (older gguf exports strip the field,
        //    etc). Replaced the pre-2026-05-20 name-substring
        //    heuristic that pattern-matched "qwen3.5" / "qwen3.6" /
        //    "qwopus" — those decisions are now declared once on the
        //    family, not re-derived from filenames at every dispatch.
        let arch_says_recurrent = is_recurrent_arch(&slot_ctx.arch);
        let quirks_say_recurrent =
            slot_ctx.arch.is_empty() && quirks.has_recurrent_layers;
        let speculative_active = slot_ctx.is_speculative();
        let prefix_cache_safe =
            !speculative_active && !arch_says_recurrent && !quirks_say_recurrent;
        tracing::debug!(
            model = %model_id,
            arch = %slot_ctx.arch,
            arch_says_recurrent,
            quirks_say_recurrent,
            mtp_session = speculative_active,
            prefix_cache_safe,
            "prefix_cache: gate decision"
        );

        // Split-borrow `ctx_mut()` + `cached_tokens` via an inline
        // destructuring match on `&mut slot_ctx.mode`. The function
        // body uses both as `&mut` simultaneously, which Rust's
        // split-borrow rules permit on direct field access but NOT
        // through method calls (which take `&mut self` on the whole
        // SlotContext). We do the split here so the rest of the
        // body can keep its prior shape.
        //
        // For mode = Speculative we still surface only the target
        // context — the draft is unused on this path (the dispatcher
        // gate above already routed any Speculative-eligible request
        // out to `generate_sync_mtp`).
        let SlotContext {
            mode,
            cached_tokens,
            ..
        } = slot_ctx;
        let ctx = match mode {
            SlotInferenceMode::SingleToken { ctx } => ctx,
            SlotInferenceMode::Speculative { target_ctx, .. } => target_ctx,
        };
        let full_prompt = format_prompt(model, model_id, request, quirks)?;

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

        // **Prefix caching.** Compute the longest common prefix
        // between what's currently in the KV cache (cached_tokens)
        // and the new prompt's tokens. Keep the LCP portion of the
        // cache and only prefill the new tail starting at position
        // lcp. When lcp == 0 (first call or different prompt), this
        // degrades to a full clear + full prefill — identical to
        // the previous unconditional behaviour.
        //
        // Defensive full-clear-on-error: cached_tokens is emptied
        // BEFORE the decode call, then repopulated AFTER success.
        // A mid-decode failure leaves the cache in an unknown state
        // but cached_tokens=[] forces the next call to full-clear
        // first, preventing M-RoPE position mismatches.
        //
        // 2026-05-17 SEP-pipeline tuning. See SlotContext.cached_tokens
        // for the workload justification.
        let cached_len_at_entry = cached_tokens.len();
        let raw_lcp = cached_tokens
            .iter()
            .zip(tokens.iter())
            // Reserve the last position for fresh decode — the model
            // needs at least one token of new context to produce
            // logits. If the new prompt is *identical* to the cached
            // one, force a 1-token re-prefill so the sampler still
            // has a fresh logit distribution to draw from.
            .take(tokens.len().saturating_sub(1))
            .take_while(|(a, b)| a == b)
            .count();
        // Apply the capability gate (see prefix_cache_safe rationale
        // at top of generate_sync). Hybrid recurrent models force
        // full prefill; pure-attention slots use the raw LCP.
        let lcp = if prefix_cache_safe { raw_lcp } else { 0 };
        // Clear cached_tokens before any cache mutation — restored on
        // success path below. If we crash between here and the
        // post-decode update, next call sees cached_tokens=[] and
        // does a defensive full clear.
        cached_tokens.clear();
        if lcp == 0 {
            ctx.clear_kv_cache();
        } else {
            // Partial keep: drop positions [lcp, end) from the
            // single sequence we use (seq_id=0). Positions [0, lcp)
            // stay resident; the new tail decode below starts at
            // position lcp.
            if let Err(e) = ctx.clear_kv_cache_seq(Some(0), Some(lcp as u32), None) {
                tracing::warn!(
                    error = ?e,
                    lcp,
                    new_prompt_len = tokens.len(),
                    "prefix_cache: partial clear failed — falling back to full clear"
                );
                ctx.clear_kv_cache();
            }
        }

        let mut batch = LlamaBatch::new(n_batch, 1);
        let tail = &tokens[lcp..];
        let last_idx = tail.len() - 1;
        for (j, &token) in tail.iter().enumerate() {
            let absolute_pos = (lcp + j) as i32;
            batch
                .add(token, absolute_pos, &[0], j == last_idx)
                .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;
        }
        // **TEMPORARY instrumentation (Phase 3c, 2026-05-19).**
        // Promoted to info so we can correlate prefix-cache state
        // with the n_tokens==0 decode failures observed in the gym.
        // Revert to debug-level once root cause is identified.
        tracing::info!(
            cache_hit_tokens = lcp,
            raw_lcp,
            new_prefill_tokens = tail.len(),
            new_prompt_len = tokens.len(),
            cached_len_at_entry,
            batch_n_tokens = batch.n_tokens(),
            "prefix_cache: prefill scope"
        );

        if let Err(e) = ctx.decode(&mut batch) {
            tracing::warn!(
                error = %e,
                cache_hit_tokens = lcp,
                raw_lcp,
                new_prefill_tokens = tail.len(),
                new_prompt_len = tokens.len(),
                cached_len_at_entry,
                batch_n_tokens = batch.n_tokens(),
                "prefix_cache: decode failed — state at failure"
            );
            return Err(Error::Inference(format!("Prompt decode failed: {e}")));
        }
        // Decode succeeded — record the new cached sequence so the
        // next call can compute its LCP against this one. The decode
        // loop below mutates `ctx` (KV grows by `n_generated` after
        // each token), but we DON'T add generated tokens to
        // cached_tokens because they're not part of the *prompt* the
        // next request will share. Only the prompt is comparable.
        *cached_tokens = tokens.clone();

        let mut sampler = build_sampler(model, request, quirks);
        let mut output = String::new();
        let mut n_generated = 0usize;
        let mut decoder = encoding_rs::UTF_8.new_decoder();

        // Wall-clock deadline. See `inference_deadline_secs` for
        // rationale. We check at the top of each sample-loop iteration
        // so a stuck mask-sample step still bounds at ~one slow token
        // (typically 100s of ms in the pathological state) past the
        // deadline before we clean up and return.
        let deadline_secs = inference_deadline_secs();
        let deadline = Instant::now()
            + std::time::Duration::from_secs(deadline_secs);
        let started_at = Instant::now();

        // Think-block budget forcing: track position inside <think>…</think>
        // and inject a closing tag once the budget is exhausted so the model
        // is forced to summarise rather than spiral indefinitely.
        let mut tail = String::with_capacity(32);
        let mut in_think = false;
        let mut think_tokens = 0usize;
        let mut think_budget_fired = false;
        // Stray-close tracking: thinking-capable models occasionally
        // emit `</think>` without a preceding `<think>` open (chat
        // template rendered with `enable_thinking: false` but the
        // model produced CoT anyway). When we observe the close
        // without the open, the leading text was de-facto thinking;
        // we wrap retroactively after the loop so the desktop's
        // think-block parser folds it correctly. See plan §1.3.
        let mut saw_open_tag = false;
        let mut saw_close_tag = false;

        // Tool-emission stop conditions for tools-using turns.
        //
        // Two emission shapes exist:
        //   A. Marker-wrapped — chat-template default, model emits
        //      `<tool_call>{...}</tool_call>` then continues.
        //   B. Grammar-locked bare JSON — when `tool_choice=required`
        //      installs the envelope schema, the model emits one
        //      balanced JSON object with NO `<tool_call>` markers.
        //
        // Both must terminate generation; otherwise the sampler runs
        // to `max_tokens` or hits the pathological-mask deadline.
        // The 2026-05-13 codex smoke saw shape B with ~10K tokens
        // generated per turn for ~80-byte envelopes — every token
        // past the close `}` was wasted on a grammar mask cycling
        // through whitespace / preferred-but-masked tokens.
        let tools_present = request.tools.as_ref().is_some_and(|t| !t.is_empty());
        // Brace-balance stop (shape B) only fires when grammar is
        // actively constraining output to the tool envelope shape —
        // i.e., `request.structured_output` is set. Without that
        // gate, the tracker counts literal `{...}` in prose (LaTeX,
        // markdown, code fences) and stops generation mid-thought.
        // Repro: Qwopus3.5-9B-Coder emitting "x_{r,c}" inside a
        // markdown bullet → false-positive stop at n_generated=87,
        // observed 2026-05-21. The marker stop (`</tool_call>`,
        // shape A) stays unconditional below.
        let tools_grammar_locked =
            tools_present && request.structured_output.is_some();
        let mut tc_json_depth = 0i32;
        let mut tc_json_in_string = false;
        let mut tc_json_escape_next = false;
        let mut tc_json_ever_opened = false;
        // Optional per-token sampler-role trace, gated on
        // SOVEREIGN_TRACE_SAMPLER_ROLES. Used during gym
        // diagnostics (gym 003 path-typo, 2026-05-13) to confirm
        // the Content/greedy sampler engages inside JSON-string
        // fields on /v1/chat/completions ingress. Counters are
        // summarised at end-of-generation. Cheap when off.
        let mut role_explore_n = 0usize;
        let mut role_content_n = 0usize;
        let mut tokens_in_string_n = 0usize;
        let role_trace_on = std::env::var("SOVEREIGN_TRACE_SAMPLER_ROLES").is_ok();

        // **Jump-forward decoding.** After each sampled token, ask
        // the constraint whether the FSM has exactly one legal next
        // token (Tier 1, mask-cache) or a deterministic byte run that
        // longest-matches into one or more vocab tokens (Tier 2,
        // FSM-byte-walk + VocabTrie). Forced tokens are emitted
        // without paying their own forward pass — their KV entries
        // ride into the next batched decode alongside the sampled
        // token. The mechanism turns N sequential 1-token decodes
        // into one wider decode at the next ambiguous position; on
        // batched-decode-friendly hardware (Strix Halo Vulkan, Metal)
        // a 5-token batch is ~2-3× faster than 5 sequential
        // singletons.
        //
        // **On by default** as of 2026-05-17. Measured ~15% tok/s lift
        // on the non-MTP path with the Twin Earth extraction schema
        // (Tier 1 alone captured 0% on Qwen3 BPE — see notes from that
        // day for the empirical finding). Opt out via
        // `SOVEREIGN_JUMP_FWD_DISABLE=1` for A/B or emergency
        // rollback. Both tiers default on independently:
        //   `SOVEREIGN_JUMP_FWD_DISABLE=1`   — kills the whole walk
        //   `SOVEREIGN_JUMP_FWD_T2_DISABLE=1` — keeps Tier 1, kills Tier 2
        //
        // Active only when the request has `structured_output` (the
        // constraint is what produces forced tokens) AND we're not
        // inside a `<think>` block.
        //
        // `MAX_JUMP_FWD_RUN` caps the per-iteration batch width so a
        // pathological forced-byte cycle can't run the verify batch
        // beyond `n_batch_max`. 32 is well under typical n_batch
        // (2048+) and covers all realistic JSON-skeleton runs we've
        // measured (typically 3-15 tokens).
        let jump_fwd_enabled = jump_fwd_enabled_from_env(|k| std::env::var(k).ok());
        let jump_fwd_t2_enabled =
            jump_fwd_t2_enabled_from_env(|k| std::env::var(k).ok());
        const MAX_JUMP_FWD_RUN: usize = 32;
        const MAX_FORCED_BYTES: usize = 64;
        let mut jump_fwd_n: usize = 0;
        let mut jump_fwd_runs: usize = 0;
        let mut jump_fwd_max: usize = 0;
        let mut jump_fwd_bytes_n: usize = 0;

        while n_generated < max_tokens {
            if Instant::now() > deadline {
                let elapsed = started_at.elapsed().as_secs();
                tracing::warn!(
                    model = %model_id,
                    elapsed_s = elapsed,
                    deadline_s = deadline_secs,
                    n_generated,
                    schema = request.structured_output.is_some(),
                    "inference:deadline exceeded — clearing KV cache and returning"
                );
                ctx.clear_kv_cache();
                return Err(Error::Inference(format!(
                    "inference deadline exceeded after {elapsed}s ({n_generated} tokens generated; \
                     deadline={deadline_secs}s) — likely pathological JSON-Schema mask state. \
                     Set SOVEREIGN_INFERENCE_TIMEOUT_SECS to override."
                )));
            }

            let role = if tools_present && tc_json_in_string {
                SamplerRole::Content
            } else {
                SamplerRole::Explore
            };
            match role {
                SamplerRole::Content => role_content_n += 1,
                SamplerRole::Explore => role_explore_n += 1,
            }
            if tc_json_in_string {
                tokens_in_string_n += 1;
            }
            let token = sampler.sample(ctx, -1, role);
            sampler.accept(token);

            if model.is_eog_token(token) {
                break;
            }

            if let Ok(piece) = model.token_to_piece(token, &mut decoder, true, None) {
                if role_trace_on && tools_present {
                    tracing::info!(
                        role = ?role,
                        in_string = tc_json_in_string,
                        depth = tc_json_depth,
                        piece = %piece.replace('\n', "\\n"),
                        "sampler_trace token"
                    );
                }
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
                    saw_open_tag = true;
                } else if in_think && tail.contains("</think>") {
                    in_think = false;
                    saw_close_tag = true;
                } else if !in_think && tail.contains("</think>") {
                    // Stray close — record so we can wrap post-loop.
                    saw_close_tag = true;
                }
                if in_think {
                    think_tokens += 1;
                }

                output.push_str(&piece);

                // Tool-emission stop. Marker-wrapped close-tag fires
                // whenever tools were requested (shape A — works on
                // any model whose chat template wraps tool calls in
                // `<tool_call>...</tool_call>`). Brace-balance stop
                // (shape B — bare JSON envelope) only fires when
                // grammar is actively constraining output to the
                // envelope schema; without that gate the tracker
                // matches any `{...}` in prose. See `tools_grammar_locked`.
                if tools_present {
                    if tail.contains("</tool_call>") {
                        tracing::info!(
                            model = %model_id,
                            n_generated,
                            tail = %tail,
                            "inference: stopping on </tool_call> marker"
                        );
                        n_generated += 1;
                        break;
                    }
                    if tools_grammar_locked && !in_think {
                        for b in piece.bytes() {
                            if tc_json_escape_next {
                                tc_json_escape_next = false;
                                continue;
                            }
                            if tc_json_in_string {
                                match b {
                                    b'\\' => tc_json_escape_next = true,
                                    b'"' => tc_json_in_string = false,
                                    _ => {}
                                }
                                continue;
                            }
                            match b {
                                b'"' => tc_json_in_string = true,
                                b'{' => {
                                    tc_json_depth += 1;
                                    tc_json_ever_opened = true;
                                }
                                b'}' => tc_json_depth -= 1,
                                _ => {}
                            }
                        }
                        if tc_json_ever_opened && tc_json_depth == 0 {
                            tracing::info!(
                                model = %model_id,
                                n_generated,
                                "inference: stopping on balanced JSON envelope"
                            );
                            n_generated += 1;
                            break;
                        }
                    }
                }
            }

            n_generated += 1;

            // **Jump-forward walk.** When the constraint is active and
            // we're outside the `<think>` block, ask the FSM whether
            // the next legal token is uniquely determined. If so, emit
            // it now and accumulate it into the next decode batch.
            // Each forced token gets the same per-token bookkeeping
            // (output, tail, tool-call JSON tracker, n_generated) the
            // sampled token just ran — minus the think-tag detection
            // since we're gated on `!in_think`.
            //
            // **Two-tier walk.** Tier 1 (`forced_next_token`) returns
            // the unique vocab token whose mask leaves it as the lone
            // survivor — O(1) on a warm cache. Tier 2
            // (`forced_next_run`) walks the FSM at byte level and
            // longest-matches the resulting forced byte run against
            // the vocab trie — catches BPE-skeleton transitions where
            // many tokens share a leading byte but only one or two
            // multi-byte tokens cover the deterministic stretch.
            // A pending queue smooths over the difference: Tier 1
            // produces 1 token per call, Tier 2 produces N. Both feed
            // into the same per-token bookkeeping below.
            //
            // Stops on: both tiers miss, MAX_JUMP_FWD_RUN cap, EOG
            // token (defensive — tier methods already filter), or
            // hitting `max_tokens`.
            let mut forced_run: Vec<LlamaToken> = Vec::new();
            let mut forced_hit_break = false;
            let mut pending: std::collections::VecDeque<LlamaToken> =
                std::collections::VecDeque::new();
            if jump_fwd_enabled && !in_think && n_generated < max_tokens {
                while forced_run.len() < MAX_JUMP_FWD_RUN && n_generated < max_tokens {
                    if pending.is_empty() {
                        // Try Tier 1 first — cheap O(1) on warm cache.
                        if let Some(t) = sampler.forced_next_token() {
                            pending.push_back(t);
                        } else if jump_fwd_t2_enabled {
                            // Tier 1 miss; fall through to Tier 2's
                            // byte-walk + longest-match. Returns ALL
                            // forced tokens up to the next ambiguity
                            // in one call, all already cleared by the
                            // probe FSM (caller still must `accept`
                            // each via the sampler).
                            let run = sampler.forced_next_run(MAX_FORCED_BYTES);
                            if run.is_empty() {
                                break;
                            }
                            pending.extend(run);
                        } else {
                            break;
                        }
                    }
                    // Safe: we either populated pending above or hit a
                    // break. If pending is somehow still empty here
                    // (defensive against a Tier 2 race that returns an
                    // empty Vec but reaches this point), bail.
                    let Some(ftok) = pending.pop_front() else {
                        break;
                    };
                    if model.is_eog_token(ftok) {
                        // forced_next_token should not return EOG (it
                        // guards on buffer_is_complete), but be safe:
                        // hand control back to the sampler loop so the
                        // existing EOG path runs cleanly.
                        break;
                    }
                    sampler.accept(ftok);
                    if let Ok(piece) = model.token_to_piece(ftok, &mut decoder, true, None) {
                        if role_trace_on && tools_present {
                            tracing::info!(
                                role = ?SamplerRole::Explore,
                                in_string = tc_json_in_string,
                                depth = tc_json_depth,
                                piece = %piece.replace('\n', "\\n"),
                                "sampler_trace token (jump_fwd)"
                            );
                        }
                        // Tail buffer + structural tag tracking. We're
                        // outside <think> by construction here, but
                        // keep the tail in sync so a subsequent
                        // sampled token's tag detection sees the right
                        // context.
                        tail.push_str(&piece);
                        if tail.len() > 32 {
                            let mut drain_to = tail.len() - 32;
                            while drain_to > 0 && !tail.is_char_boundary(drain_to) {
                                drain_to -= 1;
                            }
                            tail.drain(..drain_to);
                        }
                        // tail can't open <think> here (forced bytes
                        // are FSM-dictated JSON content, never the
                        // open tag); a stray close-tag would only
                        // appear in non-structured generation, which
                        // doesn't reach this path.

                        output.push_str(&piece);
                        jump_fwd_bytes_n += piece.len();

                        // Tool-call JSON tracker. Forced tokens
                        // contribute bytes to a tool-call envelope
                        // when one's active; if the envelope balances
                        // on a forced byte, stop here so the loop's
                        // existing close detection still fires on the
                        // next iteration's check. Brace tracker is
                        // gated on `tools_grammar_locked` per the
                        // fn-scope note; marker stop stays unconditional.
                        if tools_present {
                            if tail.contains("</tool_call>") {
                                tracing::info!(
                                    model = %model_id,
                                    n_generated,
                                    tail = %tail,
                                    "inference: stopping on </tool_call> marker (jump_fwd)"
                                );
                                forced_run.push(ftok);
                                n_generated += 1;
                                forced_hit_break = true;
                                break;
                            }
                            if tools_grammar_locked {
                                for b in piece.bytes() {
                                    if tc_json_escape_next {
                                        tc_json_escape_next = false;
                                        continue;
                                    }
                                    if tc_json_in_string {
                                        match b {
                                            b'\\' => tc_json_escape_next = true,
                                            b'"' => tc_json_in_string = false,
                                            _ => {}
                                        }
                                        continue;
                                    }
                                    match b {
                                        b'"' => tc_json_in_string = true,
                                        b'{' => {
                                            tc_json_depth += 1;
                                            tc_json_ever_opened = true;
                                        }
                                        b'}' => tc_json_depth -= 1,
                                        _ => {}
                                    }
                                }
                                if tc_json_ever_opened && tc_json_depth == 0 {
                                    tracing::info!(
                                        model = %model_id,
                                        n_generated,
                                        "inference: stopping on balanced JSON envelope (jump_fwd)"
                                    );
                                    forced_run.push(ftok);
                                    n_generated += 1;
                                    forced_hit_break = true;
                                    break;
                                }
                            }
                        }
                    }
                    forced_run.push(ftok);
                    n_generated += 1;
                }
                if !forced_run.is_empty() {
                    jump_fwd_runs += 1;
                    jump_fwd_n += forced_run.len();
                    if forced_run.len() > jump_fwd_max {
                        jump_fwd_max = forced_run.len();
                    }
                }
            }

            batch.clear();
            // Batched decode: [token, ...forced_run]. Sequential
            // positions starting at `base_pos`; logits=true only on
            // the final position, since the next iteration's
            // `sampler.sample(ctx, -1, ...)` reads only the last logit.
            // Saves softmax compute on intermediate positions when
            // jump-forward fires.
            let base_pos = (tokens.len() + n_generated - forced_run.len() - 1) as i32;
            let want_sampled_logits = forced_run.is_empty();
            batch
                .add(token, base_pos, &[0], want_sampled_logits)
                .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;
            for (i, &ftok) in forced_run.iter().enumerate() {
                let is_last = i + 1 == forced_run.len();
                batch
                    .add(ftok, base_pos + 1 + i as i32, &[0], is_last)
                    .map_err(|e| Error::Inference(format!("Batch add (jump_fwd) failed: {e}")))?;
            }

            ctx.decode(&mut batch)
                .map_err(|e| Error::Inference(format!("Decode failed: {e}")))?;

            if forced_hit_break {
                break;
            }

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

        // Intentionally do NOT clear KV here. The prefix-cache
        // machinery at the top of generate_sync (cached_tokens +
        // LCP partial-clear, ~line 756) is designed to preserve KV
        // across requests so the next prompt's shared prefix re-uses
        // the current generation's KV positions. Clearing here was an
        // uncommented copy-paste survivor that silently defeated the
        // cache — and worse, desynchronised it: cached_tokens (set
        // ~line 814 below) kept tracking the prompt while KV was
        // zeroed, so the next request computed LCP > 0 against an
        // empty KV, partial-cleared a no-op, prefilled only the
        // one-token tail at position lcp, and the model generated
        // from missing-context state. Observable symptom in the
        // search-gym 2026-05-19: replay 1 of every fixture produced
        // a coherent reply (LCP=0 path); replays 2-5 produced
        // raw-corpus-like gibberish unrelated to the prompt.
        //
        // TODO: generate_stream_sync still has an end-of-fn unconditional
        // clear at ~line 2381. Streaming-only sequences are
        // self-consistent because that path doesn't touch
        // cached_tokens, but a streaming→non-streaming sequence on
        // the same slot will re-introduce the desync (cached_tokens
        // stale from the last non-streaming call, KV empty from the
        // streaming clear). The fix is to have streaming take
        // &mut SlotContext and clear cached_tokens at start. Tracking
        // separately; the gym is non-streaming so this fix unblocks it.

        // Sampler-role summary: emitted once per generation so the
        // operator can spot when Content (greedy) didn't engage.
        // Healthy shape on a tool-call turn: a sizeable fraction of
        // tokens carry role=Content (the cmd-string bytes). When
        // role_content_n is 0 despite tools_present, the JSON-string
        // boundary tracker never triggered — diagnostic for cases
        // where the model emits an envelope-content shape without
        // `<tool_call>` markers but somehow also without entering
        // a JSON string from the tracker's perspective.
        if tools_present {
            tracing::info!(
                model = %model_id,
                role_content_n,
                role_explore_n,
                tokens_in_string_n,
                tc_json_ever_opened,
                "sampler_trace role summary"
            );
        }

        if saw_close_tag && !saw_open_tag {
            tracing::warn!(
                model = model_id,
                "thinking-tag stray close: model emitted </think> without a \
                 preceding <think> — wrapping output as thinking content"
            );
            output = format!("<think>{output}");
        }

        // End-of-generation telemetry — mirrors the MTP path's
        // `mtp: end-of-generation` line so a single log analyzer can
        // bucket per-request behaviour by path. Always emitted (no
        // guard) so the operator can decompose throughput by
        // `has_schema` / `jump_fwd_enabled`.
        let elapsed_ms = started_at.elapsed().as_millis();
        let tok_per_s = if elapsed_ms > 0 {
            n_generated as f64 * 1000.0 / elapsed_ms as f64
        } else {
            0.0
        };
        let has_schema = request.structured_output.is_some();
        let has_tools = tools_present;
        let jump_fwd_ratio = if n_generated > 0 {
            jump_fwd_n as f64 / n_generated as f64
        } else {
            0.0
        };
        tracing::info!(
            model = %model_id,
            prompt_tokens = tokens.len(),
            n_generated,
            elapsed_ms,
            tok_per_s = format!("{tok_per_s:.1}"),
            has_schema,
            has_tools,
            jump_fwd_enabled,
            jump_fwd_t2_enabled,
            jump_fwd_n,
            jump_fwd_runs,
            jump_fwd_max,
            jump_fwd_bytes_n,
            jump_fwd_ratio = format!("{jump_fwd_ratio:.3}"),
            "inference: end-of-generation"
        );

        Ok((output, tokens.len(), n_generated))
    }

    /// **MTP speculative-decoding loop (Phase A).** Direct port of the
    /// upstream `examples/mtp` example with our `ConstrainedSampler` in
    /// place of upstream's bare `LlamaSampler` so per-request sampling
    /// params (temperature, top-p, …) are honoured. Active only when
    /// `slot_ctx.draft_ctx.is_some()` — the dispatcher in `generate_sync`
    /// gates on (no structured_output) AND (no tools), keeping the loop
    /// free of the JsonConstraint mask + tool-call JSON tracker that
    /// the single-token path runs.
    ///
    /// **Sequence of API calls is load-bearing.** See
    /// [[project_mtp_invariants]] for the why behind each call; the
    /// short version:
    /// 1. Both contexts must be built with `with_n_rs_seq(>= n_draft_max)`.
    ///    Without it, partial `clear_kv_cache_seq` on rejected drafts
    ///    fails inside the target's recurrent layers and the next
    ///    verify batch hits an M-RoPE position assert.
    /// 2. After prefill decode: `session.process(&prefill)` then
    ///    `session.begin(seq_id, &tokens)`. `process` injects target's
    ///    pre-norm h into the draft side; `begin` initialises per-seq
    ///    MTP state.
    /// 3. Each loop iteration: `draft` → roll-back draft KV to `n_past`
    ///    (drops draft()'s AR pre-advancement) → verify decode →
    ///    `session.process(&verify)` → sample at each position and
    ///    find longest matching prefix → roll back rejected suffix on
    ///    BOTH ctxs → `session.accept(seq_id, n_accepted)`.
    ///
    /// **Phase A intentionally excludes:**
    /// - Prefix caching. KVs are cleared at entry; SEP-pipeline phases
    ///   that benefit most from prefix reuse all run on
    ///   `generate_sync_batched` via FastShort, not on MTP.
    /// - JsonConstraint mask, tool-call JSON tracker, think-block
    ///   budget, sampler-role tracing — Phase B.
    ///
    /// **Telemetry:** end-of-generation log with `accept_rate` and
    /// `tok_per_s` so the value of MTP per request is observable.
    /// Upstream measured 52% acceptance + 32 tok/s on Strix Halo
    /// Vulkan with Qwen3.6-A3B-MTP-Q6_K (2026-05-17 validation).
    fn generate_sync_mtp(
        model: &LlamaModel,
        model_id: &str,
        slot_ctx: &mut SlotContext,
        request: &CompletionRequest,
        quirks: &ModelQuirks,
    ) -> Result<(String, usize, usize)> {
        // **Rebuild the MTP session at the request boundary.** The
        // session that ModelSlot::load constructed is reused across
        // every request; KV cache clear (below) resets the contexts'
        // token state but does NOT touch the session's internal
        // draft-scheduler state. After 1-2 sequential requests that
        // residual state desynchronises with the fresh prompt and the
        // draft head proposes tokens from stale branches — observed
        // as garbage output ("Python sorting algorithm" responses to
        // unrelated questions) once the search-gym started running
        // --replays > 1.
        //
        // `rebuild_session_in_place` enforces the dispatcher contract
        // (slot must be `Speculative`) and uses `mem::replace` so we
        // never hold two sessions referencing the same contexts
        // simultaneously — upstream documents that as UB inside
        // `common_speculative_*`. Errors here (rare; would indicate
        // the session re-init refused for some new reason) propagate
        // up and the outer quarantine logic in `generate_sync` can
        // demote the slot to SingleToken.
        slot_ctx.rebuild_session_in_place(model_id)?;

        // Split-borrow target + draft + session via the
        // `speculative_mut` accessor. The MtpSession holds raw
        // pointers internally and is not tracked by the borrow
        // checker; the accessor returns three disjoint mutable
        // references into the enum variant, which together with
        // `cached_tokens` cover everything this function needs.
        //
        // The accessor's `None` branch is a panic-grade contract
        // violation (dispatcher gate routed us here even though the
        // slot isn't speculative). Convert to a proper error rather
        // than panicking so the caller can fall back gracefully.
        let SlotContext {
            mode,
            cached_tokens,
            ..
        } = slot_ctx;
        let (target_ctx, draft_ctx, session, n_draft_max) = match mode {
            SlotInferenceMode::Speculative {
                target_ctx,
                draft_ctx,
                session,
                n_draft_max,
            } => (target_ctx, draft_ctx, session, *n_draft_max),
            SlotInferenceMode::SingleToken { .. } => {
                return Err(Error::Inference(
                    "generate_sync_mtp: slot is in SingleToken mode — dispatcher contract violated"
                        .into(),
                ));
            }
        };

        // Phase A clears both KVs and starts fresh — no prefix cache
        // integration on this path yet. The non-MTP path keeps
        // cached_tokens for itself; we clear it here so a return to
        // the non-MTP path doesn't reuse a stale fingerprint.
        target_ctx.clear_kv_cache();
        draft_ctx.clear_kv_cache();
        cached_tokens.clear();

        let full_prompt = format_prompt(model, model_id, request, quirks)?;
        let tokens = model
            .str_to_token(&full_prompt, AddBos::Always)
            .map_err(|e| Error::Inference(format!("Tokenization failed: {e}")))?;
        if tokens.is_empty() {
            return Err(Error::Inference("MTP: empty prompt".into()));
        }

        let n_ctx = target_ctx.n_ctx() as usize;
        let max_tokens = clamp_max_tokens(request.max_tokens, tokens.len(), n_ctx)?;

        // Prefill: decode the entire prompt as one batch with
        // logits=true on every position. MTP needs that flag set
        // throughout so pre-norm embeddings can be extracted for the
        // draft context (`get_embeddings_pre_norm_ith` errors with
        // `batch.logits[N] != true` otherwise — upstream invariant).
        //
        // The MTP session's `set_embeddings_pre_norm(true)` hook was
        // installed on both contexts at slot-load time (see
        // ModelSlot::load), so this prefill decode will compute and
        // store the pre-norm hidden state that `session.process` reads
        // back below.
        let n_batch_max = target_ctx.n_batch() as usize;
        let prefill_capacity = tokens.len().max(n_batch_max);
        let mut prefill = LlamaBatch::new(prefill_capacity, 1);
        for (i, &tok) in tokens.iter().enumerate() {
            prefill
                .add(tok, i as i32, &[0], true)
                .map_err(|e| Error::Inference(format!("MTP prefill batch add failed: {e}")))?;
        }
        target_ctx
            .decode(&mut prefill)
            .map_err(|e| Error::Inference(format!("MTP prefill decode failed: {e}")))?;

        // `process(&prefill)` after every target decode is what injects
        // target's pre-norm h into the draft side. `begin(seq_id,
        // &tokens)` then resets per-seq pending-h state for THIS
        // request (the session itself persists across requests). Both
        // calls are load-bearing: skipping either makes the draft run
        // uncalibrated and acceptance collapses.
        session
            .process(&prefill)
            .map_err(|e| Error::Inference(format!("MTP process(prefill) failed: {e:?}")))?;
        session
            .begin(0, &tokens)
            .map_err(|e| Error::Inference(format!("MTP begin failed: {e:?}")))?;

        // Sample the first token from prefill's last logit position.
        // Use ConstrainedSampler::Explore — no JSON-schema mask is
        // installed on this path (dispatcher filtered structured
        // requests out), so the sampler behaves as a plain chain
        // of {temp/top-p/top-k/penalties} per the request quirks.
        let mut sampler = build_sampler(model, request, quirks);
        let mut last_token = sampler.sample(
            &*target_ctx,
            prefill.n_tokens() - 1,
            SamplerRole::Explore,
        );
        sampler.accept(last_token);

        let mut output = String::new();
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        if let Ok(piece) = model.token_to_piece(last_token, &mut decoder, true, None) {
            output.push_str(&piece);
        }

        let mut n_generated: usize = 1;
        let mut n_past = tokens.len() as i32;
        let mut n_drafts_total: u64 = 0;
        let mut n_accepted_total: u64 = 0;
        let mut n_draft_calls: u64 = 0;

        // Jump-forward state. Reads same env gates as generate_sync so
        // the MTP path picks up the operating default automatically.
        let jump_fwd_enabled = jump_fwd_enabled_from_env(|k| std::env::var(k).ok());
        let jump_fwd_t2_enabled =
            jump_fwd_t2_enabled_from_env(|k| std::env::var(k).ok());
        const MAX_JUMP_FWD_RUN: usize = 32;
        const MAX_FORCED_BYTES: usize = 64;
        let mut jump_fwd_n: usize = 0;
        let mut jump_fwd_runs: usize = 0;
        let mut jump_fwd_max: usize = 0;
        let mut jump_fwd_bytes_n: usize = 0;

        // Verification batch holds [last_token, drafts...] OR
        // [last_token, ...forced_run] on jump-forward iterations. Cap
        // at max(n_draft_max + 1, MAX_JUMP_FWD_RUN + 1, n_batch_max).
        // Allocation is one-shot, reused via `clear()`.
        let verify_cap = (n_draft_max as usize + 1)
            .max(MAX_JUMP_FWD_RUN + 1)
            .max(n_batch_max);
        let mut verify = LlamaBatch::new(verify_cap, 1);

        let deadline_secs = inference_deadline_secs();
        let deadline = Instant::now() + std::time::Duration::from_secs(deadline_secs);
        let started_at = Instant::now();

        while n_generated < max_tokens {
            if model.is_eog_token(last_token) {
                break;
            }
            if Instant::now() > deadline {
                let elapsed = started_at.elapsed().as_secs();
                tracing::warn!(
                    model = %model_id,
                    elapsed_s = elapsed,
                    deadline_s = deadline_secs,
                    n_generated,
                    "mtp:deadline exceeded — clearing KV caches"
                );
                target_ctx.clear_kv_cache();
                draft_ctx.clear_kv_cache();
                return Err(Error::Inference(format!(
                    "MTP inference deadline exceeded after {elapsed}s ({n_generated} tokens)"
                )));
            }

            // === Jump-forward peek (Tier 2 byte-walk) ===
            //
            // **Peek-then-commit**, threshold-gated. Unlike the
            // non-MTP path where we accept forced tokens incrementally
            // (T1 + T2 chained), MTP's per-iteration cost model wants
            // a yes/no decision on forced-only vs draft-with-MTP.
            //
            // `forced_next_run` returns all forced tokens up to the
            // next ambiguity via a probe clone of the FSM — it does
            // NOT mutate `sampler` state, so we can peek freely and
            // discard if the run is too short to be worth a forced-
            // only verify round.
            //
            // **Threshold motivation.** Draft iterations emit
            // `1 + accept_rate × n_draft_max ≈ 1 + 0.73 × 3 ≈ 3.19`
            // tokens on average. A forced-only iteration with K
            // forced tokens emits `K + 1` (forced + bonus). Forced
            // only beats the draft average at `K ≥ 3`. Smaller runs
            // (K=1, K=2) are throughput-negative — the draft path's
            // mask-constrained sampling will catch the same forced
            // bytes as part of its 3-token speculation, and the
            // overhead of a separate forced verify is uncompensated.
            //
            // T1 (mask-single-survivor) is implicit in T2's byte
            // walk: any state where the mask has exactly one legal
            // token is also a state where the FSM has a forced byte
            // run terminating at that token. We use the byte walk
            // exclusively here since on the MTP path we need the
            // full run length to make the threshold call.
            const MIN_FORCED_RUN_FOR_MTP: usize = 3;
            let forced_peek: Vec<LlamaToken> = if jump_fwd_enabled
                && jump_fwd_t2_enabled
                && n_generated < max_tokens
            {
                sampler.forced_next_run(MAX_FORCED_BYTES)
            } else {
                Vec::new()
            };
            // Commit only when the run clears the threshold. Cap at
            // MAX_JUMP_FWD_RUN so the verify batch stays bounded.
            let mut forced_run: Vec<LlamaToken> = Vec::new();
            if forced_peek.len() >= MIN_FORCED_RUN_FOR_MTP {
                let cap = forced_peek.len().min(MAX_JUMP_FWD_RUN);
                for &ftok in &forced_peek[..cap] {
                    if model.is_eog_token(ftok) {
                        break;
                    }
                    sampler.accept(ftok);
                    let piece = model
                        .token_to_piece(ftok, &mut decoder, true, None)
                        .unwrap_or_default();
                    output.push_str(&piece);
                    jump_fwd_bytes_n += piece.len();
                    forced_run.push(ftok);
                    n_generated += 1;
                    if n_generated >= max_tokens {
                        break;
                    }
                }
                if !forced_run.is_empty() {
                    jump_fwd_runs += 1;
                    jump_fwd_n += forced_run.len();
                    if forced_run.len() > jump_fwd_max {
                        jump_fwd_max = forced_run.len();
                    }
                }
            }

            // Forced-only verify round. No drafts; only commits the
            // forced run + advances session state. Skips the regular
            // draft-verify-accept block for this iteration.
            if !forced_run.is_empty() {
                verify.clear();
                let k = forced_run.len();
                // [last_token, forced_run[0], ..., forced_run[K-1]] at
                // positions n_past..n_past+K. logits=true on every
                // position so `session.process(&verify)` can inject
                // pre-norm h uniformly — same invariant the draft path
                // depends on.
                verify
                    .add(last_token, n_past, &[0], true)
                    .map_err(|e| Error::Inference(format!("MTP forced verify add: {e}")))?;
                for (i, &f) in forced_run.iter().enumerate() {
                    verify
                        .add(f, n_past + 1 + i as i32, &[0], true)
                        .map_err(|e| Error::Inference(format!("MTP forced verify add: {e}")))?;
                }
                target_ctx
                    .decode(&mut verify)
                    .map_err(|e| Error::Inference(format!("MTP forced decode failed: {e}")))?;
                session
                    .process(&verify)
                    .map_err(|e| Error::Inference(format!("MTP forced process failed: {e:?}")))?;
                // **No `session.accept` here.** Upstream's
                // `common_speculative_accept` asserts on the
                // speculative impl pointer (`GGML_ASSERT(impl)`)
                // which only gets set inside `session.draft`. On the
                // forced-only path we skip drafting, so impl is null,
                // and calling accept aborts the process. The session's
                // internal positional reasoning is driven by the
                // `position` arg of the next `session.draft` call —
                // skipping accept here doesn't desync the head, it
                // just means we didn't tell the speculative subsystem
                // about positions it never proposed drafts for.
                //
                // **Bonus sample at idx k.** Mirrors the draft path's
                // idx 0 bonus: the logit at position `n_past + k`
                // (last forced token's logit-after) predicts what
                // comes next in the still-ambiguous part of the
                // schema. Sampling it here produces the next
                // iteration's `last_token`. Without this sample we'd
                // carry `last_token = forced_run[k-1]` into the next
                // iteration, but that token's KV slot is already
                // filled (by this verify) — the draft path's
                // `verify.add(last_token, n_past, ...)` would then
                // try to add a token at a position libllama already
                // owns, surfacing as `Decode Error -1: n_tokens == 0`.
                let next_token = sampler.sample(
                    &*target_ctx,
                    k as i32,
                    SamplerRole::Explore,
                );
                sampler.accept(next_token);
                if !model.is_eog_token(next_token) {
                    let piece = model
                        .token_to_piece(next_token, &mut decoder, true, None)
                        .unwrap_or_default();
                    output.push_str(&piece);
                }
                n_generated += 1;
                last_token = next_token;
                // Position arithmetic: the K forced tokens filled KV
                // at n_past+1..n_past+k. last_token (next_token) is
                // not yet in the KV — it'll be added by the next
                // iteration's verify at position n_past + k + 1.
                n_past += k as i32 + 1;
                if n_generated >= max_tokens {
                    break;
                }
                if model.is_eog_token(last_token) {
                    break;
                }
                continue;
            }

            // Draft phase: ask the MTP head for up to n_draft_max
            // candidate tokens given position + last accepted token.
            // Returned vec can be shorter than n_draft_max if the head
            // short-circuits on its own confidence drop.
            let drafts = session
                .draft(0, n_past, last_token)
                .map_err(|e| Error::Inference(format!("MTP draft phase failed: {e:?}")))?;
            n_draft_calls += 1;
            n_drafts_total += drafts.len() as u64;

            // Build verify batch: [last_token, drafts...], all with
            // logits=true so the target produces per-position logits
            // we can sample at to find the longest accepted prefix.
            verify.clear();
            verify
                .add(last_token, n_past, &[0], true)
                .map_err(|e| Error::Inference(format!("MTP verify batch add: {e}")))?;
            for (i, d) in drafts.iter().enumerate() {
                verify
                    .add(*d, n_past + 1 + i as i32, &[0], true)
                    .map_err(|e| Error::Inference(format!("MTP verify batch add: {e}")))?;
            }
            let n_verify = verify.n_tokens();

            // Roll back draft KV to drop draft()'s AR pre-advancement.
            // process(verify) below will re-decode the same positions
            // on the draft side, this time with target's pre-norm h
            // injected. Without this rollback the draft side's M-RoPE
            // positions clash on the second pass.
            draft_ctx
                .clear_kv_cache_seq(Some(0), Some(n_past as u32), None)
                .map_err(|e| Error::Inference(format!("MTP draft KV pre-verify rollback failed: {e:?}")))?;

            target_ctx
                .decode(&mut verify)
                .map_err(|e| Error::Inference(format!("MTP verify decode failed: {e}")))?;
            session
                .process(&verify)
                .map_err(|e| Error::Inference(format!("MTP process(verify) failed: {e:?}")))?;

            // Sample at each output position; accept the longest prefix
            // of the drafts where the target's sample equals the draft.
            // Output index 0 corresponds to the logit AFTER last_token
            // (predicts draft[0]). On a mismatch, the sampled token is
            // the bonus (target's correction at the first mismatch
            // position) — one free correct token per step regardless
            // of draft acceptance.
            let mut n_accepted: usize = 0;
            let mut next_token = sampler.sample(&*target_ctx, 0, SamplerRole::Explore);
            sampler.accept(next_token);
            for (i, draft) in drafts.iter().enumerate() {
                if next_token == *draft {
                    n_accepted = i + 1;
                    if (i + 1) < n_verify as usize {
                        next_token = sampler.sample(
                            &*target_ctx,
                            (i + 1) as i32,
                            SamplerRole::Explore,
                        );
                        sampler.accept(next_token);
                    }
                } else {
                    break;
                }
            }
            n_accepted_total += n_accepted as u64;

            // After this step, positions [n_past, n_past + n_accepted]
            // are committed (last_token + n_accepted drafts); next_token
            // is the new generated token but its KV entry won't exist
            // until we use it as last_token next iteration.
            let new_n_past = n_past + 1 + n_accepted as i32;

            // Roll back rejected suffix on BOTH contexts. After verify
            // the target and draft both reach [0..n_past+drafts.len()];
            // keep only up to position new_n_past - 1. Both rollbacks
            // require n_rs_seq > 0 on their context.
            if (n_accepted as i32) < drafts.len() as i32 {
                target_ctx
                    .clear_kv_cache_seq(Some(0), Some(new_n_past as u32), None)
                    .map_err(|e| Error::Inference(format!("MTP target KV rollback failed: {e:?}")))?;
                draft_ctx
                    .clear_kv_cache_seq(Some(0), Some(new_n_past as u32), None)
                    .map_err(|e| Error::Inference(format!("MTP draft KV rollback failed: {e:?}")))?;
            }

            session
                .accept(0, n_accepted as u16)
                .map_err(|e| Error::Inference(format!("MTP session.accept failed: {e:?}")))?;

            // Emit accepted drafts + next_token. Each is checked for
            // EOG independently; we cap at max_tokens and bail at EOG.
            let mut hit_eos = false;
            for &tok in &drafts[..n_accepted] {
                if n_generated >= max_tokens {
                    break;
                }
                let piece = model
                    .token_to_piece(tok, &mut decoder, true, None)
                    .unwrap_or_default();
                output.push_str(&piece);
                n_generated += 1;
                if model.is_eog_token(tok) {
                    hit_eos = true;
                    break;
                }
            }
            if hit_eos || n_generated >= max_tokens {
                break;
            }
            let piece = model
                .token_to_piece(next_token, &mut decoder, true, None)
                .unwrap_or_default();
            output.push_str(&piece);
            n_generated += 1;
            last_token = next_token;
            n_past = new_n_past;
        }

        let elapsed_ms = started_at.elapsed().as_millis();
        let accept_rate = if n_drafts_total > 0 {
            n_accepted_total as f64 / n_drafts_total as f64
        } else {
            0.0
        };
        let tok_per_s = if elapsed_ms > 0 {
            n_generated as f64 * 1000.0 / elapsed_ms as f64
        } else {
            0.0
        };
        // Per-request shape — lets a log analyzer compute "average
        // accept_rate when has_schema=true" vs "false" to test the
        // hypothesis that schema-masked drafts get rejected more
        // often than unconstrained drafts. Similarly for has_tools
        // (which currently shouldn't fire — dispatcher gates tools
        // out — but logging it costs nothing and catches dispatch
        // bugs).
        let has_schema = request.structured_output.is_some();
        let has_tools = request.tools.as_ref().is_some_and(|t| !t.is_empty());
        let jump_fwd_ratio = if n_generated > 0 {
            jump_fwd_n as f64 / n_generated as f64
        } else {
            0.0
        };
        tracing::info!(
            model = %model_id,
            n_draft_calls,
            drafts_proposed = n_drafts_total,
            drafts_accepted = n_accepted_total,
            accept_rate = format!("{accept_rate:.3}"),
            prompt_tokens = tokens.len(),
            n_generated,
            elapsed_ms,
            tok_per_s = format!("{tok_per_s:.1}"),
            has_schema,
            has_tools,
            jump_fwd_enabled,
            jump_fwd_t2_enabled,
            jump_fwd_n,
            jump_fwd_runs,
            jump_fwd_max,
            jump_fwd_bytes_n,
            jump_fwd_ratio = format!("{jump_fwd_ratio:.3}"),
            "mtp: end-of-generation"
        );

        Ok((output, tokens.len(), n_generated))
    }


    /// Multi-sequence batched autoregressive decode for short-call
    /// enrichment phases routed to the FastShort slot. Caller supplies
    /// up to the slot's `n_seq_max` requests; we run one packed
    /// prefill + N autoregressive decode steps (each step decodes one
    /// new token per still-active seq), retiring seqs on EOS or per-
    /// request `max_tokens`. Returns one `(text, prompt_tok,
    /// decoded_tok)` tuple per request, in the same order as input.
    ///
    /// Methodology mirrors `bench_decode_batch.rs::run_k_calls` —
    /// proven to deliver 2.1–2.8× wall-clock speedup vs sequential
    /// `generate_sync` calls on the same shape (Strix Halo / ROCm /
    /// Qwen3.5-2B Q6_K, 900 in / 128 out).
    ///
    /// **Scope (intentionally narrower than `generate_sync`):**
    /// - No thinking-block tracking. Phase 1b/3/5/6 composers don't
    ///   enable thinking (chat closure passes `think_budget=0`); the
    ///   thinking-detection state machine in `generate_sync` would
    ///   cost per-token UTF-8 work × N seqs without ever firing.
    /// - No in-loop deadline enforcement. Outer Tokio cancellation is
    ///   sufficient for enrichment, and the loop's tail is bounded by
    ///   the smallest per-request `max_output_tokens` (typically 512).
    /// - Per-seq `ConstrainedSampler` (so per-seq JSON-schema masks
    ///   are supported just like single-seq decode).
    fn generate_sync_batched(
        model: &LlamaModel,
        model_id: &str,
        ctx: &mut crate::llama::cpp::context::LlamaContext<'_>,
        requests: &[&CompletionRequest],
        quirks: &ModelQuirks,
    ) -> Result<Vec<(String, usize, usize)>> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }

        ctx.clear_kv_cache();

        // Tokenize every request up-front. `format_prompt` runs the
        // production chat-template renderer (system + user + thinking
        // injection per `quirks`).
        let forensic = std::env::var("SOVEREIGN_FORENSIC").ok().as_deref() == Some("1");
        let tokenized: Vec<Vec<LlamaToken>> = requests
            .iter()
            .enumerate()
            .map(|(idx, r)| {
                let prompt = format_prompt(model, model_id, r, quirks)?;
                let toks = model
                    .str_to_token(&prompt, AddBos::Always)
                    .map_err(|e| Error::Inference(format!("Tokenization failed: {e}")))?;
                if forensic && toks.is_empty() {
                    // Forensic for the n_tokens==0 batched-prefill bug
                    // (root cause: llama-cpp-4 0.2.x retired
                    // `with_n_seq_max`; fixed via vendor crate, see
                    // workspace `[patch.crates-io]`). Kept as opt-in
                    // diagnostic for any future batched-prefill
                    // failure mode. Gated by SOVEREIGN_FORENSIC=1.
                    let mut end = prompt.len().min(400);
                    while end > 0 && !prompt.is_char_boundary(end) { end -= 1; }
                    tracing::warn!(
                        slot_idx = idx,
                        prompt_chars = prompt.len(),
                        prompt_head = %&prompt[..end],
                        system_message_present = r.system_message.is_some(),
                        user_prompt_chars = r.prompt.len(),
                        "batched-prefill: request tokenized to zero tokens — will fail the batch"
                    );
                }
                Ok(toks)
            })
            .collect::<Result<Vec<_>>>()?;

        let n_batch = ctx.n_batch() as usize;
        let n_ctx = ctx.n_ctx() as usize;
        // Per-seq KV slice when the context has n_seq_max > 1.
        let n_seq_max = std::cmp::max(1, requests.len());
        let n_ctx_per_seq = n_ctx / n_seq_max;

        // Per-seq output cap. Each request's prompt + cap must fit
        // its own KV slice — refuse cleanly rather than overflow mid-
        // decode.
        let max_outputs: Vec<usize> = requests
            .iter()
            .zip(tokenized.iter())
            .map(|(r, toks)| {
                clamp_max_tokens(r.max_tokens, toks.len(), n_ctx_per_seq)
            })
            .collect::<Result<_>>()?;

        // ── Prefill: pack every prompt into one batch ───────────────
        let mut batch = LlamaBatch::new(n_batch, requests.len() as i32);
        let mut current_logit_idx: Vec<i32> = Vec::with_capacity(requests.len());
        let mut batch_pos: i32 = 0;
        for (seq, tokens) in tokenized.iter().enumerate() {
            let last = tokens.len() - 1;
            for (pos, &tok) in tokens.iter().enumerate() {
                let need_logits = pos == last;
                batch
                    .add(tok, pos as i32, &[seq as i32], need_logits)
                    .map_err(|e| {
                        Error::Inference(format!("Batched prefill add failed: {e}"))
                    })?;
                if need_logits {
                    current_logit_idx.push(batch_pos);
                }
                batch_pos += 1;
            }
        }
        // Forensic: count tokens that actually made it into the
        // batch. Original use: distinguish "we never added tokens"
        // (upstream bug — empty tokenization or skipped add) from
        // "we added tokens but llama_cpp reports 0" (FFI-side bug).
        // Root cause for the historical n_tokens == 0 failure was
        // llama-cpp-4 0.2.x retiring `with_n_seq_max`; fixed in the
        // vendored crate. Counter still feeds the error message
        // below (cheap, always-on). Verbose pre-decode warning is
        // opt-in via SOVEREIGN_FORENSIC=1 for future regressions.
        let batch_n_tokens = batch.n_tokens();
        if forensic && batch_n_tokens == 0 {
            let prompt_summaries: Vec<(usize, usize)> = tokenized
                .iter()
                .zip(requests.iter())
                .map(|(t, r)| (t.len(), r.prompt.len()))
                .collect();
            tracing::warn!(
                n_requests = requests.len(),
                batch_capacity = batch.n_tokens(),
                tokenized_lens = ?tokenized.iter().map(|t| t.len()).collect::<Vec<_>>(),
                prompt_summaries = ?prompt_summaries,
                "batched prefill: about to decode with 0 tokens — root-cause for n_tokens == 0 error"
            );
        }
        ctx.decode(&mut batch).map_err(|e| {
            Error::Inference(format!(
                "Batched prefill decode failed: {e} (batch_n_tokens={batch_n_tokens}, n_requests={})",
                requests.len()
            ))
        })?;

        // ── Per-seq decode state ────────────────────────────────────
        let mut samplers: Vec<ConstrainedSampler> = requests
            .iter()
            .map(|r| build_sampler(model, r, quirks))
            .collect();
        let mut decoders: Vec<encoding_rs::Decoder> = (0..requests.len())
            .map(|_| encoding_rs::UTF_8.new_decoder())
            .collect();
        let mut outputs: Vec<String> = vec![String::new(); requests.len()];
        let mut n_generated: Vec<usize> = vec![0; requests.len()];
        let mut next_pos: Vec<i32> = tokenized
            .iter()
            .map(|t| t.len() as i32)
            .collect();
        let mut active = vec![true; requests.len()];

        // ── Autoregressive decode loop ──────────────────────────────
        loop {
            // Sample one token per still-active seq. Track the next
            // tokens we need to feed in the following decode step;
            // a seq that hits EOS or the per-request cap retires
            // and contributes no further tokens to subsequent batches.
            let mut next_tokens: Vec<Option<LlamaToken>> = vec![None; requests.len()];
            for seq in 0..requests.len() {
                if !active[seq] {
                    continue;
                }
                // Batched path doesn't track per-sequence JSON state;
                // default to Explore (matches pre-Investment-#16 behavior).
                let token = samplers[seq].sample(ctx, current_logit_idx[seq], SamplerRole::Explore);
                samplers[seq].accept(token);

                if model.is_eog_token(token) {
                    active[seq] = false;
                    continue;
                }

                if let Ok(piece) =
                    model.token_to_piece(token, &mut decoders[seq], true, None)
                {
                    outputs[seq].push_str(&piece);
                }
                n_generated[seq] += 1;

                if n_generated[seq] >= max_outputs[seq] {
                    active[seq] = false;
                    continue;
                }

                next_tokens[seq] = Some(token);
            }

            if !active.iter().any(|&a| a) {
                break;
            }

            // Build the next decode batch: one token per still-active
            // seq. Preserve seq_id so each seq's KV cache slice keeps
            // accumulating against its own positions; track the new
            // batch index so the next sample call reads the right
            // logits.
            batch.clear();
            let mut new_logit_idx = vec![-1i32; requests.len()];
            let mut bi: i32 = 0;
            for seq in 0..requests.len() {
                if !active[seq] {
                    continue;
                }
                let Some(tok) = next_tokens[seq] else {
                    // Active seq must have a token to feed unless it
                    // retired this round (handled above) — defensive
                    // skip.
                    continue;
                };
                batch
                    .add(tok, next_pos[seq], &[seq as i32], true)
                    .map_err(|e| {
                        Error::Inference(format!("Batched decode add failed: {e}"))
                    })?;
                new_logit_idx[seq] = bi;
                next_pos[seq] += 1;
                bi += 1;
            }
            current_logit_idx = new_logit_idx;

            ctx.decode(&mut batch).map_err(|e| {
                Error::Inference(format!("Batched decode step failed: {e}"))
            })?;
        }

        // Cleanup — leave the KV cache empty so the next caller (or a
        // partial-batch retry) doesn't pick up stale positions.
        ctx.clear_kv_cache();

        let mut results = Vec::with_capacity(requests.len());
        for (seq, output) in outputs.into_iter().enumerate() {
            results.push((output, tokenized[seq].len(), n_generated[seq]));
        }
        Ok(results)
    }

    fn generate_stream_sync(
        model: &LlamaModel,
        model_id: &str,
        ctx: &mut crate::llama::cpp::context::LlamaContext<'_>,
        request: &CompletionRequest,
        tx: &tokio::sync::mpsc::Sender<Result<String>>,
        quirks: &ModelQuirks,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<()> {
        // Pre-clear for the same reason as generate_sync: a prior failed decode
        // leaves the cache dirty, causing M-RoPE position errors on the next call.
        ctx.clear_kv_cache();

        let full_prompt = format_prompt(model, model_id, request, quirks)?;

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

        // Wall-clock deadline mirrors generate_sync — a stuck
        // mask-sample step can starve the slot regardless of whether
        // streaming is on. Cancellation checks below still take
        // priority for clean shutdowns; the deadline is the backstop
        // for the case where the sample step itself is slow enough
        // that cancel-checks fire too rarely to help.
        let deadline_secs = inference_deadline_secs();
        let deadline = Instant::now()
            + std::time::Duration::from_secs(deadline_secs);
        let started_at = Instant::now();

        let mut tail = String::with_capacity(32);
        let mut in_think = false;
        let mut think_tokens = 0usize;
        let mut think_budget_fired = false;
        // Stray-close tracking: see plan §1.3. We can't rewrap once
        // pieces have flushed to the receiver, but we log so the
        // operator can see this happen.
        let mut saw_open_tag = false;
        let mut saw_close_tag = false;

        // See generate_sync: stop on `</tool_call>` (marker) or
        // balanced JSON envelope (grammar-locked) when tools were
        // requested. Shape-B tracker gated on `structured_output` to
        // avoid false-positive stops on prose `{...}` — see fn-scope
        // note on the sync path.
        let tools_present = request.tools.as_ref().is_some_and(|t| !t.is_empty());
        let tools_grammar_locked =
            tools_present && request.structured_output.is_some();
        let mut tc_json_depth = 0i32;
        let mut tc_json_in_string = false;
        let mut tc_json_escape_next = false;
        let mut tc_json_ever_opened = false;

        while n_generated < max_tokens {
            if Instant::now() > deadline {
                let elapsed = started_at.elapsed().as_secs();
                tracing::warn!(
                    model = %model_id,
                    elapsed_s = elapsed,
                    deadline_s = deadline_secs,
                    n_generated,
                    schema = request.structured_output.is_some(),
                    "inference:deadline exceeded (stream) — clearing KV cache and returning"
                );
                ctx.clear_kv_cache();
                return Err(Error::Inference(format!(
                    "inference deadline exceeded after {elapsed}s ({n_generated} tokens generated; \
                     deadline={deadline_secs}s) — likely pathological JSON-Schema mask state. \
                     Set SOVEREIGN_INFERENCE_TIMEOUT_SECS to override."
                )));
            }

            // Antifragile-routing cancel check: redirect-induced
            // cancellation on the session-level CancellationToken stops
            // the sampler without waiting for the receiver to drop.
            // Receiver-drop (below) remains the de-facto cancel path when
            // no token is threaded; both converge on `break`.
            if let Some(t) = cancel {
                if t.is_cancelled() {
                    tracing::warn!(
                        tokens_emitted = n_generated,
                        "inference:cancelled via CancellationToken"
                    );
                    break;
                }
            }

            let role = if tools_present && tc_json_in_string {
                SamplerRole::Content
            } else {
                SamplerRole::Explore
            };
            let token = sampler.sample(ctx, -1, role);
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
                    saw_open_tag = true;
                } else if in_think && tail.contains("</think>") {
                    in_think = false;
                    saw_close_tag = true;
                } else if !in_think && tail.contains("</think>") {
                    saw_close_tag = true;
                }
                if in_think {
                    think_tokens += 1;
                }

                // Tool-emission JSON-balance bookkeeping — done BEFORE
                // the send so we don't need to re-borrow piece bytes
                // after the move into the channel. Gated on
                // `tools_grammar_locked` per the sync-path note.
                let mut json_envelope_complete = false;
                if tools_grammar_locked && !in_think {
                    for b in piece.bytes() {
                        if tc_json_escape_next {
                            tc_json_escape_next = false;
                            continue;
                        }
                        if tc_json_in_string {
                            match b {
                                b'\\' => tc_json_escape_next = true,
                                b'"' => tc_json_in_string = false,
                                _ => {}
                            }
                            continue;
                        }
                        match b {
                            b'"' => tc_json_in_string = true,
                            b'{' => {
                                tc_json_depth += 1;
                                tc_json_ever_opened = true;
                            }
                            b'}' => tc_json_depth -= 1,
                            _ => {}
                        }
                    }
                    if tc_json_ever_opened && tc_json_depth == 0 {
                        json_envelope_complete = true;
                    }
                }

                if tx.blocking_send(Ok(piece)).is_err() {
                    tracing::warn!(
                        tokens_emitted = n_generated,
                        "inference:cancelled via receiver-drop"
                    );
                    break;
                }

                // Tool-emission stop — see generate_sync for rationale.
                if tools_present {
                    if tail.contains("</tool_call>") {
                        tracing::info!(
                            model = %model_id,
                            n_generated,
                            tail = %tail,
                            "inference: stopping on </tool_call> marker (stream)"
                        );
                        break;
                    }
                    if json_envelope_complete {
                        tracing::info!(
                            model = %model_id,
                            n_generated,
                            "inference: stopping on balanced JSON envelope (stream)"
                        );
                        break;
                    }
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

        if saw_close_tag && !saw_open_tag {
            tracing::warn!(
                model = model_id,
                "thinking-tag stray close (legacy stream): model emitted \
                 </think> without a preceding <think> — desktop will repair \
                 the rendering but tokens already flushed; cause is usually \
                 enable_thinking:false on a thinking model"
            );
        }

        Ok(())
    }

    /// Streaming generator that yields typed [`StreamFrame`]s and
    /// always terminates the channel with [`StreamFrame::Finish`]
    /// carrying the real [`FinishReason`] (`Stop` on EOS, `Length`
    /// on `max_tokens`, `Cancelled` on receiver-drop /
    /// CancellationToken, `Error(_)` on decode/batch faults).
    ///
    /// Mirrors [`Self::generate_stream_sync`] but for the new
    /// [`InferenceProvider::complete_stream_with_finish`] surface.
    /// The legacy helper keeps working for callers still on the
    /// `Result<String>` shape.
    fn generate_stream_sync_with_finish(
        model: &LlamaModel,
        model_id: &str,
        ctx: &mut crate::llama::cpp::context::LlamaContext<'_>,
        request: &CompletionRequest,
        tx: &tokio::sync::mpsc::Sender<StreamFrame>,
        quirks: &ModelQuirks,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<()> {
        ctx.clear_kv_cache();

        let full_prompt = format_prompt(model, model_id, request, quirks)?;
        let tokens = model
            .str_to_token(&full_prompt, AddBos::Always)
            .map_err(|e| Error::Inference(format!("Tokenization failed: {e}")))?;

        let n_ctx = ctx.n_ctx() as usize;
        let max_tokens = clamp_max_tokens(
            request.max_tokens,
            tokens.len(),
            n_ctx,
        )?;
        let prompt_tokens = tokens.len();

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
        // Stray-close tracking: see plan §1.3. Streaming can't
        // rewrap once tokens have flushed; we log so the operator
        // can see this happen and the desktop's parse-message
        // synthesises a virtual `<think>` open client-side.
        let mut saw_open_tag = false;
        let mut saw_close_tag = false;

        // See generate_sync: stop on `</tool_call>` (marker) or
        // balanced JSON envelope (grammar-locked) when tools were
        // requested. Shape-B tracker gated on `structured_output` to
        // avoid false-positive stops on prose `{...}` — see fn-scope
        // note on the sync path.
        let tools_present = request.tools.as_ref().is_some_and(|t| !t.is_empty());
        let tools_grammar_locked =
            tools_present && request.structured_output.is_some();
        let mut tc_json_depth = 0i32;
        let mut tc_json_in_string = false;
        let mut tc_json_escape_next = false;
        let mut tc_json_ever_opened = false;

        // Default to Length: if the loop exits via the `while`
        // condition we hit max_tokens. Each `break` path overwrites
        // this with the more-specific reason.
        let mut reason = FinishReason::Length;

        while n_generated < max_tokens {
            if let Some(t) = cancel {
                if t.is_cancelled() {
                    tracing::warn!(
                        tokens_emitted = n_generated,
                        "inference:cancelled via CancellationToken"
                    );
                    reason = FinishReason::Cancelled;
                    break;
                }
            }

            let role = if tools_present && tc_json_in_string {
                SamplerRole::Content
            } else {
                SamplerRole::Explore
            };
            let token = sampler.sample(ctx, -1, role);
            sampler.accept(token);

            if model.is_eog_token(token) {
                reason = FinishReason::Stop;
                break;
            }

            if let Ok(piece) = model.token_to_piece(token, &mut decoder, true, None) {
                tail.push_str(&piece);
                if tail.len() > 32 {
                    let mut drain_to = tail.len() - 32;
                    while drain_to > 0 && !tail.is_char_boundary(drain_to) {
                        drain_to -= 1;
                    }
                    tail.drain(..drain_to);
                }
                if !in_think && tail.contains("<think>") {
                    in_think = true;
                    saw_open_tag = true;
                } else if in_think && tail.contains("</think>") {
                    in_think = false;
                    saw_close_tag = true;
                } else if !in_think && tail.contains("</think>") {
                    saw_close_tag = true;
                }
                if in_think {
                    think_tokens += 1;
                }

                // JSON-balance bookkeeping BEFORE the send so we don't
                // need to re-borrow piece bytes after the move. Gated
                // on `tools_grammar_locked` per the sync-path note.
                let mut json_envelope_complete = false;
                if tools_grammar_locked && !in_think {
                    for b in piece.bytes() {
                        if tc_json_escape_next {
                            tc_json_escape_next = false;
                            continue;
                        }
                        if tc_json_in_string {
                            match b {
                                b'\\' => tc_json_escape_next = true,
                                b'"' => tc_json_in_string = false,
                                _ => {}
                            }
                            continue;
                        }
                        match b {
                            b'"' => tc_json_in_string = true,
                            b'{' => {
                                tc_json_depth += 1;
                                tc_json_ever_opened = true;
                            }
                            b'}' => tc_json_depth -= 1,
                            _ => {}
                        }
                    }
                    if tc_json_ever_opened && tc_json_depth == 0 {
                        json_envelope_complete = true;
                    }
                }

                if tx.blocking_send(StreamFrame::Token(piece)).is_err() {
                    tracing::warn!(
                        tokens_emitted = n_generated,
                        "inference:cancelled via receiver-drop"
                    );
                    // Receiver is gone — don't try to send a Finish
                    // frame either. Caller already knows.
                    return Ok(());
                }

                // Tool-emission stop — see generate_sync for rationale.
                if tools_present {
                    if tail.contains("</tool_call>") {
                        tracing::info!(
                            model = %model_id,
                            n_generated,
                            tail = %tail,
                            "inference: stopping on </tool_call> marker (stream-finish)"
                        );
                        reason = FinishReason::Stop;
                        break;
                    }
                    if json_envelope_complete {
                        tracing::info!(
                            model = %model_id,
                            n_generated,
                            "inference: stopping on balanced JSON envelope (stream-finish)"
                        );
                        reason = FinishReason::Stop;
                        break;
                    }
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
                        if tx.blocking_send(StreamFrame::Token(fp)).is_err() {
                            return Ok(());
                        }
                    }
                }
                in_think = false;
            }
        }

        ctx.clear_kv_cache();

        if saw_close_tag && !saw_open_tag {
            tracing::warn!(
                model = model_id,
                "thinking-tag stray close (typed stream): model emitted \
                 </think> without a preceding <think> — desktop will repair \
                 the rendering but tokens already flushed; cause is usually \
                 enable_thinking:false on a thinking model"
            );
        }

        let usage = StreamUsage {
            prompt_tokens: prompt_tokens as u32,
            completion_tokens: n_generated as u32,
            total_tokens: (prompt_tokens + n_generated) as u32,
        };
        let _ = tx.blocking_send(StreamFrame::Finish {
            reason,
            usage: Some(usage),
        });

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
    ctx: crate::llama::cpp::context::LlamaContext<'static>,
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
    fn embed_sync(
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
    fn embed_query_sync(
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
    model: Arc<LlamaModel>,
    // std::sync::Mutex (not tokio::sync) because score_batch runs
    // inside spawn_blocking — the lock is held across a blocking
    // llama.cpp decode, never across an `.await`.
    ctx: std::sync::Mutex<RerankSlotContext>,
    /// Hard cap on the full prompt (chat-template wrapping + query +
    /// doc + trailing rerank token). Pairs longer than this are
    /// truncated on the doc side.
    max_input_tokens: usize,
    /// Token-ID of `<|score_token|>` — read once at load time. The
    /// reranker emits this token's logit at the last-input position
    /// and that logit IS the relevance score.
    score_token_id: i32,
    /// Token-ID of `<|rerank_token|>` — placed at the end of the
    /// prompt so the model conditions on "now produce a score".
    rerank_token_id: i32,
    /// Pre-tokenised chat-template prefix (everything up to where
    /// the query gets spliced in). Built once at load time so the
    /// hot path skips re-tokenising the boilerplate.
    prefix_tokens: Vec<LlamaToken>,
    /// Pre-tokenised separator between query and document (the
    /// `<|im_end|>\n<doc>` glue in the Qwen3 chat template).
    middle_tokens: Vec<LlamaToken>,
    /// Pre-tokenised suffix between document and `<|rerank_token|>`
    /// (the `<|im_end|>\n<|im_start|>assistant\n` glue).
    suffix_tokens: Vec<LlamaToken>,
    /// File stem of the loaded reranker — surfaced in provenance +
    /// telemetry so a user can tell which reranker scored a given
    /// result. Format matches `ModelSlot.model_id`.
    model_id: String,
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
    let variant = std::env::var("SOVEREIGN_RERANK_PROMPT_VARIANT")
        .unwrap_or_else(|_| "verbose".to_string());
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

struct RerankSlotContext {
    ctx: crate::llama::cpp::context::LlamaContext<'static>,
    _model: Arc<LlamaModel>,
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
        let gpu_default_available = cfg!(all(target_os = "macos", target_arch = "aarch64"))
            || cfg!(target_os = "linux");
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
                        "GPU rerank context failed, falling back to CPU"
                    );
                }
                let ctx = unsafe {
                    let model_ref: &'static LlamaModel =
                        &*(Arc::as_ptr(&model) as *const LlamaModel);
                    model_ref
                        .new_context(backend, build_params(false))
                        .map_err(|e| {
                            Error::Inference(format!(
                                "Failed to create rerank context: {e}"
                            ))
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
        let score_token_id =
            resolve_special_token(&model, "<|score_token|>").ok_or_else(|| {
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
            ctx: std::sync::Mutex::new(RerankSlotContext {
                ctx,
                _model: model,
            }),
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
        let fixed = prefix_tokens.len()
            + middle_tokens.len()
            + suffix_tokens.len()
            + 1;
        let dynamic_budget = max_input_tokens.saturating_sub(fixed);
        const QUERY_HARD_CAP: usize = 256;
        let query_budget = query_tokens.len().min(QUERY_HARD_CAP).min(dynamic_budget);
        let doc_budget = dynamic_budget.saturating_sub(query_budget);
        if dynamic_budget == 0 {
            return Err(Error::Inference(
                "Rerank context too small for prompt scaffold".to_string(),
            ));
        }

        let mut tokens: Vec<LlamaToken> =
            Vec::with_capacity(prefix_tokens.len() + query_budget + middle_tokens.len()
                + doc_budget + suffix_tokens.len() + 1);
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
fn l2_normalize(mut v: Vec<f32>) -> Vec<f32> {
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
fn llama_threads_for_host() -> usize {
    let raw = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    raw.clamp(2, 8)
}

/// Label emitted in slot-load logs when the GPU path wasn't taken
/// (GPU context creation failed, or caller didn't request it).
/// Tells the operator which CPU math backend GGML is using.
fn embed_compute_backend_label() -> &'static str {
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
fn gpu_backend_label() -> &'static str {
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
    request: CompletionRequest,
    response: tokio::sync::oneshot::Sender<Result<CompletionResponse>>,
}

struct FastShortCoalescer {
    enqueue: tokio::sync::mpsc::UnboundedSender<CoalescerJob>,
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
                        let resp = CompletionResponse {
                            text,
                            tokens_used: prompt_tokens + completion_tokens,
                            prompt_tokens,
                            model_id: model_id.clone(),
                            latency_ms,
                            oicp_meta: None,
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
    slots: HashMap<String, Arc<ModelSlot>>,
    /// Index from advertised model_id (gguf file stem) →
    /// `slots` key. Lookup is the hot path for
    /// `select_slot_for_request`, so we keep it pre-resolved.
    by_model_id: HashMap<String, String>,
    /// Per-slot quirks. Keys mirror `slots`. All entries today
    /// carry `ModelFamily::Unknown` defaults — runtime adapts to
    /// the gguf header. Reserved as its own field so a future
    /// op-flag could pin a specific family per slot.
    quirks: HashMap<String, ModelQuirks>,
    /// Optional total-memory budget in bytes for the extras
    /// lineup. When `Some(b)`, a `load_extra` that would push
    /// `(sum of size_bytes) + new_size` above `b` triggers LRU
    /// eviction of the coldest non-busy slots. `None` (default)
    /// disables eviction. Wired from
    /// `SetupConfig::ModelsSection.max_extras_memory_gb`.
    budget_bytes: Option<u64>,
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
enum SlotTarget {
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
fn pick_slot(
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
        let fast_short_disabled = std::env::var("SOVEREIGN_FAST_SHORT_DISABLE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let (fast_short, fast_short_coalescer) = if fast_short_disabled {
            tracing::info!(
                slot = "fast_short",
                "skipped (SOVEREIGN_FAST_SHORT_DISABLE=1)"
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
struct EvictionCandidate {
    slot_name: String,
    last_used_ms: u64,
    size_bytes: u64,
}

/// Outcome of [`pick_evictions`]. Three states map to three
/// caller actions: nothing to do, evict these names in order, or
/// fail loud because the cold lineup can't free enough.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EvictionPlan {
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
fn pick_evictions(
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
                    ModelSlot::generate_sync(&slot.model, &slot.model_id, &mut *ctx_lock, &request, &quirks)
                }));
                let (text, prompt_tokens, completion_tokens) = match result {
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
                    text,
                    tokens_used: prompt_tokens + completion_tokens,
                    prompt_tokens,
                    model_id: slot.model_id.clone(),
                    latency_ms,
                    oicp_meta: None,
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
                            &mut *ctx_lock,
                            &request,
                            &quirks,
                        )
                    }));
                    let (text, prompt_tokens, completion_tokens) = match result {
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
                        text,
                        tokens_used: prompt_tokens + completion_tokens,
                        prompt_tokens,
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
                    ModelSlot::generate_sync(&slot.model, &slot.model_id, &mut *ctx_lock, &request, &quirks)
                }));

                let (text, prompt_tokens, completion_tokens) = match result {
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
                    text,
                    tokens_used: prompt_tokens + completion_tokens,
                    prompt_tokens,
                    // `slot` may have been dropped on the fresh-ctx
                    // path; use the model_id cloned before the
                    // branch.
                    model_id: model_id.clone(),
                    latency_ms,
                    oicp_meta: None,
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
                    ModelSlot::generate_sync(&slot.model, &slot.model_id, &mut *ctx_lock, &request, &quirks)
                }));

                let (text, prompt_tokens, completion_tokens) = match result {
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
                    tokens_used: prompt_tokens + completion_tokens,
                    prompt_tokens,
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
/// Read the `general.architecture` metadata field from a loaded gguf.
/// Returns the raw arch string ("qwen3", "qwen3_moe", "mamba",
/// "deltanet", "gemma3", etc.) or an empty string when the metadata
/// read fails. The caller treats empty as "unknown" and falls back
/// to the name-pattern heuristic.
///
/// Cheap: a single `llama_model_meta_val_str(model, "general.architecture", buf, 256)`
/// call. Done once at slot construction; the result lives on
/// `SlotContext.arch` for the lifetime of the slot.
fn read_gguf_arch(model: &LlamaModel) -> String {
    match model.meta_val_str("general.architecture", 256) {
        Ok(s) => {
            tracing::info!(arch = %s, "gguf arch: read from general.architecture");
            s
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "gguf arch read failed — falling back to name-pattern heuristic"
            );
            String::new()
        }
    }
}

/// Is a gguf architecture string known to have recurrent layers
/// (Mamba / Gated DeltaNet / RWKV / hybrid SSM+MoE)?
///
/// Source of truth for the `prefix_cache_safe` gate when the slot
/// carries arch metadata. Empty string is "unknown" — caller falls
/// back to `ModelQuirks::has_recurrent_layers` declared per-family
/// in `model_family.rs`.
pub(crate) fn is_recurrent_arch(arch: &str) -> bool {
    if arch.is_empty() {
        return false;
    }
    let lower = arch.to_lowercase();
    // Qwen MoE families (Qwen3-MoE, Qwen3.5-MoE, Qwen3.6-MoE) carry
    // Gated DeltaNet layers in the gguf attention stack. Same
    // recurrent-state hazard as Mamba — partial-keep prefix-cache
    // returns -1 on tail decode. Observed gguf arch values include
    // `qwen3moe`, `qwen35moe`, `qwen3_moe`, `qwen36moe`. The shape
    // is "qwen<version>moe", so we substring-match `moe` after a
    // qwen prefix.
    if lower.starts_with("qwen") && lower.contains("moe") {
        return true;
    }
    // Explicit recurrent architectures published in gguf metadata.
    // Substring match catches version suffixes (mamba2, rwkv6, etc.).
    for marker in ["mamba", "rwkv", "deltanet", "ssm"] {
        if lower.contains(marker) {
            return true;
        }
    }
    false
}


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

fn format_prompt(
    model: &LlamaModel,
    model_id: &str,
    request: &CompletionRequest,
    quirks: &ModelQuirks,
) -> Result<String> {
    // Compute the rendered prompt via the inner tier dispatch, then
    // append `request.assistant_prefix` if present. The prefix lands
    // *after* the chat template's generation-position marker
    // (`<|turn>model\n`, `<|im_start|>assistant\n`, `<start_of_turn>model\n`,
    // …) and *before* the model's first generated token — letting
    // upstream nudges (frontdoor's read-attractor, failure-recovery)
    // commit the model to a known-good response prefix structurally
    // rather than via instruction. Family-agnostic: every chat
    // template the inner dispatch handles ends at the generation
    // marker, so the append point is consistent.
    let rendered = format_prompt_inner(model, model_id, request, quirks)?;
    Ok(append_assistant_prefix(rendered, request.assistant_prefix.as_deref()))
}

/// Append a non-empty `assistant_prefix` to the rendered prompt.
/// Empty / None prefixes pass through unchanged so the historical
/// behaviour (no prefill) is preserved for callers that don't set
/// the field.
fn append_assistant_prefix(mut prompt: String, prefix: Option<&str>) -> String {
    if let Some(p) = prefix {
        if !p.is_empty() {
            // The rendered prompt always ends at the generation
            // marker with no trailing whitespace contract — append
            // directly. If a future template renders trailing
            // whitespace, the prefix still composes; the model's
            // tokenizer absorbs the whitespace into the prefix's
            // first token.
            prompt.push_str(p);
            tracing::debug!(
                prefix_len = p.len(),
                "format_prompt: assistant_prefix appended"
            );
        }
    }
    prompt
}

fn format_prompt_inner(
    model: &LlamaModel,
    model_id: &str,
    request: &CompletionRequest,
    quirks: &ModelQuirks,
) -> Result<String> {
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

    // Tool-use prompt augmentation. Qwen3.5's chat template expects tool
    // schemas to appear in the system prompt inside a `<tools>...</tools>`
    // block of newline-separated JSON objects. The model then emits tool
    // calls as `<tool_call>{"name": "...", "arguments": {...}}</tool_call>`
    // which `parse_tool_calls_from_text` below decodes.
    //
    // We append to the existing system prompt (rather than replacing or
    // injecting a separate message) so the thinking-token augmentation
    // above still lands. Tools without a system prompt get a minimal
    // "You may call one of the following tools" lead-in so the model
    // doesn't try to emit tool calls in free chat by default.
    let system_with_tools = if let Some(tools) = request.tools.as_ref().filter(|t| !t.is_empty()) {
        let mut block = String::from(
            "\n\n# Tools\n\nYou may call one or more of the following tools by emitting a \
             `<tool_call>{\"name\": ..., \"arguments\": {...}}</tool_call>` block. One call per \
             block. After a block, stop — the runtime will execute the tool and feed the result \
             back to you in the next turn.\n\n<tools>\n"
        );
        for t in tools {
            let entry = serde_json::json!({
                "name": t.name,
                "description": t.description.clone().unwrap_or_default(),
                "parameters": t.parameters,
            });
            block.push_str(&entry.to_string());
            block.push('\n');
        }
        block.push_str("</tools>");
        if system_with_thinking.is_empty() {
            block.trim_start().to_string()
        } else {
            format!("{system_with_thinking}{block}")
        }
    } else {
        system_with_thinking
    };
    let system_with_thinking = system_with_tools;

    // Three-tier prompt-building strategy. Each tier is tried in
    // order; the first one that succeeds wins.
    //
    // 1. **Basic `apply_chat_template`** — calls llama.cpp's
    //    built-in `llama_chat_apply_template`. Fast, no JSON
    //    serialization, but its template parser only supports a
    //    limited Jinja-like subset (no macros, no complex control
    //    flow). Works for Qwen3 / Llama3 / base Phi-4 / etc.
    //
    // 2. **`apply_chat_template_oaicompat` with `use_jinja=true`**
    //    — calls llama.cpp's full minja-based Jinja2 path (the
    //    same one llama-server's `--jinja` flag uses). Handles
    //    templates that use macros or complex control flow.
    //    Required for Gemma 3/4 (their gguf templates start with
    //    `{%- macro format_parameters() -%}`).
    //
    // 3. **Plain-text concat** (last resort) — `{system}\n\n{user}`
    //    with no role markers. The model has no turn boundaries
    //    so it'll role-play multi-turn ("User: ... Assistant: ...")
    //    until `max_tokens`. We loud-warn so operators see this is
    //    happening — it was a silent fallback up until 2026-04-26
    //    (gemma-4-31B atlas bench debugging session).
    // Retrieve the chat template stored in the model's gguf metadata
    // (`tokenizer.chat_template`). Pre-2026-05-17 this returned a
    // `LlamaChatTemplate` struct; post-migration to llama-cpp-4 0.2.x
    // it returns an `Option<String>`. `None` ⇒ no chat-template
    // metadata in the gguf, so plain-text concat is the only path.
    let Some(template) = crate::llama::chat_template(model) else {
        tracing::warn!(
            model_id = %model_id,
            "chat_template lookup returned None — model gguf has no \
             tokenizer.chat_template metadata. Falling back to plain-text concat; \
             model output may include hallucinated role markers and may not stop \
             at the right place."
        );
        return Ok(plain_text_prompt(&system_with_thinking, &request.prompt));
    };

    // **Migration note (2026-05-17 llama-cpp-2 → llama-cpp-4):** the
    // `openai`-shaped `apply_chat_template_oaicompat` path (with its
    // `OpenAIChatTemplateParams` and `use_jinja=true` knob) was
    // retired upstream. The remaining path is `apply_chat_template`,
    // which uses llama.cpp's built-in template parser. Two
    // consequences for callers:
    //
    //   1. Templates that need full Jinja2 (Gemma 3/4 macros, e.g.)
    //      will fail at `apply_chat_template` and fall through to
    //      plain-text concat (loud warn). The MTP bench target
    //      (Qwen3.6-A3B) uses a template the built-in parser
    //      accepts, so this regression is acceptable on this branch.
    //
    //   2. Per-request `enable_thinking` is now spliced into the
    //      system message (`/no_think` / `/think`) rather than
    //      passed as a separate flag, because the binding's chat
    //      surface no longer exposes thinking control. Most callers
    //      pass `None` / `false`; the few that pin `true` (e.g. the
    //      witness path) get a `<think>` wrapper from the prefix.
    //
    //   3. `template_needs_jinja` is retained as documentation of
    //      which templates were Gemma-shaped; calling it just routes
    //      to the same `apply_chat_template` call now, but tracing
    //      the boolean preserves the diagnostic surface.
    let needs_jinja = template_needs_jinja(&template);
    if needs_jinja {
        tracing::debug!(
            model_id = %model_id,
            "chat template needs full Jinja2 (macros/sets/includes detected); \
             routing through the Rust-side minijinja renderer instead of \
             llama.cpp's limited-subset parser."
        );
        match apply_chat_template_minijinja(
            &template,
            &system_with_thinking,
            &request.prompt,
            request.enable_thinking.unwrap_or(false),
        ) {
            Ok(rendered) => return Ok(rendered),
            Err(e) => {
                tracing::warn!(
                    model_id = %model_id,
                    error = %e,
                    template_head = %template_head_for_log(&template),
                    "minijinja render failed — falling back to llama.cpp \
                     apply_chat_template, then plain-text concat."
                );
            }
        }
    }
    if request.enable_thinking.unwrap_or(false) {
        tracing::warn!(
            model_id = %model_id,
            "request.enable_thinking=true requested, but the llama-cpp-4 binding \
             dropped chat-template thinking control. Output may not be wrapped in \
             <think>...</think>; downstream strip_thinking_tags will pass through. \
             TODO: splice /think hint into system message."
        );
    }

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
    match model.apply_chat_template(Some(&template), &messages, true) {
        Ok(formatted) => return Ok(formatted),
        Err(e) => {
            tracing::warn!(
                model_id = %model_id,
                error = ?e,
                template_head = %template_head_for_log(&template),
                jinja_required = needs_jinja,
                "apply_chat_template failed — model's gguf template likely needs \
                 a Jinja construct the built-in parser doesn't support, and the \
                 0.2.x binding dropped the oaicompat Jinja2 path. Falling back to \
                 plain-text concat; model output may include hallucinated role markers."
            );
        }
    }

    // Final fallback: plain-text concat.
    Ok(plain_text_prompt(&system_with_thinking, &request.prompt))
}

/// Render a `tokenizer.chat_template` (Jinja2 source) using the
/// Rust-side `minijinja` engine. Handles macros, `set` blocks, and
/// other constructs the llama.cpp built-in parser rejects.
///
/// Returns the formatted prompt the tokenizer feeds the model.
/// `add_generation_prompt=true` matches what llama.cpp's
/// `apply_chat_template` does — appends the assistant's turn-start
/// marker so the model picks up where the template left off.
///
/// `enable_thinking` toggles the `/think` vs `/no_think` hint that
/// Qwen3-family templates honour as a Jinja variable. Models that
/// ignore the flag are unaffected.
fn apply_chat_template_minijinja(
    template: &str,
    system: &str,
    user: &str,
    enable_thinking: bool,
) -> Result<String> {
    use minijinja::{context, Environment, Value};

    // Build the messages list the template iterates over.
    let mut messages: Vec<Value> = Vec::with_capacity(2);
    if !system.is_empty() {
        messages.push(Value::from_serialize(&serde_json::json!({
            "role": "system",
            "content": system,
        })));
    }
    messages.push(Value::from_serialize(&serde_json::json!({
        "role": "user",
        "content": user,
    })));

    let mut env = Environment::new();
    // Match Hugging Face's `Jinja2 Templates` behaviour: keep the
    // raise_exception filter available — some templates call it
    // (`{{ raise_exception("…") }}`) to halt on bad input.
    env.add_function(
        "raise_exception",
        |msg: String| -> std::result::Result<String, minijinja::Error> {
            Err(minijinja::Error::new(
                minijinja::ErrorKind::InvalidOperation,
                msg,
            ))
        },
    );
    // Python-compat method shim. HF templates routinely call
    // `.get(key)`, `.get(key, default)`, `.split(sep)`,
    // `.startswith(prefix)`, `.endswith(suffix)`, `.upper()`,
    // `.lower()`, `.strip()` — methods that exist on Python's
    // `dict`/`str`/`list` but aren't in stock minijinja. Without
    // this shim, Gemma 4's template (which calls
    // `message.get('reasoning')`, `message.get('tool_calls')`,
    // `value['type'] | upper`, `part.split('<|channel>')`, …) fails
    // at the first unknown method and we fall through to plain-text
    // concat. The pycompat surface in `minijinja-contrib` would
    // also do this, but pulling in another workspace dep for a
    // half-dozen methods is excessive — handle them inline.
    env.set_unknown_method_callback(|_state, value, method, args| {
        use minijinja::value::{from_args, ValueKind};
        use minijinja::{Error, ErrorKind, Value};
        match method {
            "get" => {
                // dict.get(key) or dict.get(key, default)
                if value.kind() != ValueKind::Map {
                    return Err(Error::from(ErrorKind::UnknownMethod));
                }
                let (key, default): (Value, Option<Value>) = from_args(args)?;
                let key_str: String = key
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| key.to_string());
                match value.get_attr(&key_str) {
                    Ok(v) if !v.is_undefined() => Ok(v),
                    _ => Ok(default.unwrap_or(Value::from(())))
                }
            }
            "split" => {
                // str.split(sep) — sep is required in HF templates we've
                // seen (no zero-arg whitespace split path needed yet).
                let s = value.as_str().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidOperation, "split on non-string")
                })?;
                let (sep,): (String,) = from_args(args)?;
                let parts: Vec<Value> =
                    s.split(&sep).map(|p| Value::from(p.to_string())).collect();
                Ok(Value::from(parts))
            }
            "startswith" => {
                let s = value.as_str().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidOperation, "startswith on non-string")
                })?;
                let (prefix,): (String,) = from_args(args)?;
                Ok(Value::from(s.starts_with(&prefix)))
            }
            "endswith" => {
                let s = value.as_str().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidOperation, "endswith on non-string")
                })?;
                let (suffix,): (String,) = from_args(args)?;
                Ok(Value::from(s.ends_with(&suffix)))
            }
            "upper" => {
                let s = value.as_str().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidOperation, "upper on non-string")
                })?;
                let _: () = from_args(args)?;
                Ok(Value::from(s.to_uppercase()))
            }
            "lower" => {
                let s = value.as_str().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidOperation, "lower on non-string")
                })?;
                let _: () = from_args(args)?;
                Ok(Value::from(s.to_lowercase()))
            }
            "strip" => {
                let s = value.as_str().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidOperation, "strip on non-string")
                })?;
                let _: () = from_args(args)?;
                Ok(Value::from(s.trim().to_string()))
            }
            _ => Err(Error::from(ErrorKind::UnknownMethod)),
        }
    });
    env.add_template("chat", template).map_err(|e| {
        Error::Inference(format!("minijinja: compile chat template: {e}"))
    })?;
    let tmpl = env
        .get_template("chat")
        .map_err(|e| Error::Inference(format!("minijinja: load chat template: {e}")))?;

    // Variables every reasonable chat template touches. Most
    // templates don't use every key — providing them all is safe
    // (Jinja2 is forgiving of unused globals). Templates that
    // reference unknown vars produce empty strings, which is the
    // same behaviour as the llama.cpp path.
    let ctx = context! {
        messages => messages,
        add_generation_prompt => true,
        enable_thinking => enable_thinking,
        bos_token => "",
        eos_token => "",
        // Qwen3-family hint surfaced via a context variable on some
        // template revisions. Most templates inspect the system
        // message text instead.
        thinking_mode => if enable_thinking { "think" } else { "no_think" },
    };
    tmpl.render(ctx)
        .map_err(|e| Error::Inference(format!("minijinja: render chat template: {e}")))
}

/// Cheap heuristic for whether a chat template requires the full
/// Jinja2 (minja) renderer rather than llama.cpp's built-in
/// limited-subset parser. The built-in parser handles plain
/// `{{ var }}` substitution, simple `{% if %} ... {% endif %}`
/// blocks, and `{% for x in xs %}` loops — but trips on macros
/// (`{%- macro ... -%}`), `{% set ... %}` blocks, and a few other
/// constructs that the recent generation of model templates use.
///
/// We don't try to be exhaustive — any false negative just
/// degrades back to the existing fall-through behaviour (tier-1
/// returns FfiError, tier 2 takes over). The win here is a clean
/// fast path for known-Jinja templates so we don't log a
/// debug-level noise line on every chat call.
fn template_needs_jinja(template: &str) -> bool {
    template.contains("{%- macro")
        || template.contains("{% macro")
        || template.contains("{%- set")
        || template.contains("{% set")
        || template.contains("{%- include")
        || template.contains("{% include")
}

/// Final-fallback prompt when no chat-template path works. Just
/// concatenates system + user with a blank line. The model has no
/// turn boundaries; expect it to role-play multi-turn output.
fn plain_text_prompt(system: &str, user: &str) -> String {
    if system.is_empty() {
        user.to_string()
    } else {
        format!("{system}\n\n{user}")
    }
}

/// Serialize a (system, user) pair into the OpenAI-compatible
/// messages JSON shape that `apply_chat_template_oaicompat`
/// expects. System is omitted when empty so templates that don't
/// support a system role (e.g. Gemma) don't get a stray empty
/// turn.
fn build_oai_messages_json(system: &str, user: &str) -> Result<String> {
    let mut messages: Vec<serde_json::Value> = Vec::with_capacity(2);
    if !system.is_empty() {
        messages.push(serde_json::json!({"role": "system", "content": system}));
    }
    messages.push(serde_json::json!({"role": "user", "content": user}));
    serde_json::to_string(&messages)
        .map_err(|e| Error::Inference(format!("Failed to serialize chat messages: {e}")))
}

/// First ~80 chars of a chat template, with newlines escaped, for
/// log output. Lets operators identify which template format is
/// hitting the fallback path without dumping a full multi-KB
/// macro definition into the daemon log.
fn template_head_for_log(template: &str) -> String {
    let head: String = template.chars().take(80).collect();
    head.replace('\n', "\\n")
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
/// Sampler chain plus optional schema-driven logit mask.
///
/// History: the per-token loop used to call `LlamaSampler::sample`
/// directly, which runs the entire chain inside llama.cpp. Grammar
/// enforcement was provided either by `LlamaSampler::llguidance`
/// (silent fallthrough on every BYOM model) or `LlamaSampler::grammar`
/// (crashes the daemon process at `GGML_ASSERT(!stacks.empty())`,
/// `llama-grammar.cpp:940`, reproducible across Vulkan AND ROCm AND
/// single-slot daemon configurations). See
/// `memory/project_grammar_alpha_blocker.md`.
///
/// `ConstrainedSampler` replaces both: it owns a `LlamaSampler` chain
/// (without any grammar sampler), and optionally owns a
/// `JsonConstraint` that masks token logits in pure Rust before the
/// chain runs. No call into `llama-grammar.cpp` ever fires.

/// Resolve the jump-forward enable gate from an env-getter. Returns
/// `true` (jump-forward active) by default; only an explicit
/// `SOVEREIGN_JUMP_FWD_DISABLE` value of `"1"` or `"true"`
/// (case-insensitive) turns it off. Generic over the env-getter so
/// tests can pin behaviour without mutating process-global env (which
/// races against parallel test execution).
fn jump_fwd_enabled_from_env<F>(env_get: F) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    match env_get("SOVEREIGN_JUMP_FWD_DISABLE") {
        Some(v) => !(v == "1" || v.eq_ignore_ascii_case("true")),
        None => true,
    }
}

/// Same shape as `jump_fwd_enabled_from_env` but for the Tier 2 gate.
/// Independent of the master gate so an operator can keep Tier 1 on
/// while disabling Tier 2 (e.g. to A/B the marginal Tier 2 lift on a
/// running daemon).
fn jump_fwd_t2_enabled_from_env<F>(env_get: F) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    match env_get("SOVEREIGN_JUMP_FWD_T2_DISABLE") {
        Some(v) => !(v == "1" || v.eq_ignore_ascii_case("true")),
        None => true,
    }
}

#[cfg(test)]
mod chat_template_minijinja_tests {
    use super::apply_chat_template_minijinja;

    /// Smoke test on a Qwen3-shape `{% for %}` template — the
    /// built-in parser handles this too, so the minijinja path
    /// must produce equivalent output (not byte-equivalent — we
    /// only check the role markers + body land in the right slots).
    #[test]
    fn renders_simple_for_loop_template() {
        let template = "\
{%- for m in messages -%}\
<|im_start|>{{ m.role }}\n{{ m.content }}<|im_end|>\n\
{%- endfor -%}\
{%- if add_generation_prompt -%}<|im_start|>assistant\n{%- endif -%}";
        let out = apply_chat_template_minijinja(
            template,
            "you are a helpful assistant",
            "hello",
            false,
        )
        .expect("renders cleanly");
        assert!(out.contains("<|im_start|>system"));
        assert!(out.contains("you are a helpful assistant"));
        assert!(out.contains("<|im_start|>user"));
        assert!(out.contains("hello"));
        assert!(out.contains("<|im_start|>assistant"));
    }

    /// Pinned regression: templates that declare a Jinja `{% macro %}`
    /// were the exact construct llama.cpp's built-in parser
    /// rejected (Gemma 3/4, Qwen3.5-9B-vOP). minijinja must handle
    /// them.
    #[test]
    fn renders_template_with_macro() {
        let template = "\
{%- macro render(m) -%}\
[{{ m.role }}] {{ m.content }}\n\
{%- endmacro -%}\
{%- for m in messages -%}{{ render(m) }}{%- endfor -%}\
{%- if add_generation_prompt -%}[assistant]\n{%- endif -%}";
        let out = apply_chat_template_minijinja(template, "sys", "ask", false).unwrap();
        assert!(out.contains("[system] sys"));
        assert!(out.contains("[user] ask"));
        // Whitespace control (`{%- -%}`) strips the trailing `\n`
        // inside the {%- if -%} block — match real Jinja2 behaviour.
        assert!(out.trim_end().ends_with("[assistant]"));
    }

    /// Templates that use `{% set %}` blocks were the other
    /// construct the built-in parser rejected.
    #[test]
    fn renders_template_with_set_block() {
        let template = "\
{%- set bos = '<bos>' -%}\
{{ bos }}\
{%- for m in messages -%}<{{ m.role }}>{{ m.content }}</{{ m.role }}>{%- endfor -%}";
        let out = apply_chat_template_minijinja(template, "", "q", false).unwrap();
        assert!(out.starts_with("<bos>"));
        assert!(out.contains("<user>q</user>"));
    }

    /// `enable_thinking` is exposed to the template so Qwen3-family
    /// templates can branch on it. Verify the variable is wired.
    #[test]
    fn renders_template_branching_on_enable_thinking() {
        let template = "\
{%- if enable_thinking -%}/think{%- else -%}/no_think{%- endif -%}";
        assert_eq!(
            apply_chat_template_minijinja(template, "", "", true).unwrap(),
            "/think"
        );
        assert_eq!(
            apply_chat_template_minijinja(template, "", "", false).unwrap(),
            "/no_think"
        );
    }

    /// `raise_exception` is the canonical Hugging Face escape hatch
    /// for templates that want to halt on bad input. Forward it as a
    /// minijinja error so the caller falls through to the
    /// llama.cpp built-in tier rather than producing a malformed
    /// prompt.
    #[test]
    fn raise_exception_is_propagated_as_error() {
        let template = "{{ raise_exception('nope') }}";
        let err = apply_chat_template_minijinja(template, "", "", false).unwrap_err();
        assert!(format!("{err}").contains("nope"), "error chain: {err}");
    }

    /// Empty system message is dropped (template iterates only the
    /// user message). Pinned because some real templates assume
    /// every message has non-empty content.
    #[test]
    fn empty_system_dropped_from_messages() {
        let template = "\
{%- for m in messages -%}{{ m.role }}\n{%- endfor -%}";
        let out = apply_chat_template_minijinja(template, "", "q", false).unwrap();
        // Only user role present; no leading "system". `{%- endfor -%}`
        // strips the trailing newline, so the body is just "user".
        assert_eq!(out, "user");
    }
}

#[cfg(test)]
mod jump_fwd_env_tests {
    use super::*;

    #[test]
    fn jump_fwd_default_is_on() {
        assert!(jump_fwd_enabled_from_env(|_| None));
        assert!(jump_fwd_t2_enabled_from_env(|_| None));
    }

    #[test]
    fn jump_fwd_disabled_only_on_truthy() {
        // Explicit truthy values disable.
        assert!(!jump_fwd_enabled_from_env(|_| Some("1".to_string())));
        assert!(!jump_fwd_enabled_from_env(|_| Some("true".to_string())));
        assert!(!jump_fwd_enabled_from_env(|_| Some("TRUE".to_string())));
        // Other values keep jump-forward on — "0", garbage, empty all
        // mean "no, you're not disabling me".
        assert!(jump_fwd_enabled_from_env(|_| Some("0".to_string())));
        assert!(jump_fwd_enabled_from_env(|_| Some("false".to_string())));
        assert!(jump_fwd_enabled_from_env(|_| Some("".to_string())));
        assert!(jump_fwd_enabled_from_env(|_| Some("yes".to_string())));
    }

    #[test]
    fn jump_fwd_t2_disable_independent_of_master_gate() {
        // The two gates read different env vars; disabling one must
        // not affect the other.
        let only_t2_disabled =
            |k: &str| if k == "SOVEREIGN_JUMP_FWD_T2_DISABLE" {
                Some("1".to_string())
            } else {
                None
            };
        assert!(jump_fwd_enabled_from_env(only_t2_disabled));
        assert!(!jump_fwd_t2_enabled_from_env(only_t2_disabled));
    }
}

/// Which "role" the sampler is currently filling. The same primary
/// slot serves multiple cognitive tasks per turn — picking the next
/// tool and authoring its arguments behave very differently from
/// each other and from filling content inside a JSON string value
/// (paths, file contents, Rust source). Investment #16 (2026-05-13):
/// instead of one global temperature, the sampler holds a per-role
/// `LlamaSampler` chain and the generation loop picks which to pull
/// each token from based on its position in the emit stream.
///
/// Roles are intentionally coarse-grained at v1 — easy to extend
/// (e.g. add `Decision` for the first K tokens after a `:` in a JSON
/// envelope), but the two-way split captures the dominant cost:
/// high-T exploration helps tool selection escape attractors,
/// low-T content fills paths/code without char-level drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamplerRole {
    /// Default: tool envelope structure, decision points, anywhere
    /// outside a JSON string. Per-request `temperature` applies.
    Explore,
    /// Inside a JSON string value (path, command text, file
    /// content). Greedy or low-T to suppress char-level drift like
    /// `/Users/ale xsbryan` observed in the 2026-05-13 codex smoke.
    Content,
}

pub struct ConstrainedSampler {
    /// Per-role inner chains. `accept()` advances both so the DRY /
    /// penalty trackers stay in sync regardless of which one
    /// produced any given token.
    inner_explore: LlamaSampler,
    inner_content: LlamaSampler,
    constraint: Option<crate::json_constraint::JsonConstraint>,
    /// llguidance-driven constraint, parallel to `constraint`.
    /// Mutually exclusive in practice — `build_sampler` picks ONE of
    /// `constraint` or `llg_constraint` based on the request:
    /// `request.lark_grammar` wins when set (Lark + JSON-Schema +
    /// top-level alternation via `LlguidanceConstraint`); otherwise
    /// `request.structured_output` selects the in-house mask.
    ///
    /// Wired 2026-05-21 to close the `parse_failed_envelope` +
    /// `loop_trap` failure classes the agent-bench scanner surfaced
    /// the same day. The lark path lets the model emit either a
    /// parseable tool envelope OR plain text (no parser leniency
    /// retry chain needed; no force-tool-call loop trap).
    llg_constraint: Option<crate::llguidance_constraint::LlguidanceConstraint>,
    /// URL-allowlist constraint. When `Some`, every sampled token's
    /// bytes are simulated against a byte-trie of allowed URLs; tokens
    /// whose bytes would start or extend a URL outside the allowlist
    /// get clamped to `-INFINITY`. Prose tokens (anything that doesn't
    /// look like the start of an `http://` / `https://` sequence) pass
    /// through unchanged.
    ///
    /// Skipped when the JSON-schema `constraint` above is also active:
    /// tool-call argument URLs are validated by the schema instead,
    /// and stacking the two state machines would deadlock the byte
    /// stream (the JSON FSM emits braces and quotes that the URL FSM
    /// would treat as URL terminators).
    url_constraint: Option<crate::url_constraint::UrlAllowlistConstraint>,
    /// Evidence-id allowlist constraint (Tier 2 of the tool-framework
    /// expansion). When `Some`, every sampled token's bytes are
    /// simulated against a byte-trie of allowed `ev-Tn-NNNN` handles;
    /// tokens that would extend `[ev-T…` into a non-existent id get
    /// clamped to `-INFINITY`. Prose tokens (anything that doesn't
    /// look like the start of an `[ev-T` citation) pass through
    /// unchanged.
    ///
    /// Skipped when the JSON-schema `constraint` above is also
    /// active — same byte-stream-deadlock rationale as the URL
    /// constraint (JSON FSM emits `]` which the citation FSM would
    /// treat as a terminator).
    evidence_id_constraint:
        Option<crate::evidence_id_constraint::EvidenceIdAllowlistConstraint>,
    /// Vocab-sized bitmap of tokens whose rendered bytes contain a
    /// 3+ byte UTF-8 leading byte (CJK / Devanagari / Hangul / etc.).
    /// When `Some`, `sample()` clamps those tokens' logits to
    /// `-INFINITY` on every step, regardless of whether a JSON
    /// constraint is active. Populated by `build_sampler` when the
    /// `SOVEREIGN_BLOCK_NON_LATIN` env var is set.
    non_latin_denylist: Option<std::sync::Arc<Vec<bool>>>,
}

impl ConstrainedSampler {
    /// Per-token: pull candidates from ctx, mask via constraint (if
    /// any), apply the rest of the chain, return the selected token.
    ///
    /// Performance note (2026-05-12): the mask is currently full-vocab
    /// O(152K × per-candidate-parser-cost). A previous attempt to
    /// pre-filter via `LlamaSampler::top_k(2048)` before the mask
    /// achieved 2.3× speedup on the trivial-prompt probe but
    /// produced incorrect output on real prompts (Chinese tokens
    /// slipped through the grammar). The root issue is that
    /// per-candidate incremental parsing is structurally wrong;
    /// the right fix is a precomputed token-acceptance table per
    /// FSM state (the pattern LM-Format-Enforcer and Outlines use).
    /// Until that lands, the slower-but-correct mask stays.
    pub fn sample(
        &mut self,
        ctx: &crate::llama::cpp::context::LlamaContext<'_>,
        idx: i32,
        role: SamplerRole,
    ) -> LlamaToken {
        let mut data = if idx < 0 {
            ctx.token_data_array()
        } else {
            ctx.token_data_array_ith(idx)
        };
        // Constraint dispatch: llguidance (lark + alternation) wins
        // over the in-house JsonConstraint mask when both are set.
        // build_sampler enforces mutual exclusion; this dispatch is
        // defensive should that invariant ever drift.
        if let Some(llg) = self.llg_constraint.as_mut() {
            llg.mask(&mut data);
        } else if let Some(c) = self.constraint.as_mut() {
            c.mask(&mut data);
        } else {
            // URL + evidence-id constraints are mutually exclusive
            // with the JSON constraint (deadlock rationale per the
            // field doc-comments) but compose freely with each
            // other — URLs and citations live in disjoint byte
            // patterns (`http(s)://` vs `[ev-T`). Apply both when
            // JSON isn't claiming the byte stream.
            if let Some(uc) = self.url_constraint.as_ref() {
                uc.mask(&mut data);
            }
            if let Some(eic) = self.evidence_id_constraint.as_ref() {
                eic.mask(&mut data);
            }
        }
        // Non-Latin denylist: independent of the JSON-schema mask, so
        // it covers free-form chat and non-`structured_output` paths
        // that the grammar layer doesn't reach. A single L1 lookup per
        // candidate; the bitmap was built once at slot-load.
        if let Some(deny) = self.non_latin_denylist.as_ref() {
            for entry in data.data.iter_mut() {
                let id = entry.id().0 as usize;
                if deny.get(id).copied().unwrap_or(false) {
                    entry.set_logit(f32::NEG_INFINITY);
                }
            }
        }
        // MIGRATION 2026-05-17: llama-cpp-4 0.2.x's `apply_sampler`
        // takes `&mut LlamaSampler` (was `&LlamaSampler` in 0.1.x).
        // The mutability moved because samplers now carry per-call
        // state (e.g. DRY's repetition history) that the previous
        // API hid behind `&self`. Take &mut via match — the parent
        // struct's wrapping `Mutex` already serialises access.
        let inner = match role {
            SamplerRole::Explore => &mut self.inner_explore,
            SamplerRole::Content => &mut self.inner_content,
        };
        data.apply_sampler(inner);
        data.selected_token()
            .expect("sampler chain failed to select a token")
    }

    /// Advance both inner chains (DRY / penalties state stays in
    /// sync regardless of which role produced this token) and the
    /// constraint state machine.
    pub fn accept(&mut self, token: LlamaToken) {
        self.inner_explore.accept(token);
        self.inner_content.accept(token);
        // Constraint accept dispatch mirrors `sample`'s mask dispatch.
        // llguidance wins when set; otherwise the in-house JSON
        // constraint advances. URL + evidence-id FSMs always advance
        // (their state has to stay synchronised with emitted bytes
        // even when the JSON mask was the gating constraint).
        if let Some(llg) = self.llg_constraint.as_mut() {
            llg.accept_llama(token);
        } else if let Some(c) = self.constraint.as_mut() {
            c.accept(token);
        }
        // Advance URL FSM unconditionally (even when JSON constraint
        // is active and the URL mask was skipped) so the cursor stays
        // synchronised with the actual emitted byte stream. The state
        // machine only matters once an `http://` / `https://` marker
        // appears in prose, and feeding it every token costs ~1 lookup.
        if let Some(uc) = self.url_constraint.as_mut() {
            uc.accept(token);
        }
        // Same synchronisation discipline for the evidence-id FSM —
        // cursor must track every emitted token, masked or not.
        if let Some(eic) = self.evidence_id_constraint.as_mut() {
            eic.accept(token);
        }
    }

    /// Returns `Some(token)` iff the active JSON-schema constraint has
    /// exactly one legal token in its current state — the read surface
    /// for jump-forward decoding. Returns `None` whenever no constraint
    /// is active (free-form chat, completion without `structured_output`)
    /// since there is nothing to short-circuit. Callers that emit the
    /// returned token MUST follow with `accept(token)` to keep the FSM,
    /// DRY, and penalty trackers in sync — same contract as
    /// post-`sample` token handling.
    pub fn forced_next_token(&mut self) -> Option<LlamaToken> {
        self.constraint.as_mut().and_then(|c| c.forced_next_token())
    }

    /// **Tier 2 jump-forward.** Returns a sequence of tokens covering
    /// the FSM's deterministic byte run via vocab longest-match. Each
    /// returned token has already been `accept`-ed on the constraint
    /// (FSM is advanced), so callers MUST also call `accept(token)` on
    /// the sampler itself per token to keep DRY/penalty trackers in
    /// sync. Returns empty when no constraint is active or no run is
    /// available.
    pub fn forced_next_run(&mut self, max_bytes: usize) -> Vec<LlamaToken> {
        self.constraint
            .as_mut()
            .map(|c| c.forced_next_run(max_bytes))
            .unwrap_or_default()
    }
}

/// Resolved sampling parameters for a single role. Picked from
/// ModelQuirks based on `SamplingMode` + per-request
/// overrides.
struct ResolvedSampling {
    mode: SamplingMode,
    temp: f32,
    top_k: i32,
    top_p: f32,
    presence_pen: f32,
}

fn build_sampler(
    model: &LlamaModel,
    request: &CompletionRequest,
    quirks: &ModelQuirks,
) -> ConstrainedSampler {
    // Schema-driven constraint (pure Rust, bypasses llama-grammar.cpp
    // entirely). Compiled at request time from the OpenAI-style
    // response_format → CompletionRequest.structured_output. Failure
    // to compile is a real schema problem; warn loudly and fall back
    // to unconstrained sampling so the request doesn't hard-fail.
    // llguidance-driven constraint takes precedence when set. The
    // request carries a pre-built Lark grammar string
    // (`request.lark_grammar`); the adapter (`sovereign-mesh::
    // inference_adapter::build_completion_request`) sets this when
    // tools are present and `SOVEREIGN_ALTERNATION_GRAMMAR` is on.
    // Compile failure warns and falls back to free-form sampling so
    // the request still produces output rather than 503'ing.
    let llg_constraint = request.lark_grammar.as_deref().and_then(|lark| {
        match crate::llguidance_constraint::LlguidanceConstraint::new(lark, model) {
            Ok(c) => {
                tracing::info!(
                    grammar_bytes = lark.len(),
                    "grammar-constrained decoding enabled (llguidance, lark)"
                );
                Some(c)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "LlguidanceConstraint compile failed — falling back to free-form sampling"
                );
                None
            }
        }
    });

    // In-house JSON-Schema mask. Skipped when llguidance is already
    // claiming the constraint slot — both engines mask the same logit
    // chain; layering them would deadlock (each one would reject any
    // token the other allowed). When `lark_grammar` is set,
    // `structured_output` is intentionally ignored.
    let constraint = if llg_constraint.is_some() {
        None
    } else {
        request.structured_output.as_ref().and_then(|schema| {
            match crate::json_constraint::JsonConstraint::new(schema, model) {
                Ok(c) => {
                    tracing::info!("grammar-constrained decoding enabled (in-house mask, no llama-grammar.cpp)");
                    Some(c)
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "JsonConstraint compile failed — falling back to free-form sampling"
                    );
                    None
                }
            }
        })
    };

    // URL-allowlist constraint: built when the caller declares a
    // non-empty allowlist on the request. Shares vocab_bytes storage
    // with `JsonConstraint::new` via the per-model cache, so two
    // constraints over the same model don't double-walk the vocab.
    let url_constraint = request.url_allowlist.as_deref().and_then(|urls| {
        let vocab_bytes = crate::json_constraint::vocab_bytes_for(model);
        let constraint = crate::url_constraint::UrlAllowlistConstraint::new(urls, vocab_bytes);
        tracing::info!(
            url_count = urls.len(),
            constructed = constraint.is_some(),
            "url_allowlist constraint constructed"
        );
        constraint
    });

    // Evidence-id allowlist constraint (Tier 2 of tool-framework
    // expansion). Same per-request shape as the URL constraint —
    // built when `CompletionRequest.evidence_id_allowlist` is
    // non-empty. Shares the per-model vocab_bytes cache.
    let evidence_id_constraint =
        request.evidence_id_allowlist.as_deref().and_then(|ids| {
            let vocab_bytes = crate::json_constraint::vocab_bytes_for(model);
            let constraint =
                crate::evidence_id_constraint::EvidenceIdAllowlistConstraint::new(
                    ids, vocab_bytes,
                );
            tracing::info!(
                ev_id_count = ids.len(),
                constructed = constraint.is_some(),
                "evidence_id_allowlist constraint constructed"
            );
            constraint
        });

    // Non-Latin token denylist: opt-in via env var. Built once per
    // model and cached for the daemon's lifetime. Applies on every
    // inference path (chat, completion, structured) so callers don't
    // have to remember to set `x-asciiExtended` on every schema.
    // Default OFF — some corpora (Confucian-philosophy articles,
    // multilingual chat) legitimately need CJK tokens.
    let non_latin_denylist = if non_latin_block_enabled() {
        Some(crate::json_constraint::non_latin_denylist_for(model))
    } else {
        None
    };

    // Sampling parameters — picked from `ModelQuirks`. Three modes:
    //   * **instruct** — enable_thinking=false (any tools state).
    //                    Used by codex CLI traffic, atlas Phase 1,
    //                    structured-output extracts.
    //   * **code**     — enable_thinking=true AND tools present.
    //                    Reasoning-heavy coding work.
    //   * **think**    — enable_thinking=true AND no tools.
    //                    General reasoning (chat, planning, atlas
    //                    discourse work).
    // Each mode reads `<mode>_*` quirks fields; missing values fall
    // back to `default_*` (the think profile). Per-request overrides
    // (`request.temperature`, `request.top_p`, `request.top_k`)
    // still win over the picked profile so an operator can poke any
    // knob without rewriting quirks.
    // Per-role profile selection. Priority order:
    //   1. **Explicit caller override** via `request.sampling_mode`.
    //      Both roles use that mode. Lets non-codex callers (atlas
    //      pipelines, ATOS, eval harnesses) pin a profile without
    //      spoofing `enable_thinking` + tools signals.
    //   2. **Auto-picker** based on the role + request shape:
    //      * Explore — outside JSON string fields, between tool
    //        boundaries. Maps to Instruct when tools are present
    //        (picking what to call); otherwise the no-tools-mode
    //        below.
    //      * Content — inside a tool_call JSON string value (the
    //        `cmd` body of exec_command, where apply_patch lives).
    //        Maps to Code when tools are present (composing code);
    //        otherwise the no-tools-mode below.
    //      No-tools mode falls back to Think unless the request
    //      sets `enable_thinking: false`, in which case Instruct.
    //
    // The model's token-level structure (which role it's currently
    // in) drives the auto-pick, not any call-site heuristic.
    let no_tools_mode = match request.enable_thinking {
        Some(false) => SamplingMode::Instruct,
        _ => SamplingMode::Think,
    };
    let has_tools = request
        .tools
        .as_ref()
        .map(|t| !t.is_empty())
        .unwrap_or(false);
    let (explore_mode, content_mode) = match request.sampling_mode {
        Some(explicit) => (explicit, explicit),
        None if has_tools => (
            SamplingMode::Instruct,
            SamplingMode::Code,
        ),
        None => (no_tools_mode, no_tools_mode),
    };
    let resolve = |mode: SamplingMode| -> ResolvedSampling {
        let (m_temp, m_top_k, m_top_p, m_presence) = match mode {
            SamplingMode::Instruct => (
                quirks.instruct_temperature,
                quirks.instruct_top_k,
                quirks.instruct_top_p,
                quirks.instruct_presence_penalty,
            ),
            SamplingMode::Code => (
                quirks.code_temperature,
                quirks.code_top_k,
                quirks.code_top_p,
                quirks.code_presence_penalty,
            ),
            SamplingMode::Think => (None, None, None, None),
        };
        ResolvedSampling {
            mode,
            temp: request
                .temperature
                .unwrap_or(m_temp.unwrap_or(quirks.default_temperature)),
            top_k: request
                .top_k
                .or(m_top_k)
                .or(quirks.default_top_k)
                .unwrap_or(40) as i32,
            top_p: request
                .top_p
                .unwrap_or(m_top_p.unwrap_or(quirks.default_top_p)),
            presence_pen: m_presence.unwrap_or(quirks.default_presence_penalty),
        }
    };
    let explore = resolve(explore_mode);
    let content = resolve(content_mode);
    tracing::debug!(
        explore_mode = ?explore.mode,
        explore_temp = explore.temp,
        explore_top_p = explore.top_p,
        explore_presence = explore.presence_pen,
        content_mode = ?content.mode,
        content_temp = content.temp,
        content_top_p = content.top_p,
        content_presence = content.presence_pen,
        "sampler-profile per-role selection"
    );
    // Sampler-stage params (rep / freq / min_p) read from quirks.
    // Qwen card recommends 1.0 / 0.0 / 0.0 across all modes;
    // llama-cpp tradition for other families is 1.15 / 0.1 / 0.05
    // (the historical hardcoded values, now preserved via the
    // serde-default compat helpers in `sovereign_core::model_family`).
    let rep_pen: f32 = quirks.default_repetition_penalty;
    let freq_pen: f32 = quirks.default_frequency_penalty;
    let min_p_threshold: f32 = quirks.default_min_p;
    // Sequence breakers tell DRY where one "thought unit" ends and another
    // begins — any of these tokens resets the repeated-suffix detector.
    let breakers: &[&[u8]] = &[b"\n", b".", b"?", b"!", b":", b"\"", b"*"];

    // Content-role temperature: defaults to greedy (0.0) so the
    // apply_patch envelope syntax (markers, delimiters) stays
    // disciplined. Gym 005/008/009 regression 2026-05-13 confirmed
    // T=0.6 inside `cmd` strings drops `***` prefixes ~40% of runs
    // because the model's next-token prob for `***` isn't always
    // the argmax at T=0.6.
    //
    // The OTHER Code-profile knobs (top_p=0.95, presence=0.0,
    // top_k=20) still apply — those affect token-set shape, not
    // greedy-vs-sample. So content gets Code's discipline on every
    // axis except T, where envelope precision wins.
    //
    // `SOVEREIGN_CONTENT_TEMPERATURE` env overrides (e.g. for
    // debugging tokenizer drift on long path strings, where some
    // sampling temperature might help vs. a tokenizer-locked greedy
    // path).
    let content_temp = std::env::var("SOVEREIGN_CONTENT_TEMPERATURE")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|v| (0.0..=2.0).contains(v))
        .unwrap_or(0.0);

    let build_chain = |params: &ResolvedSampling, chain_temp: f32| {
        let mut samplers: Vec<LlamaSampler> = Vec::new();
        // MIGRATION 2026-05-17: llama-cpp-4 0.2.x reshaped DRY:
        //   * Added `n_ctx_train` as the second positional arg — scopes
        //     DRY's repetition-detection window; querying the model
        //     gives us the model's actual training span instead of a
        //     hard-coded guess.
        //   * `dry` became a method on `&LlamaSampler` rather than a
        //     free constructor. Chain off `greedy()` (a no-op identity
        //     sampler at this position in the chain) to get a base we
        //     can call `.dry(...)` against. Semantically equivalent to
        //     the old constructed-fresh form.
        samplers.push(LlamaSampler::greedy().dry(
            model,
            model.n_ctx_train() as i32,
            0.8,
            1.75,
            2,
            -1,
            breakers.iter().copied(),
        ));
        samplers.push(LlamaSampler::penalties(128, rep_pen, freq_pen, params.presence_pen));
        if chain_temp < 0.01 {
            samplers.push(LlamaSampler::greedy());
        } else {
            samplers.push(LlamaSampler::top_k(params.top_k));
            samplers.push(LlamaSampler::min_p(min_p_threshold, 1));
            samplers.push(LlamaSampler::top_p(params.top_p, 1));
            samplers.push(LlamaSampler::temp(chain_temp));
            samplers.push(LlamaSampler::dist(rand_seed()));
        }
        LlamaSampler::chain_simple(samplers)
    };

    ConstrainedSampler {
        inner_explore: build_chain(&explore, explore.temp),
        inner_content: build_chain(&content, content_temp),
        constraint,
        llg_constraint,
        url_constraint,
        evidence_id_constraint,
        non_latin_denylist,
    }
}

/// Read `SOVEREIGN_BLOCK_NON_LATIN` once per `build_sampler` call.
/// Accepts the same truthy spellings as `SOVEREIGN_GRAMMAR_TIMING`
/// (`1` / `true`, case-insensitive) so operators have a consistent
/// toggle convention across grammar features.
fn non_latin_block_enabled() -> bool {
    match std::env::var("SOVEREIGN_BLOCK_NON_LATIN") {
        Ok(v) => {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true")
        }
        Err(_) => false,
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
/// A single tool call extracted from model output. The adapter maps
/// this into `commonwealth_api::openai_types::ToolCall` (with a
/// generated id) before emitting the chat-completion response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedToolCall {
    pub name: String,
    /// Raw JSON string for the tool arguments. Kept as a string so the
    /// client sees exactly what the model produced — if the model emits
    /// partial JSON we don't silently coerce it.
    pub arguments: String,
}

/// Extract Qwen3.5-style tool calls from free-form model output.
///
/// Expected markup:
///
/// ```text
/// <tool_call>{"name": "get_weather", "arguments": {"city": "SF"}}</tool_call>
/// ```
///
/// Multiple blocks per response are supported. Whitespace inside a block
/// is ignored. A block whose body is not valid JSON, or whose JSON lacks
/// a top-level `"name"` field, is **skipped** — not treated as an error
/// — and logged at `warn` so the adapter can tag an `atos_tool_events`
/// row with `phase='parse_error'` and the raw payload. Returning an
/// empty vec on all-malformed output is intentional: a tool-less
/// response is a valid answer.
///
/// Observability: the parser stays pure (no I/O); the caller is
/// responsible for logging. This keeps the function unit-testable
/// without a tracing subscriber.
/// Find the byte length of a balanced JSON object starting at the
/// first `{` in `s`. String-aware (won't trip on `}` inside `"..."`
/// values). Returns None when the input doesn't contain a complete
/// `{...}` (depth never returns to zero) — which is also the
/// "model truncated mid-emission" signal we use to abandon parsing.
///
/// Used by the lenient tool-call extractor when `</tool_call>` is
/// missing. Some quantized models reliably emit `<tool_call>{...}`
/// with a valid JSON object but never close the XML tag; falling
/// back to brace-balancing recovers the call instead of dropping it.
fn find_balanced_json_end(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if esc {
            esc = false;
            continue;
        }
        match b {
            b'\\' if in_str => esc = true,
            b'"' => in_str = !in_str,
            b'{' if !in_str => depth += 1,
            b'}' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Escape unescaped control characters (newline, CR, tab) that appear
/// inside JSON string values. Intentionally narrow: only operates
/// inside `"..."` runs and only on the three control chars known to
/// trip serde — the rest of the body is passed through byte-for-byte.
///
/// Why this exists: some models (Qwen3-Coder-30B observed 2026-05-08)
/// emit a syntactically-correct JSON envelope around their tool call
/// but fail to escape literal `\n`/`\r`/`\t` inside the `content`
/// string value. The result balances braces and parses up to the
/// invalid-control-in-string error, so the daemon was rejecting the
/// whole call. This pre-pass converts those raw bytes to their
/// `\n`/`\r`/`\t` escape forms so `serde_json::from_str` accepts the
/// body. Already-escaped sequences (preceded by an unescaped `\`)
/// pass through untouched.
///
/// Returns the original string if nothing needed escaping (no
/// allocation), otherwise a normalized copy.
pub fn escape_unescaped_control_chars_in_string_values(
    body: &str,
) -> std::borrow::Cow<'_, str> {
    let needs_fix = {
        let mut in_string = false;
        let mut escape = false;
        let mut hit = false;
        for c in body.chars() {
            if in_string {
                if escape {
                    escape = false;
                } else {
                    match c {
                        '"' => in_string = false,
                        '\\' => escape = true,
                        '\n' | '\r' | '\t' => {
                            hit = true;
                            break;
                        }
                        _ => {}
                    }
                }
            } else if c == '"' {
                in_string = true;
            }
        }
        hit
    };
    if !needs_fix {
        return std::borrow::Cow::Borrowed(body);
    }

    let mut out = String::with_capacity(body.len() + 16);
    let mut in_string = false;
    let mut escape = false;
    for c in body.chars() {
        if in_string {
            if escape {
                out.push(c);
                escape = false;
                continue;
            }
            match c {
                '"' => {
                    in_string = false;
                    out.push(c);
                }
                '\\' => {
                    escape = true;
                    out.push(c);
                }
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                _ => out.push(c),
            }
        } else {
            if c == '"' {
                in_string = true;
            }
            out.push(c);
        }
    }
    std::borrow::Cow::Owned(out)
}

pub fn parse_tool_calls_from_text(text: &str) -> Vec<ParsedToolCall> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel_start) = text[cursor..].find("<tool_call>") {
        let start = cursor + rel_start + "<tool_call>".len();
        // Find the matching closer. Use the literal `</tool_call>`
        // marker — we intentionally do NOT allow nested `<tool_call>`
        // blocks (Qwen3.5 never emits them).
        //
        // Lenient mode: some quantized models emit `<tool_call>` then
        // the JSON body but forget the closing `</tool_call>` tag
        // (FINAL-Bench at Q6_K_M shows this consistently). When the
        // closer is missing, fall back to brace-balancing inside the
        // remaining text — if we find a complete JSON object, use
        // its end as the implicit closer and continue scanning.
        let (body, advance) = match text[start..].find("</tool_call>") {
            Some(rel_end) => (
                text[start..start + rel_end].trim().to_string(),
                start + rel_end + "</tool_call>".len(),
            ),
            None => match find_balanced_json_end(&text[start..]) {
                Some(json_len) => {
                    let body = text[start..start + json_len].trim().to_string();
                    (body, start + json_len)
                }
                None => break, // can't recover; stop scanning
            },
        };
        cursor = advance;

        // Parse the body. Accept either:
        //   {"name": "...", "arguments": {...}}
        //   {"name": "...", "arguments": "<string>"}
        // If serde rejects the raw body (commonly: raw newlines inside
        // a string value), retry on a control-char-normalized copy.
        let parsed = serde_json::from_str::<serde_json::Value>(&body)
            .or_else(|_| {
                let fixed =
                    escape_unescaped_control_chars_in_string_values(&body);
                serde_json::from_str::<serde_json::Value>(&fixed)
            })
            .or_else(|_| {
                let stripped = strip_orphan_close_brackets(&body);
                let fixed =
                    escape_unescaped_control_chars_in_string_values(&stripped);
                serde_json::from_str::<serde_json::Value>(&fixed)
            });
        match parsed {
            Ok(obj) => {
                let Some(name) = obj.get("name").and_then(|v| v.as_str()) else {
                    continue; // malformed — skip (caller may log via diagnostic API below)
                };
                let args_str = match obj.get("arguments") {
                    Some(v) if v.is_string() => v.as_str().unwrap_or("").to_string(),
                    Some(v) => v.to_string(),
                    None => "{}".to_string(),
                };
                out.push(ParsedToolCall {
                    name: name.to_string(),
                    arguments: args_str,
                });
            }
            Err(_) => {
                // malformed JSON — skip; caller logs via tracing::warn!
                continue;
            }
        }
    }
    out
}

/// Same as [`parse_tool_calls_from_text`] but returns the raw bodies
/// of any blocks that failed to parse, so the caller can attribute
/// parse-error telemetry (feeds `atos_tool_events.phase='parse_error'`).
pub fn parse_tool_calls_with_errors(text: &str) -> (Vec<ParsedToolCall>, Vec<String>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel_start) = text[cursor..].find("<tool_call>") {
        let start = cursor + rel_start + "<tool_call>".len();
        // Same lenient closer behaviour as `parse_tool_calls_from_text`:
        // accept missing `</tool_call>` when the body parses as a
        // balanced JSON object. Models with damaged closing-tag
        // emission (FINAL-Bench Q6_K_M) emit valid call payloads
        // surrounded by half-broken markup.
        let (body, advance) = match text[start..].find("</tool_call>") {
            Some(rel_end) => (
                text[start..start + rel_end].trim().to_string(),
                start + rel_end + "</tool_call>".len(),
            ),
            None => match find_balanced_json_end(&text[start..]) {
                Some(json_len) => {
                    let body = text[start..start + json_len].trim().to_string();
                    (body, start + json_len)
                }
                None => {
                    errors.push(text[start..].to_string());
                    break;
                }
            },
        };
        cursor = advance;
        let body = body.as_str();

        // Same retry-on-normalized-body pattern as the non-with-errors
        // variant (Qwen3-Coder emits raw \n inside a content string).
        // Plus a third retry that strips orphan `]` chars Qwen3.5-9B
        // observed emitting mid-envelope (2026-05-21): model duplicated
        // a key after a runaway content-string and inserted `}]}` at the
        // tail, breaking serde.
        let parsed = serde_json::from_str::<serde_json::Value>(body)
            .or_else(|_| {
                let fixed =
                    escape_unescaped_control_chars_in_string_values(body);
                serde_json::from_str::<serde_json::Value>(&fixed)
            })
            .or_else(|_| {
                let stripped = strip_orphan_close_brackets(body);
                let fixed =
                    escape_unescaped_control_chars_in_string_values(&stripped);
                serde_json::from_str::<serde_json::Value>(&fixed)
            });
        match parsed {
            Ok(obj) => {
                let Some(name) = obj.get("name").and_then(|v| v.as_str()) else {
                    errors.push(body.to_string());
                    continue;
                };
                let args_str = match obj.get("arguments") {
                    Some(v) if v.is_string() => v.as_str().unwrap_or("").to_string(),
                    Some(v) => v.to_string(),
                    None => "{}".to_string(),
                };
                out.push(ParsedToolCall {
                    name: name.to_string(),
                    arguments: args_str,
                });
            }
            Err(_) => errors.push(body.to_string()),
        }
    }
    (out, errors)
}

/// Walk a JSON candidate and drop any `]` that doesn't match an open
/// `[`. Used as a last-ditch repair when a tool-call body has been
/// damaged by mid-stream prose drift (model wrote `}]}` where it
/// meant `}}`). Mirror-image `[` are NOT dropped — that would create
/// new orphan `]` later in the stream and the repair has to stay
/// idempotent under retry.
///
/// String contents pass through verbatim (we don't want to touch
/// `]` inside JSON strings). Escape sequences within strings are
/// honoured so a `\"` doesn't prematurely close the string.
pub fn strip_orphan_close_brackets(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut bracket_depth: i32 = 0;
    let mut in_str = false;
    let mut esc = false;
    for c in s.chars() {
        if esc {
            out.push(c);
            esc = false;
            continue;
        }
        if in_str {
            if c == '\\' {
                esc = true;
                out.push(c);
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            out.push(c);
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '[' => {
                bracket_depth += 1;
                out.push(c);
            }
            ']' => {
                if bracket_depth > 0 {
                    bracket_depth -= 1;
                    out.push(c);
                }
                // else: orphan close, skip
            }
            _ => out.push(c),
        }
    }
    out
}

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
mod primary_siblings_env_tests {
    //! Pin `SOVEREIGN_PRIMARY_SIBLINGS` parsing — env access is split
    //! out so this test is pure (no process-env mutation that would
    //! race with other tests).
    use super::parse_primary_siblings;

    #[test]
    fn absent_returns_none() {
        assert!(parse_primary_siblings(None).is_none());
    }

    #[test]
    fn empty_returns_none() {
        assert!(parse_primary_siblings(Some("")).is_none());
    }

    #[test]
    fn zero_and_one_are_treated_as_disabled() {
        // 0 and 1 both mean "no parallel siblings" — caller stays on
        // the single-context lazy path. Anything else would be a
        // confusing footgun for operators who type "1".
        assert!(parse_primary_siblings(Some("0")).is_none());
        assert!(parse_primary_siblings(Some("1")).is_none());
    }

    #[test]
    fn n_two_and_above_returns_count() {
        let two = parse_primary_siblings(Some("2")).expect("N=2 should parse");
        assert_eq!(two.get(), 2);
        let four = parse_primary_siblings(Some("4")).expect("N=4 should parse");
        assert_eq!(four.get(), 4);
    }

    #[test]
    fn whitespace_is_trimmed() {
        let n = parse_primary_siblings(Some("  3  ")).expect("N=3 with padding");
        assert_eq!(n.get(), 3);
    }

    #[test]
    fn garbage_returns_none() {
        assert!(parse_primary_siblings(Some("two")).is_none());
        assert!(parse_primary_siblings(Some("-1")).is_none());
        assert!(parse_primary_siblings(Some("3.5")).is_none());
    }
}

#[cfg(test)]
mod parse_tool_calls_tests {
    //! Lock the parser's behaviour against the two real-world model
    //! emission failure modes:
    //!   1. **Closed tags** — happy path, both `<tool_call>` and
    //!      `</tool_call>` present (Qwen3.5 baseline).
    //!   2. **Missing closing tag** — quantized models (FINAL-Bench
    //!      Q6_K_M observed) emit `<tool_call>{...JSON...}` and
    //!      stop without `</tool_call>`. The lenient brace-balancer
    //!      recovers the call instead of dropping it.
    use super::{parse_tool_calls_from_text, parse_tool_calls_with_errors};

    #[test]
    fn closed_tag_extracts_one_call() {
        let text = r#"prelude <tool_call>{"name":"write","arguments":{"path":"a.rs"}}</tool_call> tail"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "write");
        assert!(calls[0].arguments.contains("a.rs"));
    }

    #[test]
    fn missing_closing_tag_with_balanced_json_recovers() {
        // FINAL-Bench Q6_K_M reliably truncates the closing tag.
        // We accept the body when the JSON balances cleanly.
        let text = r#"<tool_call>{"name":"write","arguments":{"filePath":"Cargo.toml","content":"[package]\nname = \"x\"\n"}}"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1, "lenient mode should recover the call");
        assert_eq!(calls[0].name, "write");
        assert!(calls[0].arguments.contains("Cargo.toml"));
    }

    #[test]
    fn missing_closing_tag_with_truncated_json_returns_no_calls() {
        // Body never balances — model truncated mid-string. Drop it
        // rather than emit a corrupt call. `_with_errors` should also
        // surface the leftover so telemetry catches it.
        let text = r#"<tool_call>{"name":"write","arguments":{"content":"unterminated string"#;
        let calls = parse_tool_calls_from_text(text);
        assert!(calls.is_empty());
        let (out, errors) = parse_tool_calls_with_errors(text);
        assert!(out.is_empty());
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn brace_inside_string_does_not_close_early() {
        // A string value containing `}` shouldn't close the JSON.
        let text = r#"<tool_call>{"name":"write","arguments":{"content":"loop { x += 1; }"}}"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn multiple_calls_extract_in_order_even_with_mixed_closers() {
        let text = r#"<tool_call>{"name":"a","arguments":{}}</tool_call>some text<tool_call>{"name":"b","arguments":{"x":1}}"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "a");
        assert_eq!(calls[1].name, "b");
    }

    #[test]
    fn orphan_close_bracket_inside_envelope_recovers_via_repair() {
        // Qwen3.5-9B-HighIQ observed 2026-05-21 (run r): model
        // emitted a runaway `content` string ending with `","path":"src/lib.rs"}]}`.
        // The trailing `]` is orphan — no matching `[` open — and
        // serde rejects the body. The repair pass strips the orphan
        // close bracket so the call survives.
        let body = r#"{"name":"write","arguments":{"path":"src/lib.rs","content":"// short","path":"src/lib.rs"}]}"#;
        let text = format!("<tool_call>{}</tool_call>", body);
        let calls = parse_tool_calls_from_text(&text);
        assert_eq!(
            calls.len(),
            1,
            "orphan-bracket repair should rescue the call"
        );
        assert_eq!(calls[0].name, "write");
        assert!(calls[0].arguments.contains("src/lib.rs"));

        let (out, errors) = parse_tool_calls_with_errors(&text);
        assert_eq!(out.len(), 1);
        assert!(errors.is_empty());
    }

    #[test]
    fn strip_orphan_close_brackets_idempotent_on_valid_input() {
        let s = r#"{"a":1,"b":[2,3],"c":"x]y"}"#;
        assert_eq!(super::strip_orphan_close_brackets(s), s);
    }

    #[test]
    fn strip_orphan_close_brackets_drops_lone_close() {
        let s = r#"{"a":1}]}"#;
        // Only the `]` is orphan; the outer braces balance. The
        // surrounding `}` after `]` is also at depth 0 here, but the
        // repair only touches brackets.
        let repaired = super::strip_orphan_close_brackets(s);
        assert!(!repaired.contains(']'));
        assert!(repaired.starts_with('{'));
    }

    #[test]
    fn strip_orphan_close_brackets_leaves_bracketed_inside_strings_alone() {
        // `]` inside a JSON string should NOT be stripped — it isn't
        // structural. Repair only touches characters at depth 0 of
        // string nesting.
        let s = r#"{"a":"x]y","b":"]"}"#;
        assert_eq!(super::strip_orphan_close_brackets(s), s);
    }

    #[test]
    fn raw_newlines_inside_string_value_recover_via_normalization() {
        // Qwen3-Coder-30B observed 2026-05-08: balanced JSON envelope
        // but raw `\n` (0x0A) bytes inside the `content` string value
        // instead of the `\\n` escape. Without normalization this
        // fails serde with "control character in string". The pre-pass
        // converts the raw bytes to escapes and re-parses.
        let body = "{\"name\":\"write\",\"arguments\":{\"path\":\"a.rs\",\"content\":\"fn main() {\nprintln!(\\\"hi\\\");\n}\"}}";
        let text = format!("<tool_call>{}</tool_call>", body);
        let calls = parse_tool_calls_from_text(&text);
        assert_eq!(calls.len(), 1, "normalization should recover the call");
        assert_eq!(calls[0].name, "write");
        assert!(calls[0].arguments.contains("println"));
    }

    #[test]
    fn already_escaped_sequences_are_preserved_through_normalization() {
        use super::escape_unescaped_control_chars_in_string_values;
        // `\\n` (literal backslash + n) must stay `\\n` after the
        // normalize pass — it's an already-correct escape sequence.
        let input = r#"{"x":"a\nb"}"#;
        let out = escape_unescaped_control_chars_in_string_values(input);
        assert_eq!(out.as_ref(), input, "no raw control chars present");
    }

    #[test]
    fn normalization_skips_control_chars_outside_strings() {
        use super::escape_unescaped_control_chars_in_string_values;
        // A newline between fields (outside any string value) must
        // be left alone. JSON allows it as whitespace; serde already
        // accepts it.
        let input = "{\"x\":\"a\",\n\"y\":\"b\"}";
        let out = escape_unescaped_control_chars_in_string_values(input);
        assert_eq!(out.as_ref(), input);
    }
}

#[cfg(test)]
mod pick_slot_tests {
    //! PR-E2: the slot dispatch rules ultimately drive the hot-swap
    //! decision in `complete` / `complete_stream`. Lock them here
    //! with fast, GPU-free table tests so a regression in routing
    //! is caught by `cargo test` long before it reaches a live
    //! llama.cpp load.
    use super::{pick_slot, SlotTarget, FAST_SHORT_MAX_INPUT_CHARS};
    use sovereign_core::oicp::{CapabilityHint, InferenceRequirements};
    use sovereign_core::types::{CompletionRequest, Speed};

    fn req(speed: Speed, hint: Option<CapabilityHint>) -> CompletionRequest {
        let mut r = CompletionRequest::new("hi");
        r.preferred_speed = speed;
        if let Some(h) = hint {
            r.oicp = Some(InferenceRequirements::new().with_hint(h));
        }
        r
    }

    #[test]
    fn fast_speed_without_code_hint_goes_to_fast() {
        let r = req(Speed::Fast, None);
        assert_eq!(pick_slot(&r, true, true, false, None), SlotTarget::Fast);
        assert_eq!(pick_slot(&r, false, false, false, None), SlotTarget::Fast);
    }

    #[test]
    fn fast_short_routing_picks_fast_short_for_small_max_tokens() {
        // Phase 1b/3/5/6 composers attach max_output_tokens ≤512.
        // When the FastShort companion is built, those calls route to
        // the continuous-batched path — that's where the 2.1× wall-
        // clock speedup lives.
        let mut r = req(Speed::Fast, None);
        r.max_tokens = Some(512);
        assert_eq!(
            pick_slot(&r, true, false, true, None),
            SlotTarget::FastShort
        );
        // Smaller budgets pass too.
        r.max_tokens = Some(256);
        assert_eq!(
            pick_slot(&r, true, false, true, None),
            SlotTarget::FastShort
        );
    }

    #[test]
    fn fast_short_not_selected_when_companion_absent() {
        // SOVEREIGN_FAST_SHORT_DISABLE=1 (or load failure) leaves
        // has_fast_short=false. Every short call falls through to
        // the original Fast slot — no regression vs pre-rework.
        let mut r = req(Speed::Fast, None);
        r.max_tokens = Some(256);
        assert_eq!(
            pick_slot(&r, true, false, false, None),
            SlotTarget::Fast
        );
    }

    #[test]
    fn fast_short_skipped_for_large_output_budget() {
        // Phase 1 chapter ingestion asks for the full output budget
        // (24576 via PHASE1_SEED_OUTPUT_BUDGET). It must route to Fast
        // (which keeps `n_seq_max=1` for the full per-call window),
        // not FastShort — whose 2048-per-seq slice would mid-decode
        // overflow on a long chapter.
        let mut r = req(Speed::Fast, None);
        r.max_tokens = Some(8192);
        assert_eq!(
            pick_slot(&r, true, false, true, None),
            SlotTarget::Fast
        );
    }

    #[test]
    fn fast_short_skipped_when_prompt_exceeds_per_seq_budget() {
        // 2026-05-19 desktop trace: a Fast+max_tokens=16 call from
        // (likely) the title/scope classifier carried a 9272-char
        // conversation-context prompt; pick_slot routed it to
        // FastShort whose per-seq KV slice is only 2048 tokens, and
        // decode failed with `NoKvCacheSlot (batch_n_tokens=2328,
        // n_requests=1)`. The fix gates FastShort on prompt size in
        // addition to output budget.
        let mut r = req(Speed::Fast, None);
        r.max_tokens = Some(16);
        r.prompt = "x".repeat(FAST_SHORT_MAX_INPUT_CHARS + 1);
        assert_eq!(
            pick_slot(&r, true, false, true, None),
            SlotTarget::Fast,
            "oversized prompt must fall through to Fast even when max_tokens is tiny"
        );
        // Right at the boundary — still allowed.
        r.prompt = "x".repeat(FAST_SHORT_MAX_INPUT_CHARS);
        assert_eq!(
            pick_slot(&r, true, false, true, None),
            SlotTarget::FastShort
        );
        // system_message counts too — a long system + tiny user
        // prompt should also fall through.
        r.prompt = "x".repeat(100);
        r.system_message = Some("y".repeat(FAST_SHORT_MAX_INPUT_CHARS));
        assert_eq!(
            pick_slot(&r, true, false, true, None),
            SlotTarget::Fast,
            "system+prompt combined must respect the FastShort input budget"
        );
    }

    #[test]
    fn fast_short_skipped_when_speed_is_not_fast() {
        // Slow-speed callers (chat synthesis on the primary) ignore
        // FastShort entirely even if their max_output is small.
        // FastShort is for the Fast-slot model only.
        let mut r = req(Speed::Slow, None);
        r.max_tokens = Some(256);
        assert_eq!(
            pick_slot(&r, true, false, true, None),
            SlotTarget::Primary
        );
    }

    #[test]
    fn slow_speed_without_code_hint_picks_primary_when_available() {
        let r = req(Speed::Slow, None);
        assert_eq!(pick_slot(&r, true, false, false, None), SlotTarget::Primary);
        assert_eq!(pick_slot(&r, true, true, false, None), SlotTarget::Primary);
    }

    #[test]
    fn slow_speed_without_primary_falls_back_to_fast() {
        // Degraded config: user only configured a fast GGUF. Pre-E2
        // behaviour that must not regress — a Medium/Slow request
        // still runs instead of erroring out.
        let r = req(Speed::Slow, None);
        assert_eq!(pick_slot(&r, false, false, false, None), SlotTarget::Fast);
    }

    #[test]
    fn code_hint_with_code_slot_picks_code_even_on_fast_speed() {
        // Code specialist wins over Speed::Fast dispatch. The hint
        // semantics are "this work needs code reasoning"; Fast-slot
        // generals can't do that well.
        let r = req(Speed::Fast, Some(CapabilityHint::code()));
        assert_eq!(pick_slot(&r, true, true, false, None), SlotTarget::Code);
    }

    #[test]
    fn code_hint_without_code_slot_follows_speed_rules() {
        // Solo-user with no dedicated coder: fall back to whatever
        // speed-tier would normally serve this request. The peer-
        // mesh scheduler is responsible for routing the hint to a
        // better-matched peer; locally we do what we can.
        let r_fast = req(Speed::Fast, Some(CapabilityHint::code()));
        assert_eq!(pick_slot(&r_fast, true, false, false, None), SlotTarget::Fast);

        let r_slow = req(Speed::Slow, Some(CapabilityHint::code()));
        assert_eq!(pick_slot(&r_slow, true, false, false, None), SlotTarget::Primary);
    }

    #[test]
    fn non_code_hint_does_not_pick_code_even_when_configured() {
        // The extension hint `x:prose` must not accidentally trip
        // the code dispatch — only the standardized `code` hint
        // should.
        let hint = CapabilityHint::extension("prose").unwrap();
        let r = req(Speed::Slow, Some(hint));
        assert_eq!(pick_slot(&r, true, true, false, None), SlotTarget::Primary);
    }

    #[test]
    fn extras_match_wins_over_speed_routing() {
        // Operator-declared per-phase routing must override the
        // Speed-based heuristic. A Fast-speed request with
        // model_id matching an extras slot lands on the extras
        // slot, NOT on the fast slot.
        let r = req(Speed::Fast, None);
        assert_eq!(
            pick_slot(&r, true, false, false, Some("reasoning".into())),
            SlotTarget::Extra("reasoning".into())
        );
    }

    #[test]
    fn extras_match_wins_over_code_hint() {
        // Even a code-hinted request defers to an explicit extras
        // routing — the operator's declared model recruitment is
        // higher-precedence than any heuristic.
        let r = req(Speed::Fast, Some(CapabilityHint::code()));
        assert_eq!(
            pick_slot(&r, true, true, false, Some("bulk".into())),
            SlotTarget::Extra("bulk".into())
        );
    }

    #[test]
    fn no_extras_match_falls_through_to_speed_routing() {
        // Untagged request OR tagged request whose model_id missed
        // the extras lookup → existing Speed/code rules apply
        // unchanged. Locks the back-compat invariant.
        let r = req(Speed::Slow, None);
        assert_eq!(pick_slot(&r, true, false, false, None), SlotTarget::Primary);
    }
}

#[cfg(test)]
mod eviction_tests {
    //! Exercises the LRU eviction selection algorithm. Pure and
    //! lock-free — runs without loading any real llama.cpp model.
    use super::{pick_evictions, EvictionCandidate, EvictionPlan};

    fn cand(name: &str, last_used_ms: u64, size_mb: u64) -> EvictionCandidate {
        EvictionCandidate {
            slot_name: name.into(),
            last_used_ms,
            size_bytes: size_mb * 1024 * 1024,
        }
    }

    #[test]
    fn fits_returns_no_eviction_needed() {
        // current 5 GB + new 3 GB = 8 GB ≤ 12 GB budget → Fits.
        let c = vec![cand("warm", 1000, 5 * 1024)];
        let plan = pick_evictions(&c, 5 * 1024 * 1024 * 1024, 3 * 1024 * 1024 * 1024, 12 * 1024 * 1024 * 1024);
        assert_eq!(plan, EvictionPlan::Fits);
    }

    #[test]
    fn evicts_coldest_slot_first() {
        // Two candidates: "old" used at t=100, "fresh" used at t=999.
        // Need to free 1 MB. Algorithm picks the colder one ("old")
        // even though "fresh" alone would also free enough — the
        // ordering is the contract.
        let c = vec![
            cand("fresh", 999, 5),
            cand("old", 100, 5),
        ];
        // current 10 MB + new 4 MB = 14, budget 12 → need to free 2 MB
        let plan = pick_evictions(&c, 10 * 1024 * 1024, 4 * 1024 * 1024, 12 * 1024 * 1024);
        match plan {
            EvictionPlan::Evict(names) => {
                assert_eq!(names, vec!["old".to_string()]);
            }
            other => panic!("expected Evict, got {other:?}"),
        }
    }

    #[test]
    fn evicts_multiple_when_one_isnt_enough() {
        // Three cold slots, all 1 MB. Need to free 3 MB. Walks LRU
        // order ("a" → "b" → "c"). Stops once enough freed.
        let c = vec![
            cand("c", 300, 1),
            cand("b", 200, 1),
            cand("a", 100, 1),
        ];
        // current 3 MB + new 5 MB = 8, budget 5 → need 3 MB
        let plan = pick_evictions(&c, 3 * 1024 * 1024, 5 * 1024 * 1024, 5 * 1024 * 1024);
        match plan {
            EvictionPlan::Evict(names) => {
                assert_eq!(names, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
            }
            other => panic!("expected Evict, got {other:?}"),
        }
    }

    #[test]
    fn stops_evicting_once_enough_freed() {
        // Multiple candidates available, but only the coldest's
        // capacity is needed. Stops early to preserve the warmer
        // slots.
        let c = vec![
            cand("a", 100, 10), // coldest, 10 MB — alone enough
            cand("b", 200, 10),
            cand("c", 300, 10),
        ];
        // current 30 MB + new 5 MB = 35, budget 30 → need 5 MB
        let plan = pick_evictions(&c, 30 * 1024 * 1024, 5 * 1024 * 1024, 30 * 1024 * 1024);
        match plan {
            EvictionPlan::Evict(names) => {
                assert_eq!(names, vec!["a".to_string()]);
            }
            other => panic!("expected single eviction, got {other:?}"),
        }
    }

    #[test]
    fn insufficient_when_cold_capacity_too_small() {
        // Only 1 MB of cold inventory, but need to free 5 MB.
        // Algorithm reports the shortfall so the caller can
        // surface an error.
        let c = vec![cand("a", 100, 1)];
        // current 1 MB + new 10 MB = 11, budget 5 → need 6 MB but
        // cold total is only 1 MB.
        let plan = pick_evictions(&c, 1024 * 1024, 10 * 1024 * 1024, 5 * 1024 * 1024);
        match plan {
            EvictionPlan::Insufficient {
                need_to_free,
                cold_total,
            } => {
                assert_eq!(need_to_free, 6 * 1024 * 1024);
                assert_eq!(cold_total, 1024 * 1024);
            }
            other => panic!("expected Insufficient, got {other:?}"),
        }
    }

    #[test]
    fn empty_candidates_with_overflow_returns_insufficient() {
        // Edge case: budget would be exceeded but no cold slots
        // available (everything is busy in-flight). Caller can't
        // load.
        let c = vec![];
        let plan = pick_evictions(&c, 10 * 1024 * 1024, 5 * 1024 * 1024, 8 * 1024 * 1024);
        match plan {
            EvictionPlan::Insufficient {
                need_to_free,
                cold_total,
            } => {
                assert_eq!(need_to_free, 7 * 1024 * 1024);
                assert_eq!(cold_total, 0);
            }
            other => panic!("expected Insufficient on empty cold, got {other:?}"),
        }
    }

    #[test]
    fn ties_break_deterministically_by_input_order() {
        // Two candidates with the same last_used. The algorithm
        // sorts by last_used; for equal keys, sort_by_key is
        // stable, so input order wins. Lock the contract — a
        // future change to unstable sort would surface here.
        let c = vec![
            cand("first", 100, 1),
            cand("second", 100, 1),
        ];
        // current 2 MB + new 1 MB = 3, budget 1 → need 2 MB.
        let plan = pick_evictions(&c, 2 * 1024 * 1024, 1024 * 1024, 1024 * 1024);
        match plan {
            EvictionPlan::Evict(names) => {
                assert_eq!(names, vec!["first".to_string(), "second".to_string()]);
            }
            other => panic!("expected Evict on tie, got {other:?}"),
        }
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

#[cfg(test)]
mod tool_call_parser_tests {
    use super::{parse_tool_calls_from_text, parse_tool_calls_with_errors};

    #[test]
    fn happy_path_single_call() {
        let text = r#"Sure, I'll check the weather.
<tool_call>{"name": "get_weather", "arguments": {"city": "SF"}}</tool_call>"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
        // Object arguments serialize with no spaces — lock the shape.
        assert_eq!(calls[0].arguments, r#"{"city":"SF"}"#);
    }

    #[test]
    fn multiple_calls_in_one_response() {
        let text = r#"<tool_call>{"name":"a","arguments":{"x":1}}</tool_call>
then <tool_call>{"name":"b","arguments":{"y":"hi"}}</tool_call>"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "a");
        assert_eq!(calls[1].name, "b");
    }

    #[test]
    fn string_arguments_preserved_as_is() {
        // Some models (including older Qwen variants) stringify the
        // arguments object. We round-trip that form verbatim.
        let text = r#"<tool_call>{"name":"f","arguments":"{\"x\":1}"}</tool_call>"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, r#"{"x":1}"#);
    }

    #[test]
    fn missing_arguments_defaults_to_empty_object() {
        let text = r#"<tool_call>{"name":"noargs"}</tool_call>"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "noargs");
        assert_eq!(calls[0].arguments, "{}");
    }

    #[test]
    fn malformed_json_is_skipped_silently() {
        let text = r#"<tool_call>{"name": "truncated", "arguments": {</tool_call>
and <tool_call>{"name":"ok","arguments":{}}</tool_call>"#;
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "ok");
    }

    #[test]
    fn unterminated_tag_stops_scanning() {
        // No closing </tool_call> → parser stops rather than munching
        // arbitrary text as JSON. Anything before the open tag is
        // already ignored.
        let text = r#"preamble <tool_call>{"name":"never_closed""#;
        assert!(parse_tool_calls_from_text(text).is_empty());
    }

    #[test]
    fn no_tool_call_tags_returns_empty() {
        let text = "plain reply, no tools here";
        assert!(parse_tool_calls_from_text(text).is_empty());
    }

    #[test]
    fn error_variant_reports_malformed_bodies() {
        let text = r#"<tool_call>{ not json }</tool_call>
<tool_call>{"name":"ok","arguments":{}}</tool_call>
<tool_call>{"missing_name":true}</tool_call>"#;
        let (calls, errors) = parse_tool_calls_with_errors(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "ok");
        assert_eq!(errors.len(), 2);
        assert!(errors[0].contains("not json"));
        assert!(errors[1].contains("missing_name"));
    }

    #[test]
    fn whitespace_inside_block_tolerated() {
        let text = "<tool_call>\n  {\"name\": \"spaced\", \"arguments\": {}}  \n</tool_call>";
        let calls = parse_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "spaced");
    }
}
