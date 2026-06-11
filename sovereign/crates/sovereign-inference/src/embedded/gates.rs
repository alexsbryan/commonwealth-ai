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
pub(crate) fn prefix_cache_gate(
    model_is_recurrent: bool,
    model_is_hybrid: bool,
    arch: &str,
    quirks_has_recurrent_layers: bool,
    speculative_active: bool,
) -> PrefixCacheGate {
    let model_says_recurrent = model_is_recurrent || model_is_hybrid;
    let arch_says_recurrent = is_recurrent_arch(arch);
    let quirks_say_recurrent = arch.is_empty() && quirks_has_recurrent_layers;
    let safe = !speculative_active
        && !model_says_recurrent
        && !arch_says_recurrent
        && !quirks_say_recurrent;
    PrefixCacheGate {
        model_says_recurrent,
        arch_says_recurrent,
        quirks_say_recurrent,
        speculative_active,
        safe,
    }
}

/// Outcome of the FastShort-companion construction gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FastShortGate {
    /// Operator opt-out via `SOVEREIGN_FAST_SHORT_DISABLE`.
    Disabled,
    /// Recurrent arch — `from_existing_model` can't propagate
    /// `n_rs_seq`, so the ctx crashes on its first continuous-batched
    /// decode (`Decode Error -3`).
    /// [[invariant_fast_short_recurrent_arch]]
    UnsafeRecurrent,
    /// MTP-by-name model on a non-recurrent arch — the speculative
    /// draft/verify path provisions recurrent-state seq slots, same
    /// hazard. (A `SOVEREIGN_MTP_DISABLE`d model runs single-token,
    /// so FastShort is safe for it.)
    UnsafeMtp,
    /// Build the companion.
    Safe,
}

/// Decide whether to construct the FastShort continuous-batching
/// companion for the Fast slot. Skipping forfeits the batched-call
/// speedup; all callers route through `fast` (n_seq_max=1) — slower,
/// never crashing. See the construction site in `engine.rs` for the
/// full incident narrative (2026-05-24 recurrent, then the
/// MTP-by-name gap).
pub(crate) fn fast_short_gate(
    arch: &str,
    model_id: &str,
    env_get: impl Fn(&str) -> Option<String>,
) -> FastShortGate {
    if env_flag_truthy(&env_get, "SOVEREIGN_FAST_SHORT_DISABLE") {
        return FastShortGate::Disabled;
    }
    if is_recurrent_arch(arch) {
        return FastShortGate::UnsafeRecurrent;
    }
    let mtp_disabled_at_load = env_flag_truthy(&env_get, "SOVEREIGN_MTP_DISABLE");
    if model_id.to_lowercase().contains("mtp") && !mtp_disabled_at_load {
        return FastShortGate::UnsafeMtp;
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
        for arch in ["qwen3moe", "qwen35moe", "qwen3_moe", "qwen36moe", "Qwen3MoE"] {
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
            assert!(!is_recurrent_arch(arch), "{arch} must NOT classify recurrent");
        }
    }

    // ── prefix_cache_gate ────────────────────────────────────────

    #[test]
    fn prefix_cache_safe_for_plain_attention_model() {
        let g = prefix_cache_gate(false, false, "qwen3", false, false);
        assert!(g.safe);
        assert!(!g.model_says_recurrent);
        assert!(!g.arch_says_recurrent);
        assert!(!g.quirks_say_recurrent);
    }

    #[test]
    fn prefix_cache_unsafe_for_qwen_moe_arch() {
        // The original P0: gated DeltaNet layers can't survive
        // partial KV keep; cache_hit_tokens must be forced to 0.
        let g = prefix_cache_gate(false, false, "qwen35moe", false, false);
        assert!(!g.safe);
        assert!(g.arch_says_recurrent);
    }

    #[test]
    fn prefix_cache_unsafe_for_hybrid_dense_model_arch_ladder_misses() {
        // 2026-06-09 P0: dense Qwen3.5 gguf arch is plain `qwen35` —
        // no "moe", so the string ladder misses it — but libllama's
        // is_hybrid() knows. Decode Error -1 on every lcp>0 prefill
        // without this clause.
        let g = prefix_cache_gate(false, true, "qwen35", false, false);
        assert!(!g.safe);
        assert!(g.model_says_recurrent);
        assert!(!g.arch_says_recurrent, "ladder must NOT be what catches this");
    }

    #[test]
    fn prefix_cache_quirks_fallback_only_when_arch_is_empty() {
        // Quirks are the per-family fallback for ggufs with missing
        // arch metadata — they must not veto when arch IS present.
        let empty_arch = prefix_cache_gate(false, false, "", true, false);
        assert!(!empty_arch.safe);
        assert!(empty_arch.quirks_say_recurrent);

        let arch_present = prefix_cache_gate(false, false, "llama", true, false);
        assert!(arch_present.safe, "quirks must be ignored when arch is present");
        assert!(!arch_present.quirks_say_recurrent);
    }

    #[test]
    fn prefix_cache_unsafe_on_speculative_slot() {
        // MTP slots own their KV discipline; the single-token prefix
        // cache must stand down even on a pure-attention model.
        let g = prefix_cache_gate(false, false, "qwen3", false, true);
        assert!(!g.safe);
        assert!(g.speculative_active);
    }

    // ── fast_short_gate ──────────────────────────────────────────

    #[test]
    fn fast_short_safe_for_plain_attention_non_mtp_model() {
        assert_eq!(
            fast_short_gate("qwen3", "Qwen3.5-9B.Q8_0", no_env),
            FastShortGate::Safe
        );
    }

    #[test]
    fn fast_short_skipped_for_recurrent_arch() {
        // 2026-05-24 P0: from_existing_model doesn't propagate
        // n_rs_seq; first continuous-batched decode → Decode Error -3.
        // [[invariant_fast_short_recurrent_arch]]
        assert_eq!(
            fast_short_gate("qwen3moe", "APEX-I-Compact.Q4", no_env),
            FastShortGate::UnsafeRecurrent
        );
    }

    #[test]
    fn fast_short_skipped_for_mtp_by_name_on_non_recurrent_arch() {
        // The follow-up gap: Qwopus*-MTP on a plain qwen3 arch slipped
        // past the recurrent-arch-only check and crashed every
        // continuous-batched call. Name carries the signal.
        assert_eq!(
            fast_short_gate("qwen3", "Qwopus3.5-9B-MTP.Q8_0", no_env),
            FastShortGate::UnsafeMtp
        );
    }

    #[test]
    fn fast_short_safe_for_mtp_model_when_mtp_disabled_at_load() {
        // SOVEREIGN_MTP_DISABLE'd model runs single-token decode, so
        // FastShort's ctx never provisions speculative seq slots.
        assert_eq!(
            fast_short_gate(
                "qwen3",
                "Qwopus3.5-9B-MTP.Q8_0",
                env(&[("SOVEREIGN_MTP_DISABLE", "1")])
            ),
            FastShortGate::Safe
        );
    }

    #[test]
    fn fast_short_operator_disable_wins_over_everything() {
        assert_eq!(
            fast_short_gate(
                "qwen3moe",
                "Qwopus-MTP",
                env(&[("SOVEREIGN_FAST_SHORT_DISABLE", "true")])
            ),
            FastShortGate::Disabled
        );
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

    // ── compute_lcp ──────────────────────────────────────────────

    #[test]
    fn lcp_identical_prompt_reserves_last_token_for_fresh_decode() {
        // The sampler needs at least one fresh logit distribution:
        // an identical prompt must re-prefill exactly 1 token, never 0.
        let toks = [1, 2, 3, 4];
        let lcp = compute_lcp(&toks, &toks, true);
        assert_eq!(lcp.raw, 3, "raw LCP must stop at len-1 on identical prompts");
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
        assert!(forced_choice_candidates(&r).is_none(), "no marker → no sentinel");

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

    // ── env_flag_truthy ──────────────────────────────────────────

    #[test]
    fn env_flag_truthy_accepts_one_and_case_insensitive_true_only() {
        for v in ["1", "true", "TRUE", "True"] {
            assert!(env_flag_truthy(env(&[("F", v)]), "F"), "{v} must be truthy");
        }
        for v in ["0", "false", "yes", "on", ""] {
            assert!(!env_flag_truthy(env(&[("F", v)]), "F"), "{v} must NOT be truthy");
        }
        assert!(!env_flag_truthy(no_env, "F"));
    }
}
