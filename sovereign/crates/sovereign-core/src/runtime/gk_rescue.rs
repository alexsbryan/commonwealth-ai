// SPDX-License-Identifier: AGPL-3.0-or-later
//! OOD general-knowledge rescue — the "profoundly helpful" half of the
//! epistemic bar (EPISTEMIC_STATE.md §1: never a dead end).
//!
//! When a gated turn ABSTAINS and the coverage probe says
//! `TopicUncovered` — the enabled corpora have no region near the
//! question — verifying a parametric answer against that evidence is a
//! category error: the chunks can neither support nor refute it. The
//! honest, helpful move (the chaos bench's HYBRID ideal) is a caveated
//! parametric answer: "Not in your sources — from general knowledge: …"
//! plus the ledger's acquisition routes.
//!
//! The probe verdict is the discriminator the 2026-07-01 exactval fix
//! lacked: it closed the GK-caveat exemption because labelled-but-
//! confident IN-WORLD fabrications rode it ("Winnie's former lover was
//! Eddie Henderson"). Those turns probe `ClaimUncovered` (in-topic,
//! ~0.71 nearest-sim) and are structurally NEVER rescued here; only
//! off-topic turns (0.17–0.49) qualify. Callers must also exclude
//! entity-anchored / corpus-deictic questions.
//!
//! June opus chaos runs answered 5/5 OOD probes with the caveat; the
//! small local slot declines instead (measured 1/5, 2026-07-20). The
//! rescue restores the behavior as a pipeline property rather than a
//! model property — whatever model runs inside (§1).

use crate::traits::InferenceProvider;
use crate::types::{CompletionRequest, Speed};

/// `SOVEREIGN_GK_RESCUE=0|false|off|no` disables the rescue (the
/// abstention then ships as-is). Default ON.
pub(crate) fn gk_rescue_enabled() -> bool {
    !matches!(
        std::env::var("SOVEREIGN_GK_RESCUE")
            .unwrap_or_default()
            .to_lowercase()
            .as_str(),
        "0" | "false" | "off" | "no"
    )
}

/// Output budget for the rescue — a caveated parametric answer, not an
/// essay. The caveat prefix is decode-committed, so the budget is all
/// content.
const RESCUE_MAX_TOKENS: u32 = 320;

/// Synthesize the caveated parametric answer for a probe-confirmed
/// out-of-domain question. Returns the FULL user-facing text (caveat
/// prefix prepended — `assistant_prefix` is decode-commit only, per the
/// GK_CAVEAT_PREFIX convention) or `None` when the rescue declined,
/// errored, or produced nothing worth shipping — the caller then keeps
/// the original abstention. Never a second rescue, never a retry: one
/// call, fail-open to the abstention.
pub(crate) async fn rescue_ood_answer(
    inference: &dyn InferenceProvider,
    question: &str,
) -> Option<String> {
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let q: String = question.chars().take(600).collect();
    let request = CompletionRequest {
        prompt: format!(
            "The user's connected sources don't cover this question. Answer it \
             from your general knowledge, briefly and directly:\n\n{q}"
        ),
        system_message: Some(format!(
            "Current date: {today}. Answer concisely from general knowledge. \
             If the answer is time-sensitive and may have changed, say so. If \
             you genuinely do not know, reply with exactly: UNKNOWN"
        )),
        preferred_speed: Speed::Slow,
        max_tokens: Some(RESCUE_MAX_TOKENS as usize),
        temperature: Some(0.2),
        think_budget: None,
        structured_output: None,
        top_k: None,
        top_p: None,
        oicp: None,
        tools: None,
        tool_choice: None,
        model_id: None,
        enable_thinking: None,
        sampling_mode: None,
        // Decode-commit the caveat: instruction-only caveat compliance
        // measured ~60% (the GK_CAVEAT_PREFIX precedent) — committing
        // the opening makes the provenance flag structural.
        assistant_prefix: Some(crate::runtime::prompts::GK_CAVEAT_PREFIX.to_string()),
        cmd_prefix: None,
        url_allowlist: None,
        evidence_id_allowlist: None,
        lark_grammar: None,
    };
    let resp = inference.complete(&request).await.ok()?;
    let body = crate::title::strip_think_blocks(&resp.text);
    let body = body.trim();
    // The model saying it doesn't know (or echoing a decline) is a
    // legitimate outcome — ship the ORIGINAL abstention, not a caveat
    // wrapping a non-answer.
    let low = body.to_lowercase();
    if body.is_empty()
        || body.len() < 8
        || body.to_uppercase().contains("UNKNOWN")
        || low.contains("i don't know")
        || low.contains("i do not know")
        || low.contains("don't have reliable information")
        || low.contains("do not have reliable information")
    {
        return None;
    }
    Some(format!(
        "{}{}",
        crate::runtime::prompts::GK_CAVEAT_PREFIX,
        body
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_switch_parses() {
        // Default (unset in the test env) is ON.
        assert!(gk_rescue_enabled());
    }
}
