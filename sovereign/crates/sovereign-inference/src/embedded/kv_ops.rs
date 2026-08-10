// SPDX-License-Identifier: AGPL-3.0-or-later
//! One decider for PARTIAL KV-cache truncation — "drop positions
//! `[keep, ∞)` from sequence `seq`, and tell me whether the memory
//! module actually did it".
//!
//! **Why this module exists (2026-08-10 gate-repro triage).**
//! [`crate::llama::cpp::context::LlamaContext::clear_kv_cache_seq`]
//! returns `Result<bool, KvCacheConversionError>`. The `Err` arm is
//! ONLY an integer-conversion overflow; llama.cpp's own verdict —
//! `llama_memory_seq_rm`, which returns **false** when the memory
//! module cannot honour a *partial* range removal — rides in the `Ok`
//! payload. Every one of the nine call sites dropped that bool (`let _
//! =` or `if let Err(e)`), so:
//!
//!   * The recovery already written at the prefix-cache site —
//!     "partial clear failed — falling back to full clear" — could
//!     never fire. It is reachable only via the impossible `Err`.
//!   * A refusal instead surfaced downstream as
//!     `Decode Error -1: n_tokens == 0`, which is doubly misleading:
//!     the batch was NOT empty (13 tokens), and the real llama.cpp
//!     diagnostic is an M-RoPE position check —
//!     `the last position stored in the memory module for sequence 0
//!     is X = 209 / the tokens ... have a starting position of Y = 189
//!     / for M-RoPE, it is required that the position satisfies X < Y`.
//!     Positions never rewound *because the removal was refused*.
//!
//! Measured on this host with a differential control (release build,
//! `SOVEREIGN_PREFIX_CACHE_FORCE=1`, `SOVEREIGN_PREFIX_STATE=0`):
//!
//! | model | arch | `llama_memory_seq_rm` |
//! |---|---|---|
//! | `Qwen3.5-2B.Q6_K` | `qwen35` (hybrid) | **false** → decode -1 |
//! | `FINAL-Bench_Darwin-36B-Opus` | `qwen35moe` (hybrid) | **false** → decode -1 |
//! | `gemma-4-E4B-it` | `gemma4` (attention) | **true** → succeeds |
//!
//! **The second bug in the same three lines.** The old recovery
//! full-cleared but left `lcp` unchanged, so the tail decode would
//! still have started at position `lcp` against an empty cache — a
//! hole at `[0, lcp)`. That is why [`SeqTruncate::surviving_prefix`]
//! exists and why [`truncate_or_full_clear`] returns it: the return
//! value IS the caller's corrected `lcp`, so honouring it is the
//! natural way to use the function and ignoring it is visibly wrong.
//!
//! **Relationship to `gates::prefix_cache_gate`.** The gate refuses
//! partial keep on recurrent/hybrid architectures up front, so in
//! normal operation nothing here should ever see a refusal. This
//! module is the second line: it makes a refusal *survivable and
//! visible* rather than a hard decode error, which matters for the
//! arches the gate's string ladder has not met yet — the model zoo
//! grows faster than the ladder does. Belt and braces, not a
//! replacement: [`refusals`] going non-zero in production means an
//! arch slipped the gate and should be added to it.
//!
//! Decision logic ([`SeqTruncate::surviving_prefix`]) is pure and
//! unit-tested weight-free below; the context IO stays in the policy
//! functions.

use std::sync::atomic::{AtomicU64, Ordering};

use sovereign_core::error::Error;

use crate::llama::cpp::context::LlamaContext;

/// Process-wide count of partial truncations llama.cpp REFUSED.
///
/// Non-zero in production is a signal, not noise: it means a model
/// reached the partial-keep path that `gates::prefix_cache_gate`
/// should have vetoed. `gate_repros.rs` reads it to tell "the hazard
/// is present and we degraded" apart from "upstream fixed partial
/// keep" — two outcomes that otherwise both look like success.
static REFUSALS: AtomicU64 = AtomicU64::new(0);

/// How many partial truncations have been refused by the memory
/// module in this process.
#[must_use]
pub fn refusals() -> u64 {
    REFUSALS.load(Ordering::Relaxed)
}

/// Reset the refusal counter. Test-support only — production code
/// reads [`refusals`] and never resets it.
pub fn reset_refusals() {
    REFUSALS.store(0, Ordering::Relaxed);
}

/// What the memory module did with a partial-truncation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a truncation outcome that is dropped re-introduces the \
              position desync this module exists to prevent"]
pub(crate) enum SeqTruncate {
    /// Positions `[keep, ∞)` were removed. The prefix `[0, keep)` is
    /// resident and the caller may decode from position `keep`.
    Applied,
    /// `llama_memory_seq_rm` returned false — the memory module
    /// cannot honour a PARTIAL range removal. Recurrent and hybrid
    /// (Gated DeltaNet) memory modules refuse; pure-attention ones
    /// do not. NOTHING was removed.
    Refused,
    /// The seq id or position did not fit in `i32`. Also "nothing was
    /// removed", but from our side of the FFI rather than llama's.
    InvalidRange,
}

impl SeqTruncate {
    /// How many prefix tokens are actually resident after this
    /// outcome, given the `keep` that was asked for — `keep` when the
    /// truncation applied, `0` otherwise (the caller full-clears and
    /// re-prefills from scratch).
    ///
    /// This is the whole reason the outcome is returned rather than
    /// logged: a caller that keeps using its original `keep` after a
    /// refusal decodes a tail at positions `[keep, …)` with an EMPTY
    /// cache underneath, which is a different corruption from the one
    /// the refusal caused.
    pub(crate) fn surviving_prefix(self, keep: usize) -> usize {
        match self {
            Self::Applied => keep,
            Self::Refused | Self::InvalidRange => 0,
        }
    }

    /// True when nothing was removed, for whatever reason.
    pub(crate) fn is_refusal(self) -> bool {
        !matches!(self, Self::Applied)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Refused => "refused-by-memory-module",
            Self::InvalidRange => "invalid-range",
        }
    }
}

/// The single FFI call site. Every policy below routes through here so
/// the `Ok(false)` arm cannot be forgotten again, and so the refusal
/// counter has exactly one writer.
fn truncate(ctx: &mut LlamaContext<'_>, seq: u32, keep: u32) -> SeqTruncate {
    let outcome = match ctx.clear_kv_cache_seq(Some(seq), Some(keep), None) {
        Ok(true) => SeqTruncate::Applied,
        Ok(false) => SeqTruncate::Refused,
        Err(_) => SeqTruncate::InvalidRange,
    };
    if outcome.is_refusal() {
        REFUSALS.fetch_add(1, Ordering::Relaxed);
    }
    outcome
}

/// **Policy: degrade.** Try to keep `[0, keep)`; if the memory module
/// refuses, full-clear and tell the caller the prefix is gone.
///
/// Returns the number of prefix tokens still resident — `keep` on
/// success, `0` after a fallback full clear. **Use the return value as
/// your `lcp`**; see [`SeqTruncate::surviving_prefix`].
///
/// For the prefix-cache paths, where losing the prefix costs a full
/// prefill but is otherwise correct.
#[must_use = "the return value is the corrected prefix length — ignoring \
              it decodes a tail against a cleared cache"]
pub(crate) fn truncate_or_full_clear(
    ctx: &mut LlamaContext<'_>,
    seq: u32,
    keep: usize,
    site: &'static str,
) -> usize {
    let outcome = truncate(ctx, seq, keep as u32);
    if outcome.is_refusal() {
        tracing::warn!(
            site,
            seq,
            requested_keep = keep,
            outcome = outcome.label(),
            "kv: partial truncation refused — full clear, full prefill. \
             The arch reached partial keep despite prefix_cache_gate; \
             it likely needs adding to the gate's recurrent ladder."
        );
        ctx.clear_kv_cache();
    }
    outcome.surviving_prefix(keep)
}

/// **Policy: fail.** For call sites where losing the truncation is a
/// correctness bug rather than a performance one — the MTP draft/verify
/// rollbacks, where an un-rewound suffix means the next decode samples
/// against tokens the verifier already rejected.
pub(crate) fn truncate_strict(
    ctx: &mut LlamaContext<'_>,
    seq: u32,
    keep: usize,
    site: &'static str,
) -> Result<(), Error> {
    let outcome = truncate(ctx, seq, keep as u32);
    if outcome.is_refusal() {
        return Err(Error::Inference(format!(
            "{site}: KV rollback to {keep} refused by the memory module \
             ({}) — cannot continue without re-sampling against stale state",
            outcome.label()
        )));
    }
    Ok(())
}

/// **Policy: report.** For call sites that already have a downstream
/// fallback for a bad cache state and only need the refusal to stop
/// being invisible.
pub(crate) fn truncate_best_effort(
    ctx: &mut LlamaContext<'_>,
    seq: u32,
    keep: usize,
    site: &'static str,
) {
    let outcome = truncate(ctx, seq, keep as u32);
    if outcome.is_refusal() {
        tracing::warn!(
            site,
            seq,
            requested_keep = keep,
            outcome = outcome.label(),
            "kv: partial truncation refused — stale positions remain \
             resident on this sequence; downstream results may be \
             computed against a polluted cache"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applied_keeps_the_requested_prefix() {
        assert_eq!(SeqTruncate::Applied.surviving_prefix(189), 189);
        assert!(!SeqTruncate::Applied.is_refusal());
    }

    #[test]
    fn refusal_collapses_the_prefix_to_zero() {
        // THE bug this module exists for: after a refusal the caller
        // must decode from 0, not from the `keep` it asked for.
        assert_eq!(SeqTruncate::Refused.surviving_prefix(189), 0);
        assert!(SeqTruncate::Refused.is_refusal());
    }

    #[test]
    fn invalid_range_is_also_a_refusal_not_a_success() {
        assert_eq!(SeqTruncate::InvalidRange.surviving_prefix(4096), 0);
        assert!(SeqTruncate::InvalidRange.is_refusal());
    }

    #[test]
    fn surviving_prefix_of_zero_keep_is_zero_either_way() {
        assert_eq!(SeqTruncate::Applied.surviving_prefix(0), 0);
        assert_eq!(SeqTruncate::Refused.surviving_prefix(0), 0);
    }

    #[test]
    fn counter_accessor_round_trips() {
        reset_refusals();
        assert_eq!(refusals(), 0);
        REFUSALS.fetch_add(1, Ordering::Relaxed);
        assert_eq!(refusals(), 1);
        reset_refusals();
        assert_eq!(refusals(), 0);
    }
}
