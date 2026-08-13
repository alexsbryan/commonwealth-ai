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

use commonwealth_core::fair_sched::EtaEwma;
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
pub fn total_model_bytes(model_path: &Path) -> u64 {
    let single = std::fs::metadata(model_path)
        .map(|m| m.len())
        .unwrap_or(u64::MAX);
    let Some(name) = model_path.file_name().and_then(|n| n.to_str()) else {
        return single;
    };
    let Some(dir) = model_path.parent() else {
        return single;
    };
    // One parser for the `<stem>-<idx>-of-<count>.gguf` convention, shared with
    // `shard_files` and the mesh's fetch side — see `split_shard_names`.
    let Some(names) = super::rpc_warm_cache::split_shard_names(name) else {
        return single;
    };
    let mut total = 0u64;
    for shard_name in names {
        match std::fs::metadata(dir.join(shard_name)) {
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
    /// backed by the same `Arc<LlamaModel>`.
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
    /// **The `MtpSession` is deliberately NOT a field here.** It is
    /// constructed per request, at the top of `generate_sync_mtp_impl`,
    /// from `&mut target_ctx` + `&mut draft_ctx`, and dropped when the
    /// request ends.
    ///
    /// Two reasons, and they now agree. Behaviourally, the session's
    /// internal draft-scheduler state persists across calls and
    /// pollutes subsequent generations — clearing the KV cache does not
    /// touch it — so the session was ALREADY being rebuilt at every
    /// request boundary while only the contexts were reused. Storing it
    /// bought nothing. Structurally, `MtpSession<'ctx, 'model>` (binding
    /// 0.5.1) holds `&'ctx mut LlamaContext<'model>` for both sides, so
    /// a variant holding the session next to the contexts it borrows is
    /// self-referential and cannot be expressed. Before 0.5.1 the
    /// session held untracked raw pointers and the borrow checker could
    /// not see the aliasing at all; the per-request lifetime is what
    /// that arrangement was already relying on, now enforced.
    ///
    /// `MtpSession::new` calls upstream's `common_speculative_init`,
    /// which installs `set_embeddings_pre_norm(true)` on BOTH contexts
    /// so subsequent decodes compute the pre-norm hidden state the draft
    /// side reads in `session.process(batch)`. That hook outlives the
    /// session and is why demotion must drop both contexts rather than
    /// merely drop the session — see `demote_to_single_token`.
    Speculative {
        target_ctx: crate::llama::cpp::context::LlamaContext<'static>,
        draft_ctx: crate::llama::cpp::context::LlamaContext<'static>,
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
    /// **Measured** capability record for this slot, established once at
    /// construction by `capabilities::probe_partial_kv` (see that
    /// module for why the arch string above is evidence rather than a
    /// decision). Shadow phase: recorded and reported, not yet
    /// consulted by `prefix_cache_gate`.
    pub(crate) capabilities: capabilities::ModelCapabilities,
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

    /// **Demote a Speculative slot to SingleToken mode.** Replaces
    /// the pre-2026-05-20 `slot_ctx.mtp_session = None` quarantine
    /// path, which left the target context's
    /// `set_embeddings_pre_norm(true)` hook installed but disconnected
    /// from any draft-side consumer — every subsequent `ctx.decode`
    /// returned `Decode Error -3: unknown` for the rest of the
    /// slot's lifetime.
    ///
    /// Because `common_speculative_init` has no upstream inverse, the
    /// only safe demotion is to **drop both contexts and rebuild the
    /// target from scratch** using the params
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
/// Takes the session by `&mut` and reaches the contexts through it —
/// under binding 0.5.1 the session holds both `&mut LlamaContext`, so
/// there is no way to hold the contexts separately while it is alive.
/// That is the same aliasing this function always had; it is now
/// checked rather than asserted in a comment.
fn probe_mtp_roundtrip(
    session: &mut MtpSession<'_, '_>,
    model: &LlamaModel,
) -> std::result::Result<(), String> {
    session.target_context_mut().clear_kv_cache(); // kv-phase: Probe
    session.draft_context_mut().clear_kv_cache(); // kv-phase: Probe
    let bos = model.token_bos();
    let prompt = [bos];
    let mut batch = LlamaBatch::new(1, 1);
    batch
        .add(bos, 0, &[0], true)
        .map_err(|e| format!("probe: batch add failed: {e}"))?;
    session
        .target_context_mut()
        .decode(&mut batch)
        .map_err(|e| format!("probe: target decode failed: {e}"))?;
    session
        .process(&batch)
        .map_err(|e| format!("probe: session.process failed: {e:?}"))?;
    session
        .begin(0, &prompt)
        .map_err(|e| format!("probe: session.begin failed: {e:?}"))?;
    session.target_context_mut().clear_kv_cache(); // kv-phase: Probe
    session.draft_context_mut().clear_kv_cache(); // kv-phase: Probe
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
    // The probe session is SCOPED: it borrows both contexts, runs the
    // round-trip, and is dropped before the contexts are moved into the
    // returned variant. The slot deliberately stores no session — see
    // `SlotInferenceMode::Speculative` for why, and
    // `generate_sync_mtp_impl` for where the per-request one is built.
    // `probe_result` carries the outcome out of the borrow scope so the
    // error arms can still move `target_ctx` into the Err payload.
    let probe_result: std::result::Result<(), Error> = {
        match MtpSession::new(&mut target_ctx, &mut draft_ctx, 1, n_draft_max) {
            Ok(mut session) => {
                super::ffi_trace::record(super::ffi_trace::FfiCall::MtpSessionBuilt);
                probe_mtp_roundtrip(&mut session, model).map_err(|probe_err| {
                    Error::Inference(format!("MTP probe roundtrip failed: {probe_err}"))
                })
            }
            Err(e) => Err(Error::Inference(format!("MtpSession::new failed: {e:?}"))),
        }
    };
    if let Err(e) = probe_result {
        return Err((target_ctx, e));
    }
    Ok(SlotInferenceMode::Speculative {
        target_ctx,
        draft_ctx,
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
    /// Generation gate: permit, depth gauge, turn EWMA and shed bound.
    /// See [`SlotQueue`] — the one decider for this slot's admission.
    pub(crate) queue: Arc<SlotQueue>,
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

/// Decrements a slot's `queued` gauge on drop.
///
/// The decrement MUST be structural rather than a statement after the
/// `.await`: every caller of [`ModelSlot::acquire_inflight`] is inside a
/// future the client can cancel (a dropped SSE connection, a cancelled
/// chat turn), and a cancellation between the increment and the await's
/// completion would never reach a trailing `fetch_sub`. The gauge would
/// then ratchet upward forever and every subsequent queue reading would
/// be wrong — a broken instrument that still reports confidently, which
/// is worse than no instrument (ARCH_PRINCIPLES §18.4).
struct QueuedGuard<'a>(&'a std::sync::atomic::AtomicU32);

impl Drop for QueuedGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Default ceiling on how long a caller may be made to wait SILENTLY for a
/// model permit before the host sheds instead (`0` disables the bound).
///
/// Thirty seconds, set by operator decision 2026-08-06 against the measured
/// unit of serialization. A caller who would wait longer than this is better
/// served by an immediate "busy, retry in N" than by silence — the difference
/// between "slow" and "appears broken" (MESH_N4_TOPOLOGY M5).
///
/// **The tradeoff this number encodes**, so it can be re-decided rather than
/// re-derived: shedding gives a hub deployment with no alternative holder
/// *nothing* instead of a slow answer. Thirty seconds bets that a consumer
/// who would wait half a minute prefers to know. Override with
/// `SOVEREIGN_MAX_QUEUE_WAIT_SECS`.
pub(crate) const DEFAULT_MAX_QUEUE_WAIT_MS: u64 = 30_000;

/// Seed for a slot's turn EWMA before any turn completes.
///
/// Deliberately SMALL. The seed only matters for the handful of requests
/// before the first turn lands, and an over-large seed would shed callers on
/// the strength of a guess — refusing work the host could have served. Better
/// to under-predict briefly and let the EWMA climb to the truth.
const DEFAULT_TURN_SEED_MS: u64 = 1_000;

/// Read the shed threshold once, from `SOVEREIGN_MAX_QUEUE_WAIT_SECS`.
/// `0` means "never shed" — today's unbounded behaviour, kept reachable
/// because it is what every deployment ran before this bound existed.
fn max_queue_wait_ms() -> u64 {
    match std::env::var("SOVEREIGN_MAX_QUEUE_WAIT_SECS") {
        Ok(v) => match v.trim().parse::<u64>() {
            Ok(secs) => secs.saturating_mul(1_000),
            Err(_) => {
                tracing::warn!(
                    value = %v,
                    default_ms = DEFAULT_MAX_QUEUE_WAIT_MS,
                    "SOVEREIGN_MAX_QUEUE_WAIT_SECS is not a number — using the default"
                );
                DEFAULT_MAX_QUEUE_WAIT_MS
            }
        },
        Err(_) => DEFAULT_MAX_QUEUE_WAIT_MS,
    }
}

/// One model permit, plus everything needed to decide whether waiting for it
/// is reasonable and to report the wait afterwards.
///
/// THE ONE DECIDER for "may this caller have the model, and if not now, how
/// long would it wait" (ARCH_PRINCIPLES §10.6, principle 8). Before this
/// type there were thirteen bare `.acquire_owned().await` sites across two
/// separate semaphores, none of them timed, bounded or traced.
///
/// Deliberately owns four things that were previously scattered or absent:
/// the semaphore, the depth gauge, the turn-duration EWMA, and the shed
/// threshold. They belong together because the shed decision is a function of
/// all four, and splitting them is what let the queue go unmeasured.
pub(crate) struct SlotQueue {
    inflight: Arc<tokio::sync::Semaphore>,
    /// Callers parked in [`Self::acquire`], excluding the permit holder.
    /// `tokio::sync::Semaphore` does not expose its waiter count, so we keep
    /// our own — and without it the queue that actually serialises this host
    /// is invisible: the only evidence a request waited is client wall clock.
    queued: std::sync::atomic::AtomicU32,
    /// Observed turn duration, shared implementation with the chat
    /// scheduler's ETA (`commonwealth_core::fair_sched::EtaEwma`).
    eta: std::sync::Mutex<EtaEwma>,
    /// Shed past this predicted wait. `0` = never shed.
    max_wait_ms: u64,
    /// Names this queue in every event it emits.
    label: String,
}

impl SlotQueue {
    pub(crate) fn new(label: impl Into<String>, seed_turn_ms: u64) -> Self {
        Self {
            inflight: Arc::new(tokio::sync::Semaphore::new(1)),
            queued: std::sync::atomic::AtomicU32::new(0),
            eta: std::sync::Mutex::new(EtaEwma::new(seed_turn_ms)),
            max_wait_ms: max_queue_wait_ms(),
            label: label.into(),
        }
    }

    /// Override the shed bound. Used by tests to pin a threshold rather than
    /// depend on the ambient environment — a test whose verdict moves with
    /// `SOVEREIGN_MAX_QUEUE_WAIT_SECS` is not a gate.
    pub(crate) fn with_max_wait_ms(mut self, ms: u64) -> Self {
        self.max_wait_ms = ms;
        self
    }

    /// Free permits right now. Drives `fast_slot_busy`'s overflow-lane rule,
    /// which is a ROUTING decision and deliberately unchanged by the bound.
    pub(crate) fn available_permits(&self) -> usize {
        self.inflight.available_permits()
    }

    /// Callers currently waiting, excluding the holder.
    pub(crate) fn depth(&self) -> u32 {
        self.queued.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn eta_snapshot(&self) -> EtaEwma {
        *self
            .eta
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Close the permit semaphore — test-only, to drive the torn-down-slot
    /// path without standing up a real eviction.
    #[cfg(test)]
    pub(crate) fn close_for_test(&self) {
        self.inflight.close();
    }

    pub(crate) fn record_turn(&self, dur_ms: u64) {
        self.eta
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record(dur_ms);
    }
}

/// A held model permit. Records the turn's duration into the queue's EWMA
/// on drop, which is what makes the next caller's wait prediction real
/// rather than a seeded guess.
///
/// Drop-based for the same reason [`QueuedGuard`] is: the permit is released
/// on paths that never run to completion — a cancelled stream, a panicking
/// blocking task — and a prediction fed only by successful turns would drift
/// exactly when the host is in trouble.
pub(crate) struct SlotPermit {
    _permit: tokio::sync::OwnedSemaphorePermit,
    queue: Arc<SlotQueue>,
    started: std::time::Instant,
}

impl std::fmt::Debug for SlotPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlotPermit")
            .field("slot", &self.queue.label)
            .field("held_ms", &self.started.elapsed().as_millis())
            .finish()
    }
}

impl Drop for SlotPermit {
    fn drop(&mut self) {
        let ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        self.queue.record_turn(ms);
    }
}

/// Acquire a model permit, bounding and reporting the wait.
///
/// Three outcomes, and the middle one is the whole point of M5:
///   - permit free            -> granted immediately, `debug` event
///   - short predicted wait   -> park, `info` events on both sides
///   - long predicted wait    -> SHED with position + retry hint, no park
///
/// **The bound is on PREDICTED WAIT, not on queue depth**, and that is a
/// measured choice rather than a stylistic one. On 2026-08-06 the same depth
/// of 8 cost 6.2 s when nine callers shared a prompt prefix and 90.7 s when
/// they did not — a 15x swing, because the unit of serialization is uncached
/// prefill. A depth bound cannot express a rule that is fine in one shape and
/// catastrophic in the other; a wait bound can, because the EWMA tracks
/// whichever shape the host is actually in.
pub(super) async fn acquire_with_queue_gauge(
    queue: &Arc<SlotQueue>,
    phase: &'static str,
) -> Result<SlotPermit> {
    use std::sync::atomic::Ordering;

    let grant = |permit| SlotPermit {
        _permit: permit,
        queue: Arc::clone(queue),
        started: std::time::Instant::now(),
    };

    // Fast path: the permit is free, which is every request on an idle host.
    // It must not pay for the accounting below, so this is a `try_` probe
    // rather than an instrumented await.
    if let Ok(permit) = Arc::clone(&queue.inflight).try_acquire_owned() {
        tracing::debug!(
            slot = %queue.label,
            phase,
            waited_ms = 0_u64,
            ahead = 0_u32,
            "inference.queue: permit free, admitted immediately"
        );
        return Ok(grant(permit));
    }

    // Contended. `position` is where this caller would land: the holder,
    // plus everyone already parked ahead of it.
    let position = queue.depth() + 1;
    let eta = queue.eta_snapshot();
    let predicted_wait_ms = eta.predict_wait_ms(position, 1);

    if queue.max_wait_ms > 0 && predicted_wait_ms > queue.max_wait_ms {
        // Shed BEFORE parking. Refusing after a wait would be the worst of
        // both worlds — the caller pays the latency and still gets nothing.
        // One decider for the retry hint — `Error::queue_shed` derives
        // it, so this site and the FastShort coalescer's queue-bound
        // shed cannot drift apart.
        let shed = Error::queue_shed(position, predicted_wait_ms);
        let retry_after_secs = match &shed {
            Error::QueueShed {
                retry_after_secs, ..
            } => *retry_after_secs,
            _ => unreachable!("queue_shed constructs QueueShed"),
        };
        tracing::info!(
            slot = %queue.label,
            phase,
            position,
            predicted_wait_ms,
            max_wait_ms = queue.max_wait_ms,
            avg_turn_ms = eta.avg_turn_ms(),
            retry_after_secs,
            "inference.queue: SHED — predicted wait exceeds the bound"
        );
        return Err(shed);
    }

    let ahead = queue.queued.fetch_add(1, Ordering::SeqCst) + 1;
    let _queued_guard = QueuedGuard(&queue.queued);
    tracing::info!(
        slot = %queue.label,
        phase,
        ahead,
        predicted_wait_ms,
        avg_turn_ms = eta.avg_turn_ms(),
        "inference.queue: slot busy, waiting for permit"
    );

    let started = std::time::Instant::now();
    let acquired = Arc::clone(&queue.inflight).acquire_owned().await;
    let waited_ms = started.elapsed().as_millis() as u64;

    match acquired {
        Ok(permit) => {
            tracing::info!(
                slot = %queue.label,
                phase,
                ahead,
                waited_ms,
                predicted_wait_ms,
                "inference.queue: permit acquired after wait"
            );
            Ok(grant(permit))
        }
        Err(e) => {
            // The semaphore is closed — the slot is being torn down under
            // us. Name the phase so the caller's error still says which
            // entry point died.
            tracing::warn!(
                slot = %queue.label,
                phase,
                waited_ms,
                error = %e,
                "inference.queue: slot permit closed while waiting"
            );
            Err(Error::Inference(format!(
                "{phase}: slot {:?} permit closed: {e}",
                queue.label
            )))
        }
    }
}

impl ModelSlot {
    /// Acquire this slot's inflight permit, reporting the wait.
    ///
    /// THE ONE PLACE a caller may take `inflight` (ARCH_PRINCIPLES
    /// §10.6). Before this existed the same three lines appeared at
    /// eight sites in `engine.rs` — one per completion/streaming entry
    /// point — each a bare `.acquire_owned().await` with no timing, no
    /// depth and no event. That is the queue this host actually
    /// serialises on, so the single most load-bearing wait in the
    /// system was the one thing `tracing=debug` could not show you.
    ///
    /// `phase` names the entry point so a contended host's log says
    /// which kind of call is queueing, not just that something is.
    ///
    /// Deliberately still UNBOUNDED — this change makes the queue
    /// visible, it does not yet bound it. Bounding is a policy decision
    /// (MESH_N4_TOPOLOGY M5) that wants a measured depth distribution
    /// first, and this is the accessor that will carry it: one decider,
    /// one place to add the ceiling.
    pub(crate) async fn acquire_inflight(
        slot: &Arc<Self>,
        phase: &'static str,
    ) -> Result<SlotPermit> {
        acquire_with_queue_gauge(&slot.queue, phase).await
    }

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
        // `cpu_only` reaches the local-fit gate: a CPU-only load mmaps the
        // weights (reclaimable page cache — a box can legitimately run a
        // model near or beyond RAM that way), while a GPU load pins them
        // in GTT/unified memory, which is what starved the desktop in the
        // 2026-07-27 incident. Only the pinned case is gated.
        let placement = resolve_placement(
            model_path,
            model_bytes,
            distributable,
            effective_gpu_layers == 0,
            context_size,
        );

        // **MTP weights are opt-in at LOAD time as of llama.cpp PR #25784.**
        // `qwen35.cpp` (and every other MTP arch) now gates its NextN tensors
        // on the loader's `load_mtp` flag:
        //
        //     int mtp_flags = !ml.load_mtp ? TENSOR_SKIP : 0;
        //     layer.nextn.eh_proj = create_tensor(..., mtp_flags);
        //
        // The flag defaults to FALSE. Leave it there and the MTP block's
        // tensors are silently skipped while `hparams.n_layer_nextn` stays
        // non-zero — so the model looks MTP-capable, `build_arch_graph`
        // dispatches to `graph_mtp` when the draft context asks for
        // `LLM_GRAPH_TYPE_DECODER_MTP`, and llama.cpp then hits
        // `GGML_ASSERT(layer.nextn.eh_proj)` and **aborts the process**.
        // Not an Err we can fall back from — `ggml_abort`. Reproduced
        // 2026-08-03 on Qwen3.5-4B-UD-MTP-Q6_K_XL, `qwen35.cpp:502`.
        //
        // `deepseek4.cpp` got a guard for the adjacent case (it probes for
        // `blk.N.nextn.eh_proj.weight` in the FILE and zeroes n_layer_nextn
        // when absent); `qwen35.cpp` did not, and that guard would not help
        // here anyway — the tensor IS in the file, we just declined to load
        // it. Passing the flag is the caller's job and it is ours.
        //
        // Set unconditionally rather than behind the MTP-candidacy
        // heuristic, because candidacy is not yet known here: `mtp_by_arch`
        // needs the arch string, which needs the loaded model. Doing it
        // unconditionally is both safe and cheap — the MTP tensor loop runs
        // only over `n_layer..n_layer_all`, which is empty unless the gguf
        // actually declares MTP layers, so a non-MTP model pays nothing and
        // loads identically. The env kill-switch is honoured so
        // `SOVEREIGN_MTP_DISABLE=1` still means "don't even load the
        // weights", not merely "don't speculate with them".
        let mtp_disabled_at_load = std::env::var("SOVEREIGN_MTP_DISABLE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let mut model_params = LlamaModelParams::default()
            .with_n_gpu_layers(effective_gpu_layers)
            .with_load_mtp(!mtp_disabled_at_load);
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
                    .with_tensor_buft_overrides(&dist.overrides)
                    // The tensor_split pin is MANDATORY alongside the
                    // overrides: llama.cpp computes its per-layer device map
                    // (`model.dev_layer`, consulted by `resolve_fused_ops`)
                    // from tensor_split over n_layer+1 units — the output head
                    // is the extra unit — and never from the overrides. Unpinned
                    // (or pinned to naive block FRACTIONS, the 2026-07-27 H2
                    // attempt: 0.25×33 = 8.25 straddles layer 8), dev_layer
                    // disagrees with the -ot cut by one layer, the fused GDN
                    // kernel is disabled, and the unfused GGML_OP_SET path
                    // aborts the host on first distributed decode.
                    // `dist.tensor_split` puts each cut point HALFWAY between
                    // block boundaries in (n_layer+1)-unit space, so dev_layer
                    // reproduces the plan exactly (see `dev_layer_tensor_split`
                    // and docs/DISTRIBUTED_GDN_CRASH_STATUS.md).
                    .with_tensor_split(&dist.tensor_split);
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
            LoadPlacement::LocalUnfit { need_mb, usable_mb } => {
                // Local-fit gate refusal: the fallback load would starve the
                // host (2026-07-27: a 91 GB local fallback of the shared 122B
                // killed the desktop session on unified memory). Same posture
                // as InsufficientCluster — stay unavailable, retry on the
                // next worker-set change.
                return Err(Error::Inference(format!(
                    "local-fit gate: {} needs ~{need_mb} MiB (weights + KV + scratch) but only \
                     ~{usable_mb} MiB of system memory is usable after the host reserve — not \
                     loading locally; retries as workers rejoin. \
                     SOVEREIGN_SKIP_LOCAL_FIT_CHECK=1 overrides.",
                    model_path.display()
                )));
            }
            LoadPlacement::WorkerUnfit(overflow) => {
                // The cluster has the memory; one device does not have its
                // share. Do NOT load — neither distributed (the shard would
                // fail on that device) nor locally (a model this size on the
                // host is the 2026-07-27 session-kill). The message carries the
                // numbers and both real fixes; see `WorkerOverflow::refusal`
                // for why it says LOWER the headroom.
                return Err(Error::Inference(format!(
                    "{} — {}",
                    model_path.display(),
                    overflow.refusal()
                )));
            }
        }

        let model =
            LlamaModel::load_from_file(backend, model_path, &model_params).map_err(|e| {
                Error::Inference(crate::llama::describe_model_load_failure(
                    "primary/fast",
                    model_path,
                    e,
                ))
            })?;

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
        // Its own predicate, not the prefix-cache ladder. Same answer
        // today; different question (see `arch_ships_mtp_draft_heads`).
        let mtp_by_arch = arch_ships_mtp_draft_heads(&slot_arch);
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
        // `mtp_disabled_at_load` is read ONCE, up at the model-params build
        // (it also gates `with_load_mtp`), so the "don't load the weights"
        // and "don't speculate" decisions can never disagree.
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
                .with_n_ubatch(chat_slot_n_ubatch());
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
            n_ubatch: chat_slot_n_ubatch(),
            n_rs_seq: if is_mtp_model {
                Some(mtp_n_rs_seq)
            } else {
                None
            },
            used_gpu,
        };
        // `mut` because the capability probe below borrows the target
        // context out of this before it is moved into `SlotContext`.
        let mut mode: SlotInferenceMode = if is_mtp_model {
            let draft_params = LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(context_size))
                .with_ctx_type(LlamaContextType::Mtp)
                .with_n_rs_seq(mtp_n_rs_seq)
                .with_n_batch(ctx_n_batch(context_size))
                .with_n_ubatch(chat_slot_n_ubatch())
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
        // set `[models].context_size = 16000` in ~/.svrnmesh/config.toml
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

        // **Capability probe (shadow phase).** Establish by measurement
        // what this slot's memory module can do, rather than inferring
        // it from the arch string. Runs on the constructed context
        // before any request touches it and leaves the KV cleared; the
        // verdict is RECORDED ONLY in this phase — `prefix_cache_gate`
        // still reads the arch ladder, so behaviour is unchanged.
        //
        // Skipped for distributed children: their single slot is the
        // mesh's sharded primary and a probe would decode across the
        // RPC fabric for a verdict that path never consults.
        // The two cross-check fields are derived inside
        // `ModelCapabilities::observe` so the offline shadow sweep and
        // this path cannot drift apart (§10.6). Comparing the probe
        // against what the gate will ACTUALLY do — rather than against
        // the arch ladder alone — makes a disagreement mean "the flip
        // would change behaviour", which is the question the shadow
        // phase exists to answer.
        let (partial_kv, partial_kv_provenance) = if distributable {
            (
                capabilities::PartialKvVerdict::CouldNotJudge {
                    reason: "distributed child — probe not run",
                },
                capabilities::Provenance::Unknown,
            )
        } else if !capabilities::probe_enabled() {
            (
                capabilities::PartialKvVerdict::CouldNotJudge {
                    reason: "SOVEREIGN_CAPABILITY_PROBE=0",
                },
                capabilities::Provenance::Unknown,
            )
        } else {
            let probe_started = Instant::now();
            let ctx = match &mut mode {
                SlotInferenceMode::SingleToken { ctx } => ctx,
                SlotInferenceMode::Speculative { target_ctx, .. } => target_ctx,
            };
            let v = capabilities::probe_partial_kv(&model, ctx);
            tracing::info!(
                target: "capability",
                model = %model_id,
                arch = %slot_arch,
                verdict = v.label(),
                probe_ms = probe_started.elapsed().as_millis() as u64,
                "capability: partial-KV probe complete"
            );
            (v, capabilities::Provenance::Measured)
        };
        let capabilities = capabilities::ModelCapabilities::observe(
            &model,
            &slot_arch,
            partial_kv,
            partial_kv_provenance,
        );
        let declarations_say_unsafe = capabilities.declarations_say_unsafe;
        let arch_ladder_alone_says_unsafe = capabilities.arch_ladder_alone_says_unsafe;
        if capabilities.arch_ladder_alone_is_wrong() {
            // Diagnostic, not an alarm: the ladder no longer decides,
            // so being wrong here costs nothing on its own. It is the
            // standing evidence for WHY it does not decide — it fires
            // on every dense `qwen35` and on `nemotron_h_moe`.
            tracing::info!(
                target: "capability",
                model = %model_id,
                arch = %slot_arch,
                probe_says = capabilities.partial_kv.label(),
                arch_ladder_alone_says_unsafe,
                declarations_say_unsafe,
                "capability: the arch declaration is wrong for this model \
                 (cross-check only — the measurement decides)"
            );
        }
        if capabilities.measurement_contradicts_declaration() {
            // §9.2 warn: a recoverable divergence the operator should
            // see. Post-flip this is no longer "behaviour would change
            // if we trusted the probe" — it IS the behaviour, and the
            // line exists so a probe regression is visible as a
            // divergence from what the declarations expected rather
            // than as a silent performance or correctness shift.
            tracing::warn!(
                target: "capability",
                model = %model_id,
                arch = %slot_arch,
                probe_says = capabilities.partial_kv.label(),
                declarations_say_unsafe,
                arch_ladder_alone_says_unsafe,
                "capability: MEASUREMENT CONTRADICTS THE DECLARATIONS for this model — \
                 the measurement is what the gate follows. If this is a model the \
                 declarations had right, the probe has regressed; re-run \
                 `rs_rollback_spike::capability_probe_detects_both_regimes`."
            );
        }

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
                capabilities,
                mtp_rebuild: if is_mtp_model {
                    Some(rebuild_params)
                } else {
                    None
                },
                prefix_state: PrefixStateCache::new(&model_id),
            }),
            queue: Arc::new(SlotQueue::new(model_id.clone(), DEFAULT_TURN_SEED_MS)),
            model_id,
            size_bytes,
            last_used: std::sync::atomic::AtomicU64::new(now_millis()),
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
        n_gpu_layers: u32,
    ) -> Result<Self> {
        let force_cpu = std::env::var("SOVEREIGN_FORCE_CPU_CHAT")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        // `&& n_gpu_layers > 0` matches the three sibling sites (`ModelSlot::load`,
        // `EmbedSlot::load`, `RerankSlot::load`). This constructor was the only one
        // deciding on the OS alone, and that is a bug: `HardwareProfile::detect` has
        // ALREADY resolved whether this host has a usable GPU, and on an Intel Mac it
        // answers "no" on purpose ("Metal + llama.cpp on discrete AMD GPUs produces
        // garbage output" — see hardware.rs::detect_gpu), yielding n_gpu_layers == 0.
        //
        // Ignoring that put the companion slot's KV cache in a Metal buffer on a
        // machine whose weights are entirely on CPU. Metal buffers are only shared
        // (host-addressable) when the device reports `hasUnifiedMemory`; on a Mac with
        // a discrete GPU they are private, and ggml then services every KV read/write
        // by wrapping the HOST pointer in an MTLBuffer via `newBufferWithBytesNoCopy:`,
        // which returns nil for a non-page-aligned pointer:
        //
        //   ggml-metal-device.m:1743  GGML_ASSERT(buf_src)  ggml_metal_buffer_set_tensor
        //   ggml-metal-device.m:1800  GGML_ASSERT(buf_dst)  ggml_metal_buffer_get_tensor
        //
        // GGML_ASSERT(nil) calls abort(), so the daemon died with SIGABRT mid-request —
        // observed six times in one morning on a MacBookPro16,1 (2026-07-27), five on
        // the set path and one on the get path. Apple Silicon takes the memcpy fast
        // path in both functions and never reaches that code, which is why the crash
        // was invisible on the machines we develop on.
        //
        // Scope: this changes behaviour ONLY where n_gpu_layers == 0 — hosts already
        // determined to have no usable GPU. Apple Silicon (999) and Linux-with-GPU are
        // bit-identical to before.
        let wants_gpu =
            !force_cpu && cfg!(any(target_os = "macos", target_os = "linux")) && n_gpu_layers > 0;

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
        // Same assembly as the primary slot's — in particular the same
        // `declarations_say_unsafe`, which is libllama's flags OR the ladder,
        // not the ladder alone (§10.6).
        let sibling_capabilities = capabilities::ModelCapabilities::observe(
            &model,
            &arch,
            capabilities::PartialKvVerdict::CouldNotJudge {
                reason: "FastShort sibling — batched path has no prefix reuse",
            },
            capabilities::Provenance::Unknown,
        );
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
                // The sibling is NOT probed: it exists to serve
                // `generate_sync_batched`, which carries no prefix
                // reuse at all ("no prefix cache on the batched path"),
                // so a partial-KV verdict here would describe a
                // decision this slot never makes. Recorded as
                // could-not-judge rather than inheriting the primary's
                // verdict, because its context params differ
                // (`n_seq_max=8`) and capability is per-context.
                capabilities: sibling_capabilities,
                arch,
                mtp_rebuild: None,
                prefix_state: PrefixStateCache::new(&model_id),
            }),
            queue: Arc::new(SlotQueue::new(model_id.clone(), DEFAULT_TURN_SEED_MS)),
            model_id,
            size_bytes,
            last_used: std::sync::atomic::AtomicU64::new(now_millis()),
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
                    let is_prefill_error = mtp_error_is_prefill(&msg);
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
        // work on hybrid Mamba/SSM models.
        //
        // MECHANISM, corrected 2026-08-10 (this comment previously
        // said the clear "succeeds against the attention layers but
        // doesn't rewind the recurrent layers" — it does not succeed
        // at all). `llama_memory_seq_rm` REFUSES a partial range
        // removal on a recurrent/hybrid memory module: it returns
        // FALSE and removes nothing. Nothing is corrupted at that
        // moment; the KV simply still holds every position it had.
        // The failure lands one step later, when the tail decode
        // starts at `lcp` while the memory module's last stored
        // position is still the end of the PREVIOUS request:
        //
        //   init: ... inconsistent sequence positions:
        //     last position stored ... for sequence 0 is X = 209
        //     the tokens ... have a starting position of Y = 189
        //     for M-RoPE, it is required that: X < Y
        //
        // `llama_decode` then returns -1, which the Rust binding
        // statically labels "n_tokens == 0" — doubly misleading: the
        // batch was not empty (13 tokens), and the real complaint is
        // position monotonicity, not batch size. Verified 2026-05-19
        // with batch_n_tokens=1 and 50 failing identically, and
        // re-verified 2026-08-10 by instrumenting the dropped bool on
        // both `qwen35` and `qwen35moe` against a `gemma4` control
        // that returned TRUE. The bug is general to any partial-keep
        // on this model class, not an edge case of small tails.
        //
        // Because the refusal is a return VALUE, not an error, it was
        // invisible at all nine call sites until `kv_ops` centralised
        // it. That module is the second line of defence — it turns a
        // refusal into a full clear + full prefill — so this gate is
        // now an optimisation (skip an attempt known to be doomed)
        // rather than the only thing preventing a hard failure. A
        // non-zero `kv_ops::refusals()` in production means an arch
        // slipped this ladder and should be added to it.
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
        //    never empty; the -1 is the M-RoPE position check firing
        //    after a REFUSED truncation, see the mechanism above).
        // Decision matrix extracted to `gates::prefix_cache_gate` so
        // the ladder above is pinned by weight-free tests.
        let gate = prefix_cache_gate(
            &slot_ctx.capabilities,
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
        // `measured` is what the load-time probe established by
        // actually rolling back and replaying, and since 2026-08-10 it
        // DECIDES (with libllama's flags). `arch_says_recurrent` is the
        // declaration, kept on the line as a cross-check — when it
        // disagrees with `measured` the ladder is what is wrong, and
        // `authority` names which source answered.
        tracing::debug!(
            model = %model_id,
            arch = %slot_ctx.arch,
            model_says_recurrent = gate.model_says_recurrent,
            arch_says_recurrent = gate.arch_says_recurrent,
            quirks_say_recurrent = gate.quirks_say_recurrent,
            mtp_session = gate.speculative_active,
            forced = gate.forced,
            prefix_cache_safe,
            measured = gate.measured,
            authority = gate.authority.label(),
            "prefix_cache: gate decision"
        );
        if gate.authority == gates::PrefixCacheAuthority::DeclaredFallback {
            // §18.3 — the substitution is named, not silent. Nothing
            // was measured for this slot, so a DECLARATION answered.
            tracing::info!(
                target: "capability",
                model = %model_id,
                arch = %slot_ctx.arch,
                reason = gate.measured,
                prefix_cache_safe,
                "capability: no measurement for this slot — the arch declaration decided"
            );
        }

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
        // THE prefill. Prefix reuse lives in one body shared with the
        // streaming path — see `prefill_reusing_prefix` for why that
        // matters and what it measured.
        Self::prefill_reusing_prefix(
            model,
            model_id,
            request,
            &full_prompt,
            &tokens,
            n_batch,
            prefix_cache_safe,
            ctx,
            cached_tokens,
            prefix_state,
        )?;

        // The generation loop below reuses one batch buffer, clearing it before
        // every decode. The prefill used to leave its own batch in scope for
        // that; now it owns one internally, so declare the loop's here.
        let mut batch = LlamaBatch::new(n_batch, 1);

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
        Self::generate_sync_mtp_impl(model, model_id, slot_ctx, request, quirks, None, None)
    }

    /// Streaming twin of [`Self::generate_sync_mtp`] — the same loop,
    /// with each accepted piece forwarded to `sink` as it lands and a
    /// terminal `Finish { reason, usage }` frame on typed sinks. The
    /// gate + quarantine wrapper is [`Self::generate_stream_dispatch`].
    fn generate_stream_sync_mtp(
        model: &LlamaModel,
        model_id: &str,
        slot_ctx: &mut SlotContext,
        request: &CompletionRequest,
        quirks: &ModelQuirks,
        sink: &StreamSink<'_>,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<()> {
        Self::generate_sync_mtp_impl(
            model,
            model_id,
            slot_ctx,
            request,
            quirks,
            Some(sink),
            cancel,
        )
        .map(|_| ())
    }

    /// One MTP loop for both delivery modes. `sink: None` is the
    /// classic accumulate-then-return arm (`generate_sync` dispatch);
    /// `Some` streams every accepted piece — first sampled token,
    /// jump-forward forced runs, accepted drafts, and bonus tokens —
    /// the moment it is committed, so a streamed MTP turn has real
    /// TTFT/ITL instead of a buffered blob. Receiver-drop stops the
    /// loop without a terminal frame (the consumer is gone); `cancel`
    /// mirrors `stream_generate_loop`'s token check per iteration.
    fn generate_sync_mtp_impl(
        model: &LlamaModel,
        model_id: &str,
        slot_ctx: &mut SlotContext,
        request: &CompletionRequest,
        quirks: &ModelQuirks,
        sink: Option<&StreamSink<'_>>,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<(String, usize, usize)> {
        // **Build the MTP session for THIS request.** A session must
        // never outlive the request that made it: KV cache clear (below)
        // resets the contexts' token state but does NOT touch the
        // session's internal draft-scheduler state. After 1-2 sequential
        // requests that residual state desynchronises with the fresh
        // prompt and the draft head proposes tokens from stale branches
        // — observed as garbage output ("Python sorting algorithm"
        // responses to unrelated questions) once the search-gym started
        // running --replays > 1.
        //
        // The session is a local, so that is now structural: it is
        // dropped at every `return` from this function and there is no
        // field for a stale one to survive in. `MtpSession::new` also
        // takes `&mut` on both contexts, so the old hazard — two live
        // sessions over the same contexts, which upstream documents as
        // UB inside `common_speculative_*` — is a borrow error rather
        // than a convention.
        //
        // Construction must happen HERE, before any decode whose hidden
        // state the draft side consumes: `common_speculative_init`
        // installs `set_embeddings_pre_norm(true)` on both contexts, and
        // a decode issued before that hook is in place produces post-norm
        // h that `session.process` would silently misread.
        //
        // The `SingleToken` arm is a dispatcher contract violation (the
        // gate routed us here for a non-speculative slot). Return an
        // error rather than panicking so the caller can fall back.
        let SlotContext {
            mode,
            cached_tokens,
            prefix_state,
            ..
        } = slot_ctx;
        let (target_ctx_slot, draft_ctx_slot, n_draft_max) = match mode {
            SlotInferenceMode::Speculative {
                target_ctx,
                draft_ctx,
                n_draft_max,
            } => (target_ctx, draft_ctx, *n_draft_max),
            SlotInferenceMode::SingleToken { .. } => {
                return Err(Error::Inference(
                    "generate_sync_mtp: slot is in SingleToken mode — dispatcher contract violated"
                        .into(),
                ));
            }
        };
        let mut session = MtpSession::new(target_ctx_slot, draft_ctx_slot, 1, n_draft_max)
            .map_err(|e| Error::Inference(format!("MTP session build failed: {e:?}")))?;
        super::ffi_trace::record(super::ffi_trace::FfiCall::MtpSessionBuilt);
        tracing::debug!(
            model_id = %model_id,
            n_draft_max = %n_draft_max,
            "mtp: built session at request boundary"
        );

        // The draft side always starts fresh (the session above is built
        // per request); cached_tokens is the non-MTP path's fingerprint
        // and is cleared so a return to that path doesn't reuse a stale
        // one.
        session.draft_context_mut().clear_kv_cache(); // kv-phase: RequestStartReset
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
                session.target_context_mut().clear_kv_cache(); // kv-phase: PrefixStateRestore
                let path = prefix_state.entry_path(key);
                let t0 = Instant::now();
                // `n_ctx` is hoisted out of the closure: reading it and
                // calling `load_session_file` are two borrows of the same
                // context, and only one of them can be live at a time now
                // that the context is reached through the session.
                let target_n_ctx = session.target_context_mut().n_ctx() as usize;
                let loaded = path.as_ref().and_then(|p| {
                    session
                        .target_context_mut()
                        .load_session_file(p, target_n_ctx)
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
                        session.target_context_mut().clear_kv_cache(); // kv-phase: RequestStartReset
                    }
                }
            }
            PrefixPlan::Learn { key, pin_len } => {
                session.target_context_mut().clear_kv_cache(); // kv-phase: PrefixStateLearn
                let n_batch_for_stage = session.target_context_mut().n_batch() as usize;
                let mut stage1 = LlamaBatch::new(pin_len.max(n_batch_for_stage), 1);
                let mut stage1_ok = true;
                for (i, &tok) in tokens[..pin_len].iter().enumerate() {
                    if stage1.add(tok, i as i32, &[0], false).is_err() {
                        stage1_ok = false;
                        break;
                    }
                }
                if stage1_ok && session.target_context_mut().decode(&mut stage1).is_ok() {
                    let t0 = Instant::now();
                    let path = prefix_state.state_path(key);
                    let saved = prefix_state.ensure_dir().is_ok()
                        && session
                            .target_context_mut()
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
                    session.target_context_mut().clear_kv_cache(); // kv-phase: PrefixStateLearn
                }
            }
            PrefixPlan::Pass => {
                session.target_context_mut().clear_kv_cache(); // kv-phase: RequestStartReset
            }
        }

        let n_ctx = session.target_context_mut().n_ctx() as usize;
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
        let n_batch_max = session.target_context_mut().n_batch() as usize;
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
        session
            .target_context_mut()
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
        let mut last_token = sampler.sample(
            session.target_context(),
            prefill.n_tokens() - 1,
            SamplerRole::Explore,
        );
        sampler.accept(last_token);

        let mut output = String::new();
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        // Streaming bookkeeping. `finish_reason` defaults to Length —
        // the `while` condition exiting means max_tokens was hit; every
        // break overwrites it with the specific reason (same idiom as
        // stream_generate_loop). `receiver_gone` latches a failed send:
        // stop decoding, and never send a terminal frame afterwards.
        let mut receiver_gone = false;
        let mut finish_reason = FinishReason::Length;
        if let Ok(piece) = model.token_to_piece(last_token, &mut decoder, true, None) {
            output.push_str(&piece);
            if let Some(s) = sink {
                receiver_gone = !s.send_piece(&piece);
            }
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

        while !receiver_gone && n_generated < max_tokens {
            if model.is_eog_token(last_token) {
                finish_reason = FinishReason::Stop;
                break;
            }
            if cancel.is_some_and(|t| t.is_cancelled()) {
                tracing::warn!(
                    tokens_emitted = n_generated,
                    "mtp:cancelled via CancellationToken"
                );
                finish_reason = FinishReason::Cancelled;
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
                session.target_context_mut().clear_kv_cache(); // kv-phase: ErrorAbort
                session.draft_context_mut().clear_kv_cache(); // kv-phase: ErrorAbort
                return Err(Error::Inference(format!(
                    "MTP inference deadline exceeded after {elapsed}s ({n_generated} tokens)"
                )));
            }

            // Constraint-engine failure abort — see generate_sync for
            // rationale. Latched matcher failure means fail-closed
            // masks forever; draft/verify cycles can't recover it.
            if let Some(cause) = sampler.constraint_failure() {
                session.target_context_mut().clear_kv_cache(); // kv-phase: ErrorAbort
                session.draft_context_mut().clear_kv_cache(); // kv-phase: ErrorAbort
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
                    if let Some(s) = sink {
                        if !s.send_piece(&piece) {
                            receiver_gone = true;
                            break;
                        }
                    }
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
            // Consumer dropped mid-forced-run: skip the forced verify
            // round (its only purpose is to keep generating) and exit.
            if receiver_gone {
                break;
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
                session
                    .target_context_mut()
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
                let next_token =
                    sampler.sample(session.target_context(), k as i32, SamplerRole::Explore);
                sampler.accept(next_token);
                if !model.is_eog_token(next_token) {
                    let piece = model
                        .token_to_piece(next_token, &mut decoder, true, None)
                        .unwrap_or_default();
                    output.push_str(&piece);
                    if let Some(s) = sink {
                        receiver_gone = !s.send_piece(&piece);
                    }
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
                    finish_reason = FinishReason::Stop;
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
            // Strict: a refused rollback leaves draft()'s AR
            // pre-advancement resident, and the re-decode below would
            // then clash on M-RoPE positions instead of overwriting
            // them. `kv_ops` reads llama's refusal bool, which the
            // bare `.map_err()?` here could never see.
            kv_ops::truncate_strict(
                session.draft_context_mut(),
                0,
                n_past as usize,
                "MTP draft KV pre-verify rollback",
            )?;

            session
                .target_context_mut()
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
            let mut next_token = sampler.sample(session.target_context(), 0, SamplerRole::Explore);
            sampler.accept(next_token);
            for (i, draft) in drafts.iter().enumerate() {
                if next_token == *draft {
                    n_accepted = i + 1;
                    if (i + 1) < n_verify as usize {
                        next_token = sampler.sample(
                            session.target_context(),
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
                // Strict on BOTH: an un-rewound rejected suffix means
                // the next iteration samples against tokens the
                // verifier already threw away — silent divergence
                // rather than a loud failure.
                kv_ops::truncate_strict(
                    session.target_context_mut(),
                    0,
                    new_n_past as usize,
                    "MTP target KV rollback",
                )?;
                kv_ops::truncate_strict(
                    session.draft_context_mut(),
                    0,
                    new_n_past as usize,
                    "MTP draft KV rollback",
                )?;
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
                if let Some(s) = sink {
                    if !s.send_piece(&piece) {
                        receiver_gone = true;
                        break;
                    }
                }
                if model.is_eog_token(tok) {
                    hit_eos = true;
                    break;
                }
            }
            if hit_eos {
                finish_reason = FinishReason::Stop;
            }
            if receiver_gone || hit_eos || n_generated >= max_tokens {
                break;
            }
            let piece = model
                .token_to_piece(next_token, &mut decoder, true, None)
                .unwrap_or_default();
            output.push_str(&piece);
            n_generated += 1;
            if let Some(s) = sink {
                if !s.send_piece(&piece) {
                    receiver_gone = true;
                    break;
                }
            }
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
            streamed = sink.is_some(),
            receiver_gone,
            "mtp: end-of-generation"
        );

        if let Some(s) = sink {
            if !receiver_gone {
                s.send_finish(
                    finish_reason,
                    StreamUsage {
                        prompt_tokens: tokens.len() as u32,
                        completion_tokens: n_generated as u32,
                        total_tokens: (tokens.len() + n_generated) as u32,
                    },
                );
            }
        }

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
    /// Prefill `tokens` into `ctx`, reusing whatever of the prompt is
    /// already resident. THE one implementation of that protocol.
    ///
    /// Extracted 2026-08-06 because the two chat surfaces disagreed about
    /// whether prefix reuse existed at all. `generate_sync` carried this
    /// whole body inline; `generate_stream_sync_with_finish` — the path
    /// every streaming chat completion takes — took `&mut LlamaContext`
    /// rather than `&mut SlotContext` and so could not even see
    /// `cached_tokens` or `prefix_state`. It cleared the KV cache at request
    /// start and again at the end, and re-prefilled every token of every
    /// prompt, forever.
    ///
    /// Measured before the fix, same model and prompt, back to back:
    /// non-streaming 8554 ms cold then ~478 ms on a repeat (17.9x, with a
    /// novel-prefix control staying at 9607 ms to prove it was a cache and
    /// not warm-up); streaming flat at ~3.2 s forever, even with the pin
    /// already warm from the run minutes before.
    ///
    /// Copying this body to the second caller was the wrong fix (§10.6): a
    /// duplicated decider is how the forward budget, the privacy gate and
    /// the outcome join each ended up present on one routing surface and
    /// absent on the other. One body, two callers.
    ///
    /// NOTE the two mechanisms here are not interchangeable. The LCP
    /// partial keep is vetoed for every recurrent/hybrid model — which is
    /// the whole Qwen3.5/3.6 lineup, and for MTP slots as well — so on this
    /// fleet the win comes entirely from `prefix_state`, whose whole-state
    /// restore survives recurrent state. Anyone porting the FIM sibling
    /// instead would ship `lcp = 0` forever and measure no change.
    #[allow(clippy::too_many_arguments)]
    fn prefill_reusing_prefix(
        model: &LlamaModel,
        model_id: &str,
        request: &CompletionRequest,
        full_prompt: &str,
        tokens: &[LlamaToken],
        n_batch: usize,
        prefix_cache_safe: bool,
        ctx: &mut crate::llama::cpp::context::LlamaContext<'_>,
        cached_tokens: &mut Vec<LlamaToken>,
        prefix_state: &mut PrefixStateCache,
    ) -> Result<()> {
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
                // A recurrent/hybrid memory module REFUSES a partial
                // range removal (returns false) rather than erroring,
                // so the outcome has to be read, not assumed — see
                // `kv_ops`. The returned length is the CORRECTED lcp:
                // 0 after a fallback full clear, so the tail below
                // cannot decode at [lcp, …) into an empty cache.
                lcp = kv_ops::truncate_or_full_clear(
                    ctx,
                    0,
                    lcp, // kv-phase: PrefixCacheSetup
                    "prefix_cache",
                );
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
        *cached_tokens = tokens.to_vec();
        Ok(())
    }

    /// **Takes `&mut SlotContext`, not `&mut LlamaContext`** — that signature
    /// is the entire fix (2026-08-06). Every streaming chat completion comes
    /// through here, and while it held only the raw context it could not see
    /// `cached_tokens` or `prefix_state`, so it cleared the KV cache at request
    /// start, re-prefilled every token of every prompt, and cleared again at the
    /// end. Measured on the 35B: 8554 ms cold and ~478 ms on a repeat via the
    /// non-streaming path (17.9x), against a flat ~3.2 s forever here — with the
    /// pin already warm from minutes earlier.
    ///
    /// Why this mattered beyond one turn: at product scale the holder serializes,
    /// nine concurrent originators produced nine ~8 s slots (the ninth answering
    /// after 74 s), and **~99% of each slot was prefill**. Reusing a prefix does
    /// not shave a turn, it shrinks the unit the queue is made of.
    ///
    /// End-of-generation does NOT clear the KV cache any more: positions
    /// `[0, prompt_len)` must stay resident for the next request's
    /// reconciliation. That is the same contract `generate_sync` has always had,
    /// and it is why the `ffi_trace` module doc no longer claims stream paths
    /// never populate `cached_tokens`.
    pub(crate) fn generate_stream_sync_with_finish(
        model: &LlamaModel,
        model_id: &str,
        slot_ctx: &mut SlotContext,
        request: &CompletionRequest,
        tx: &tokio::sync::mpsc::Sender<StreamFrame>,
        quirks: &ModelQuirks,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<()> {
        // Same gate and same split-borrow idiom as `generate_sync`: direct field
        // access, because going through `ctx_mut()` would borrow the whole
        // SlotContext and put `cached_tokens` out of reach.
        let gate = prefix_cache_gate(
            &slot_ctx.capabilities,
            quirks.has_recurrent_layers,
            slot_ctx.is_speculative(),
            |k| std::env::var(k).ok(),
        );
        let prefix_cache_safe = gate.safe;
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
        let max_tokens = clamp_max_tokens(request.max_tokens, tokens.len(), n_ctx)?;
        let prompt_tokens = tokens.len();

        Self::prefill_reusing_prefix(
            model,
            model_id,
            request,
            &full_prompt,
            &tokens,
            n_batch,
            prefix_cache_safe,
            ctx,
            cached_tokens,
            prefix_state,
        )?;

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
            // FALSE now, and that is load-bearing: an end-of-generation clear
            // here would silently desync from the `cached_tokens` this path now
            // populates — the exact 2026-05 incident the phase discipline was
            // built for (garbage on every replay-with-a-similar-prompt).
            clear_kv_at_end: false,
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
            &slot_ctx.capabilities,
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
            effective: mut lcp,
        } = compute_lcp(cached_tokens, &tokens, prefix_cache_safe);
        // Clear BEFORE any cache mutation — restored on decode success
        // below; a mid-decode crash forces a defensive full clear next call.
        cached_tokens.clear();
        if lcp == 0 {
            ctx.clear_kv_cache(); // kv-phase: FimPrefixCacheSetup
        } else {
            // Same refusal contract as the chat path — see `kv_ops`.
            // The returned length is the corrected lcp.
            lcp = kv_ops::truncate_or_full_clear(
                ctx,
                0,
                lcp, // kv-phase: FimPrefixCacheSetup
                "fim_prefix_cache",
            );
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

    /// Streaming front door — the stream twin of [`Self::generate_sync`]'s
    /// MTP dispatch. Routes an eligible request (speculative slot, no
    /// tools, no forced-choice sentinel, `SOVEREIGN_MTP_DISABLE` unset)
    /// through the speculative loop with per-accepted-piece emission;
    /// everything else takes the single-token stream loop for `sink`'s
    /// channel type.
    ///
    /// Quarantine mirrors `generate_sync`: on the prefill-error set
    /// (`gates::mtp_error_is_prefill`) the slot demotes to SingleToken
    /// and the request re-runs on the single-token stream. That
    /// fall-through is wire-safe ONLY because every quarantining error
    /// fires before the first frame is emitted — the predicate's doc
    /// comment pins that invariant. All other errors propagate; the
    /// engine call site turns them into a terminal `Error` frame.
    ///
    /// Until 2026-07-31 the streaming entries called
    /// `generate_stream_sync*` directly, so a streamed request could
    /// never ride MTP — every streamed benchmark of an MTP model was
    /// silently measuring single-token decode (note cd938d47).
    pub(crate) fn generate_stream_dispatch(
        model: &LlamaModel,
        model_id: &str,
        slot_ctx: &mut SlotContext,
        request: &CompletionRequest,
        sink: StreamSink<'_>,
        quirks: &ModelQuirks,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<()> {
        if mtp_dispatch_eligible(request, slot_ctx.is_speculative(), |k| {
            std::env::var(k).ok()
        }) {
            match Self::generate_stream_sync_mtp(
                model, model_id, slot_ctx, request, quirks, &sink, cancel,
            ) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    let msg = e.to_string();
                    let quarantine_disabled = std::env::var("SOVEREIGN_MTP_QUARANTINE_DISABLE")
                        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                        .unwrap_or(false);
                    if mtp_error_is_prefill(&msg) && !quarantine_disabled {
                        tracing::warn!(
                            model_id = %model_id,
                            error = %msg,
                            "MTP prefill failed on the streaming path — quarantining MTP \
                             on this slot and falling through to single-token streaming. \
                             Restart the daemon to re-enable; set \
                             SOVEREIGN_MTP_QUARANTINE_DISABLE=1 to surface the error \
                             instead."
                        );
                        slot_ctx.demote_to_single_token(model_id)?;
                        // Fall through — zero frames emitted (the
                        // quarantine set is prefill-only), so the
                        // single-token rerun starts a clean stream.
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        match sink {
            // `slot_ctx`, NOT `slot_ctx.ctx_mut()` — the downgrade to a raw
            // context is what kept the chat path from reaching the prefix cache.
            StreamSink::Typed(tx) => Self::generate_stream_sync_with_finish(
                model, model_id, slot_ctx, request, tx, quirks, cancel,
            ),
            StreamSink::Legacy(tx) => Self::generate_stream_sync(
                model,
                model_id,
                slot_ctx.ctx_mut(),
                request,
                tx,
                quirks,
                cancel,
            ),
        }
    }
}

/// Channel adapter over the two streaming surfaces, so the MTP loop is
/// written once against both. The legacy `Result<String>` channel has
/// no terminal-frame vocabulary (consumers treat channel-close as
/// end-of-stream), so `send_finish` is a typed-only concern.
pub(crate) enum StreamSink<'a> {
    /// `StreamFrame` channel — the `complete_stream_with_finish` path.
    Typed(&'a tokio::sync::mpsc::Sender<StreamFrame>),
    /// Legacy `Result<String>` channel — the `complete_stream` path.
    Legacy(&'a tokio::sync::mpsc::Sender<Result<String>>),
}

impl StreamSink<'_> {
    /// Forward one decoded piece. Returns false when the receiver is
    /// gone (consumer dropped the stream) — the generation loop must
    /// stop decoding and must NOT send a terminal frame afterwards.
    /// Empty pieces are skipped: the incremental UTF-8 decoder yields
    /// "" mid-codepoint, and both client-side parsers drop empty
    /// deltas anyway.
    fn send_piece(&self, piece: &str) -> bool {
        if piece.is_empty() {
            return true;
        }
        match self {
            Self::Typed(tx) => tx
                .blocking_send(StreamFrame::Token(piece.to_string()))
                .is_ok(),
            Self::Legacy(tx) => tx.blocking_send(Ok(piece.to_string())).is_ok(),
        }
    }

    /// Terminal frame with the real finish reason + usage. No-op on
    /// the legacy channel.
    fn send_finish(&self, reason: FinishReason, usage: StreamUsage) {
        if let Self::Typed(tx) = self {
            let _ = tx.blocking_send(StreamFrame::Finish {
                reason,
                usage: Some(usage),
            });
        }
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
mod queue_gauge_tests {
    //! Validating the INSTRUMENT and the BOUND (ARCH_PRINCIPLES §18.4).
    //! These exist because the depth gauge sizes MESH_N4_TOPOLOGY's M5
    //! bound, and a counter that drifts would send that decision
    //! confidently wrong.
    use super::{acquire_with_queue_gauge, SlotQueue};
    use sovereign_core::Error;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    /// A queue with the shed bound pinned, so no test depends on ambient env.
    fn queue(seed_ms: u64, max_wait_ms: u64) -> Arc<SlotQueue> {
        Arc::new(SlotQueue::new("slot-a", seed_ms).with_max_wait_ms(max_wait_ms))
    }

    /// Acquire, expecting a SHED — bounded so a regression FAILS instead of
    /// hanging.
    ///
    /// If the bound stops working, the caller parks behind a permit these
    /// tests deliberately never release, and an unbounded `.await` would hang
    /// forever. Verified 2026-08-06 by disabling the shed check: the test
    /// suite wedged with no output rather than reporting. A gate that hangs
    /// on the failure it exists to catch is worse than no gate.
    async fn expect_shed(q: &Arc<SlotQueue>, phase: &'static str) -> Error {
        match tokio::time::timeout(
            std::time::Duration::from_secs(3),
            acquire_with_queue_gauge(q, phase),
        )
        .await
        {
            Ok(Ok(_permit)) => panic!("{phase}: expected a shed, got a permit"),
            Ok(Err(e)) => e,
            Err(_) => panic!(
                "{phase}: expected an IMMEDIATE shed but the caller parked — \
                 the predicted-wait bound is not firing"
            ),
        }
    }

    /// Spin (yielding) until `cond` holds, or FAIL after a deadline.
    ///
    /// Bounded deliberately: with the drop guard sabotaged, an unbounded
    /// `while gauge != 0 { yield }` hangs forever instead of asserting —
    /// verified 2026-08-06. A gate that hangs on the failure it was written
    /// to catch is a worse gate than none: CI reports a timeout with no
    /// message rather than the leak.
    async fn wait_until(mut cond: impl FnMut() -> bool, what: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !cond() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn uncontended_acquire_never_touches_the_gauge() {
        let q = queue(1_000, 30_000);
        let permit = acquire_with_queue_gauge(&q, "test")
            .await
            .expect("free permit must be granted");
        // The fast path is the common case on an idle host: it must not
        // register as a queued caller, or every idle request would inflate
        // the depth M5 is sized against.
        assert_eq!(q.depth(), 0);
        drop(permit);
        assert_eq!(q.depth(), 0);
    }

    #[tokio::test]
    async fn contended_acquire_reports_depth_then_returns_to_zero() {
        // Bound disabled so this test measures the GAUGE, not the shed.
        let q = queue(1_000, 0);
        let held = acquire_with_queue_gauge(&q, "holder")
            .await
            .expect("first caller takes the permit");

        let mut waiters = Vec::new();
        for _ in 0..2 {
            let q = Arc::clone(&q);
            waiters.push(tokio::spawn(async move {
                acquire_with_queue_gauge(&q, "waiter")
                    .await
                    .map(|p| drop(p))
            }));
        }

        wait_until(|| q.depth() >= 2, "both waiters to park").await;
        assert_eq!(
            q.depth(),
            2,
            "both parked callers must be visible as queue depth — this is \
             the number that was invisible before the accessor existed"
        );

        drop(held);
        for w in waiters {
            w.await.expect("waiter task").expect("waiter got a permit");
        }
        assert_eq!(
            q.depth(),
            0,
            "gauge must return to zero once the queue drains"
        );
    }

    #[tokio::test]
    async fn cancelled_waiter_does_not_leak_the_gauge() {
        // THE REGRESSION FENCE for `QueuedGuard`. A client that hangs up
        // mid-wait (dropped SSE connection, cancelled chat turn) drops the
        // acquiring future between the increment and the await's completion.
        // Without the drop guard the counter ratchets up and never comes
        // down, so a host that had merely seen some disconnects would report
        // a permanently deep queue — and would then SHED on the strength of
        // a fiction, refusing work it could serve.
        let q = queue(1_000, 0);
        let held = acquire_with_queue_gauge(&q, "holder")
            .await
            .expect("first caller takes the permit");

        let qc = Arc::clone(&q);
        let waiter = tokio::spawn(async move {
            let _ = acquire_with_queue_gauge(&qc, "doomed").await;
        });
        wait_until(|| q.depth() >= 1, "the waiter to park").await;

        waiter.abort();
        let _ = waiter.await;

        wait_until(
            || q.depth() == 0,
            "the cancelled waiter to release its gauge slot",
        )
        .await;
        assert_eq!(
            q.depth(),
            0,
            "a cancelled waiter must release its gauge slot"
        );
        drop(held);
    }

    #[tokio::test]
    async fn predicted_wait_over_the_bound_sheds_without_parking() {
        // Seed 10 s/turn against a 5 s bound: the first waiter's predicted
        // wait is 10 s, so it must be refused rather than parked.
        let q = queue(10_000, 5_000);
        let _held = acquire_with_queue_gauge(&q, "holder")
            .await
            .expect("first caller takes the permit");

        let err = expect_shed(&q, "complete_stream_with_finish/lazy").await;

        match err {
            Error::QueueShed {
                position,
                predicted_wait_ms,
                retry_after_secs,
            } => {
                assert_eq!(position, 1, "shed caller was next in line");
                assert_eq!(predicted_wait_ms, 10_000);
                assert_eq!(retry_after_secs, 10, "Retry-After hints the real wait");
            }
            other => panic!("expected a structured QueueShed, got: {other:?}"),
        }
        // Shedding must happen BEFORE parking — a caller that pays the wait
        // AND gets refused is the worst of both worlds.
        assert_eq!(q.depth(), 0, "a shed caller must never have parked");
    }

    #[tokio::test]
    async fn zero_bound_never_sheds() {
        // `0` is the escape hatch back to the pre-M5 behaviour every
        // deployment ran before the bound existed. A wildly over-budget
        // predicted wait must still park.
        let q = queue(3_600_000, 0);
        let held = acquire_with_queue_gauge(&q, "holder")
            .await
            .expect("first caller takes the permit");

        let qc = Arc::clone(&q);
        let waiter = tokio::spawn(async move { acquire_with_queue_gauge(&qc, "waiter").await });
        wait_until(
            || q.depth() >= 1,
            "the waiter to park despite a 1 h estimate",
        )
        .await;
        drop(held);
        assert!(
            waiter.await.expect("task").is_ok(),
            "must be served, not shed"
        );
    }

    #[tokio::test]
    async fn the_bound_adapts_to_observed_turn_duration() {
        // THE LOAD-BEARING TEST. The whole reason M5 bounds on predicted
        // WAIT rather than on depth is that the same depth cost 6.2 s in one
        // measured shape and 90.7 s in another. That only works if the
        // estimate tracks reality, so: identical depth, identical bound —
        // and the verdict must flip purely on observed turn duration.
        let q = queue(100, 5_000);

        // Depth 1 at a 100 ms estimate: comfortably admitted.
        let held = acquire_with_queue_gauge(&q, "holder")
            .await
            .expect("holder");
        let qc = Arc::clone(&q);
        let waiter = tokio::spawn(async move { acquire_with_queue_gauge(&qc, "fast-turn").await });
        wait_until(|| q.depth() >= 1, "the cheap waiter to park").await;
        drop(held);
        let first = waiter.await.expect("task").expect("cheap turn is admitted");
        drop(first);

        // Now teach the queue that turns are expensive. Same call, same
        // depth, same bound — only the observed duration differs.
        for _ in 0..8 {
            q.record_turn(60_000);
        }

        let _held2 = acquire_with_queue_gauge(&q, "holder")
            .await
            .expect("holder");
        let err = expect_shed(&q, "slow-turn").await;
        assert!(
            matches!(err, Error::QueueShed { .. }),
            "expected QueueShed, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn closed_semaphore_names_the_phase_in_the_error() {
        // A torn-down slot must produce an error that says WHICH entry
        // point died — a bare "permit closed" left the caller guessing
        // across thirteen call sites.
        let q = queue(1_000, 0);
        let _held = acquire_with_queue_gauge(&q, "holder")
            .await
            .expect("first caller takes the permit");

        let qc = Arc::clone(&q);
        let waiter =
            tokio::spawn(
                async move { acquire_with_queue_gauge(&qc, "complete_stream/fast").await },
            );
        wait_until(|| q.depth() >= 1, "the waiter to park").await;
        q.close_for_test();

        let err = waiter
            .await
            .expect("waiter task")
            .expect_err("closed semaphore must surface as an error");
        let msg = err.to_string();
        assert!(
            msg.contains("complete_stream/fast"),
            "error must name the phase, got: {msg}"
        );
        assert_eq!(q.depth(), 0);
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
