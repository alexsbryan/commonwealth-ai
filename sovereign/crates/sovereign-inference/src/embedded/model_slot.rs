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

use super::prefix_state::{PrefixPlan, PrefixStateCache};
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

/// Total on-disk size of the model, summing ALL shards when `model_path` is one
/// shard of a split GGUF (`…-00001-of-00003.gguf`). llama.cpp is pointed at
/// shard `00001` but loads every shard, so the PLACEMENT decision must see the
/// whole model's size — not the (often tiny, header-only) first shard. Getting
/// this wrong is a real deadlock: an 86 GB model whose `00001` shard is ~10 MB
/// reads as "safe to bulk-stream" (< `SOVEREIGN_RPC_SAFE_STREAM_MB`), so a
/// distributed load takes the StreamSplit path and wedges in ggml's RPC
/// `set_tensor` send() instead of the warm-cache `OwnedOverrides` path that
/// avoids the bulk transfer entirely. Falls back to the single-file size for a
/// non-sharded model or if any sibling shard is missing (never guess).
pub(crate) fn total_model_bytes(model_path: &Path) -> u64 {
    let single = std::fs::metadata(model_path)
        .map(|m| m.len())
        .unwrap_or(u64::MAX);
    let Some(name) = model_path.file_name().and_then(|n| n.to_str()) else {
        return single;
    };
    let Some(dir) = model_path.parent() else {
        return single;
    };
    // Parse `<stem>-<idx>-of-<count>.gguf`.
    let parsed = name.rfind("-of-").and_then(|of| {
        let count: u32 = name.get(of + 4..)?.strip_suffix(".gguf")?.parse().ok()?;
        let before = name.get(..of)?; // "<stem>-<idx>"
        let dash = before.rfind('-')?;
        let idx = before.get(dash + 1..)?;
        idx.parse::<u32>().ok()?; // validate numeric
        Some((before.get(..dash)?.to_string(), count, idx.len()))
    });
    let Some((stem, count, width)) = parsed else {
        return single;
    };
    if count <= 1 {
        return single;
    }
    let mut total = 0u64;
    for i in 1..=count {
        let shard = dir.join(format!("{stem}-{i:0width$}-of-{count:0width$}.gguf"));
        match std::fs::metadata(&shard) {
            Ok(m) => total += m.len(),
            Err(_) => return single, // a shard missing → don't guess
        }
    }
    total
}

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
pub(crate) enum SlotInferenceMode {
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
pub(crate) struct MtpRebuildParams {
    pub(crate) backend: Arc<LlamaBackend>,
    pub(crate) context_size: u32,
    pub(crate) n_threads: i32,
    pub(crate) n_batch: u32,
    pub(crate) n_ubatch: u32,
    /// Set iff the gguf is genuinely recurrent. The `with_n_rs_seq`
    /// call has to stay on the target ctx after demotion — recurrent
    /// architectures need it regardless of whether speculative
    /// decoding is active — or `seq_rm` on the recurrent layers fails
    /// at decode time.
    pub(crate) n_rs_seq: Option<u32>,
    /// Was the original target built on GPU? Preserved so a rebuild
    /// after demotion stays on the same backend; mismatching here
    /// would force a CPU↔GPU transition mid-life and almost certainly
    /// confuse the operator (silent perf cliff).
    pub(crate) used_gpu: bool,
}

pub(crate) struct SlotContext {
    /// **Drop order is load-bearing — do not reorder these two fields.**
    /// The `LlamaContext`s inside `mode` borrow `_model` as a fake
    /// `&'static LlamaModel` (minted via `Arc::as_ptr`); `LlamaContext::drop`
    /// calls `llama_free`, which dereferences that model. Rust drops struct
    /// fields top-to-bottom, so `mode` MUST be declared before `_model` or the
    /// contexts would free against a dangling model pointer (use-after-free).
    /// The laundered `'static` + the `unsafe impl Send/Sync` below mean the
    /// borrow checker cannot catch a reorder — this comment is the only guard.
    pub(crate) mode: SlotInferenceMode,
    pub(crate) _model: Arc<LlamaModel>,
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
    pub(crate) cached_tokens: Vec<crate::llama::cpp::token::LlamaToken>,
    /// Architecture string from the gguf's `general.architecture`
    /// metadata key, read once at slot construction. Drives the
    /// `prefix_cache_safe` gate in `generate_sync` — recurrent-layer
    /// architectures cannot use the partial-keep prefix cache because
    /// `clear_kv_cache_seq` doesn't rewind their recurrent state.
    /// Empty string when metadata read failed (defensive — treat as
    /// "unknown" and fall back to the name-pattern heuristic).
    pub(crate) arch: String,
    /// Params used to rebuild the target ctx on demote. `Some` iff
    /// the slot was originally constructed as an MTP candidate (the
    /// only case where demote is possible); `None` on plain slots
    /// built via `from_existing_model` (FastShort) — those start in
    /// SingleToken and never transition.
    pub(crate) mtp_rebuild: Option<MtpRebuildParams>,
    /// **Pinned-prefix full-state cache** (`prefix_state.rs`) — prefix
    /// reuse for the recurrent/hybrid architectures where the
    /// partial-keep path above is vetoed, via whole-context
    /// save/restore (proven arch-safe by `state_cartridge_spike.rs`).
    /// Boot-scoped; survives demote (state files embed model identity,
    /// not context identity — `llama_load_session_file` validates).
    pub(crate) prefix_state: PrefixStateCache,
}

unsafe impl Send for SlotContext {}
unsafe impl Sync for SlotContext {}

impl SlotContext {
    /// Mutable handle to the target context — the one every decode
    /// (single-token or MTP-verify) runs against. The single
    /// accessor replaces direct `slot_ctx.ctx` field access that the
    /// pre-2026-05-20 code used.
    pub(crate) fn ctx_mut(&mut self) -> &mut crate::llama::cpp::context::LlamaContext<'static> {
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
        let rebuilt = MtpSession::new(&*target_ctx, &*draft_ctx, 1, *n_draft_max)
            .map_err(|e| Error::Inference(format!("MTP session rebuild failed: {e:?}")))?;
        super::ffi_trace::record(super::ffi_trace::FfiCall::MtpSessionBuilt);
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
        let rebuild = self.mtp_rebuild.as_ref().ok_or_else(|| {
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
        let new_target =
            build_target_ctx_for_slot(&self._model, rebuild, model_id).map_err(|e| {
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

/// Build a target `LlamaContext<'static>` for a SingleToken slot
/// given the rebuild params captured at construction. Used by:
///   - the MTP-probe-failure fallback at initial slot load
///     (`ModelSlot::load`'s `Err((bad_target, ...))` arm), AND
///   - mid-life `demote_to_single_token` after an MTP runtime fault.
///
/// Both call sites target SingleToken mode — the speculative draft
/// path is the only consumer of MTP-specific context state. As such
/// this function **deliberately strips `n_rs_seq`** before building.
///
/// **Diagnosed 2026-05-23.** Pre-fix: `MtpRebuildParams.n_rs_seq` was
/// applied here unconditionally for MTP-candidate models, and the
/// initial target ctx (line 762+ in `ModelSlot::load`) was likewise
/// built with `with_n_rs_seq(mtp_n_rs_seq)` from `build_ctx_params`.
/// That config provisions recurrent-state sequence slots that ONLY
/// the MTP draft-process drives. When the speculative upgrade probe
/// failed on a candidate slot — observed on the Qwen3.6-35B-A3B
/// Compact slot whose draft ctx build returned "null reference from
/// llama.cpp" — the rebuild propagated `n_rs_seq=4` into a SingleToken
/// ctx that would never feed those slots. Every subsequent
/// `ctx.decode(batch)` on the rebuilt target returned
/// `Decode Error -3: unknown`, regardless of prompt size (the user
/// saw it on Recipe Author with prompts as small as 338 tokens, KV
/// cache empty, on every retry). Stripping `n_rs_seq` here gives the
/// fallback a vanilla single-token ctx that decodes normally.
///
/// SAFETY: the returned context borrows from `model` via a transmute
/// to `'static`. The caller MUST keep `model` alive (in practice via
/// the `_model: Arc<LlamaModel>` field on `SlotContext`) for the
/// lifetime of the returned context.
fn build_target_ctx_for_slot(
    model: &Arc<LlamaModel>,
    params: &MtpRebuildParams,
    model_id: &str,
) -> Result<crate::llama::cpp::context::LlamaContext<'static>> {
    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(params.context_size))
        .with_n_batch(params.n_batch)
        .with_n_ubatch(params.n_ubatch)
        .with_n_threads(params.n_threads)
        .with_n_threads_batch(params.n_threads)
        .with_offload_kqv(params.used_gpu);
    // n_rs_seq intentionally NOT applied — see fn doc above.
    // params.n_rs_seq retained on the struct so callers can observe
    // whether the slot was originally an MTP candidate, but it is
    // never propagated into a fallback SingleToken ctx.
    let _ = params.n_rs_seq;
    let mut ctx = unsafe {
        let model_ref: &'static LlamaModel = &*(Arc::as_ptr(model));
        model_ref
            .new_context(&params.backend, ctx_params)
            .map_err(|e| Error::Inference(format!("Failed to create target context: {e}")))?
    };
    super::control_vector::maybe_apply(&mut ctx, model_id, model.n_embd(), model.n_layer());
    Ok(ctx)
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
    target_ctx.clear_kv_cache(); // kv-phase: Probe
    draft_ctx.clear_kv_cache(); // kv-phase: Probe
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
    target_ctx.clear_kv_cache(); // kv-phase: Probe
    draft_ctx.clear_kv_cache(); // kv-phase: Probe
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
    (crate::llama::cpp::context::LlamaContext<'static>, Error),
> {
    let mut target_ctx = target;
    let draft_ctx_res = unsafe {
        let model_ref: &'static LlamaModel = &*(Arc::as_ptr(model));
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
        Ok(s) => {
            super::ffi_trace::record(super::ffi_trace::FfiCall::MtpSessionBuilt);
            s
        }
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

pub(crate) struct ModelSlot {
    pub(crate) model: Arc<LlamaModel>,
    pub(crate) context: Mutex<SlotContext>,
    pub(crate) model_id: String,
    /// In-memory footprint of the loaded model, in bytes — captured
    /// at load time. Sourced from `LlamaModel::size()` (the
    /// backend's own accounting of the buffers it allocated) with a
    /// gguf file-size fallback when `size()` reports zero. Used by
    /// the LRU eviction policy to decide which slots to drop when a
    /// new load would exceed the operator's `max_extras_memory_gb`
    /// budget.
    pub(crate) size_bytes: u64,
    /// Last time this slot was used to serve a request, expressed
    /// as milliseconds since UNIX epoch. Updated by `complete()` /
    /// `complete_stream()` after dispatch. The eviction policy
    /// orders slots by ascending `last_used` and drops the coldest
    /// first. `AtomicU64` so updates don't need a write lock on
    /// `ExtrasState`.
    pub(crate) last_used: std::sync::atomic::AtomicU64,
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
    pub(crate) inflight: Arc<tokio::sync::Semaphore>,
}

/// Current millis-since-epoch as `u64`. Saturates at 0 if the
/// system clock is set before 1970 (impossible in practice;
/// guarded so we never panic).
/// Outcome of one `generate_sync` call. Replaces the prior 3-tuple
/// `(text, prompt_tokens, completion_tokens)` return so callers can
/// see WHY generation stopped without re-deriving it from a counts
/// comparison. The single-token loop tracks `finish_reason` from its
/// actual exit branches (EOS → Stop, budget exhausted → Length); MTP
/// + batched paths still synthesise it from `finish_reason_from_counts`
/// until their own loops are threaded too.
#[derive(Debug, Clone)]
pub(crate) struct GenerationOutcome {
    pub text: String,
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub finish_reason: FinishReason,
}

/// Instrumentation for the spontaneous mid-word EOS class (persona-QA
/// 2026-07-10: answers shipping "compression-t" / mid-`[Source:` tails —
/// every observed cut was finish=stop, none finish=length). Logs a WARN
/// receipt when a generation STOPS on a mid-word tail well under its token
/// budget, labeled by loop, so soak runs can attribute frequency and path
/// (mtp vs plain) BEFORE any guard is designed. Log-only; no behavior
/// change. Some noise is acceptable — answers legitimately ending on a bare
/// word without punctuation will fire; the tail in the log adjudicates.
pub(crate) fn log_midword_stop(
    loop_label: &str,
    model_id: &str,
    outcome: &GenerationOutcome,
    request: &CompletionRequest,
) {
    if !matches!(outcome.finish_reason, FinishReason::Stop) {
        return;
    }
    let text = outcome.text.trim_end();
    if text.chars().count() < 80 {
        return; // short answers end tersely all the time; not the class
    }
    let Some(last) = text.chars().last() else {
        return;
    };
    if !(last.is_alphabetic() || last == '-') {
        return;
    }
    // Near-budget stops are the LENGTH class wearing a stop mask; the
    // spontaneous-EOS class fires with plenty of budget left.
    if let Some(max) = request.max_tokens {
        if outcome.completion_tokens as u32 >= (max as u32).saturating_mul(9) / 10 {
            return;
        }
    }
    let tail: String = text
        .chars()
        .rev()
        .take(48)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    tracing::warn!(
        target: "midword_stop",
        loop_label,
        model_id = %model_id,
        completion_tokens = outcome.completion_tokens,
        tail = %tail,
        "generation stopped on a mid-word tail under budget"
    );
}

// ── Forced-choice logprob elicitation (the reasoning-fidelity K-killer) ──
//
// The reasoning-fidelity harness needs a calibrated P(choice) over a
// small candidate set. Repeated sampling (K=32–200 draws) is how it gets
// one today because no logprobs are surfaced. These two helpers replace
// that with ONE forward pass: the harness carries a sentinel inside
// `structured_output` — `{"type":"string","enum":[...],
// "x_forced_choice":true}` — and `generate_sync`, right after the prompt
// decode (logits at the final position are live), reads the next-token
// distribution over the candidate labels and returns it as JSON in
// `text`, skipping the generation loop entirely. `structured_output` is
// only ever *compiled* in `build_sampler` (which the branch bypasses), so
// the sentinel rides the existing HTTP plumbing untouched.

/// Detect the forced-choice sentinel and return its candidate labels.
/// Thin delegate to [`CompletionRequest::forced_choice_candidates`] — the
/// detector was hoisted into sovereign-contracts so the mesh scheduler
/// (which cannot see engine internals) and this local slot picker share
/// one predicate. `None` for ordinary requests (every path unaffected).
pub(crate) fn forced_choice_candidates(request: &CompletionRequest) -> Option<Vec<String>> {
    request.forced_choice_candidates()
}

/// Map a caller-declared stable-prefix byte length (over the RAW user
/// prompt, `CompletionRequest.stable_prefix_len`) to a conservative
/// token boundary in the rendered prompt's token stream, for the
/// pinned-prefix cache's directed plan. `None` (→ sighting-based plan)
/// when the declaration is absent, malformed, or unlocatable.
///
/// Method: locate the declared prefix substring inside the rendered
/// prompt (chat templates concatenate message content verbatim),
/// tokenize the rendered text UP TO that boundary, and take its LCP
/// with the full token stream, backing off 2 tokens. The back-off
/// matters: BPE merges at the cut can differ from the full stream's
/// (the boundary token may fuse with suffix bytes), and LCP+back-off
/// makes the pin a guaranteed common token prefix of every sibling
/// sharing the declared bytes. Cost: one extra tokenize of the prefix
/// per request. Failures degrade to the undirected plan — full
/// prefill at worst, never wrong output.
fn directed_pin_tokens(
    model: &LlamaModel,
    request: &CompletionRequest,
    full_prompt: &str,
    tokens: &[crate::llama::cpp::token::LlamaToken],
) -> Option<usize> {
    let n = request.stable_prefix_len?;
    // `.get` enforces both the range and the char-boundary contract.
    let raw_prefix = request.prompt.get(..n)?;
    if raw_prefix.is_empty() {
        return None;
    }
    let start = full_prompt.find(raw_prefix)?;
    let rendered_prefix = &full_prompt[..start + raw_prefix.len()];
    let prefix_tokens = model.str_to_token(rendered_prefix, AddBos::Always).ok()?;
    let lcp = prefix_tokens
        .iter()
        .zip(tokens.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let pin = lcp.saturating_sub(2);
    (pin > 0).then_some(pin)
}

/// Read the model's next-token distribution over `candidates` in one
/// forward pass (prompt already decoded). For each candidate we sum the
/// mass over its single-token encodings — bare and space-prefixed, to
/// absorb BPE's leading-space split — then softmax over the union and
/// re-sum per candidate. Returns `(label, probability)` summing to ~1.0.
pub(crate) fn forced_choice_probs(
    model: &LlamaModel,
    ctx: &crate::llama::cpp::context::LlamaContext<'_>,
    candidates: &[String],
) -> Result<Vec<(String, f32)>> {
    // candidate-leading token id -> candidate index.
    let mut tok_to_cand: std::collections::HashMap<i32, usize> = std::collections::HashMap::new();
    for (ci, c) in candidates.iter().enumerate() {
        for variant in [c.clone(), format!(" {c}")] {
            if let Ok(toks) = model.str_to_token(&variant, AddBos::Never) {
                if toks.len() == 1 {
                    tok_to_cand.entry(toks[0].0).or_insert(ci);
                }
            }
        }
    }
    if tok_to_cand.is_empty() {
        return Err(Error::Inference(
            "forced_choice: no candidate encodes to a single token".into(),
        ));
    }
    // One O(vocab) pass to gather the candidate tokens' raw logits at the
    // final (next-token) position.
    let data = ctx.token_data_array();
    let mut gathered: Vec<(usize, f32)> = Vec::with_capacity(tok_to_cand.len());
    for entry in data.data.iter() {
        if let Some(&ci) = tok_to_cand.get(&entry.id().0) {
            gathered.push((ci, entry.logit()));
        }
    }
    if gathered.is_empty() {
        return Err(Error::Inference(
            "forced_choice: candidate tokens absent from the vocab logits".into(),
        ));
    }
    // Numerically-stable softmax over just the candidate tokens.
    let m = gathered
        .iter()
        .map(|(_, l)| *l)
        .fold(f32::NEG_INFINITY, f32::max);
    let mut denom = 0.0f32;
    let exps: Vec<(usize, f32)> = gathered
        .iter()
        .map(|(ci, l)| {
            let e = (l - m).exp();
            denom += e;
            (*ci, e)
        })
        .collect();
    let mut out = vec![0.0f32; candidates.len()];
    for (ci, e) in exps {
        out[ci] += e / denom;
    }
    Ok(candidates.iter().cloned().zip(out).collect())
}

/// Synthesise `FinishReason` from observed token counts. Real source
/// of truth is the decode loop's exit branch (EOS vs `n_generated >=
/// max_tokens`); this helper is the fallback for variants that haven't
/// been migrated to return the typed reason directly (MTP +
/// generate_sync_batched as of 2026-05-25). The only false positive
/// is the vanishing edge where EOS lands at exactly the budget
/// boundary. Single-token path uses its real exit-branch signal
/// instead — see `GenerationOutcome` construction at the end of the
/// single-token loop in `generate_sync`.
pub(crate) fn finish_reason_from_counts(
    request: &CompletionRequest,
    completion_tokens: usize,
) -> FinishReason {
    match request.max_tokens {
        Some(budget) if completion_tokens >= budget => FinishReason::Length,
        _ => FinishReason::Stop,
    }
}

pub(crate) use sovereign_core::time::unix_millis as now_millis;

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

// RPC distribution / sharding / worker-serving moved to
// `rpc_distribution.rs` (2026-06-10 decomposition).

impl ModelSlot {
    pub(crate) fn load(
        backend: &Arc<LlamaBackend>,
        model_path: &Path,
        context_size: u32,
        n_gpu_layers: u32,
        distributable: bool,
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

        // Distributed inference + never-wedge guard. Streaming a large model's
        // weight share to a remote RPC worker wedges the host in send() (the
        // load-time RPC set_tensor deadlock, >~800MB share). We never stream a
        // large shard: instead a worker holds its shard's bytes in its RPC cache,
        // so the host sends only a hash per tensor — a cache hit, zero bulk send.
        // `resolve_placement` decides the strategy (and, for the auto-warm path,
        // seeds the workers' caches first); we then just apply it. The default in
        // every uncertain case is LocalOnly — the load NEVER wedges.
        // Total across ALL shards — not the first-shard file size — so a split
        // GGUF is classified by its real weight, not its ~10 MB header shard.
        let model_bytes = total_model_bytes(model_path);
        let placement = resolve_placement(model_path, model_bytes, distributable);

        let mut model_params = LlamaModelParams::default().with_n_gpu_layers(effective_gpu_layers);
        match &placement {
            LoadPlacement::LocalOnly => {
                // Load on the local GPU only, excluding any RPC device a prior
                // (smaller) load left in ggml's global registry.
                let local = local_gpu_device_list();
                if !local.is_empty() {
                    model_params = model_params.with_devices(&local);
                }
            }
            LoadPlacement::StreamSplit => {
                // Small model, safe to stream: prune any dead worker and apply the
                // optional memory-proportional split (the proven path).
                if let Some(devices) = live_device_list_if_pruning_needed() {
                    model_params = model_params.with_devices(&devices);
                }
                if let Some(split) = rpc_tensor_split() {
                    tracing::info!(
                        ?split,
                        "applying SOVEREIGN_RPC_TENSOR_SPLIT (device order: RPC workers first, then local GPU)"
                    );
                    model_params = model_params.with_tensor_split(&split);
                }
            }
            LoadPlacement::OwnedOverrides(dist) => {
                // Workers hold their shards warm: OWN the placement with explicit
                // per-block `-ot` overrides so every weight is a SET_TENSOR_HASH
                // cache hit — no bulk send, no deadlock.
                tracing::info!(
                    devices = dist.devs.len(),
                    overrides = dist.overrides.len(),
                    workers = dist.assignments.len(),
                    "distributed load: explicit tensor placement via -ot overrides \
                     (warm shards — host skips the bulk weight send)"
                );
                model_params = model_params
                    .with_devices(&dist.devs)
                    .with_tensor_buft_overrides(&dist.overrides);
            }
            LoadPlacement::InsufficientCluster { eligible, quorum } => {
                // Shared-model host can't form the cluster yet — do NOT load (a
                // too-big primary loaded locally would OOM the host). Stay
                // unavailable; the discovery loop's reload-on-worker-change retries
                // as anchors join, and the model reports "forming" meanwhile.
                return Err(Error::Inference(format!(
                    "shared-model cluster forming: {eligible} eligible anchor(s), need {quorum} \
                     (or pooled memory short) — not loading; retries as anchors join"
                )));
            }
        }

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
        let wants_gpu =
            !force_cpu && cfg!(any(target_os = "macos", target_os = "linux")) && n_gpu_layers > 0;
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
        // NEGATIVE RESULT (2026-07-10, mechanism removed per the
        // complexity-must-move-a-metric rule): a with_flash_attention(true)
        // experiment measured ZERO envelope/throughput effect here (paired
        // probe identical on/off; engagement unverifiable — the llama log
        // callback suppresses context-init lines). See git history
        // (3af2070d) and the RSS-probe sovereign note before re-trying FA
        // on this backend.
        let build_ctx_params = move |gpu: bool| {
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
                // n_batch must cover the full context window so the whole prompt is
                // decodable. llama pads n_ctx up to a 256-multiple, so n_batch is
                // rounded the same way (ctx_n_batch) — leaving it at the un-padded
                // context_size lets a prompt in the padding gap trip the
                // GGML_ASSERT(n_tokens_all <= n_batch) daemon-abort. llama.cpp still
                // splits the batch into n_ubatch=512 micro-batches for kernel calls,
                // so memory pressure is unchanged — only the Rust-side assertion
                // limit moves.
                .with_n_batch(ctx_n_batch(context_size))
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

        let (mut ctx, used_gpu) = match if wants_gpu {
            unsafe {
                let model_ref: &'static LlamaModel = &*(Arc::as_ptr(&model));
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
                    let model_ref: &'static LlamaModel = &*(Arc::as_ptr(&model));
                    model_ref
                        .new_context(backend, build_ctx_params(false))
                        .map_err(|e| Error::Inference(format!("Failed to create context: {e}")))?
                };
                (ctx, false)
            }
        };

        let compute_backend = if used_gpu {
            gpu_backend_label()
        } else {
            embed_compute_backend_label()
        };

        // Operator-gated control-vector steering (SOVEREIGN_CVEC*). No-op
        // unless configured. Applied here so both the SingleToken path and
        // the speculative target (the ctx moves into the upgrade below and
        // keeps its adapter state) are steered; the MTP-failure rebuild
        // reapplies inside `build_target_ctx_for_slot`.
        super::control_vector::maybe_apply(&mut ctx, &model_id, model.n_embd(), model.n_layer());

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
            // Cover the padded n_ctx — see ctx_n_batch (a prompt in the
            // n_ctx padding gap otherwise SIGABRTs the daemon in decode).
            n_batch: ctx_n_batch(context_size),
            n_ubatch: 512,
            n_rs_seq: if is_mtp_model {
                Some(mtp_n_rs_seq)
            } else {
                None
            },
            used_gpu,
        };
        let mode: SlotInferenceMode = if is_mtp_model {
            let draft_params = LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(context_size))
                .with_ctx_type(LlamaContextType::Mtp)
                .with_n_rs_seq(mtp_n_rs_seq)
                .with_n_batch(ctx_n_batch(context_size))
                .with_n_ubatch(512)
                .with_n_threads(n_threads as i32)
                .with_n_threads_batch(n_threads as i32)
                .with_offload_kqv(used_gpu);
            match try_upgrade_to_speculative(ctx, &model, backend, draft_params, n_draft_max) {
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
                    let fresh = build_target_ctx_for_slot(&model, &rebuild_params, &model_id)
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

        // Capture the post-allocation effective context window so
        // operators can see when llama.cpp silently caps the requested
        // ctx_size at the gguf's `n_ctx_train`. Repro 2026-05-25: user
        // set `[models].context_size = 16000` in ~/.sovereign/config.toml
        // but the primary slot reported "context window of 8192" at
        // decode time. If `requested_n_ctx > effective_n_ctx` here, the
        // gguf's trained context is the bottleneck — use a RoPE-scaled
        // rebuild or pick a model with larger n_ctx_train.
        let effective_n_ctx = match &mode {
            SlotInferenceMode::SingleToken { ctx } => ctx.n_ctx(),
            SlotInferenceMode::Speculative { target_ctx, .. } => target_ctx.n_ctx(),
        };
        let n_ctx_train = model.n_ctx_train();
        if context_size != effective_n_ctx {
            tracing::warn!(
                model_id = %model_id,
                requested_n_ctx = context_size,
                effective_n_ctx,
                n_ctx_train,
                "ModelSlot::load — llama.cpp adjusted context window from configured value. \
                 If `effective_n_ctx == n_ctx_train`, the gguf's trained context is the cap; \
                 rebuild with RoPE scaling or pick a model with larger n_ctx_train."
            );
        }
        tracing::info!(
            model_id = %model_id,
            params = model.n_params(),
            layers = model.n_layer(),
            size_mb = model.size() / (1024 * 1024),
            n_threads,
            compute_backend,
            mtp = matches!(&mode, SlotInferenceMode::Speculative { .. }),
            requested_n_ctx = context_size,
            effective_n_ctx,
            n_ctx_train,
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
        let file_size = std::fs::metadata(model_path).map(|m| m.len()).unwrap_or(0);
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
                prefix_state: PrefixStateCache::new(&model_id),
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
    pub(crate) fn from_existing_model(
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
                // Cover the padded n_ctx — see ctx_n_batch.
                .with_n_batch(ctx_n_batch(context_size))
                .with_n_ubatch(n_ubatch)
                .with_n_threads(n_threads as i32)
                .with_n_threads_batch(n_threads as i32)
                .with_offload_kqv(gpu)
            // MIGRATION 2026-05-17: .with_op_offload(...) retired in llama-cpp-4 0.2.x — see crate::llama
        };

        let (ctx, used_gpu) = match if wants_gpu {
            unsafe {
                let model_ref: &'static LlamaModel = &*(Arc::as_ptr(&model));
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
                    let model_ref: &'static LlamaModel = &*(Arc::as_ptr(&model));
                    model_ref
                        .new_context(backend, build_ctx_params(false))
                        .map_err(|e| {
                            Error::Inference(format!("FastShort context creation failed: {e}"))
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
                prefix_state: PrefixStateCache::new(&model_id),
            }),
            model_id,
            size_bytes,
            last_used: std::sync::atomic::AtomicU64::new(now_millis()),
            inflight: Arc::new(tokio::sync::Semaphore::new(1)),
        })
    }

    pub(crate) fn generate_sync(
        model: &LlamaModel,
        model_id: &str,
        slot_ctx: &mut SlotContext,
        request: &CompletionRequest,
        quirks: &ModelQuirks,
    ) -> Result<GenerationOutcome> {
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
        // Predicate extracted to `gates::mtp_dispatch_eligible` so the
        // tools-never-enter-MTP invariant is pinned by tests.
        if mtp_dispatch_eligible(request, slot_ctx.is_speculative(), |k| {
            std::env::var(k).ok()
        }) {
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
                Ok((text, prompt_tokens, completion_tokens)) => {
                    // MTP loop hasn't been threaded for typed FinishReason
                    // yet — synthesise via the counts helper. Migrating
                    // MTP would require touching the draft+verify loop's
                    // exit branches; deferred until the verify-side
                    // acceptance-rate work settles.
                    let finish_reason = finish_reason_from_counts(request, completion_tokens);
                    let outcome = GenerationOutcome {
                        text,
                        prompt_tokens,
                        completion_tokens,
                        finish_reason,
                    };
                    log_midword_stop("mtp", model_id, &outcome, request);
                    return Ok(outcome);
                }
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
        // 0. (authoritative) libllama's own model flags. Hybrid
        //    models — interleaved linear-attention blocks, e.g. the
        //    DENSE Qwen3.5 lineup whose gguf arch is plain `qwen35`
        //    (no "moe", so the string ladder below misses it) — break
        //    partial-keep exactly like pure recurrent ones. Found
        //    2026-06-09 by the desktop real-mode e2e: ask_document on
        //    Qwen3.5-2B failed `Decode Error -1: n_tokens == 0` on
        //    every lcp>0 prefill (batch_n_tokens=444 — the batch was
        //    never empty; the -1 is the recurrent-state rejection).
        // Decision matrix extracted to `gates::prefix_cache_gate` so
        // the ladder above is pinned by weight-free tests.
        let gate = prefix_cache_gate(
            model.is_recurrent(),
            model.is_hybrid(),
            &slot_ctx.arch,
            quirks.has_recurrent_layers,
            slot_ctx.is_speculative(),
            |k| std::env::var(k).ok(),
        );
        let prefix_cache_safe = gate.safe;
        if gate.forced {
            tracing::warn!(
                model = %model_id,
                arch = %slot_ctx.arch,
                "prefix_cache: SOVEREIGN_PREFIX_CACHE_FORCE overrode the \
                 recurrent/hybrid veto — DIAGNOSTIC ONLY; expect decode \
                 errors on partial keep unless upstream fixed the hazard"
            );
        }
        tracing::debug!(
            model = %model_id,
            arch = %slot_ctx.arch,
            model_says_recurrent = gate.model_says_recurrent,
            arch_says_recurrent = gate.arch_says_recurrent,
            quirks_say_recurrent = gate.quirks_say_recurrent,
            mtp_session = gate.speculative_active,
            forced = gate.forced,
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
            prefix_state,
            ..
        } = slot_ctx;
        let ctx = match mode {
            SlotInferenceMode::SingleToken { ctx } => ctx,
            SlotInferenceMode::Speculative { target_ctx, .. } => target_ctx,
        };
        let full_prompt = format_prompt(model, model_id, request, quirks)?;

        let tokens = model
            .str_to_token(&full_prompt, add_bos_for(request))
            .map_err(|e| Error::Inference(format!("Tokenization failed: {e}")))?;

        let n_batch = ctx.n_batch() as usize;
        let n_ctx = ctx.n_ctx() as usize;
        // Admit against min(n_ctx, n_batch): a prompt longer than n_batch
        // cannot be decoded (GGML_ASSERT n_tokens_all <= n_batch aborts the
        // daemon). ctx_n_batch keeps n_batch >= n_ctx so this normally
        // resolves to n_ctx; the min is a belt-and-suspenders guard that
        // demotes an over-long prompt to a graceful "prompt too long" error
        // instead of a SIGABRT if that invariant ever regresses.
        let admit_ctx = n_ctx.min(n_batch);
        let max_tokens = clamp_max_tokens(request.max_tokens, tokens.len(), admit_ctx)?;

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
        // LCP arithmetic lives in `gates::compute_lcp` (tested
        // weight-free): the reserve-last-token rule — identical
        // prompts re-prefill exactly 1 token so the sampler gets a
        // fresh logit distribution — and the capability gate (see
        // prefix_cache_safe rationale at top of generate_sync;
        // hybrid recurrent models force full prefill).
        let PrefixLcp {
            raw: raw_lcp,
            effective: mut lcp,
        } = compute_lcp(cached_tokens, &tokens, prefix_cache_safe);
        // Clear cached_tokens before any cache mutation — restored on
        // success path below. If we crash between here and the
        // post-decode update, next call sees cached_tokens=[] and
        // does a defensive full clear.
        cached_tokens.clear();

        // **Pinned-prefix full-state cache** (prefix_state.rs): the
        // prefix-reuse path that works where partial keep cannot —
        // whole-context restore is recurrent-state-faithful (spike
        // 2026-07-12). Restore/Learn override the LCP machinery for
        // this request (full clear either way); Pass leaves the
        // pre-existing behavior byte-identical.
        let plan = match directed_pin_tokens(model, request, &full_prompt, &tokens) {
            Some(pin) => prefix_state.plan_directed(&tokens, pin),
            None => prefix_state.plan(&tokens),
        };
        let mut state_prefix_ready = false;
        match plan {
            PrefixPlan::Restore { key, prefix_len } => {
                ctx.clear_kv_cache(); // kv-phase: PrefixStateRestore
                let path = prefix_state.entry_path(key);
                let t0 = Instant::now();
                let loaded = path
                    .as_ref()
                    .and_then(|p| ctx.load_session_file(p, ctx.n_ctx() as usize).ok());
                match loaded {
                    Some(restored) if restored.len() == prefix_len => {
                        lcp = prefix_len;
                        state_prefix_ready = true;
                        tracing::info!(
                            target: "prefix_state",
                            model = %model_id,
                            key = format_args!("{key:016x}"),
                            restored_tokens = prefix_len,
                            suffix_tokens = tokens.len() - prefix_len,
                            restore_ms = t0.elapsed().as_millis() as u64,
                            "prefix_state: HIT — restored pinned prefix (single-token path)"
                        );
                    }
                    _ => {
                        tracing::warn!(
                            target: "prefix_state",
                            model = %model_id,
                            key = format_args!("{key:016x}"),
                            "prefix_state: restore failed — invalidating pin, full prefill"
                        );
                        prefix_state.invalidate(key);
                        lcp = 0;
                        state_prefix_ready = true; // cache already cleared
                    }
                }
            }
            PrefixPlan::Learn { key, pin_len } => {
                ctx.clear_kv_cache(); // kv-phase: PrefixStateLearn
                                      // Stage-1: prefill the pin prefix with NO outputs (a
                                      // logits-bearing save balloons the state file by
                                      // n_outputs × n_vocab), save the state, then let the
                                      // ordinary tail code below prefill the rest from
                                      // position pin_len.
                let mut stage1 = LlamaBatch::new(n_batch, 1);
                let mut stage1_ok = true;
                for (i, &tok) in tokens[..pin_len].iter().enumerate() {
                    if stage1.add(tok, i as i32, &[0], false).is_err() {
                        stage1_ok = false;
                        break;
                    }
                }
                if stage1_ok && ctx.decode(&mut stage1).is_ok() {
                    let t0 = Instant::now();
                    let path = prefix_state.state_path(key);
                    let saved = prefix_state.ensure_dir().is_ok()
                        && ctx.save_session_file(&path, &tokens[..pin_len]).is_ok();
                    if saved {
                        prefix_state.commit(key, tokens[..pin_len].to_vec(), path);
                        tracing::info!(
                            target: "prefix_state",
                            model = %model_id,
                            key = format_args!("{key:016x}"),
                            pinned_tokens = pin_len,
                            save_ms = t0.elapsed().as_millis() as u64,
                            "prefix_state: LEARNED — pinned stable prefix (single-token path)"
                        );
                    } else {
                        tracing::warn!(
                            target: "prefix_state",
                            model = %model_id,
                            "prefix_state: save failed — continuing unpinned"
                        );
                    }
                    lcp = pin_len;
                    state_prefix_ready = true;
                } else {
                    tracing::warn!(
                        target: "prefix_state",
                        model = %model_id,
                        "prefix_state: stage-1 prefill failed — full clear + full prefill"
                    );
                    ctx.clear_kv_cache(); // kv-phase: PrefixStateLearn
                    lcp = 0;
                    state_prefix_ready = true;
                }
            }
            PrefixPlan::Pass => {}
        }

        if !state_prefix_ready {
            if lcp == 0 {
                ctx.clear_kv_cache(); // kv-phase: PrefixCacheSetup
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
                    ctx.clear_kv_cache(); // kv-phase: PrefixCacheSetup
                }
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

        // Forced-choice logprob elicitation (reasoning-fidelity K-killer):
        // the prompt is decoded, so the next-token logits are live. Read
        // the distribution over the candidate labels in ONE pass and
        // return it as JSON in `text` — no generation loop, no sampler.
        if let Some(cands) = forced_choice_candidates(request) {
            let probs = forced_choice_probs(model, ctx, &cands)?;
            let map: std::collections::BTreeMap<String, f32> = probs.into_iter().collect();
            let text = serde_json::to_string(&map)
                .map_err(|e| Error::Inference(format!("forced_choice serialize: {e}")))?;
            return Ok(GenerationOutcome {
                text,
                prompt_tokens: tokens.len(),
                completion_tokens: 0,
                finish_reason: FinishReason::Stop,
            });
        }

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
        let deadline = Instant::now() + std::time::Duration::from_secs(deadline_secs);
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
        // Shape-B (balanced JSON envelope) stop — state machine and
        // BOTH gates (grammar-locked, think-suspend) live in
        // `gates::ToolStopTracker`; see its doc for the LaTeX
        // `x_{r,c}` false-positive P0 (2026-05-21) the grammar-locked
        // gate guards against. The marker stop (`</tool_call>`,
        // shape A) stays unconditional below.
        let mut tool_stop =
            ToolStopTracker::new(tools_present, request.structured_output.is_some());
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
        let jump_fwd_t2_enabled = jump_fwd_t2_enabled_from_env(|k| std::env::var(k).ok());
        const MAX_JUMP_FWD_RUN: usize = 32;
        const MAX_FORCED_BYTES: usize = 64;
        let mut jump_fwd_n: usize = 0;
        let mut jump_fwd_runs: usize = 0;
        let mut jump_fwd_max: usize = 0;
        let mut jump_fwd_bytes_n: usize = 0;

        // Real finish_reason tracking — replaces the chars-per-token
        // heuristic the runtime used to do post-hoc. Default to Length
        // (the `while n_generated < max_tokens` predicate is the exit
        // condition when the loop body doesn't break early). EOS path
        // overwrites with Stop. Set this AFTER each potential exit
        // branch — leaving it Length on a clean budget exhaustion is
        // the correct answer.
        let mut exit_reason: FinishReason = FinishReason::Length;

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
                ctx.clear_kv_cache(); // kv-phase: ErrorAbort
                return Err(Error::Inference(format!(
                    "inference deadline exceeded after {elapsed}s ({n_generated} tokens generated; \
                     deadline={deadline_secs}s) — likely pathological JSON-Schema mask state. \
                     Set SOVEREIGN_INFERENCE_TIMEOUT_SECS to override."
                )));
            }

            // Constraint-engine failure abort: once the llguidance
            // matcher errors (latched in `record_failure`), every mask
            // fails closed — no token is grammar-legal — so this loop
            // would emit clamp-garbage until the deadline trips (the
            // "pathological JSON-Schema mask state" above). Fail fast
            // and loudly instead; the root cause is already on the
            // `llguidance.health` error event.
            if let Some(cause) = sampler.constraint_failure() {
                ctx.clear_kv_cache(); // kv-phase: ErrorAbort
                return Err(Error::Inference(format!(
                    "llguidance constraint failed after {n_generated} tokens: {cause}"
                )));
            }

            let role = if tools_present && tool_stop.in_json_string() {
                SamplerRole::Content
            } else {
                SamplerRole::Explore
            };
            match role {
                SamplerRole::Content => role_content_n += 1,
                SamplerRole::Explore => role_explore_n += 1,
            }
            if tool_stop.in_json_string() {
                tokens_in_string_n += 1;
            }
            let token = sampler.sample(ctx, -1, role);
            sampler.accept(token);

            if model.is_eog_token(token) {
                exit_reason = FinishReason::Stop;
                break;
            }

            if let Ok(piece) = model.token_to_piece(token, &mut decoder, true, None) {
                if role_trace_on && tools_present {
                    tracing::info!(
                        role = ?role,
                        in_string = tool_stop.in_json_string(),
                        depth = tool_stop.depth(),
                        piece = %piece.replace('\n', "\\n"),
                        "sampler_trace token"
                    );
                }
                // Update sliding tail for tag detection.
                push_sliding_tail(&mut tail, &piece);
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
                // matches any `{...}` in prose. See `ToolStopTracker`.
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
                    // llguidance grammar-stop: when the schema-driven
                    // constraint reports `is_stopped() == true`, the
                    // JSON envelope has closed. Without this break, the
                    // chat template's post-tool tail (`</think>`,
                    // `</tool_call>`) leaks into the response content
                    // and the model can keep generating free-form prose
                    // until token cap. Distinct from the brace-balance
                    // stop below (`ToolStopTracker`, gated on the
                    // legacy JsonConstraint case) — both close the same
                    // class but llguidance is the authoritative signal
                    // when it's the active constraint.
                    if sampler.grammar_is_stopped() {
                        tracing::info!(
                            model = %model_id,
                            n_generated,
                            "inference: stopping on llguidance grammar accept"
                        );
                        n_generated += 1;
                        break;
                    }
                    if tool_stop.observe(&piece, in_think) {
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
                                in_string = tool_stop.in_json_string(),
                                depth = tool_stop.depth(),
                                piece = %piece.replace('\n', "\\n"),
                                "sampler_trace token (jump_fwd)"
                            );
                        }
                        // Tail buffer + structural tag tracking. We're
                        // outside <think> by construction here, but
                        // keep the tail in sync so a subsequent
                        // sampled token's tag detection sees the right
                        // context.
                        push_sliding_tail(&mut tail, &piece);
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
                        // grammar-lock-gated inside `ToolStopTracker`;
                        // marker stop stays unconditional.
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
                            // Outside <think> by construction here
                            // (the walk is gated on `!in_think`).
                            if tool_stop.observe(&piece, false) {
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

            if let Err(e) = ctx.decode(&mut batch) {
                // Mid-generation decode failure (Decode Error -3 etc).
                // Observed 2026-05-25 on the primary slot AFTER a prior
                // request blew the context window: cached_tokens was
                // already cleared at prompt-prefill time, but the
                // llama.cpp-level KV / SSM state on the recurrent
                // (DeltaNet) layers can carry residual data that
                // `clear_kv_cache_seq` alone doesn't rewind.
                //
                // Diagnostics on the way out so the next debug pass has
                // generation progress at error time + the same context
                // surface the prefix-cache warn above logs. Then force
                // a FULL `clear_kv_cache()` so the next request lands
                // on a clean slot regardless of what the prefix-cache
                // gate would otherwise decide.
                tracing::warn!(
                    error = %e,
                    n_generated,
                    prompt_tokens = tokens.len(),
                    forced_run_len = forced_run.len(),
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    "inference: mid-generation decode failed — clearing slot KV to protect next request"
                );
                ctx.clear_kv_cache(); // kv-phase: ErrorAbort
                return Err(Error::Inference(format!("Decode failed: {e}")));
            }

            if forced_hit_break {
                break;
            }

            // Budget forcing: inject </think> if the think block runs too long.
            if in_think
                && think_tokens >= request.think_budget.unwrap_or(THINK_BUDGET)
                && !think_budget_fired
            {
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
                        if let Err(e) = ctx.decode(&mut batch) {
                            // Same defensive cleanup as the main loop
                            // decode above — KV may carry residual
                            // state on hybrid recurrent layers.
                            tracing::warn!(
                                error = %e,
                                n_generated,
                                phase = "think_budget_close",
                                "inference: think-close decode failed — clearing slot KV"
                            );
                            ctx.clear_kv_cache(); // kv-phase: ErrorAbort
                            return Err(Error::Inference(format!("Decode failed: {e}")));
                        }
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
                tc_json_ever_opened = tool_stop.ever_opened(),
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
        // debug!, not info!: mirrors the MTP `end-of-generation` line above
        // (kept at the same level so one log analyzer sees both paths
        // identically). Fired once per generation; summarised at INFO by
        // `inference.complete: done`. `RUST_LOG=sovereign_inference=debug`.
        tracing::debug!(
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
            finish_reason = exit_reason.as_openai_str(),
            "inference: end-of-generation"
        );

        let outcome = GenerationOutcome {
            text: output,
            prompt_tokens: tokens.len(),
            completion_tokens: n_generated,
            finish_reason: exit_reason,
        };
        log_midword_stop("plain", model_id, &outcome, request);
        Ok(outcome)
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
            prefix_state,
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

        // The draft side always starts fresh (its state is rebuilt per
        // request via `rebuild_session_in_place` above); cached_tokens
        // is the non-MTP path's fingerprint and is cleared so a return
        // to that path doesn't reuse a stale one.
        draft_ctx.clear_kv_cache(); // kv-phase: RequestStartReset
        cached_tokens.clear();

        let full_prompt = format_prompt(model, model_id, request, quirks)?;
        let tokens = model
            .str_to_token(&full_prompt, add_bos_for(request))
            .map_err(|e| Error::Inference(format!("Tokenization failed: {e}")))?;
        if tokens.is_empty() {
            return Err(Error::Inference("MTP: empty prompt".into()));
        }

        // **Pinned-prefix full-state cache on the MTP path.** The
        // partial-keep prefix cache is permanently vetoed here (the
        // recurrent-layer hazard is why MTP models exist on this
        // gate), but whole-state restore is recurrent-faithful, so
        // the target context can resume a pinned stable prefix and
        // prefill only the suffix. Two deliberate consequences:
        //
        //  - The draft side (`session.process`) sees pre-norm h for
        //    the SUFFIX positions only — on hit runs the prefix was
        //    never live-decoded, and on learn runs stage-1 decodes
        //    with no outputs (a logits-bearing save would balloon the
        //    state file by n_outputs × n_vocab ≈ GBs). Draft
        //    calibration is therefore identical between learn and hit
        //    runs. Worst case is a LOWER DRAFT ACCEPTANCE RATE on
        //    cached requests — never wrong output: every draft is
        //    verify-gated by the target. Decomposable via the
        //    end-of-generation acceptance log.
        //
        //  - `session.begin(0, &tokens)` still receives the FULL
        //    token list, so per-seq position bookkeeping is unchanged.
        let plan = match directed_pin_tokens(model, request, &full_prompt, &tokens) {
            Some(pin) => prefix_state.plan_directed(&tokens, pin),
            None => prefix_state.plan(&tokens),
        };
        let mut prefix_base: usize = 0;
        match plan {
            PrefixPlan::Restore { key, prefix_len } => {
                target_ctx.clear_kv_cache(); // kv-phase: PrefixStateRestore
                let path = prefix_state.entry_path(key);
                let t0 = Instant::now();
                let loaded = path.as_ref().and_then(|p| {
                    target_ctx
                        .load_session_file(p, target_ctx.n_ctx() as usize)
                        .ok()
                });
                match loaded {
                    Some(restored) if restored.len() == prefix_len => {
                        prefix_base = prefix_len;
                        tracing::info!(
                            target: "prefix_state",
                            model = %model_id,
                            key = format_args!("{key:016x}"),
                            restored_tokens = prefix_len,
                            suffix_tokens = tokens.len() - prefix_len,
                            restore_ms = t0.elapsed().as_millis() as u64,
                            "prefix_state: HIT — restored pinned prefix (MTP path)"
                        );
                    }
                    _ => {
                        tracing::warn!(
                            target: "prefix_state",
                            model = %model_id,
                            key = format_args!("{key:016x}"),
                            "prefix_state: restore failed — invalidating pin, full prefill"
                        );
                        prefix_state.invalidate(key);
                        target_ctx.clear_kv_cache(); // kv-phase: RequestStartReset
                    }
                }
            }
            PrefixPlan::Learn { key, pin_len } => {
                target_ctx.clear_kv_cache(); // kv-phase: PrefixStateLearn
                let n_batch_for_stage = target_ctx.n_batch() as usize;
                let mut stage1 = LlamaBatch::new(pin_len.max(n_batch_for_stage), 1);
                let mut stage1_ok = true;
                for (i, &tok) in tokens[..pin_len].iter().enumerate() {
                    if stage1.add(tok, i as i32, &[0], false).is_err() {
                        stage1_ok = false;
                        break;
                    }
                }
                if stage1_ok && target_ctx.decode(&mut stage1).is_ok() {
                    let t0 = Instant::now();
                    let path = prefix_state.state_path(key);
                    let saved = prefix_state.ensure_dir().is_ok()
                        && target_ctx
                            .save_session_file(&path, &tokens[..pin_len])
                            .is_ok();
                    if saved {
                        prefix_state.commit(key, tokens[..pin_len].to_vec(), path);
                        tracing::info!(
                            target: "prefix_state",
                            model = %model_id,
                            key = format_args!("{key:016x}"),
                            pinned_tokens = pin_len,
                            save_ms = t0.elapsed().as_millis() as u64,
                            "prefix_state: LEARNED — pinned stable prefix (MTP path)"
                        );
                    } else {
                        tracing::warn!(
                            target: "prefix_state",
                            model = %model_id,
                            "prefix_state: save failed — continuing unpinned"
                        );
                    }
                    prefix_base = pin_len;
                } else {
                    tracing::warn!(
                        target: "prefix_state",
                        model = %model_id,
                        "prefix_state: stage-1 prefill failed — full clear + full prefill"
                    );
                    target_ctx.clear_kv_cache(); // kv-phase: PrefixStateLearn
                }
            }
            PrefixPlan::Pass => {
                target_ctx.clear_kv_cache(); // kv-phase: RequestStartReset
            }
        }

        let n_ctx = target_ctx.n_ctx() as usize;
        // Reserve KV headroom for the verify batch's speculative draft
        // tokens. Each verify decode places [last_token, ..n_draft_max]
        // at positions starting at `n_past`; if generation is allowed to
        // fill all of n_ctx, that batch overflows the KV cache and
        // llama_decode returns "Decode Error 1: NoKvCacheSlot". Observed
        // 2026-06-02: a 196-chapter referential atlas delta on a 16128-ctx
        // primary slot ran clean for ~108 small chapters, then died on the
        // first entity-rich chapter whose prompt+output filled the
        // context — MTP had no slot left for its drafts. Clamping
        // generation to leave n_draft_max+1 free slots fixes it (a prompt
        // that's itself within the draft window of n_ctx makes
        // clamp_max_tokens error, which the outer quarantine demotes to
        // SingleToken — the correct fallback for a near-full context).
        // Admit against min(n_ctx, n_batch): the prefill below decodes the
        // whole prompt as a SINGLE batch (MTP needs logits on every position
        // for pre-norm extraction), so a prompt longer than n_batch trips
        // GGML_ASSERT(n_tokens_all <= n_batch) inside llama_decode and
        // aborts the daemon. ctx_n_batch keeps n_batch >= n_ctx so this
        // normally resolves to n_ctx; the min is a regression guard — an
        // over-long prompt then errors from clamp_max_tokens and the outer
        // quarantine demotes to SingleToken rather than SIGABRT-ing.
        let n_batch_max = target_ctx.n_batch() as usize;
        let mtp_ctx = n_ctx
            .min(n_batch_max)
            .saturating_sub(n_draft_max as usize + 1);
        let max_tokens = clamp_max_tokens(request.max_tokens, tokens.len(), mtp_ctx)?;

        // Prefill: decode the prompt SUFFIX (`tokens[prefix_base..]`,
        // the whole prompt when no pinned prefix restored) as one
        // batch with logits=true on every position. MTP needs that
        // flag set throughout so pre-norm embeddings can be extracted
        // for the draft context (`get_embeddings_pre_norm_ith` errors
        // with `batch.logits[N] != true` otherwise — upstream
        // invariant). See the prefix_state block above for why the
        // pinned prefix carries no pre-norm h.
        //
        // The MTP session's `set_embeddings_pre_norm(true)` hook was
        // installed on both contexts at slot-load time (see
        // ModelSlot::load), so this prefill decode will compute and
        // store the pre-norm hidden state that `session.process` reads
        // back below.
        let prefill_capacity = tokens.len().max(n_batch_max);
        let mut prefill = LlamaBatch::new(prefill_capacity, 1);
        for (i, &tok) in tokens[prefix_base..].iter().enumerate() {
            prefill
                .add(tok, (prefix_base + i) as i32, &[0], true)
                .map_err(|e| Error::Inference(format!("MTP prefill batch add failed: {e}")))?;
        }
        target_ctx
            .decode(&mut prefill)
            .map_err(|e| Error::Inference(format!("MTP prefill decode failed: {e}")))?;
        super::ffi_trace::record(super::ffi_trace::FfiCall::MtpDecode);

        // `process(&prefill)` after every target decode is what injects
        // target's pre-norm h into the draft side. `begin(seq_id,
        // &tokens)` then resets per-seq pending-h state for THIS
        // request (the session itself persists across requests). Both
        // calls are load-bearing: skipping either makes the draft run
        // uncalibrated and acceptance collapses.
        session
            .process(&prefill)
            .map_err(|e| Error::Inference(format!("MTP process(prefill) failed: {e:?}")))?;
        super::ffi_trace::record(super::ffi_trace::FfiCall::SessionProcess);
        session
            .begin(0, &tokens)
            .map_err(|e| Error::Inference(format!("MTP begin failed: {e:?}")))?;
        super::ffi_trace::record(super::ffi_trace::FfiCall::SessionBegin);

        // Sample the first token from prefill's last logit position.
        // Use ConstrainedSampler::Explore — no JSON-schema mask is
        // installed on this path (dispatcher filtered structured
        // requests out), so the sampler behaves as a plain chain
        // of {temp/top-p/top-k/penalties} per the request quirks.
        let mut sampler = build_sampler(model, request, quirks);
        let mut last_token =
            sampler.sample(&*target_ctx, prefill.n_tokens() - 1, SamplerRole::Explore);
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
        let jump_fwd_t2_enabled = jump_fwd_t2_enabled_from_env(|k| std::env::var(k).ok());
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
                target_ctx.clear_kv_cache(); // kv-phase: ErrorAbort
                draft_ctx.clear_kv_cache(); // kv-phase: ErrorAbort
                return Err(Error::Inference(format!(
                    "MTP inference deadline exceeded after {elapsed}s ({n_generated} tokens)"
                )));
            }

            // Constraint-engine failure abort — see generate_sync for
            // rationale. Latched matcher failure means fail-closed
            // masks forever; draft/verify cycles can't recover it.
            if let Some(cause) = sampler.constraint_failure() {
                target_ctx.clear_kv_cache(); // kv-phase: ErrorAbort
                draft_ctx.clear_kv_cache(); // kv-phase: ErrorAbort
                return Err(Error::Inference(format!(
                    "llguidance constraint failed after {n_generated} tokens (mtp): {cause}"
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
            let forced_peek: Vec<LlamaToken> =
                if jump_fwd_enabled && jump_fwd_t2_enabled && n_generated < max_tokens {
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
                super::ffi_trace::record(super::ffi_trace::FfiCall::MtpDecode);
                session
                    .process(&verify)
                    .map_err(|e| Error::Inference(format!("MTP forced process failed: {e:?}")))?;
                super::ffi_trace::record(super::ffi_trace::FfiCall::SessionProcess);
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
                let next_token = sampler.sample(&*target_ctx, k as i32, SamplerRole::Explore);
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
                .map_err(|e| {
                    Error::Inference(format!("MTP draft KV pre-verify rollback failed: {e:?}"))
                })?;

            target_ctx
                .decode(&mut verify)
                .map_err(|e| Error::Inference(format!("MTP verify decode failed: {e}")))?;
            super::ffi_trace::record(super::ffi_trace::FfiCall::MtpDecode);
            session
                .process(&verify)
                .map_err(|e| Error::Inference(format!("MTP process(verify) failed: {e:?}")))?;
            super::ffi_trace::record(super::ffi_trace::FfiCall::SessionProcess);

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
                        next_token =
                            sampler.sample(&*target_ctx, (i + 1) as i32, SamplerRole::Explore);
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
                    .map_err(|e| {
                        Error::Inference(format!("MTP target KV rollback failed: {e:?}"))
                    })?;
                draft_ctx
                    .clear_kv_cache_seq(Some(0), Some(new_n_past as u32), None)
                    .map_err(|e| {
                        Error::Inference(format!("MTP draft KV rollback failed: {e:?}"))
                    })?;
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
        // debug!, not info!: rich per-request MTP acceptance telemetry.
        // Fired once per generation; the request is summarised at INFO by
        // `inference.complete: done`. `RUST_LOG=sovereign_inference=debug`
        // to decompose MTP accept-rate / jump-forward per request.
        tracing::debug!(
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
    pub(crate) fn generate_sync_batched(
        model: &LlamaModel,
        model_id: &str,
        ctx: &mut crate::llama::cpp::context::LlamaContext<'_>,
        requests: &[&CompletionRequest],
        quirks: &ModelQuirks,
    ) -> Result<Vec<(String, usize, usize)>> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }

        ctx.clear_kv_cache(); // kv-phase: RequestStartReset

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
                    while end > 0 && !prompt.is_char_boundary(end) {
                        end -= 1;
                    }
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
            .map(|(r, toks)| clamp_max_tokens(r.max_tokens, toks.len(), n_ctx_per_seq))
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
                    .map_err(|e| Error::Inference(format!("Batched prefill add failed: {e}")))?;
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
        let mut next_pos: Vec<i32> = tokenized.iter().map(|t| t.len() as i32).collect();
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

                if let Ok(piece) = model.token_to_piece(token, &mut decoders[seq], true, None) {
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
                    .map_err(|e| Error::Inference(format!("Batched decode add failed: {e}")))?;
                new_logit_idx[seq] = bi;
                next_pos[seq] += 1;
                bi += 1;
            }
            current_logit_idx = new_logit_idx;

            ctx.decode(&mut batch)
                .map_err(|e| Error::Inference(format!("Batched decode step failed: {e}")))?;
        }

        // Cleanup — leave the KV cache empty so the next caller (or a
        // partial-batch retry) doesn't pick up stale positions. Legal
        // as an end-of-generation clear ONLY because this runs on the
        // batched FastShort companion ctx, which never populates
        // cached_tokens (no prefix cache on the batched path).
        ctx.clear_kv_cache(); // kv-phase: EndOfGenerationNoPrefixCache

        let mut results = Vec::with_capacity(requests.len());
        for (seq, output) in outputs.into_iter().enumerate() {
            results.push((output, tokenized[seq].len(), n_generated[seq]));
        }
        Ok(results)
    }

    pub(crate) fn generate_stream_sync(
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
        ctx.clear_kv_cache(); // kv-phase: RequestStartReset

        let full_prompt = format_prompt(model, model_id, request, quirks)?;

        let tokens = model
            .str_to_token(&full_prompt, add_bos_for(request))
            .map_err(|e| Error::Inference(format!("Tokenization failed: {e}")))?;

        let n_ctx = ctx.n_ctx() as usize;
        let max_tokens = clamp_max_tokens(request.max_tokens, tokens.len(), n_ctx)?;

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
        let deadline = Instant::now() + std::time::Duration::from_secs(deadline_secs);
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
        // balanced JSON envelope when tools were requested. The
        // shape-B state machine + gates live in `gates::ToolStopTracker`.
        let tools_present = request.tools.as_ref().is_some_and(|t| !t.is_empty());
        let mut tool_stop =
            ToolStopTracker::new(tools_present, request.structured_output.is_some());

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
                ctx.clear_kv_cache(); // kv-phase: ErrorAbort
                return Err(Error::Inference(format!(
                    "inference deadline exceeded after {elapsed}s ({n_generated} tokens generated; \
                     deadline={deadline_secs}s) — likely pathological JSON-Schema mask state. \
                     Set SOVEREIGN_INFERENCE_TIMEOUT_SECS to override."
                )));
            }

            // Constraint-engine failure abort — see generate_sync for
            // rationale. Stream variant: the receiver gets the Err as
            // an error frame instead of a silent garbage tail.
            if let Some(cause) = sampler.constraint_failure() {
                ctx.clear_kv_cache(); // kv-phase: ErrorAbort
                return Err(Error::Inference(format!(
                    "llguidance constraint failed after {n_generated} tokens (stream): {cause}"
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

            let role = if tools_present && tool_stop.in_json_string() {
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
                push_sliding_tail(&mut tail, &piece);
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
                // after the move into the channel. Gates live inside
                // `ToolStopTracker`.
                let json_envelope_complete = tool_stop.observe(&piece, in_think);

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
                    if sampler.grammar_is_stopped() {
                        tracing::info!(
                            model = %model_id,
                            n_generated,
                            "inference: stopping on llguidance grammar accept (stream)"
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

            if in_think
                && think_tokens >= request.think_budget.unwrap_or(THINK_BUDGET)
                && !think_budget_fired
            {
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

        ctx.clear_kv_cache(); // kv-phase: EndOfGenerationNoPrefixCache

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
    pub(crate) fn generate_stream_sync_with_finish(
        model: &LlamaModel,
        model_id: &str,
        ctx: &mut crate::llama::cpp::context::LlamaContext<'_>,
        request: &CompletionRequest,
        tx: &tokio::sync::mpsc::Sender<StreamFrame>,
        quirks: &ModelQuirks,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<()> {
        ctx.clear_kv_cache(); // kv-phase: RequestStartReset

        let full_prompt = format_prompt(model, model_id, request, quirks)?;
        let tokens = model
            .str_to_token(&full_prompt, add_bos_for(request))
            .map_err(|e| Error::Inference(format!("Tokenization failed: {e}")))?;

        let n_ctx = ctx.n_ctx() as usize;
        let max_tokens = clamp_max_tokens(request.max_tokens, tokens.len(), n_ctx)?;
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

        stream_generate_loop(StreamLoopParams {
            model,
            model_id,
            ctx,
            request,
            tx,
            quirks,
            cancel,
            prompt_len: prompt_tokens,
            max_tokens,
            clear_kv_at_end: true,
        })
    }

    /// LCP partial-keep sibling of [`Self::generate_stream_sync_with_finish`]
    /// for Raw FIM prompts (INLINE_COMPLETION.md §4, plan F2).
    ///
    /// The legacy streaming entry unconditionally clears the KV cache at
    /// request start — on the keystroke path that means a full 1–2k
    /// prefill per ghost-text request. FIM prompts are PSM-ordered, so
    /// the prefix section is append-only across keystrokes: computing
    /// the longest common prefix against the previous request's
    /// (prompt-only) token sequence and keeping that KV slice drops
    /// steady-state re-prefill to the suffix window + typing delta.
    ///
    /// Mechanism identical to `generate_sync`'s prefix cache (same
    /// `compute_lcp` reserve-last-token rule, same `prefix_cache_gate`
    /// capability veto, same defensive clear-on-error bookkeeping) —
    /// minus the `prefix_state` pin machinery, which exists for chat's
    /// stable system prompts and buys nothing here.
    ///
    /// End-of-generation does NOT clear the KV cache: positions
    /// `[0, prompt_len)` stay resident for the next request's partial
    /// clear (`cached_tokens` holds prompt tokens only, so generated
    /// tokens beyond the prompt are dropped by the next
    /// `clear_kv_cache_seq(lcp..)`).
    pub(crate) fn generate_stream_sync_fim(
        model: &LlamaModel,
        model_id: &str,
        slot_ctx: &mut SlotContext,
        request: &CompletionRequest,
        tx: &tokio::sync::mpsc::Sender<StreamFrame>,
        quirks: &ModelQuirks,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<()> {
        let gate = prefix_cache_gate(
            model.is_recurrent(),
            model.is_hybrid(),
            &slot_ctx.arch,
            quirks.has_recurrent_layers,
            slot_ctx.is_speculative(),
            |k| std::env::var(k).ok(),
        );
        let prefix_cache_safe = gate.safe;
        // Split-borrow per the generate_sync idiom (direct field access,
        // not through `ctx_mut()`, so cached_tokens stays reachable).
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
            .str_to_token(&full_prompt, add_bos_for(request))
            .map_err(|e| Error::Inference(format!("Tokenization failed: {e}")))?;

        let n_batch = ctx.n_batch() as usize;
        let n_ctx = ctx.n_ctx() as usize;
        let admit_ctx = n_ctx.min(n_batch);
        let max_tokens = clamp_max_tokens(request.max_tokens, tokens.len(), admit_ctx)?;
        let prompt_tokens = tokens.len();

        let cached_len_at_entry = cached_tokens.len();
        let PrefixLcp {
            raw: raw_lcp,
            effective: lcp,
        } = compute_lcp(cached_tokens, &tokens, prefix_cache_safe);
        // Clear BEFORE any cache mutation — restored on decode success
        // below; a mid-decode crash forces a defensive full clear next call.
        cached_tokens.clear();
        if lcp == 0 {
            ctx.clear_kv_cache(); // kv-phase: FimPrefixCacheSetup
        } else if let Err(e) = ctx.clear_kv_cache_seq(Some(0), Some(lcp as u32), None) {
            tracing::warn!(
                error = ?e,
                lcp,
                new_prompt_len = tokens.len(),
                "fim prefix_cache: partial clear failed — falling back to full clear"
            );
            ctx.clear_kv_cache(); // kv-phase: FimPrefixCacheSetup
        }

        let tail = &tokens[lcp..];
        let mut batch = LlamaBatch::new(tail.len().max(512), 1);
        let last_idx = tail.len() - 1;
        for (j, &token) in tail.iter().enumerate() {
            batch
                .add(token, (lcp + j) as i32, &[0], j == last_idx)
                .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;
        }
        tracing::info!(
            target: "fim",
            cache_hit_tokens = lcp,
            raw_lcp,
            new_prefill_tokens = tail.len(),
            new_prompt_len = tokens.len(),
            cached_len_at_entry,
            "fim: prefill scope"
        );
        ctx.decode(&mut batch)
            .map_err(|e| Error::Inference(format!("Prompt decode failed: {e}")))?;
        // Prompt-only fingerprint — generated tokens are intentionally
        // excluded (see generate_sync's rationale).
        *cached_tokens = tokens.clone();

        stream_generate_loop(StreamLoopParams {
            model,
            model_id,
            ctx,
            request,
            tx,
            quirks,
            cancel,
            prompt_len: prompt_tokens,
            max_tokens,
            clear_kv_at_end: false,
        })
    }
}

/// Parameter bundle for [`stream_generate_loop`] — the decode loop
/// shared by the full-clear legacy streaming entry and the LCP
/// partial-keep FIM entry.
struct StreamLoopParams<'a, 'ctx> {
    model: &'a LlamaModel,
    model_id: &'a str,
    ctx: &'a mut crate::llama::cpp::context::LlamaContext<'ctx>,
    request: &'a CompletionRequest,
    tx: &'a tokio::sync::mpsc::Sender<StreamFrame>,
    quirks: &'a ModelQuirks,
    cancel: Option<&'a tokio_util::sync::CancellationToken>,
    /// Full prompt length in tokens — the position base for generated
    /// tokens (NOT the prefilled tail length under partial-keep).
    prompt_len: usize,
    max_tokens: usize,
    /// Legacy behaviour: clear the KV cache at end-of-generation. The
    /// FIM entry passes false so the prompt's KV slice survives for
    /// the next keystroke's LCP.
    clear_kv_at_end: bool,
}

/// The shared sampling/decode loop behind the two typed streaming
/// entries. Ends with a `Finish` frame carrying the real reason
/// (`Stop` on EOG, `Length` on `max_tokens`, `Cancelled` on token /
/// receiver-drop, `Error` on constraint failure).
fn stream_generate_loop(p: StreamLoopParams<'_, '_>) -> Result<()> {
    let StreamLoopParams {
        model,
        model_id,
        ctx,
        request,
        tx,
        quirks,
        cancel,
        prompt_len,
        max_tokens,
        clear_kv_at_end,
    } = p;
    {
        let tokens_len = prompt_len;
        let prompt_tokens = prompt_len;

        let mut sampler = build_sampler(model, request, quirks);
        let mut n_generated = 0usize;
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        let mut batch = LlamaBatch::new(512, 1);

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
        // balanced JSON envelope when tools were requested. The
        // shape-B state machine + gates live in `gates::ToolStopTracker`.
        let tools_present = request.tools.as_ref().is_some_and(|t| !t.is_empty());
        let mut tool_stop =
            ToolStopTracker::new(tools_present, request.structured_output.is_some());

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
            // Constraint-engine failure abort — see generate_sync for
            // rationale. This loop's exit idiom is `reason` + break
            // (tokens already streamed can't be unsent), so the
            // failure surfaces as finish_reason "error" on the final
            // frame rather than a transport-level Err.
            if let Some(cause) = sampler.constraint_failure() {
                reason = FinishReason::Error(format!("llguidance constraint failed: {cause}"));
                break;
            }

            let role = if tools_present && tool_stop.in_json_string() {
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
                push_sliding_tail(&mut tail, &piece);
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
                // need to re-borrow piece bytes after the move. Gates
                // live inside `ToolStopTracker`.
                let json_envelope_complete = tool_stop.observe(&piece, in_think);

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
                    if sampler.grammar_is_stopped() {
                        tracing::info!(
                            model = %model_id,
                            n_generated,
                            "inference: stopping on llguidance grammar accept (stream-finish)"
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
                .add(token, (tokens_len + n_generated - 1) as i32, &[0], true)
                .map_err(|e| Error::Inference(format!("Batch add failed: {e}")))?;

            ctx.decode(&mut batch)
                .map_err(|e| Error::Inference(format!("Decode failed: {e}")))?;

            if in_think
                && think_tokens >= request.think_budget.unwrap_or(THINK_BUDGET)
                && !think_budget_fired
            {
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
                            .add(ct, (tokens_len + n_generated) as i32, &[0], true)
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

        if clear_kv_at_end {
            ctx.clear_kv_cache(); // kv-phase: EndOfGenerationNoPrefixCache
        }
        // else: FIM partial-keep — the prompt's KV slice stays resident
        // for the next request's LCP (see generate_stream_sync_fim).

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

#[cfg(test)]
mod total_model_bytes_tests {
    use super::total_model_bytes;
    use std::path::PathBuf;

    /// Write a file of `len` bytes and return its path.
    fn shard(dir: &std::path::Path, name: &str, len: usize) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, vec![0u8; len]).unwrap();
        p
    }

    #[test]
    fn split_gguf_sums_all_shards() {
        // The fix-#2 regression fence: a split GGUF must be classified by the
        // TOTAL across shards, not the (tiny, header-only) first shard —
        // getting this wrong re-opens the StreamSplit bulk-stream deadlock.
        let dir = tempfile::tempdir().unwrap();
        let first = shard(dir.path(), "m-00001-of-00003.gguf", 10);
        shard(dir.path(), "m-00002-of-00003.gguf", 500);
        shard(dir.path(), "m-00003-of-00003.gguf", 300);
        assert_eq!(total_model_bytes(&first), 810);
    }

    #[test]
    fn stem_with_dashes_and_last_of_marker_win() {
        // Real-world names carry dashes and could even contain "-of-" in the
        // stem; the LAST "-of-" must be the shard marker.
        let dir = tempfile::tempdir().unwrap();
        let first = shard(dir.path(), "best-of-Q5-K-XL-00001-of-00002.gguf", 7);
        shard(dir.path(), "best-of-Q5-K-XL-00002-of-00002.gguf", 11);
        assert_eq!(total_model_bytes(&first), 18);
    }

    #[test]
    fn non_sharded_model_is_its_own_size() {
        let dir = tempfile::tempdir().unwrap();
        let single = shard(dir.path(), "plain-model.gguf", 42);
        assert_eq!(total_model_bytes(&single), 42);
    }

    #[test]
    fn missing_sibling_shard_falls_back_to_single() {
        // A missing sibling means the load itself will fail at open time; the
        // placement answer just must not be an invented sum.
        let dir = tempfile::tempdir().unwrap();
        let first = shard(dir.path(), "m-00001-of-00003.gguf", 10);
        shard(dir.path(), "m-00002-of-00003.gguf", 500);
        // 00003 never written.
        assert_eq!(total_model_bytes(&first), 10);
    }

    #[test]
    fn count_of_one_is_treated_as_single() {
        let dir = tempfile::tempdir().unwrap();
        let only = shard(dir.path(), "m-00001-of-00001.gguf", 9);
        assert_eq!(total_model_bytes(&only), 9);
    }

    #[test]
    fn unparseable_index_is_single() {
        // "-of-" present but the index isn't numeric → not a shard pattern.
        let dir = tempfile::tempdir().unwrap();
        let odd = shard(dir.path(), "m-final-of-00002.gguf", 5);
        assert_eq!(total_model_bytes(&odd), 5);
    }
}
