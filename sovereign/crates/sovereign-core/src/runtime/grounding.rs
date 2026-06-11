// SPDX-License-Identifier: AGPL-3.0-or-later
//! Production grounding gate — the runtime port of the chaos-bench
//! critic (`bench_cmd/live_runner.rs::verify_grounding`), wired into
//! the KQ synthesis stream as **hold → gate → retry → abstain**.
//!
//! Mechanism (validated on two corpora, 2026-06-11): the synthesized
//! answer's single central claim is extracted (one small completion),
//! then checked per-retrieved-chunk with a forced-choice logprob pass
//! ("does THIS passage support THIS claim"); `violation_prob = 1 −
//! max(per-chunk support)`. On the contamination-free holdout bank the
//! verdicts separated cleanly: fabricated claims scored 0.96–1.00,
//! the highest-scoring CORRECT answer 0.45 — and every gated answer on
//! both banks contained genuine confabulation (zero false positives).
//!
//! Gate semantics, in order:
//!   1. `vp < τ`  → release the answer unchanged.
//!   2. `vp ≥ τ`  → ONE retry: re-synthesize with the failed claim
//!      quoted back as a constraint (minimal best-of-N — the second
//!      draft knows exactly which assertion failed verification).
//!   3. retry still `≥ τ` → release a grounded abstention instead.
//!      The user gets "your sources don't establish this" — never the
//!      confabulation.
//!
//! Env surface: `SOVEREIGN_GROUNDING_GATE=1` turns the gate on
//! (default off — A/B-able, desktop opt-in); `SOVEREIGN_GV_THRESHOLD`
//! tunes τ (default 0.9, from the dual-bank shadow sweeps).
//!
//! Scope guards (same as the bench critic): long-form answers
//! (>1800 chars) pass ungated — one extracted claim is the wrong
//! instrument for an essay; declines and explicitly GK-attributed
//! answers extract as NO_CLAIM and pass (the honest OOD-caveat case
//! must not be gated).

use std::sync::Arc;

use crate::traits::InferenceProvider;
use crate::types::{CompletionRequest, Speed};

/// Stderr mirror for bench/CLI surfaces that install no tracing
/// subscriber — same pattern (and same env var) as the agentic
/// loop's dbg().
pub(crate) fn dbg(msg: &str) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let on = *ON.get_or_init(|| {
        std::env::var("SOVEREIGN_AGENTIC_KQ_DEBUG")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    });
    if on {
        eprintln!("    [gate] {msg}");
    }
}

pub(crate) fn grounding_gate_enabled() -> bool {
    std::env::var("SOVEREIGN_GROUNDING_GATE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub(crate) fn grounding_gate_threshold() -> f64 {
    std::env::var("SOVEREIGN_GV_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.9)
}

/// Outcome of one gate pass, carried into message metadata so the
/// desktop can render provenance ("verified" / "regenerated" /
/// "abstained") and the bench can read what happened.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct GateVerdict {
    pub violation_prob: f64,
    /// The extracted claim the verdict is about (None = NO_CLAIM).
    pub claim: Option<String>,
}

/// The grounded abstention released when both drafts fail the gate.
/// Names what was claimed and what was checked — glassbox, not a bare
/// refusal.
pub(crate) fn grounded_abstention(claim: &str, chunks_checked: usize) -> String {
    format!(
        "Your sources don't establish this. The draft answer asserted that {claim} \
         but none of the {chunks_checked} retrieved passages support that, so I'm \
         not presenting it as fact. If this is in your sources somewhere, try \
         rephrasing with the specific names or terms involved; otherwise this may \
         simply not be recorded there.",
        claim = claim.trim_end_matches('.'),
    )
}

/// System-message suffix for the single gated retry. Quotes the failed
/// claim back — the second draft knows exactly which assertion failed
/// verification and must either ground it or drop it.
pub(crate) fn retry_system_note(claim: &str) -> String {
    format!(
        "\n\nGROUNDING CHECK FAILED on your previous draft. It asserted: \"{claim}\" — \
         no retrieved passage supports that assertion. Write a new answer using ONLY \
         what the passages state. If the passages do not contain the asked-for fact, \
         say plainly that the sources do not state it. Do not repeat the unsupported \
         assertion."
    )
}

/// One forced-choice A/B logprob pass on the primary (Critic) tier. Returns
/// `(p_A, p_B)`.
async fn forced_choice_ab(
    inference: &Arc<dyn InferenceProvider>,
    prompt: &str,
) -> Option<(f64, f64)> {
    let req = CompletionRequest {
        prompt: prompt.to_string(),
        system_message: Some("You are a careful classifier. Answer with a single letter.".into()),
        // Critic role runs on the PRIMARY tier (role.rs: "a model
        // grading its own single pass is self-confirmation bias"; the
        // 4B's support distributions are squashed — measured 0.42-0.76
        // on known fabrications vs the primary critic's 0.96-0.98).
        preferred_speed: Speed::Medium,
        model_id: Some("primary".into()),
        max_tokens: Some(1),
        structured_output: Some(serde_json::json!({
            "type": "string", "enum": ["A", "B"], "x_forced_choice": true
        })),
        think_budget: Some(0),
        enable_thinking: Some(false),
        temperature: Some(0.0),
        ..Default::default()
    };
    match inference.complete(&req).await {
        Ok(resp) => {
            let m: std::collections::HashMap<String, f64> =
                serde_json::from_str(resp.text.trim()).ok()?;
            Some((
                m.get("A").copied().unwrap_or(0.0),
                m.get("B").copied().unwrap_or(0.0),
            ))
        }
        Err(e) => {
            tracing::warn!(target: "grounding_gate", error = %e, "forced-choice pass failed");
            dbg(&format!("forced-choice failed: {e}"));
            None
        }
    }
}

/// Two-step external grounding verifier (claim extraction → per-chunk
/// forced-choice support). Returns `None` on judge failure (caller
/// must FAIL OPEN — release the answer; the gate is a quality lever,
/// not an availability risk). `violation_prob` semantics and prompts
/// are byte-identical to the bench critic so the bench-calibrated
/// threshold transfers; divergence between the two is a bug in
/// whichever changed (same contract as sovereign-lint vs sovereign-test).
pub(crate) async fn verify_grounding(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    answer: &str,
    chunks: &[String],
    entity_anchored: bool,
) -> Option<GateVerdict> {
    if answer.trim().is_empty() || chunks.is_empty() {
        return Some(GateVerdict { violation_prob: 0.0, claim: None });
    }
    if answer.chars().count() > 1_800 {
        tracing::info!(
            target: "grounding_gate",
            chars = answer.chars().count(),
            "long-form answer — out of gate scope"
        );
        return Some(GateVerdict { violation_prob: 0.0, claim: None });
    }
    // The GK-attribution exemption is sound for world-general
    // questions (a caveated "capital of Australia" answer is the
    // honest shape and must not be gated) but UNSOUND for in-world
    // (entity-anchored) ones: outside knowledge structurally cannot
    // establish a fact about the corpus's own world, so a GK-caveated
    // in-world assertion is a fabrication in honest clothing and must
    // still be extracted and verified (measured: a gated retry
    // re-asserted the same invented first name wearing the caveat and
    // slipped through the exemption).
    let no_claim_rule = if entity_anchored {
        "Reply with exactly NO_CLAIM if the assistant declined or said the \
         information is not in its sources. If the assistant asserted a fact \
         while attributing it to general knowledge, still state that claim."
    } else {
        "Reply with exactly NO_CLAIM if the assistant declined, said the information \
         is not in its sources, or explicitly attributed the fact to general \
         knowledge rather than the sources."
    };
    let claim_prompt = format!(
        "A user asked: {}\n\nAn assistant answered:\n\"\"\"\n{}\n\"\"\"\n\n\
         State the single central factual claim the assistant asserts as its answer, \
         as one short standalone sentence that names BOTH sides of the relation \
         (who/what is claimed to be/do what). Do not add qualifiers or sources.\n\
         {no_claim_rule}",
        question.chars().take(400).collect::<String>(),
        answer.chars().take(2000).collect::<String>(),
    );
    let claim_req = CompletionRequest {
        prompt: claim_prompt,
        system_message: Some(
            "You extract claims precisely. Reply with one sentence or NO_CLAIM.".into(),
        ),
        preferred_speed: Speed::Medium,
        model_id: Some("primary".into()),
        max_tokens: Some(64),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        ..Default::default()
    };
    let claim = match inference.complete(&claim_req).await {
        Ok(resp) => {
            let t = resp.text.trim().to_string();
            if t.is_empty() || t.to_uppercase().contains("NO_CLAIM") {
                tracing::info!(target: "grounding_gate", "claim=NO_CLAIM → vp=0");
                dbg("claim=NO_CLAIM → vp=0");
                return Some(GateVerdict { violation_prob: 0.0, claim: None });
            }
            dbg(&format!("claim={:?}", t.chars().take(90).collect::<String>()));
            t
        }
        Err(e) => {
            tracing::warn!(target: "grounding_gate", error = %e, "claim extraction failed");
            dbg(&format!("claim extraction failed: {e}"));
            return None;
        }
    };

    let mut max_support: f64 = 0.0;
    let mut checked = 0usize;
    for c in chunks.iter().take(12) {
        let passage: String = c.chars().take(2_400).collect();
        let prompt = format!(
            "PASSAGE:\n\"\"\"\n{passage}\n\"\"\"\n\n\
             CLAIM: {claim}\n\n\
             Does the passage state or clearly imply this claim? Paraphrase counts; \
             the passage merely mentioning the people or things involved, without \
             establishing the claimed connection between them, does NOT count.\n\n\
             Answer with exactly one letter — A = the passage supports the claim, \
             B = it does not."
        );
        if let Some((a, b)) = forced_choice_ab(inference, &prompt).await {
            let denom = a + b;
            let support = if denom > 0.0 { a / denom } else { 0.0 };
            if support > max_support {
                max_support = support;
            }
            checked += 1;
            if max_support >= 0.95 {
                break;
            }
        }
    }
    if checked == 0 {
        dbg("no support checks completed — judge unavailable, failing open");
        return None;
    }
    let vp = 1.0 - max_support;
    tracing::info!(
        target: "grounding_gate",
        claim = %claim.chars().take(90).collect::<String>(),
        chunks_checked = checked,
        max_support = format!("{max_support:.3}").as_str(),
        violation_prob = format!("{vp:.3}").as_str(),
        "grounding verdict"
    );
    dbg(&format!("chunks_checked={checked} max_support={max_support:.3} vp={vp:.3}"));
    Some(GateVerdict { violation_prob: vp, claim: Some(claim) })
}

/// Final outcome of a full gate ladder over one draft answer.
pub(crate) struct GateOutcome {
    pub text: String,
    /// `grounding_gate` metadata for the message (action, retried,
    /// violation_prob / failed_claims, threshold).
    pub meta: serde_json::Value,
}

/// The complete gate ladder, shared by every synthesis surface
/// (streaming KQ, non-streaming KQ, DeepQuery): short answers go
/// through the single-claim verify → retry → abstain ladder; long-form
/// answers (>1800 chars) go through the per-claim audit → rewrite →
/// annotate ladder. Fail-open on judge failure everywhere — the gate
/// is a quality lever, not an availability risk.
pub(crate) async fn gate_answer(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    draft: String,
    chunks: &[String],
    entity_anchored: bool,
    base_request: &CompletionRequest,
) -> GateOutcome {
    let tau = grounding_gate_threshold();
    if draft.chars().count() > 1_800 {
        return gate_longform(inference, question, draft, chunks, base_request, tau).await;
    }
    let mut text = draft;
    let mut action = "released";
    let mut retried = false;
    let mut final_vp: Option<f64> = None;
    match verify_grounding(inference, question, &text, chunks, entity_anchored).await {
        Some(v) => {
            final_vp = Some(v.violation_prob);
            if v.violation_prob >= tau {
                if let Some(claim) = v.claim.clone() {
                    retried = true;
                    let mut retry_req = base_request.clone();
                    let base_sys = retry_req.system_message.clone().unwrap_or_default();
                    retry_req.system_message =
                        Some(format!("{base_sys}{}", retry_system_note(&claim)));
                    retry_req.assistant_prefix = None;
                    match inference.complete(&retry_req).await {
                        Ok(resp) => {
                            let second = resp.text;
                            match verify_grounding(
                                inference,
                                question,
                                &second,
                                chunks,
                                entity_anchored,
                            )
                            .await
                            {
                                Some(v2) if v2.violation_prob < tau => {
                                    final_vp = Some(v2.violation_prob);
                                    text = second;
                                    action = "retry_released";
                                }
                                Some(v2) => {
                                    final_vp = Some(v2.violation_prob);
                                    text = grounded_abstention(&claim, chunks.len().min(12));
                                    action = "abstained";
                                }
                                None => {
                                    // Retry verdict unavailable — fail open
                                    // on the retry (written under the
                                    // grounding constraint; safer than
                                    // draft 1).
                                    text = second;
                                    action = "retry_released_unverified";
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                target: "grounding_gate",
                                error = %e,
                                "gated retry synthesis failed — releasing abstention"
                            );
                            text = grounded_abstention(&claim, chunks.len().min(12));
                            action = "abstained_retry_error";
                        }
                    }
                }
            }
        }
        None => {
            action = "judge_failed_open";
        }
    }
    dbg(&format!("verdict action={action} retried={retried} vp={final_vp:?} tau={tau}"));
    tracing::info!(
        target: "grounding_gate",
        action,
        retried,
        vp = ?final_vp,
        tau,
        "grounding gate verdict"
    );
    GateOutcome {
        text,
        meta: serde_json::json!({
            "action": action,
            "retried": retried,
            "violation_prob": final_vp,
            "threshold": tau,
            "mode": "single_claim",
        }),
    }
}

/// Per-chunk forced-choice support for ONE claim over the top of the
/// chunk set; returns the violation probability. Shared by the
/// long-form audit (the single-claim path keeps its inline loop in
/// `verify_grounding` to preserve byte-identical bench parity).
async fn claim_violation(
    inference: &Arc<dyn InferenceProvider>,
    claim: &str,
    chunks: &[String],
    per_claim_chunks: usize,
) -> Option<f64> {
    let mut max_support: f64 = 0.0;
    let mut checked = 0usize;
    for c in chunks.iter().take(per_claim_chunks) {
        let passage: String = c.chars().take(2_400).collect();
        let prompt = format!(
            "PASSAGE:\n\"\"\"\n{passage}\n\"\"\"\n\n\
             CLAIM: {claim}\n\n\
             Does the passage state or clearly imply this claim? Paraphrase counts; \
             the passage merely mentioning the people or things involved, without \
             establishing the claimed connection between them, does NOT count.\n\n\
             Answer with exactly one letter — A = the passage supports the claim, \
             B = it does not."
        );
        if let Some((a, b)) = forced_choice_ab(inference, &prompt).await {
            let denom = a + b;
            let support = if denom > 0.0 { a / denom } else { 0.0 };
            if support > max_support {
                max_support = support;
            }
            checked += 1;
            if max_support >= 0.95 {
                break;
            }
        }
    }
    if checked == 0 {
        return None;
    }
    Some(1.0 - max_support)
}

/// Extract up to 4 specific, checkable factual claims from a
/// long-form answer. Empty vec = nothing checkable (essay of analysis
/// / opinion) — passes ungated.
async fn extract_claim_list(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    answer: &str,
) -> Option<Vec<String>> {
    let prompt = format!(
        "A user asked: {}\n\nAn assistant wrote this long answer:\n\"\"\"\n{}\n\"\"\"\n\n\
         List the SPECIFIC factual claims the answer asserts — concrete who/what/when \
         relations a passage could confirm or refute (names, identifications, events, \
         attributions). One claim per line, each a short standalone sentence naming \
         both sides of the relation. At most 4 lines; pick the most load-bearing \
         claims. Skip opinions, summaries of the question, and anything the answer \
         itself flags as not from the sources.\n\
         Reply with exactly NO_CLAIM if there are no such checkable claims.",
        question.chars().take(400).collect::<String>(),
        answer.chars().take(6000).collect::<String>(),
    );
    let req = CompletionRequest {
        prompt,
        system_message: Some(
            "You extract claims precisely. Reply with up to 4 lines, or NO_CLAIM.".into(),
        ),
        preferred_speed: Speed::Medium,
        model_id: Some("primary".into()),
        max_tokens: Some(160),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        ..Default::default()
    };
    match inference.complete(&req).await {
        Ok(resp) => {
            let t = resp.text.trim();
            if t.is_empty() || t.to_uppercase().contains("NO_CLAIM") {
                return Some(Vec::new());
            }
            Some(
                t.lines()
                    .map(|l| l.trim().trim_start_matches(['-', '*', '•']).trim())
                    .map(|l| {
                        // strip "1." / "2)" enumeration heads
                        l.trim_start_matches(|c: char| c.is_ascii_digit())
                            .trim_start_matches(['.', ')'])
                            .trim()
                            .to_string()
                    })
                    .filter(|l| l.len() > 12)
                    .take(4)
                    .collect(),
            )
        }
        Err(e) => {
            tracing::warn!(target: "grounding_gate", error = %e, "claim-list extraction failed");
            None
        }
    }
}

/// Rewrite-request system note listing every failed claim.
fn rewrite_system_note(failed: &[String]) -> String {
    let list = failed
        .iter()
        .map(|c| format!("- \"{c}\""))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "\n\nGROUNDING AUDIT FAILED on your previous draft. These assertions are not \
         supported by any retrieved passage:\n{list}\n\
         Rewrite the answer: keep everything the passages support, and for each \
         unsupported assertion either remove it or replace it with what the passages \
         actually state. If a fact is simply not in the passages, say so rather than \
         asserting it."
    )
}

/// Long-form ladder: per-claim audit → one rewrite → annotate.
/// An essay with one bad claim is REWRITTEN, not abstained; if the
/// rewrite still carries unsupported claims, they are listed in a
/// visible verification note appended to the answer — the reader sees
/// exactly which assertions didn't verify, instead of either losing
/// the whole essay or trusting it blind.
async fn gate_longform(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    draft: String,
    chunks: &[String],
    base_request: &CompletionRequest,
    tau: f64,
) -> GateOutcome {
    const PER_CLAIM_CHUNKS: usize = 6;
    let audit = |text: String| {
        let inference = inference.clone();
        async move {
            let claims = extract_claim_list(&inference, question, &text).await?;
            let mut failed: Vec<String> = Vec::new();
            for claim in &claims {
                match claim_violation(&inference, claim, chunks, PER_CLAIM_CHUNKS).await {
                    Some(vp) => {
                        dbg(&format!("longform claim vp={vp:.3} {claim:?}"));
                        if vp >= tau {
                            failed.push(claim.clone());
                        }
                    }
                    None => {} // unverifiable claim — fail open per claim
                }
            }
            Some((text, claims.len(), failed))
        }
    };

    let draft_backup = draft.clone();
    let Some((text, n_claims, failed)) = audit(draft).await else {
        // Claim-list extraction failed — fail open with the draft.
        return GateOutcome {
            text: draft_backup,
            meta: serde_json::json!({
                "action": "judge_failed_open", "retried": false,
                "threshold": tau, "mode": "per_claim",
            }),
        };
    };
    if failed.is_empty() {
        dbg(&format!("longform released claims={n_claims} failed=0"));
        return GateOutcome {
            text,
            meta: serde_json::json!({
                "action": "released", "retried": false,
                "claims_checked": n_claims, "failed_claims": [],
                "threshold": tau, "mode": "per_claim",
            }),
        };
    }
    dbg(&format!("longform rewrite: {} failed of {n_claims}", failed.len()));
    let mut rewrite_req = base_request.clone();
    let base_sys = rewrite_req.system_message.clone().unwrap_or_default();
    rewrite_req.system_message = Some(format!("{base_sys}{}", rewrite_system_note(&failed)));
    rewrite_req.assistant_prefix = None;
    match inference.complete(&rewrite_req).await {
        Ok(resp) => {
            let second = resp.text;
            let second_backup = second.clone();
            match audit(second).await {
                Some((text2, n2, failed2)) if failed2.is_empty() => GateOutcome {
                    text: text2,
                    meta: serde_json::json!({
                        "action": "rewrite_released", "retried": true,
                        "claims_checked": n2, "failed_claims": [],
                        "threshold": tau, "mode": "per_claim",
                    }),
                },
                Some((text2, n2, failed2)) => {
                    let note = format!(
                        "\n\n---\n*Verification note: the following could not be \
                         confirmed against your sources — treat as unverified:*\n{}",
                        failed2
                            .iter()
                            .map(|c| format!("- {c}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                    GateOutcome {
                        text: format!("{text2}{note}"),
                        meta: serde_json::json!({
                            "action": "rewrite_annotated", "retried": true,
                            "claims_checked": n2, "failed_claims": failed2,
                            "threshold": tau, "mode": "per_claim",
                        }),
                    }
                }
                None => GateOutcome {
                    text: second_backup,
                    meta: serde_json::json!({
                        "action": "rewrite_released_unverified", "retried": true,
                        "threshold": tau, "mode": "per_claim",
                    }),
                },
            }
        }
        Err(e) => {
            // Rewrite unavailable: release draft 1 WITH the visible
            // verification note (never silently release known-failed
            // claims; never destroy an essay over judge availability).
            tracing::warn!(target: "grounding_gate", error = %e, "longform rewrite failed — annotating draft");
            let note = format!(
                "\n\n---\n*Verification note: the following could not be \
                 confirmed against your sources — treat as unverified:*\n{}",
                failed
                    .iter()
                    .map(|c| format!("- {c}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            GateOutcome {
                text: format!("{text}{note}"),
                meta: serde_json::json!({
                    "action": "annotated_rewrite_error", "retried": false,
                    "claims_checked": n_claims, "failed_claims": failed,
                    "threshold": tau, "mode": "per_claim",
                }),
            }
        }
    }
}
