// SPDX-License-Identifier: AGPL-3.0-or-later
//! FFI sequence invariants — the executable form of the inference
//! layer's load-bearing call-order rules.
//!
//! `model_slot.rs` is unsafe-heavy FFI where *call order* is
//! correctness: several past incidents were sequencing bugs that
//! presented as model failures (garbage tokens, silent quality loss),
//! and the rules that prevent them lived only in memory notes and the
//! original author's head. This module makes them three things:
//!
//! 1. **An enum** (`ClearKvPhase`) every `clear_kv_cache()` call site
//!    must name in a trailing `// kv-phase: <variant>` marker —
//!    enforced by the source-discipline test below. Adding a clear
//!    without declaring WHY it is legal breaks the build's test run,
//!    with the incident story in the failure message.
//! 2. **A transcript recorder** (`record` / `enable` / `take`) the
//!    slot code calls at the load-bearing FFI boundaries. Zero-cost
//!    when disabled (one relaxed atomic load).
//! 3. **Pure verifiers** over a transcript (`verify_transcript`),
//!    unit-tested here on synthetic transcripts. HONESTY NOTE: the
//!    recorder is wired at the FFI boundaries, but `enable()` is not
//!    called on any production or model-smoke path today — so the
//!    verifiers run ONLY against the hand-built transcripts in this
//!    module's `#[cfg(test)]` block, never against a real decode. They
//!    are *ready* for a gated model-smoke harness (a tiny GGUF on a dev
//!    box; these sequences cannot be CI-verified without weights) but
//!    that harness does not exist yet. Do not read the `record()` calls
//!    in `model_slot.rs` as active runtime verification.
//!
//! ## The invariants and their incidents
//!
//! - **KV clear is phase-disciplined.** An end-of-generation
//!   `clear_kv_cache()` on the prefix-cached sync path silently
//!   desynced from `cached_tokens`, producing garbage on every
//!   replay-with-a-similar-prompt (2026-05, ARCH §1.3). The clear
//!   must defer to the next request's prefix-cache reconciliation.
//!   The stream paths DO clear at end of generation — legal only
//!   because they never populate `cached_tokens`; that's what
//!   `EndOfGenerationNoPrefixCache` declares.
//! - **MTP session is rebuilt per request**, and prefill order is
//!   `decode → session.process → session.begin`, with `process`
//!   after EVERY verify decode. Violations decode garbage at 32
//!   tok/s (2026-05 Strix Halo bring-up).
//! - **`set_embeddings(true)` precedes EVERY embed decode.** The
//!   constructor flag does not stick; a missing per-decode call
//!   leaves `embd.data` null and every read path errors
//!   (llama-cpp-4 0.2.x INVARIANT, 2026-05).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Why a `clear_kv_cache()` call is legal. Every call site in
/// `model_slot.rs` names one of these in a trailing
/// `// kv-phase: <variant>` marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearKvPhase {
    /// Next request's prefix-cache reconciliation (`cached_tokens`
    /// cleared in the same scope). The ONLY sanctioned routine clear
    /// on the prefix-cached sync path.
    PrefixCacheSetup,
    /// Unconditional reset at request start on a path that does not
    /// integrate the prefix cache (MTP phase A, legacy stream entry).
    /// Must clear `cached_tokens` too if the path shares the slot.
    RequestStartReset,
    /// End-of-generation clear on a path that NEVER populates
    /// `cached_tokens`. FORBIDDEN on the prefix-cached sync path —
    /// that exact pattern caused the 2026-05 KV/cached_tokens desync
    /// (garbage on replay-with-similar-prompt).
    EndOfGenerationNoPrefixCache,
    /// Defensive cleanup on an error exit (deadline, decode failure,
    /// constraint failure). Pairs with a returned `Err`.
    ErrorAbort,
    /// Diagnostic / capability probe outside the request path.
    Probe,
}

/// One load-bearing FFI call, as recorded by the slot code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiCall {
    /// `MtpSession::new` — must happen once per MTP request.
    MtpSessionBuilt,
    /// `session.begin(...)` after prefill.
    SessionBegin,
    /// `session.process(...)` — required after every MTP decode.
    SessionProcess,
    /// A `decode()` on the MTP target context (prefill or verify).
    MtpDecode,
    /// `set_embeddings(true)` on the embed context.
    SetEmbeddings,
    /// A `decode()` on the embed context.
    EmbedDecode,
    /// `clear_kv_cache()` with its declared phase.
    ClearKv(ClearKvPhase),
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static TRANSCRIPT: Mutex<Vec<FfiCall>> = Mutex::new(Vec::new());

/// Record one call. No-op (one relaxed atomic load) unless `enable()`
/// was called — production decode pays nothing.
#[inline]
pub fn record(call: FfiCall) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    TRANSCRIPT.lock().unwrap_or_else(|e| e.into_inner()).push(call);
}

/// Start recording (model-smoke tier only).
pub fn enable() {
    TRANSCRIPT.lock().unwrap_or_else(|e| e.into_inner()).clear();
    ENABLED.store(true, Ordering::Relaxed);
}

/// Stop recording and take the transcript.
pub fn take() -> Vec<FfiCall> {
    ENABLED.store(false, Ordering::Relaxed);
    std::mem::take(&mut TRANSCRIPT.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Run every sequence verifier over one request's transcript.
/// The model-smoke tier calls this after each real request.
pub fn verify_transcript(t: &[FfiCall]) -> Result<(), Vec<String>> {
    let mut violations = Vec::new();
    if let Err(e) = verify_embed_pairing(t) {
        violations.push(e);
    }
    if let Err(e) = verify_mtp_sequence(t) {
        violations.push(e);
    }
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

/// Every `EmbedDecode` must be immediately preceded by
/// `SetEmbeddings` (no other embed-context call between them).
pub fn verify_embed_pairing(t: &[FfiCall]) -> Result<(), String> {
    let mut armed = false;
    for (i, call) in t.iter().enumerate() {
        match call {
            FfiCall::SetEmbeddings => armed = true,
            FfiCall::EmbedDecode => {
                if !armed {
                    return Err(format!(
                        "EmbedDecode at transcript[{i}] without a preceding SetEmbeddings: \
                         this re-introduces the llama-cpp-4 0.2.x invariant violation — \
                         with_embeddings(true) at construction does NOT stick; embd.data \
                         ends up null and every read path (seq_ith/ith/get_embeddings) \
                         errors. Call ctx.set_embeddings(true) before EVERY embed decode \
                         (see memory: project_embed_set_embeddings_per_decode)."
                    ));
                }
                armed = false; // each decode consumes its arming
            }
            _ => {}
        }
    }
    Ok(())
}

/// MTP request discipline: session built before any begin/process;
/// prefill order decode → process → begin; every MtpDecode followed
/// by SessionProcess before the next MtpDecode.
pub fn verify_mtp_sequence(t: &[FfiCall]) -> Result<(), String> {
    let mtp_used = t.iter().any(|c| {
        matches!(
            c,
            FfiCall::MtpDecode | FfiCall::SessionBegin | FfiCall::SessionProcess
        )
    });
    if !mtp_used {
        return Ok(());
    }
    let built_at = t.iter().position(|c| *c == FfiCall::MtpSessionBuilt);
    let first_use = t
        .iter()
        .position(|c| matches!(c, FfiCall::SessionBegin | FfiCall::SessionProcess))
        .unwrap_or(usize::MAX);
    match built_at {
        None => {
            return Err(
                "MTP calls present but no MtpSessionBuilt in this request's transcript: \
                 the MTP session MUST be rebuilt per request in a long-running daemon — \
                 reusing a prior request's session decodes garbage (see memory: \
                 project_mtp_invariants, validated 2026-05 Strix Halo)."
                    .to_string(),
            );
        }
        Some(b) if b > first_use => {
            return Err(format!(
                "SessionBegin/Process at transcript[{first_use}] before MtpSessionBuilt \
                 at [{b}]: session must exist before use (rebuild-per-request order)."
            ));
        }
        _ => {}
    }
    // Every MtpDecode must see a SessionProcess before the next MtpDecode.
    let mut pending_decode: Option<usize> = None;
    for (i, call) in t.iter().enumerate() {
        match call {
            FfiCall::MtpDecode => {
                if let Some(p) = pending_decode {
                    return Err(format!(
                        "MtpDecode at transcript[{i}] while the decode at [{p}] has no \
                         SessionProcess yet: `session.process` must run after EVERY \
                         decode on the MTP path or the draft head reads a stale hidden \
                         state and acceptance collapses (project_mtp_invariants)."
                    ));
                }
                pending_decode = Some(i);
            }
            FfiCall::SessionProcess => pending_decode = None,
            _ => {}
        }
    }
    if let Some(p) = pending_decode {
        return Err(format!(
            "transcript ends with MtpDecode at [{p}] un-processed: `session.process` \
             must follow every MTP decode (project_mtp_invariants)."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ClearKvPhase as P;
    use FfiCall as C;

    // ── Verifier unit tests (synthetic transcripts) ────────────────

    #[test]
    fn embed_pairing_accepts_disciplined_sequence() {
        let t = [C::SetEmbeddings, C::EmbedDecode, C::SetEmbeddings, C::EmbedDecode];
        assert!(verify_embed_pairing(&t).is_ok());
    }

    #[test]
    fn embed_pairing_rejects_unarmed_decode() {
        // Second decode reuses the first arming — the exact
        // "constructor flag doesn't stick" shape.
        let t = [C::SetEmbeddings, C::EmbedDecode, C::EmbedDecode];
        let err = verify_embed_pairing(&t).unwrap_err();
        assert!(err.contains("set_embeddings(true) before EVERY embed decode"));
    }

    #[test]
    fn mtp_accepts_disciplined_request() {
        let t = [
            C::ClearKv(P::RequestStartReset),
            C::MtpSessionBuilt,
            C::MtpDecode, // prefill
            C::SessionProcess,
            C::SessionBegin,
            C::MtpDecode, // verify round 1
            C::SessionProcess,
            C::MtpDecode, // verify round 2
            C::SessionProcess,
        ];
        assert!(verify_mtp_sequence(&t).is_ok());
        assert!(verify_transcript(&t).is_ok());
    }

    #[test]
    fn mtp_rejects_session_reuse_across_requests() {
        // A second request's transcript with no rebuild: the daemon
        // longevity bug class.
        let t = [C::MtpDecode, C::SessionProcess, C::SessionBegin];
        let err = verify_mtp_sequence(&t).unwrap_err();
        assert!(err.contains("rebuilt per request"), "{err}");
    }

    #[test]
    fn mtp_rejects_unprocessed_decode() {
        let t = [
            C::MtpSessionBuilt,
            C::MtpDecode,
            C::SessionProcess,
            C::SessionBegin,
            C::MtpDecode,
            C::MtpDecode, // second decode without process between
        ];
        let err = verify_mtp_sequence(&t).unwrap_err();
        assert!(err.contains("after EVERY"), "{err}");
    }

    #[test]
    fn recorder_roundtrip_and_disabled_noop() {
        record(C::SetEmbeddings); // disabled — must not land
        enable();
        record(C::SetEmbeddings);
        record(C::EmbedDecode);
        let t = take();
        assert_eq!(t, vec![C::SetEmbeddings, C::EmbedDecode]);
        record(C::EmbedDecode); // disabled again
        enable();
        assert_eq!(take(), vec![]);
    }

    // ── Source-discipline tests ─────────────────────────────────────
    //
    // These give the invariants CI teeth WITHOUT model weights: every
    // full `clear_kv_cache()` in model_slot.rs must declare its phase
    // in a trailing `// kv-phase: <variant>` marker, and every embed
    // decode in embed_slot.rs must sit within two lines of its
    // `set_embeddings(true)`.

    const MODEL_SLOT_SRC: &str = include_str!("model_slot.rs");
    const EMBED_SLOT_SRC: &str = include_str!("embed_slot.rs");

    fn is_code_line(line: &str) -> bool {
        let t = line.trim_start();
        !t.starts_with("//") && !t.starts_with("*")
    }

    #[test]
    fn every_kv_clear_in_model_slot_declares_a_phase() {
        let known = [
            "PrefixCacheSetup",
            "RequestStartReset",
            "EndOfGenerationNoPrefixCache",
            "ErrorAbort",
            "Probe",
        ];
        let mut undeclared = Vec::new();
        let mut unknown_phase = Vec::new();
        for (n, line) in MODEL_SLOT_SRC.lines().enumerate() {
            if !is_code_line(line) || !line.contains(".clear_kv_cache()") {
                continue;
            }
            match line.split("// kv-phase:").nth(1) {
                None => undeclared.push(n + 1),
                Some(tag) => {
                    let tag = tag.trim();
                    if !known.contains(&tag) {
                        unknown_phase.push(format!("line {}: `{tag}`", n + 1));
                    }
                }
            }
        }
        assert!(
            undeclared.is_empty(),
            "model_slot.rs lines {undeclared:?}: `clear_kv_cache()` without a trailing \
             `// kv-phase: <variant>` marker. Every KV clear must declare WHY it is \
             legal — an undeclared end-of-generation clear on the prefix-cached sync \
             path is exactly how the 2026-05 KV/cached_tokens desync shipped (garbage \
             on replay-with-similar-prompt; the offending line carried no comment). \
             Pick a `ffi_trace::ClearKvPhase` variant (or add one WITH a rationale \
             doc) and append `// kv-phase: <Variant>` to the call line."
        );
        assert!(
            unknown_phase.is_empty(),
            "model_slot.rs kv-phase markers naming unknown variants: {unknown_phase:?} \
             — keep markers in sync with ffi_trace::ClearKvPhase."
        );
    }

    #[test]
    fn every_embed_decode_is_adjacent_to_set_embeddings() {
        // Find each `.decode(` on the embed path and require a
        // `set_embeddings(true)` within the two preceding code lines.
        let lines: Vec<&str> = EMBED_SLOT_SRC.lines().collect();
        let mut violations = Vec::new();
        for (n, line) in lines.iter().enumerate() {
            if !is_code_line(line) || !line.contains(".decode(") {
                continue;
            }
            let window_start = n.saturating_sub(3);
            let armed = lines[window_start..n]
                .iter()
                .any(|l| is_code_line(l) && l.contains("set_embeddings(true)"));
            if !armed {
                violations.push(n + 1);
            }
        }
        assert!(
            violations.is_empty(),
            "embed_slot.rs lines {violations:?}: `.decode(` without `set_embeddings(true)` \
             in the immediately preceding lines. llama-cpp-4 0.2.x INVARIANT: the \
             constructor's with_embeddings(true) does NOT stick — without the per-decode \
             call, embd.data is null and every embedding read errors. If you moved the \
             arming further away on purpose, update this window with a comment saying why."
        );
    }
}
