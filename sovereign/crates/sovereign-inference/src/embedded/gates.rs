// SPDX-License-Identifier: AGPL-3.0-or-later
//! `gates` — pure decision functions guarding the embedded engine's
//! model-architecture and request-shape hazards.
//!
//! Every function here is a relocation of an inline boolean ladder
//! that once shipped (or fixed) a production P0. They take plain data
//! — arch strings, request fields, injected env lookups — instead of
//! FFI handles, so the decision matrix is pinned by `cargo test`
//! without model weights. FFI-derived inputs (`model.is_recurrent()`,
//! slot mode) arrive as parameters; verifying that the *wiring* of
//! those parameters is correct is the gguf-smoke tier's job, not
//! this module's.
//!
//! Incident index (each has a named test below):
//! - Qwen-MoE / recurrent prefix-cache corruption → [`prefix_cache_gate`]
//! - Hybrid dense Qwen3.5 `Decode Error -1` on lcp>0 → [`prefix_cache_gate`]
//! - FastShort `Decode Error -3` on recurrent archs → [`fast_short_gate`]
//! - MTP-by-name model slipping past the recurrent check → [`fast_short_gate`]
//! - Tool calls entering MTP (unbounded text, no stop tracker) →
//!   [`mtp_dispatch_eligible`]
//! - Identical-prompt re-prefill starvation (sampler needs ≥1 fresh
//!   logit) → [`compute_lcp`]

use sovereign_core::types::CompletionRequest;

use super::model_slot::forced_choice_candidates;
use super::prompt_helpers::is_recurrent_arch;

/// Shared truthy parse for `SOVEREIGN_*` boolean env flags. The same
/// `"1" | "true"` convention was previously copy-pasted at each read
/// site (`engine.rs` and `model_slot.rs` both parsed
/// `SOVEREIGN_MTP_DISABLE` inline) — one definition ends the drift
/// risk.
pub(crate) fn env_flag_truthy(env_get: impl Fn(&str) -> Option<String>, name: &str) -> bool {
    env_get(name)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// The prefix-cache capability decision for one `generate_sync` call,
/// with each contributing clause preserved for the glassbox
/// `prefix_cache: gate decision` log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PrefixCacheGate {
    pub(crate) model_says_recurrent: bool,
    pub(crate) arch_says_recurrent: bool,
    pub(crate) quirks_say_recurrent: bool,
    pub(crate) speculative_active: bool,
    /// `SOVEREIGN_PREFIX_CACHE_FORCE` overrode a recurrent/hybrid
    /// veto — DIAGNOSTIC ONLY (see `prefix_cache_gate` doc).
    pub(crate) forced: bool,
    /// True only when NO clause vetoes partial KV keep.
    pub(crate) safe: bool,
}

/// Decide whether partial KV-cache keep (prefix caching) is safe for
/// this slot. Recurrent / hybrid architectures carry per-sequence
/// state that does not survive a partial keep — tail decode fails
/// with `Decode Error -1` (hybrid dense, observed 2026-06-09 on
/// Qwen3.5-2B `ask_document`) or returns wrong state (Qwen-MoE Gated
/// DeltaNet, [[invariant_qwen_moe_prefix_cache_disabled]]).
///
/// Clause order of authority:
/// 0. libllama's own flags (`is_recurrent` / `is_hybrid`) — catches
///    hybrids whose arch string looks pure-attention (`qwen35`).
/// 1. The gguf arch string ladder (`is_recurrent_arch`).
/// 2. `ModelQuirks::has_recurrent_layers`, consulted ONLY when the
///    arch string is empty (gguf metadata missing).
/// 3. A speculative (MTP) slot always vetoes — the draft/verify KV
///    discipline owns the cache.
///
/// **`SOVEREIGN_PREFIX_CACHE_FORCE=1` (diagnostic only)** overrides
/// the recurrent/hybrid veto (clauses 0–2) so the underlying hazard
/// can be re-checked against newer llama.cpp builds — the gate may be
/// over-applied, and this is the lever `tests/gate_repros.rs` and a
/// live A/B both use to find out. It deliberately does NOT override
/// the speculative veto: that one is slot-ownership discipline, not a
/// hazard workaround, and forcing it would corrupt the MTP session's
/// KV state.
pub(crate) fn prefix_cache_gate(
    model_is_recurrent: bool,
    model_is_hybrid: bool,
    arch: &str,
    quirks_has_recurrent_layers: bool,
    speculative_active: bool,
    env_get: impl Fn(&str) -> Option<String>,
) -> PrefixCacheGate {
    let model_says_recurrent = model_is_recurrent || model_is_hybrid;
    let arch_says_recurrent = is_recurrent_arch(arch);
    let quirks_say_recurrent = arch.is_empty() && quirks_has_recurrent_layers;
    let recurrent_veto = model_says_recurrent || arch_says_recurrent || quirks_say_recurrent;
    let forced = recurrent_veto && env_flag_truthy(&env_get, "SOVEREIGN_PREFIX_CACHE_FORCE");
    let safe = !speculative_active && (forced || !recurrent_veto);
    PrefixCacheGate {
        model_says_recurrent,
        arch_says_recurrent,
        quirks_say_recurrent,
        speculative_active,
        forced,
        safe,
    }
}

/// Outcome of the FastShort-companion construction gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FastShortGate {
    /// Operator opt-out via `SOVEREIGN_FAST_SHORT_DISABLE`.
    Disabled,
    /// Explicit recurrent arch (mamba / rwkv / deltanet / ssm) that
    /// the 2026-06-11 repro campaign could NOT burst-test (no local
    /// weights). Vetoed until cleared — see [`fast_short_gate`].
    UnsafeRecurrent,
    /// `qwen*moe` arch — RE-VETOED 2026-06-16 after the bite-back the
    /// 2026-06-11 narrowing's doc predicted. The narrowing cleared
    /// qwen-MoE on `max_tokens=8` bursts, but a sustained recipe-author
    /// workload (a ~10k-token prefill + a background heartbeat hitting
    /// FastShort every ~30s) reproduced `Decode Error -3` on both the
    /// `fast_short` slot (batch_n_tokens as low as 18) AND — via the
    /// shared model/Metal state — the `fast`/`primary` slot's prompt
    /// decode, on the APEX `qwen35moe` model in BOTH the daemon and the
    /// desktop. See [`fast_short_gate`].
    UnsafeQwenMoeBiteback,
    /// `SOVEREIGN_FAST_SHORT_FORCE=1` overrode the remaining veto —
    /// DIAGNOSTIC ONLY, for clearing an untested arch via
    /// `tests/gate_repros.rs`. Build the companion and warn loudly.
    ForcedSafe,
    /// Build the companion.
    Safe,
}

/// Explicit recurrent families the FastShort burst repro has NEVER
/// cleared (no local weights as of 2026-06-11). Deliberately narrower
/// than [`is_recurrent_arch`]: qwen-MoE was cleared (see below), so
/// only the marker families remain vetoed.
fn fast_short_untested_recurrent_arch(arch: &str) -> bool {
    let lower = arch.to_lowercase();
    ["mamba", "rwkv", "deltanet", "ssm"]
        .iter()
        .any(|marker| lower.contains(marker))
}

/// `qwen*moe` (qwen3moe / qwen35moe / qwen36moe …) — RE-VETOED 2026-06-16 for
/// the FastShort companion. NOT recurrent: this is the MoE continuous-batch
/// `Decode Error -3` bite-back (see [`FastShortGate::UnsafeQwenMoeBiteback`]).
/// Dense qwen (`qwen35`, `qwen3`) is intentionally NOT matched — only the MoE
/// variants carry the hazard.
fn fast_short_qwen_moe_biteback(arch: &str) -> bool {
    let lower = arch.to_lowercase();
    lower.contains("qwen") && lower.contains("moe")
}

/// Decide whether to construct the FastShort continuous-batching
/// companion for the Fast slot. Skipping forfeits the batched-call
/// speedup; all callers route through `fast` (n_seq_max=1) — slower,
/// never crashing.
///
/// **NARROWED 2026-06-11 after the gate-repro campaign.** This gate
/// originally vetoed every recurrent arch AND every MTP-by-name model
/// ("from_existing_model doesn't propagate n_rs_seq → Decode Error -3
/// on the first continuous-batched call", incidents 2026-05-24 ff,
/// [[invariant_fast_short_recurrent_arch]]). The repro campaign
/// (`scripts/gate-repros.sh`) could no longer reproduce either case:
/// the canonical incident model (APEX `qwen35moe`) survived a
/// SATURATED 8-concurrent burst — all 8 served `slot="fast_short"`
/// in one coalesced batch — and Qwopus3.5-4B-MTP likewise. The
/// original story was likely a misattribution: the 2026-05-23
/// diagnosis on `build_target_ctx_for_slot` shows `Decode Error -3`
/// arises from `n_rs_seq` being APPLIED to a ctx the MTP draft never
/// drives (opposite polarity), and llama.cpp provisions recurrent
/// state from `n_seq_max` on its own. So the qwen-MoE and MTP-by-name
/// vetoes were removed; mamba/rwkv/deltanet/ssm stay vetoed only
/// because no local weights existed to clear them.
///
/// **qwen-MoE RE-VETOED 2026-06-16 — the bite-back this doc predicted.**
/// The narrowing's clearing evidence was `max_tokens=8` bursts; it did
/// NOT exercise a large prefill or a sustained background call rate. The
/// recipe-author workload does both: a ~10k-token prefill (skill + 19k-char
/// grammar) plus a heartbeat hitting FastShort every ~30s reproduced
/// `Decode Error -3` on the `fast_short` slot (batch_n_tokens as low as 18)
/// AND — via shared model/Metal state — the `fast`/`primary` slot's prompt
/// decode, on APEX `qwen35moe`, in BOTH the daemon and the desktop. (A lone
/// large prefill on the primary slot decoded fine; the failures correlated
/// with FastShort activity.) Per step 3 below the veto is restored via
/// [`fast_short_qwen_moe_biteback`] → [`FastShortGate::UnsafeQwenMoeBiteback`];
/// MTP-by-name stays cleared (no evidence it bit). qwen-MoE now routes all
/// short calls to `fast` (n_seq_max=1) — forfeits the batched speedup,
/// never crashes. `SOVEREIGN_FAST_SHORT_FORCE=1` overrides for diagnostics.
///
/// **IF THIS BITES AGAIN** — suspect signature: `Decode Error -3:
/// unknown` on a continuous-batched call. The clearing evidence was
/// `max_tokens=8` bursts; LONG generations, sustained multi-hour
/// batch pipelines, and KV-pressure shapes were NOT exercised:
/// 1. Immediate mitigation: `SOVEREIGN_FAST_SHORT_DISABLE=1` — all
///    callers route to `fast`; slower, never crashing.
/// 2. Re-adjudicate: `./scripts/gate-repros.sh --fastshort <gguf>`
///    (extend the burst shape to match the failing workload first).
/// 3. If it reproduces, restore the veto arm from git history AND add
///    the failing shape to `tests/gate_repros.rs` so the veto carries
///    its evidence this time.
pub(crate) fn fast_short_gate(
    arch: &str,
    env_get: impl Fn(&str) -> Option<String>,
) -> FastShortGate {
    if env_flag_truthy(&env_get, "SOVEREIGN_FAST_SHORT_DISABLE") {
        return FastShortGate::Disabled;
    }
    let forced = env_flag_truthy(&env_get, "SOVEREIGN_FAST_SHORT_FORCE");
    if fast_short_untested_recurrent_arch(arch) {
        return if forced {
            FastShortGate::ForcedSafe
        } else {
            FastShortGate::UnsafeRecurrent
        };
    }
    if fast_short_qwen_moe_biteback(arch) {
        return if forced {
            FastShortGate::ForcedSafe
        } else {
            FastShortGate::UnsafeQwenMoeBiteback
        };
    }
    FastShortGate::Safe
}

/// Decide whether this request may take the MTP (speculative
/// draft/verify) path.
///
/// Tools stay gated out UNCONDITIONALLY: the tool-call JSON-depth
/// tracker that stops generation at the close brace lives in the
/// single-token path only — MTP would emit unbounded text past the
/// envelope. (`structured_output` is intentionally NOT gated; the
/// constraint mask applies at every verify position.)
///
/// The forced-choice sentinel also bypasses MTP — its one-forward-pass
/// logprob read happens right after prompt decode in the single-token
/// path.
pub(crate) fn mtp_dispatch_eligible(
    request: &CompletionRequest,
    slot_is_speculative: bool,
    env_get: impl Fn(&str) -> Option<String>,
) -> bool {
    !env_flag_truthy(&env_get, "SOVEREIGN_MTP_DISABLE")
        && slot_is_speculative
        && request.tools.as_ref().is_none_or(|t| t.is_empty())
        && forced_choice_candidates(request).is_none()
}

/// Classify an MTP-path error as "draft side broken on this slot,
/// single-token decode still works" — the set that quarantines the
/// slot (demote to SingleToken) and retries the request on the
/// non-MTP path. Shared by the sync and streaming dispatchers so the
/// two can't drift.
///
/// INVARIANT the streaming dispatcher depends on: every error in this
/// set fires BEFORE the first token is emitted (prefill decode,
/// prefill batch add, process(prefill) — all precede the first
/// sample). The streamed fallback re-runs the request from scratch on
/// the single-token path, which is only wire-safe when zero frames
/// have been sent. Do NOT add a post-emission error site to this set
/// without teaching `generate_stream_dispatch` to refuse the
/// fall-through once emission has started.
pub(crate) fn mtp_error_is_prefill(msg: &str) -> bool {
    msg.contains("MTP prefill decode failed")
        || msg.contains("MTP prefill batch add failed")
        || msg.contains("MTP process(prefill) failed")
}

/// Longest-common-prefix result for the prefix cache: `raw` is the
/// honest LCP (logged for diagnostics), `effective` is what the KV
/// keep actually uses after the capability gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PrefixLcp {
    pub(crate) raw: usize,
    pub(crate) effective: usize,
}

/// Compute the prefix-cache LCP between the cached prompt tokens and
/// the new prompt's tokens.
///
/// Reserve-last-token rule: the comparison never extends past
/// `new.len() - 1`, so an *identical* prompt yields `raw = len - 1`
/// and forces a 1-token re-prefill — the model needs at least one
/// token of fresh decode to produce logits for the sampler. When the
/// capability gate says unsafe, `effective` is 0 (full clear + full
/// prefill, the pre-prefix-cache behaviour).
pub(crate) fn compute_lcp<T: PartialEq>(
    cached: &[T],
    new: &[T],
    prefix_cache_safe: bool,
) -> PrefixLcp {
    let raw = cached
        .iter()
        .zip(new.iter())
        .take(new.len().saturating_sub(1))
        .take_while(|(a, b)| a == b)
        .count();
    PrefixLcp {
        raw,
        effective: if prefix_cache_safe { raw } else { 0 },
    }
}

/// Tool-call envelope stop tracker — the shape-B (bare JSON envelope)
/// stop decision for the decode loops.
///
/// Tracks brace depth across decoded pieces, JSON-string-aware (braces
/// inside string values don't count; `\"` doesn't close a string), and
/// reports "the envelope just balanced" so the loop can stop instead
/// of burning tokens on a grammar mask cycling through whitespace.
///
/// Two gates, both enforced here so call sites can't drift:
/// - **Enabled only when grammar is actively constraining output to
///   the envelope shape** (`tools_present && structured_output` —
///   `tools_grammar_locked`). Without that gate the tracker counts
///   literal `{...}` in prose: Qwopus3.5-9B-Coder emitting `x_{r,c}`
///   in a markdown bullet false-positive-stopped at n_generated=87,
///   2026-05-21. (The `</tool_call>` marker stop — shape A — stays
///   unconditional at the call sites; it isn't this tracker's job.)
/// - **Suspended inside `<think>` blocks** (`in_think` parameter) —
///   reasoning prose may contain braces.
///
/// Was hand-duplicated 4× across the decode loops (generate_sync's
/// sampled + jump-forward branches, generate_stream_sync,
/// generate_stream_sync_with_finish) before extraction.
#[derive(Debug, Clone)]
pub(crate) struct ToolStopTracker {
    enabled: bool,
    depth: i32,
    in_string: bool,
    escape_next: bool,
    ever_opened: bool,
}

impl ToolStopTracker {
    pub(crate) fn new(tools_present: bool, structured_output_set: bool) -> Self {
        Self {
            enabled: tools_present && structured_output_set,
            depth: 0,
            in_string: false,
            escape_next: false,
            ever_opened: false,
        }
    }

    /// Feed one decoded piece. Returns `true` when the JSON envelope
    /// is balanced (ever opened, depth back to 0) as of this piece —
    /// the caller should stop generation. Gated off (returns `false`,
    /// consumes nothing) when the tracker is disabled or the piece is
    /// inside a `<think>` block; string/escape state persists across
    /// pieces, so a `\` at the end of one piece correctly escapes a
    /// `"` at the start of the next.
    pub(crate) fn observe(&mut self, piece: &str, in_think: bool) -> bool {
        if !self.enabled || in_think {
            return false;
        }
        for b in piece.bytes() {
            if self.escape_next {
                self.escape_next = false;
                continue;
            }
            if self.in_string {
                match b {
                    b'\\' => self.escape_next = true,
                    b'"' => self.in_string = false,
                    _ => {}
                }
                continue;
            }
            match b {
                b'"' => self.in_string = true,
                b'{' => {
                    self.depth += 1;
                    self.ever_opened = true;
                }
                b'}' => self.depth -= 1,
                _ => {}
            }
        }
        self.ever_opened && self.depth == 0
    }

    /// Inside a JSON string value? Drives the sampler-role decision
    /// (`Content`/greedy inside strings) and the per-token trace.
    pub(crate) fn in_json_string(&self) -> bool {
        self.in_string
    }

    /// Current brace depth (per-token trace).
    pub(crate) fn depth(&self) -> i32 {
        self.depth
    }

    /// Has an envelope ever opened? (end-of-generation role summary).
    pub(crate) fn ever_opened(&self) -> bool {
        self.ever_opened
    }
}

/// Sliding 32-byte tail window for tag detection (`<think>`,
/// `</tool_call>`), draining only at UTF-8 char boundaries so a
/// multi-byte sequence (Qwen3 CJK pieces) is never sliced mid-char.
/// Was hand-duplicated at every decode loop's piece handler.
pub(crate) fn push_sliding_tail(tail: &mut String, piece: &str) {
    tail.push_str(piece);
    if tail.len() > 32 {
        let mut drain_to = tail.len() - 32;
        while drain_to > 0 && !tail.is_char_boundary(drain_to) {
            drain_to -= 1;
        }
        tail.drain(..drain_to);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::types::{CompletionRequest, ToolSchema};

    /// env closure over a fixed (name, value) list.
    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| {
            owned
                .iter()
                .find(|(name, _)| name == k)
                .map(|(_, v)| v.clone())
        }
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn tool(name: &str) -> ToolSchema {
        ToolSchema {
            name: name.to_string(),
            description: None,
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    // ── is_recurrent_arch ────────────────────────────────────────
    // The arch-string ladder behind both prefix_cache_gate and
    // fast_short_gate. [[invariant_qwen_moe_prefix_cache_disabled]]

    #[test]
    fn recurrent_arch_matches_every_observed_qwen_moe_spelling() {
        // Observed gguf arch values across the Qwen MoE families.
        for arch in [
            "qwen3moe",
            "qwen35moe",
            "qwen3_moe",
            "qwen36moe",
            "Qwen3MoE",
        ] {
            assert!(is_recurrent_arch(arch), "{arch} must classify recurrent");
        }
    }

    #[test]
    fn recurrent_arch_matches_explicit_recurrent_families_with_version_suffixes() {
        for arch in ["mamba", "mamba2", "rwkv6", "deltanet", "ssm-hybrid"] {
            assert!(is_recurrent_arch(arch), "{arch} must classify recurrent");
        }
    }

    #[test]
    fn recurrent_arch_rejects_pure_attention_and_unknown() {
        // `qwen35` (dense hybrid) deliberately does NOT match — the
        // string ladder can't see hybrids; libllama's is_hybrid()
        // flag covers them (see prefix_cache_gate clause 0).
        for arch in ["qwen3", "qwen35", "llama", "gemma3", "phi4", ""] {
            assert!(
                !is_recurrent_arch(arch),
                "{arch} must NOT classify recurrent"
            );
        }
    }

    // ── prefix_cache_gate ────────────────────────────────────────

    #[test]
    fn prefix_cache_safe_for_plain_attention_model() {
        let g = prefix_cache_gate(false, false, "qwen3", false, false, no_env);
        assert!(g.safe);
        assert!(!g.model_says_recurrent);
        assert!(!g.arch_says_recurrent);
        assert!(!g.quirks_say_recurrent);
    }

    #[test]
    fn prefix_cache_unsafe_for_qwen_moe_arch() {
        // The original P0: gated DeltaNet layers can't survive
        // partial KV keep; cache_hit_tokens must be forced to 0.
        let g = prefix_cache_gate(false, false, "qwen35moe", false, false, no_env);
        assert!(!g.safe);
        assert!(g.arch_says_recurrent);
    }

    #[test]
    fn prefix_cache_unsafe_for_hybrid_dense_model_arch_ladder_misses() {
        // 2026-06-09 P0: dense Qwen3.5 gguf arch is plain `qwen35` —
        // no "moe", so the string ladder misses it — but libllama's
        // is_hybrid() knows. Decode Error -1 on every lcp>0 prefill
        // without this clause.
        let g = prefix_cache_gate(false, true, "qwen35", false, false, no_env);
        assert!(!g.safe);
        assert!(g.model_says_recurrent);
        assert!(
            !g.arch_says_recurrent,
            "ladder must NOT be what catches this"
        );
    }

    #[test]
    fn prefix_cache_quirks_fallback_only_when_arch_is_empty() {
        // Quirks are the per-family fallback for ggufs with missing
        // arch metadata — they must not veto when arch IS present.
        let empty_arch = prefix_cache_gate(false, false, "", true, false, no_env);
        assert!(!empty_arch.safe);
        assert!(empty_arch.quirks_say_recurrent);

        let arch_present = prefix_cache_gate(false, false, "llama", true, false, no_env);
        assert!(
            arch_present.safe,
            "quirks must be ignored when arch is present"
        );
        assert!(!arch_present.quirks_say_recurrent);
    }

    #[test]
    fn prefix_cache_unsafe_on_speculative_slot() {
        // MTP slots own their KV discipline; the single-token prefix
        // cache must stand down even on a pure-attention model.
        let g = prefix_cache_gate(false, false, "qwen3", false, true, no_env);
        assert!(!g.safe);
        assert!(g.speculative_active);
    }

    #[test]
    fn prefix_cache_force_overrides_recurrent_veto_only() {
        // The gate_repros lever: force re-enables partial keep on a
        // recurrent model (to re-check the hazard against newer
        // llama.cpp)...
        let force = env(&[("SOVEREIGN_PREFIX_CACHE_FORCE", "1")]);
        let g = prefix_cache_gate(false, false, "qwen35moe", false, false, &force);
        assert!(g.safe);
        assert!(g.forced, "override must be visible in the glassbox log");

        // ...but NEVER the speculative veto — that's slot-ownership
        // discipline, not a hazard workaround.
        let spec = prefix_cache_gate(false, false, "qwen35moe", false, true, &force);
        assert!(!spec.safe, "force must not touch the MTP slot veto");

        // And it's inert (not even reported) when nothing was vetoed.
        let plain = prefix_cache_gate(false, false, "qwen3", false, false, &force);
        assert!(plain.safe);
        assert!(!plain.forced, "no veto → nothing forced");
    }

    #[test]
    fn fast_short_disable_wins_even_on_cleared_archs() {
        // The bite-back mitigation from the gate doc: operator disable
        // must always win, including for archs the repro cleared.
        let disable = env(&[("SOVEREIGN_FAST_SHORT_DISABLE", "1")]);
        assert_eq!(
            fast_short_gate("qwen3moe", &disable),
            FastShortGate::Disabled
        );
        assert_eq!(fast_short_gate("qwen3", &disable), FastShortGate::Disabled);
    }

    // ── fast_short_gate ──────────────────────────────────────────

    #[test]
    fn fast_short_safe_for_plain_attention_model() {
        assert_eq!(fast_short_gate("qwen3", no_env), FastShortGate::Safe);
    }

    #[test]
    fn fast_short_revetoes_qwen_moe_but_not_dense_qwen() {
        // RE-VETOED 2026-06-16: the 2026-06-11 narrowing cleared qwen-MoE on
        // max_tokens=8 bursts, but a ~10k-prefill + sustained-FastShort
        // workload reproduced `Decode Error -3` on APEX qwen35moe (daemon +
        // desktop). The veto is restored for the MoE variants — see the
        // fast_short_gate doc for the evidence + the FORCE diagnostic escape.
        for arch in [
            "qwen3moe",
            "qwen35moe",
            "qwen36moe",
            "qwen3_moe",
            "Qwen35MoE",
        ] {
            assert_eq!(
                fast_short_gate(arch, no_env),
                FastShortGate::UnsafeQwenMoeBiteback,
                "{arch} is qwen*moe — must be re-vetoed (Decode -3 bite-back)"
            );
        }
        // DENSE qwen (no MoE) carries no bite-back and stays safe — the veto
        // is narrow to the MoE variants, not all qwen.
        for arch in ["qwen35", "qwen3", "qwen2"] {
            assert_eq!(
                fast_short_gate(arch, no_env),
                FastShortGate::Safe,
                "{arch} is dense qwen — must NOT be vetoed"
            );
        }
    }

    #[test]
    fn fast_short_still_vetoes_untested_recurrent_archs() {
        // No local weights existed to burst-clear these families —
        // the veto stays until scripts/gate-repros.sh clears them.
        for arch in ["mamba", "mamba2", "rwkv6", "deltanet", "ssm-hybrid"] {
            assert_eq!(
                fast_short_gate(arch, no_env),
                FastShortGate::UnsafeRecurrent,
                "{arch} is uncleared — veto must hold"
            );
        }
    }

    #[test]
    fn fast_short_force_overrides_remaining_veto_but_not_disable() {
        let force = env(&[("SOVEREIGN_FAST_SHORT_FORCE", "1")]);
        // The clearing lever for an untested recurrent arch.
        assert_eq!(fast_short_gate("mamba2", &force), FastShortGate::ForcedSafe);
        // FORCE also overrides the qwen-MoE bite-back veto (diagnostic escape).
        assert_eq!(
            fast_short_gate("qwen3moe", &force),
            FastShortGate::ForcedSafe
        );
        // Force is inert when nothing is vetoed (dense attention model).
        assert_eq!(fast_short_gate("qwen3", &force), FastShortGate::Safe);
        // Operator disable still wins over the diagnostic force.
        let both = env(&[
            ("SOVEREIGN_FAST_SHORT_FORCE", "1"),
            ("SOVEREIGN_FAST_SHORT_DISABLE", "1"),
        ]);
        assert_eq!(fast_short_gate("mamba2", &both), FastShortGate::Disabled);
        assert_eq!(fast_short_gate("qwen3", &both), FastShortGate::Disabled);
    }

    // ── mtp_dispatch_eligible ────────────────────────────────────

    #[test]
    fn mtp_eligible_for_plain_request_on_speculative_slot() {
        let r = CompletionRequest::new("hi");
        assert!(mtp_dispatch_eligible(&r, true, no_env));
    }

    #[test]
    fn mtp_never_takes_requests_with_tools() {
        // The tool-loop P0 family: the close-brace stop tracker lives
        // in the single-token path only; MTP would emit unbounded
        // text past the envelope. Tools must NEVER enter MTP.
        let mut r = CompletionRequest::new("hi");
        r.tools = Some(vec![tool("search")]);
        assert!(!mtp_dispatch_eligible(&r, true, no_env));
    }

    #[test]
    fn mtp_treats_empty_tool_list_as_no_tools() {
        let mut r = CompletionRequest::new("hi");
        r.tools = Some(vec![]);
        assert!(mtp_dispatch_eligible(&r, true, no_env));
    }

    #[test]
    fn mtp_skipped_for_forced_choice_sentinel() {
        // The mechanism-fidelity logprob read happens right after
        // prompt decode in the single-token path.
        let mut r = CompletionRequest::new("hi");
        r.structured_output = Some(serde_json::json!({
            "type": "string",
            "enum": ["yes", "no"],
            "x_forced_choice": true
        }));
        assert!(!mtp_dispatch_eligible(&r, true, no_env));
    }

    #[test]
    fn mtp_allows_plain_structured_output() {
        // structured_output WITHOUT the sentinel is deliberately not
        // gated — the constraint mask applies at every verify
        // position (graceful acceptance-rate degradation, measured
        // not avoided).
        let mut r = CompletionRequest::new("hi");
        r.structured_output = Some(serde_json::json!({"type": "object"}));
        assert!(mtp_dispatch_eligible(&r, true, no_env));
    }

    #[test]
    fn mtp_requires_speculative_slot_and_respects_env_disable() {
        let r = CompletionRequest::new("hi");
        assert!(!mtp_dispatch_eligible(&r, false, no_env));
        assert!(!mtp_dispatch_eligible(
            &r,
            true,
            env(&[("SOVEREIGN_MTP_DISABLE", "true")])
        ));
    }

    #[test]
    fn mtp_prefill_errors_quarantine_and_nothing_else_does() {
        // The quarantine set — all three fire BEFORE the first token
        // is emitted, which is what makes the streaming dispatcher's
        // fall-through to single-token wire-safe (see the predicate's
        // doc comment before extending this list).
        for msg in [
            "Inference error: MTP prefill decode failed: Decode Error -3: unknown",
            "MTP prefill batch add failed: batch full",
            "MTP process(prefill) failed: SomeFfiError",
        ] {
            assert!(mtp_error_is_prefill(msg), "{msg:?} must quarantine");
        }
        // Post-emission / non-prefill MTP errors must NOT quarantine:
        // frames may already be on the wire, so the dispatchers
        // propagate these instead of re-running the request.
        for msg in [
            "MTP verify decode failed: Decode Error 1: NoKvCacheSlot",
            "MTP draft phase failed: SomeFfiError",
            "MTP begin failed: SomeFfiError",
            "MTP session rebuild failed: SomeFfiError",
            "Prompt too long: 32000 tokens already meets or exceeds the context window",
        ] {
            assert!(!mtp_error_is_prefill(msg), "{msg:?} must NOT quarantine");
        }
    }

    // ── compute_lcp ──────────────────────────────────────────────

    #[test]
    fn lcp_identical_prompt_reserves_last_token_for_fresh_decode() {
        // The sampler needs at least one fresh logit distribution:
        // an identical prompt must re-prefill exactly 1 token, never 0.
        let toks = [1, 2, 3, 4];
        let lcp = compute_lcp(&toks, &toks, true);
        assert_eq!(
            lcp.raw, 3,
            "raw LCP must stop at len-1 on identical prompts"
        );
        assert_eq!(lcp.effective, 3);
    }

    #[test]
    fn lcp_shared_prefix_counts_up_to_divergence() {
        let cached = [1, 2, 3, 9, 9];
        let new = [1, 2, 3, 4, 5, 6];
        assert_eq!(compute_lcp(&cached, &new, true).effective, 3);
    }

    #[test]
    fn lcp_gate_unsafe_forces_full_prefill() {
        // Recurrent/hybrid models: raw stays honest for diagnostics,
        // effective is 0 so the KV cache is fully cleared.
        let toks = [1, 2, 3, 4];
        let lcp = compute_lcp(&toks, &toks, false);
        assert_eq!(lcp.raw, 3);
        assert_eq!(lcp.effective, 0);
    }

    #[test]
    fn lcp_empty_cache_and_empty_prompt_are_zero() {
        let empty: [i32; 0] = [];
        assert_eq!(compute_lcp(&empty, &[1, 2], true).effective, 0);
        assert_eq!(compute_lcp(&[1, 2], &empty, true).effective, 0);
        assert_eq!(compute_lcp(&empty, &empty, true).raw, 0);
    }

    // ── forced_choice_candidates (sentinel contract) ─────────────

    #[test]
    fn forced_choice_sentinel_requires_explicit_marker_and_enum() {
        // Ordinary structured_output must NOT trip the sentinel.
        let mut r = CompletionRequest::new("hi");
        r.structured_output = Some(serde_json::json!({
            "type": "string",
            "enum": ["a", "b"]
        }));
        assert!(
            forced_choice_candidates(&r).is_none(),
            "no marker → no sentinel"
        );

        r.structured_output = Some(serde_json::json!({
            "type": "string",
            "enum": ["a", "b"],
            "x_forced_choice": true
        }));
        assert_eq!(
            forced_choice_candidates(&r),
            Some(vec!["a".to_string(), "b".to_string()])
        );
    }

    #[test]
    fn forced_choice_sentinel_rejects_degenerate_shapes() {
        let mut r = CompletionRequest::new("hi");
        // marker=false
        r.structured_output = Some(serde_json::json!({
            "enum": ["a"], "x_forced_choice": false
        }));
        assert!(forced_choice_candidates(&r).is_none());
        // marker but empty enum
        r.structured_output = Some(serde_json::json!({
            "enum": [], "x_forced_choice": true
        }));
        assert!(forced_choice_candidates(&r).is_none());
        // marker but no enum at all
        r.structured_output = Some(serde_json::json!({ "x_forced_choice": true }));
        assert!(forced_choice_candidates(&r).is_none());
        // no structured_output
        r.structured_output = None;
        assert!(forced_choice_candidates(&r).is_none());
    }

    // ── ToolStopTracker ──────────────────────────────────────────

    #[test]
    fn tracker_latex_prose_braces_never_stop_without_structured_output() {
        // 2026-05-21 P0: Qwopus3.5-9B-Coder emitting "x_{r,c}" inside
        // a markdown bullet false-positive-stopped generation at
        // n_generated=87. Tools present but no structured_output →
        // tracker must be inert.
        let mut t = ToolStopTracker::new(true, false);
        assert!(!t.observe("the term x_", false));
        assert!(!t.observe("{r,c}", false));
        assert!(!t.observe(" appears {twice} {here}", false));
        assert!(!t.ever_opened());
    }

    #[test]
    fn tracker_disabled_without_tools_even_with_schema() {
        // structured_output alone (plain JSON mode, no tools) must not
        // arm the envelope stop — that path stops on EOS/grammar.
        let mut t = ToolStopTracker::new(false, true);
        assert!(!t.observe("{\"a\": 1}", false));
    }

    #[test]
    fn tracker_stops_on_balanced_envelope() {
        let mut t = ToolStopTracker::new(true, true);
        assert!(!t.observe("{\"name\": \"search\", ", false));
        assert!(!t.observe("\"args\": {\"q\": \"x\"}", false));
        assert!(t.observe("}", false), "envelope balanced — must stop");
    }

    #[test]
    fn tracker_braces_inside_json_strings_do_not_count() {
        let mut t = ToolStopTracker::new(true, true);
        // The string value contains braces that must not affect depth.
        assert!(!t.observe("{\"cmd\": \"if x { y } else { z }", false));
        assert!(t.in_json_string());
        assert!(t.observe("\"}", false), "real close brace balances");
    }

    #[test]
    fn tracker_escaped_quote_does_not_close_string() {
        let mut t = ToolStopTracker::new(true, true);
        assert!(!t.observe("{\"msg\": \"say \\\"hi\\\" {", false));
        assert!(
            t.in_json_string(),
            "escaped quotes must not close the string"
        );
        assert!(t.observe("\"}", false));
    }

    #[test]
    fn tracker_escape_state_survives_piece_boundaries() {
        // BPE pieces can split anywhere — a trailing backslash in one
        // piece escapes a leading quote in the next.
        let mut t = ToolStopTracker::new(true, true);
        assert!(!t.observe("{\"a\": \"x\\", false));
        // The carried escape consumes the leading quote (it does NOT
        // close the string), and the brace after it is string content.
        assert!(!t.observe("\" } ", false));
        assert!(t.in_json_string());
        assert!(t.observe("\"}", false));
    }

    #[test]
    fn tracker_suspended_inside_think_block() {
        // Reasoning prose may contain braces; in_think pieces must not
        // advance the tracker (matches the `!in_think` gate the
        // sampled-token branch always had).
        let mut t = ToolStopTracker::new(true, true);
        assert!(!t.observe("consider {a} vs {b}", true));
        assert!(!t.ever_opened(), "think-block braces must not count");
        assert!(!t.observe("{\"name\": \"x\"", false));
        assert!(t.observe("}", false));
    }

    #[test]
    fn tracker_no_stop_before_envelope_ever_opens() {
        // depth==0 alone must not stop — prose before the envelope.
        let mut t = ToolStopTracker::new(true, true);
        assert!(!t.observe("Sure, calling the tool now: ", false));
        assert!(!t.observe("", false));
    }

    #[test]
    fn tracker_nested_envelope_stops_only_at_outer_close() {
        let mut t = ToolStopTracker::new(true, true);
        assert!(!t.observe("{\"a\": {\"b\": {\"c\": 1}", false));
        assert!(!t.observe("}", false), "inner close — depth 1, no stop");
        assert!(t.observe("}", false), "outer close — stop");
    }

    // ── push_sliding_tail ────────────────────────────────────────

    #[test]
    fn sliding_tail_keeps_recent_window_for_tag_detection() {
        let mut tail = String::new();
        push_sliding_tail(&mut tail, "some long preamble text here....");
        push_sliding_tail(&mut tail, "x</tool_call>");
        assert!(tail.contains("</tool_call>"));
        assert!(tail.len() <= 32 + "x</tool_call>".len());
    }

    #[test]
    fn sliding_tail_never_slices_multibyte_utf8() {
        // Qwen3 CJK pieces: drain must walk back to a char boundary.
        let mut tail = String::new();
        for _ in 0..8 {
            push_sliding_tail(&mut tail, "日本語のテキスト"); // 3-byte chars
        }
        // If the drain sliced mid-char this would have panicked; also
        // the result must still be valid UTF-8 ending with the source.
        assert!(tail.ends_with("テキスト"));
    }

    // ── env_flag_truthy ──────────────────────────────────────────

    #[test]
    fn env_flag_truthy_accepts_one_and_case_insensitive_true_only() {
        for v in ["1", "true", "TRUE", "True"] {
            assert!(env_flag_truthy(env(&[("F", v)]), "F"), "{v} must be truthy");
        }
        for v in ["0", "false", "yes", "on", ""] {
            assert!(
                !env_flag_truthy(env(&[("F", v)]), "F"),
                "{v} must NOT be truthy"
            );
        }
        assert!(!env_flag_truthy(no_env, "F"));
    }
}
