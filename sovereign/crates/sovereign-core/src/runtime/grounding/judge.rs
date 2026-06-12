// SPDX-License-Identifier: AGPL-3.0-or-later
//! The gate's judges. Prompts are byte-identical to the bench critic
//! (`bench_cmd/live_runner.rs`) so the bench-calibrated threshold
//! transfers; divergence between the two is a bug in whichever
//! changed (same contract as sovereign-lint vs sovereign-test).

use std::sync::Arc;

use crate::traits::InferenceProvider;
use crate::types::{CompletionRequest, Speed};

use super::config::dbg;
use super::search::SealedEvidenceSearch;

/// Outcome of one gate pass, carried into message metadata so the
/// desktop can render provenance ("verified" / "regenerated" /
/// "abstained") and the bench can read what happened.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct GateVerdict {
    pub violation_prob: f64,
    /// The extracted claim the verdict is about (None = NO_CLAIM).
    pub claim: Option<String>,
    /// Claim-conditioned passages the sealed search returned for this
    /// claim (empty when no searcher / no hits). On a failed verdict
    /// these are the retry's correction material — the second draft
    /// gets the passages that state the truth, not just the news that
    /// its claim failed.
    #[serde(skip)]
    pub claim_evidence: Vec<String>,
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
    searcher: Option<&Arc<dyn SealedEvidenceSearch>>,
) -> Option<GateVerdict> {
    if answer.trim().is_empty() || chunks.is_empty() {
        return Some(GateVerdict { violation_prob: 0.0, claim: None, claim_evidence: Vec::new() });
    }
    if answer.chars().count() > 1_800 {
        tracing::info!(
            target: "grounding_gate",
            chars = answer.chars().count(),
            "long-form answer — out of gate scope"
        );
        return Some(GateVerdict { violation_prob: 0.0, claim: None, claim_evidence: Vec::new() });
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
                return Some(GateVerdict { violation_prob: 0.0, claim: None, claim_evidence: Vec::new() });
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

    // Claim-conditioned widening (Phase 3): verify against the sealed
    // evidence UNIVERSE, not just the prompt snapshot. Hits go first
    // (most relevant to THIS claim) and the cap widens by their count,
    // so they never displace a snapshot chunk the unwidened judge
    // would have checked. Measured motivation: a TRUE claim the
    // answer itself cited ("Brett Street") judged at max_support
    // 0.000 against 2 monolithic tool-result strings (attached lane,
    // 2026-06-11); the same shape as chat-lane distract-money-keeper
    // (correct answer abstained at vp 0.95).
    let extra: Vec<String> = match searcher {
        Some(s) => {
            let hits = s.search(&claim).await;
            if !hits.is_empty() {
                dbg(&format!(
                    "claim_search hits={} for {:?}",
                    hits.len(),
                    claim.chars().take(60).collect::<String>()
                ));
            }
            hits
        }
        None => Vec::new(),
    };
    // Rescue floor: a widened (claim-searched) hit may only raise
    // max_support when its support is DECISIVE — a passage that
    // states the claim (genuine rescues measure ~0.99; Brett Street
    // 0.999), not one that merely mentions its words. Without the
    // floor, each extra hit is another draw from the judge's noise
    // distribution and max() drifts up: measured 2026-06-11, the
    // fabricated "Professor's real name is Comrade Ossipon" rode a
    // 0.144 co-occurrence score from vp 0.96 to 0.856 — under τ —
    // and released. Prompt-snapshot chunks keep the old contract
    // (any support counts): they were the model's actual evidence.
    const CLAIM_RESCUE_FLOOR: f64 = 0.5;
    let judged: Vec<(bool, &String)> = extra
        .iter()
        .map(|c| (true, c))
        .chain(chunks.iter().map(|c| (false, c)))
        .collect();
    let cap = 12 + extra.len();
    let mut max_support: f64 = 0.0;
    let mut checked = 0usize;
    for (is_extra, c) in judged.into_iter().take(cap) {
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
            let effective = if is_extra && support < CLAIM_RESCUE_FLOOR {
                0.0
            } else {
                support
            };
            if effective > max_support {
                max_support = effective;
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
    Some(GateVerdict { violation_prob: vp, claim: Some(claim), claim_evidence: extra })
}


/// Extract up to 4 specific, checkable factual claims from a
/// long-form answer. Empty vec = nothing checkable (essay of analysis
/// / opinion) — passes ungated.
pub(super) async fn extract_claim_list(
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

pub(super) async fn claim_violation_joint(
    inference: &Arc<dyn InferenceProvider>,
    claim: &str,
    chunks: &[String],
    n_chunks: usize,
) -> Option<f64> {
    let joined: String = chunks
        .iter()
        .take(n_chunks)
        .map(|c| c.chars().take(1_500).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n---\n");
    let prompt = format!(
        "PASSAGES (multiple, separated by ---):\n\"\"\"\n{joined}\n\"\"\"\n\n\
         CLAIM: {claim}\n\n\
         Do the passages, taken together, state or clearly imply this claim? \
         Support assembled across several passages counts; paraphrase counts; \
         the passages merely mentioning the people or things involved, without \
         establishing the claimed connection, does NOT count.\n\n\
         Answer with exactly one letter — A = the passages support the claim, \
         B = they do not."
    );
    let (a, b) = forced_choice_ab(inference, &prompt).await?;
    let denom = a + b;
    let support = if denom > 0.0 { a / denom } else { 0.0 };
    Some(1.0 - support)
}
