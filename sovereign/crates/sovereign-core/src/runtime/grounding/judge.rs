// SPDX-License-Identifier: AGPL-3.0-or-later
//! The gate's judges. Both registers the bench critic
//! (`bench_cmd/live_runner.rs`) runs are rendered HERE and called from
//! there, so the bench-calibrated threshold transfers by construction
//! rather than by convention:
//!
//!   step 1  [`claim_extraction_prompt`] + [`CLAIM_EXTRACTION_SYSTEM`]
//!   step 2  [`chunk_judge_prompt`] + [`CHUNK_JUDGE_SYSTEM`]
//!
//! Step 2 was unified 2026-08-13. Step 1 was left as a duplicate literal
//! in two crates and had DIVERGED by the time anyone checked: production
//! grew the `entity_anchored` branch while the bench copy kept the
//! unanchored rule, so tau was calibrated on a prompt production does not
//! send for entity-anchored turns (measured 2026-08-19). Unified now —
//! the compiler enforces it, so this comment cannot go stale the way the
//! last one did.

use std::sync::Arc;

use crate::oicp::ShardingPrivacy;
use crate::slot_policy::Workload;
use crate::traits::InferenceProvider;
use crate::types::{CompletionRequest, Speed};

use super::call_census::gate_call;
use super::config::dbg;
use super::search::SealedEvidenceSearch;
use sovereign_contracts::types::GateCallMechanism;

/// Outcome of one gate pass, carried into message metadata so the
/// desktop can render provenance ("verified" / "regenerated" /
/// "abstained") and the bench can read what happened.
/// Why this verdict has the `violation_prob` it has.
///
/// `violation_prob = 0.0` is returned by three structurally different
/// paths, and collapsing them is how a turn the gate NEVER RAN ON was
/// reported to the UI as `Supported` (measured 2026-08-19: 44.3% of
/// banked gate rows sit at exactly 0.0, of which the long-form
/// short-circuit alone is 15.6%). Absence is reported, never defaulted
/// — ARCH §18.3, and §18.1's "four verdicts, not two".
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClaimCheckOutcome {
    /// Gate did not run: no answer text, or no evidence to check against.
    NotEvaluatedNoInput,
    /// Gate did not run: long-form answer, outside the single-claim
    /// gate's scope. `violation_prob` is a placeholder, NOT a measurement.
    NotEvaluatedLongForm,
    /// Nothing to check: the assistant declined, or asserted no
    /// world-claim. An HONESTY SUCCESS — not a clean bill of health on
    /// a claim that was examined.
    NoClaim,
    /// A claim was extracted and checked. `violation_prob` is a real
    /// measurement and `tau` applies to it.
    Measured,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct GateVerdict {
    pub violation_prob: f64,
    /// Why `violation_prob` is what it is. Read this before comparing
    /// `violation_prob` to `tau` — on a non-`Measured` outcome the
    /// comparison is meaningless.
    pub outcome: ClaimCheckOutcome,
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
/// `(p_A, p_B)`. `stable_prefix_len` declares how many leading BYTES of
/// `prompt` are byte-identical across sibling calls (the shared evidence
/// window of a per-claim gate pass) so the engine's pinned-prefix cache can
/// checkpoint/restore there instead of re-prefilling — `None` for one-off
/// prompts.
///
/// `mechanism` names which judge is asking — the two callers
/// ([`claim_violation_joint`] over the shared window, [`claim_chunk_support`]
/// over one passage) have very different prefill shapes, and a census that
/// could not tell them apart is the blindness `call_census` exists to end.
async fn forced_choice_ab(
    inference: &Arc<dyn InferenceProvider>,
    prompt: &str,
    stable_prefix_len: Option<usize>,
    posture: ShardingPrivacy,
    mechanism: GateCallMechanism,
) -> Option<(f64, f64)> {
    let req = CompletionRequest {
        prompt: prompt.to_string(),
        stable_prefix_len,
        system_message: Some(CHUNK_JUDGE_SYSTEM.into()),
        // Critic role runs on the PRIMARY tier (role.rs: "a model
        // grading its own single pass is self-confirmation bias"; the
        // 4B's support distributions are squashed — measured 0.42-0.76
        // on known fabrications vs the primary critic's 0.96-0.98).
        preferred_speed: Speed::Slow,
        // SLOT_POLICY §7: route the Critic through the privacy-gated OICP
        // path instead of pinning `model_id: "primary"`. The pin was a
        // latent privacy hole — `primary` is a mesh-advertised alias and
        // `locate_named_model` load-balances named models across peers
        // with no privacy check, so a pinned judge could cross the network
        // on a LocalOnly turn. The Judge envelope carries the session's
        // sharding posture, so offload happens only when the turn allows.
        oicp: Some(Workload::Judge.requirements(posture)),
        max_tokens: Some(1),
        structured_output: Some(serde_json::json!({
            "type": "string", "enum": ["A", "B"], "x_forced_choice": true
        })),
        think_budget: Some(0),
        enable_thinking: Some(false),
        temperature: Some(0.0),
        ..Default::default()
    };
    match gate_call(&**inference, &req, mechanism).await {
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
    posture: ShardingPrivacy,
) -> Option<GateVerdict> {
    if answer.trim().is_empty() || chunks.is_empty() {
        return Some(GateVerdict {
            violation_prob: 0.0,
            outcome: ClaimCheckOutcome::NotEvaluatedNoInput,
            claim: None,
            claim_evidence: Vec::new(),
        });
    }
    if answer.chars().count() > 1_800 {
        tracing::info!(
            target: "grounding_gate",
            chars = answer.chars().count(),
            "long-form answer — out of gate scope"
        );
        return Some(GateVerdict {
            violation_prob: 0.0,
            outcome: ClaimCheckOutcome::NotEvaluatedLongForm,
            claim: None,
            claim_evidence: Vec::new(),
        });
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
    let claim_prompt = claim_extraction_prompt(question, answer, entity_anchored);
    let claim_req = CompletionRequest {
        prompt: claim_prompt,
        system_message: Some(CLAIM_EXTRACTION_SYSTEM.into()),
        preferred_speed: Speed::Slow,
        // SLOT_POLICY §7: route the Critic through the privacy-gated OICP
        // path instead of pinning `model_id: "primary"`. The pin was a
        // latent privacy hole — `primary` is a mesh-advertised alias and
        // `locate_named_model` load-balances named models across peers
        // with no privacy check, so a pinned judge could cross the network
        // on a LocalOnly turn. The Judge envelope carries the session's
        // sharding posture, so offload happens only when the turn allows.
        oicp: Some(Workload::Judge.requirements(posture)),
        max_tokens: Some(64),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        ..Default::default()
    };
    let claim = match gate_call(&**inference, &claim_req, GateCallMechanism::ClaimExtraction).await
    {
        Ok(resp) => {
            let t = resp.text.trim().to_string();
            if t.is_empty() || t.to_uppercase().contains("NO_CLAIM") {
                tracing::info!(target: "grounding_gate", "claim=NO_CLAIM → vp=0");
                dbg("claim=NO_CLAIM → vp=0");
                return Some(GateVerdict {
                    violation_prob: 0.0,
                    outcome: ClaimCheckOutcome::NoClaim,
                    claim: None,
                    claim_evidence: Vec::new(),
                });
            }
            dbg(&format!(
                "claim={:?}",
                t.chars().take(90).collect::<String>()
            ));
            t
        }
        Err(e) => {
            tracing::warn!(target: "grounding_gate", error = %e, "claim extraction failed");
            dbg(&format!("claim extraction failed: {e}"));
            return None;
        }
    };

    // Jurisdiction scalpel: the extractor's NO_CLAIM exemption is
    // LLM-mediated and misses declines that carry an explanatory rider —
    // it then dutifully extracts the rider as "the central claim". When
    // that rider is meta-language about the evidence/system (not a
    // world-claim), auditing it is out of the gate's jurisdiction; treat
    // it as NO_CLAIM deterministically. See `decline_rider_exempt`.
    if decline_rider_exempt(answer, &claim) {
        tracing::info!(
            target: "grounding_gate",
            claim = %claim.chars().take(90).collect::<String>(),
            "claim is a decline meta-rider — exempt (jurisdiction) → vp=0"
        );
        dbg("claim is a decline meta-rider → NO_CLAIM → vp=0");
        return Some(GateVerdict {
            violation_prob: 0.0,
            outcome: ClaimCheckOutcome::NoClaim,
            claim: None,
            claim_evidence: Vec::new(),
        });
    }

    // First-principles fix for entity-anchored (in-world) questions. The
    // per-passage support loop below is CONFIRMATORY ("does this passage support
    // claim X?"), and a small forced-choice judge has a yes-bias: it grounds a
    // fabrication whose value is a real corpus token in a DIFFERENT role ("Mr
    // Vladimir's first name is Vladimir") or a partly-true claim ("the Russian
    // embassy"). A strict EXTRACTIVE check ("does the corpus STATE the answer?")
    // over-corrects: it requires role-INFERENCE ("does the text state Yundt's
    // first name is Karl?") and so abstains a CORRECT answer the corpus only
    // implies — "Karl Yundt" names him but never says "his first name is Karl".
    //
    // The right bar is BLATANT confabulation, at the highest generalization: did
    // the claim assert a specific (name/place/number) that appears NOWHERE in the
    // evidence — invented from nothing (Heat's "Vernon", the "Russian" embassy,
    // the Professor's "Stepanovich Haldin")? That, and only that, is the failure.
    // A value-present-but-mis-roled answer ("Vladimir" for Mr Vladimir's first
    // name) or an implied-but-correct one ("Karl" from "Karl Yundt") is the
    // system's best effort, not a fabrication — release it. So we check TOKEN
    // PRESENCE of the answer's specific value ("is 'Karl' anywhere in the
    // passages?" — yes, inside "Karl Yundt"; "is 'Vernon'?" — no), NOT whether
    // the text states the role, sidestepping the inference that makes extractive
    // over-abstain. Two steps: an LLM extracts the answer's value (the one job a
    // judge does reliably here), then a DETERMINISTIC substring test decides
    // presence — measured more reliable than asking the judge to presence-check
    // (a forced-choice judge false-positived an absent "Thomas"; substring can't)
    // and than a gestalt "list the claim's absent specifics" (the frame drowns
    // the one invented token: it missed "Russian" in "the Russian embassy").
    if entity_anchored {
        use super::value_presence::{assess_asserted_value, AssertedValue};
        match assess_asserted_value(&**inference, question, answer, chunks, posture).await {
            AssertedValue::Grounded(value) => {
                dbg(&format!(
                    "value-presence: {value:?} present in corpus → vp=0.0 (release best-effort)"
                ));
                return Some(GateVerdict {
                    violation_prob: 0.0,
                    outcome: ClaimCheckOutcome::Measured,
                    claim: Some(claim),
                    claim_evidence: Vec::new(),
                });
            }
            AssertedValue::Ungrounded(value) => {
                tracing::info!(
                    target: "grounding_gate",
                    value = %value,
                    claim = %claim.chars().take(90).collect::<String>(),
                    "value-presence: the answer's specific is absent from the corpus → vp=1.0"
                );
                dbg(&format!(
                    "value-presence: {value:?} absent from corpus → vp=1.0 (blatant confab)"
                ));
                return Some(GateVerdict {
                    violation_prob: 1.0,
                    outcome: ClaimCheckOutcome::Measured,
                    claim: Some(claim),
                    claim_evidence: Vec::new(),
                });
            }
            // No checkable value (a decline, or extraction unavailable) — fall
            // through to the confirmatory loop rather than fail the turn.
            AssertedValue::NoValue => {
                dbg("value-presence: no asserted value → confirmatory fallback");
            }
        }
    }

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
        if let Some(support) = claim_chunk_support(inference, c, &claim, posture).await {
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
    dbg(&format!(
        "chunks_checked={checked} max_support={max_support:.3} vp={vp:.3}"
    ));
    Some(GateVerdict {
        violation_prob: vp,
        outcome: ClaimCheckOutcome::Measured,
        claim: Some(claim),
        claim_evidence: extra,
    })
}

/// System turn for claim extraction — step 1 of the two-step gate.
pub const CLAIM_EXTRACTION_SYSTEM: &str =
    "You extract claims precisely. Reply with one sentence or NO_CLAIM.";

/// Render step 1's prompt — the claim the gate will then verify.
///
/// **The one renderer, for the gate and for the bench critic alike.**
/// Step 2 (`chunk_judge_prompt`) was unified for exactly this reason: a
/// duplicate literal in two crates is a claim that holds only while
/// nobody edits one side. Step 1 was left duplicated and duly diverged —
/// production grew the `entity_anchored` branch below while the bench
/// critic kept the unanchored rule, so `tau` was calibrated on a prompt
/// production does not send for entity-anchored turns (measured
/// 2026-08-19). Callers pass their own `entity_anchored`; the STRING is
/// no longer forkable.
///
/// `entity_anchored` turns keep the GK-attribution exemption narrow:
/// outside knowledge cannot establish a fact about the corpus's own
/// world, so a general-knowledge-caveated in-world assertion must still
/// be extracted and verified.
pub fn claim_extraction_prompt(question: &str, answer: &str, entity_anchored: bool) -> String {
    let no_claim_rule = if entity_anchored {
        "Reply with exactly NO_CLAIM if the assistant declined or said the \
         information is not in its sources. If the assistant asserted a fact \
         while attributing it to general knowledge, still state that claim."
    } else {
        "Reply with exactly NO_CLAIM if the assistant declined, said the information \
         is not in its sources, or explicitly attributed the fact to general \
         knowledge rather than the sources."
    };
    format!(
        "A user asked: {}\n\nAn assistant answered:\n\"\"\"\n{}\n\"\"\"\n\n\
         State the single central factual claim the assistant asserts as its answer, \
         as one short standalone sentence that names BOTH sides of the relation \
         (who/what is claimed to be/do what). Do not add qualifiers or sources.\n\
         {no_claim_rule}",
        question.chars().take(400).collect::<String>(),
        answer.chars().take(2000).collect::<String>(),
    )
}

/// One per-chunk support probe — the exact register `verify_grounding`'s
/// per-claim loop runs (passage cap 2,400 chars, forced-choice A/B,
/// support = p(A)/(p(A)+p(B))). Shared with the bench faithfulness lane
/// via the `grounding::claim_chunk_support` wrapper so the two registers
/// can never drift — same contract as `extract_claim_list`'s wrapper.
/// `None` = judge failure (caller decides fail-open vs retry).
pub(super) async fn claim_chunk_support(
    inference: &Arc<dyn InferenceProvider>,
    passage: &str,
    claim: &str,
    posture: ShardingPrivacy,
) -> Option<f64> {
    let prompt = chunk_judge_prompt(passage, claim);
    let (a, b) = forced_choice_ab(
        inference,
        &prompt,
        None,
        posture,
        GateCallMechanism::ChunkJudge,
    )
    .await?;
    let denom = a + b;
    Some(if denom > 0.0 { a / denom } else { 0.0 })
}

/// Extract up to 4 specific, checkable factual claims from a
/// long-form answer. Empty vec = nothing checkable (essay of analysis
/// / opinion) — passes ungated.
pub(super) async fn extract_claim_list(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    answer: &str,
    max_claims: usize,
    posture: ShardingPrivacy,
) -> Option<Vec<String>> {
    let prompt = format!(
        "A user asked: {}\n\nAn assistant wrote this long answer:\n\"\"\"\n{}\n\"\"\"\n\n\
         List the SPECIFIC factual claims the answer asserts — concrete who/what/when \
         relations a passage could confirm or refute (names, identifications, events, \
         attributions). One claim per line, each a short standalone sentence naming \
         both sides of the relation. At most {n} lines; pick the most load-bearing \
         claims, and when the answer is long, sample across ALL of it — include \
         specific claims from the later sections, not only the opening. Skip \
         opinions, summaries of the question, and anything the answer itself flags \
         as not from the sources.\n\
         Reply with exactly NO_CLAIM if there are no such checkable claims.",
        question.chars().take(400).collect::<String>(),
        answer.chars().take(14_000).collect::<String>(),
        n = max_claims,
    );
    let req = CompletionRequest {
        prompt,
        system_message: Some(format!(
            "You extract claims precisely. Reply with up to {max_claims} lines, or NO_CLAIM."
        )),
        preferred_speed: Speed::Slow,
        // SLOT_POLICY §7: route the Critic through the privacy-gated OICP
        // path instead of pinning `model_id: "primary"`. The pin was a
        // latent privacy hole — `primary` is a mesh-advertised alias and
        // `locate_named_model` load-balances named models across peers
        // with no privacy check, so a pinned judge could cross the network
        // on a LocalOnly turn. The Judge envelope carries the session's
        // sharding posture, so offload happens only when the turn allows.
        oicp: Some(Workload::Judge.requirements(posture)),
        max_tokens: Some((max_claims * 48).max(160)),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        ..Default::default()
    };
    match gate_call(&**inference, &req, GateCallMechanism::ClaimList).await {
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
                    // Honour the caller's budget — was a hardcoded take(4) that
                    // silently defeated the length-scaled claim_budget (up to
                    // 10): a padded 6000-char answer still had only its first 4
                    // claims extracted, so later-section fabricated specifics /
                    // misattributions were never audited (2026-06-30 gate gap).
                    .take(max_claims)
                    .collect(),
            )
        }
        Err(e) => {
            tracing::warn!(target: "grounding_gate", error = %e, "claim-list extraction failed");
            None
        }
    }
}

/// Holistic supporting-specifics scan — the complement to the per-claim audit.
///
/// `extract_claim_list` pulls the answer's most LOAD-BEARING claims (its
/// headline assertions), which on a padded answer are often the correct part;
/// the fabrication hides in the SUPPORTING SPECIFICS a long answer invents to
/// look thorough — a fake constant value, a quote misattributed to the wrong
/// speaker (Hamilton's point credited to Madison), a section/version number
/// that isn't in the sources, the wrong programming language. The per-claim
/// audit never extracts those (2026-06-30 gate blind spot; see the faithfulness
/// audit), so they ship inside a `released` verdict.
///
/// This is ONE holistic pass: the judge sees the WHOLE answer against the FULL
/// evidence and returns the specific details that are absent from or
/// contradicted by the evidence. It is deliberately CONSERVATIVE — instructed
/// to list a detail only when confident it is unsupported — because the
/// downstream action (route through the rewrite/annotate path) should correct
/// real fabrications, not prune legitimately-grounded content. Returns the
/// offending specifics verbatim (answer wording), or an empty vec when every
/// specific checks out. `None` on inference error → caller fails open.
pub async fn scan_unsupported_specifics(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    answer: &str,
    leaf_chunks: &[String],
    summary_chunks: &[String],
    max_items: usize,
    posture: ShardingPrivacy,
) -> Option<Vec<String>> {
    // D3 CANDIDATE A (order audit-economy): the scan JOINS the judges' prefix
    // family — the same system turn and the same leaf-window opening bytes as
    // `claim_violation_joint` / `claims_support_batched`, summaries appended
    // AFTER the declared boundary (exactly as thematic claim checks append
    // theirs). D0 measured this scan as the audit's largest single term
    // (9.7s median, 35% of the stage) precisely because its private system
    // turn put it in its own pin family; joining the family makes its
    // evidence prefill a restore of state a sibling already paid for, on
    // clean and rewrite turns alike. This IS a judge-input change: it is
    // priced replay-first against the 9 labeled scan items and the
    // scan-vs-main deltas before any live arm (the land-C caution does not
    // transfer — this register is generative, no forced-choice margin
    // exists here to compress — but the claim is measured, not argued).
    if leaf_chunks
        .iter()
        .chain(summary_chunks.iter())
        .all(|c| c.trim().is_empty())
    {
        return Some(Vec::new());
    }
    // Audit the CONTENT of honestly-labeled spans, not the label: the wrapper
    // words bias the judge against supported content (see
    // `unwrap_unverified_excerpts`).
    let answer = &unwrap_unverified_excerpts(answer);
    let family = EvidenceFamily::new(leaf_chunks);
    let (prompt, stable_prefix_len) =
        family.scan_prompt(summary_chunks, question, answer, max_items);
    let req = CompletionRequest {
        prompt,
        stable_prefix_len,
        system_message: Some(CHUNK_JUDGE_SYSTEM.into()),
        preferred_speed: Speed::Slow,
        // SLOT_POLICY §7: route the Critic through the privacy-gated OICP
        // path instead of pinning `model_id: "primary"` (see
        // `forced_choice_ab` for the full rationale).
        oicp: Some(Workload::Judge.requirements(posture)),
        max_tokens: Some((max_items * 40).max(160)),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        ..Default::default()
    };
    match gate_call(&**inference, &req, GateCallMechanism::SpecificsScan).await {
        Ok(resp) => Some(scan_items_from_reply(&resp.text, answer, max_items)),
        Err(e) => {
            tracing::warn!(target: "grounding_gate", error = %e, "specifics scan failed");
            None
        }
    }
}

/// The specifics scan's reply → the flagged answer spans. Pure, so the
/// judge's raw output can be replayed in a test without an inference
/// provider — which is how the judge-prose defect below is pinned.
///
/// Line discipline first (bullet/number prefixes, the NONE sentinel, a
/// length floor), then [`anchor_scan_item`] decides, per line, whether
/// the judge quoted the ANSWER or wrote about it. Only the former survive:
/// a scan item is a claim the answer made, never the judge's commentary on
/// it.
fn scan_items_from_reply(reply: &str, answer: &str, max_items: usize) -> Vec<String> {
    let t = reply.trim();
    if t.is_empty() || t.to_uppercase().contains("NONE") {
        return Vec::new();
    }
    t.lines()
        .map(|l| l.trim().trim_start_matches(['-', '*', '•']).trim())
        .map(|l| {
            l.trim_start_matches(|c: char| c.is_ascii_digit())
                .trim_start_matches(['.', ')'])
                .trim()
                .to_string()
        })
        .filter(|l| l.len() > 8)
        .filter_map(|l| match anchor_scan_item(&l, answer) {
            Some(span) => Some(span),
            None => {
                // Reported, never defaulted: the line is named at the level
                // that reads it, so a judge drifting off the verbatim
                // contract is visible as a drop count rather than as
                // commentary appearing in someone's ledger.
                tracing::info!(
                    target: "grounding_gate",
                    event = "scan_item_dropped",
                    reason = "not a span of the answer",
                    line = %l.chars().take(120).collect::<String>(),
                    "specifics scan: judge wrote about the answer, not from it"
                );
                None
            }
        })
        .take(max_items)
        .collect()
}

/// Strip the app's own honest `[unverified excerpt: X]` wrappers down to X.
/// The wrapper is presentation metadata from quote_verification.rs; fed back
/// into a judge it reads as an admission and biases the verdict against
/// SUPPORTED content (observed 2026-07-01: "As Samuelson (1954) noted…" —
/// verbatim in the evidence at offset 2410 — was flagged unsupported only when
/// wrapped, and the verification note then listed it as unverified while the
/// body cited it: a self-contradiction the re-judge scored confabulation).
/// Same principle as the offline rubric's clause: judge X's content, never the
/// wrapper.
pub(super) fn unwrap_unverified_excerpts(s: &str) -> String {
    const OPEN: &str = "[unverified excerpt:";
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(OPEN) {
        out.push_str(&rest[..i]);
        let after = &rest[i + OPEN.len()..];
        match after.find(']') {
            Some(j) => {
                out.push_str(after[..j].trim());
                rest = &after[j + 1..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Reduce a scan line toward the ANSWER SPAN it flags. The prompt demands the
/// answer's exact wording, but the 35B routinely appends judgment chatter
/// ("… — The evidence does not mention this") or frames the item as commentary
/// ("The answer cites \"[Source: X]\" for …"). These lines flow into the
/// rewrite instructions AND the user-visible verification note — where the
/// chatter reads as the assistant indicting itself (observed live 2026-07-01:
/// a released answer footnoted "… is a fabricated specific not found in the
/// Deterministic jurisdiction filter: self-referential DECLINE statements —
/// negated capability/coverage claims whose subject is the system itself or
/// its evidence ("the system does not have access to…", "the provided
/// passages do not contain…", "there is no evidence in the sources…").
/// These are honesty meta-language, not world-claims: no passage can state
/// them, so auditing them prosecutes the answer's own honesty. Observed
/// 2026-07-10 (persona-QA): refined honest declines rejected at vp
/// 0.85–0.98 on exactly these sentences, reverting the web-search
/// refinement to the original. A decline asserts the ABSENCE of
/// information — it cannot launder a false world-claim — so exempting the
/// SHAPE is safe. Same family as the offline judge's decline-shape
/// override (calibration gate) and the `[Source:]` scan-jurisdiction rule.
/// T1 P1.4 claim-class decision. FACTUAL/SPECIFIC claims must be
/// supported by Leaf-class evidence; THEMATIC/STRUCTURAL claims (about
/// the text's themes, structure, or discourse rather than in-world
/// specifics) may additionally rest on Summary-class evidence.
///
/// Two layers, in order:
/// 1. Structural specificity — digits or quotations in the claim →
///    factual, deterministically. These are features of the claim's
///    FORM, reliable regardless of vocabulary.
/// 2. Semantic class — the centroid-of-embeddings classifier
///    (`claim_class_classifier`, same shape as the current-info and
///    scope routers). No marker lists: a substring heuristic here
///    would be the keyword-classifier failure the routers already
///    replaced twice, and this decision gates honesty.
///
/// DEFAULT-FACTUAL everywhere: low signal, thin margin, classifier
/// unavailable, embed failure — all keep the conservative bar.
pub(super) async fn claim_is_factual_specific(
    inference: &Arc<dyn InferenceProvider>,
    claim: &str,
) -> bool {
    if claim_has_structural_specificity(claim) {
        return true;
    }
    match crate::claim_class_classifier::shared_claim_classifier(inference).await {
        Some(classifier) => matches!(
            classifier.classify(claim, inference).await,
            crate::claim_class_classifier::ClaimClass::Factual
        ),
        None => true,
    }
}

/// Layer-1 structural check: numbers, years, quantities, or quoted
/// spans make a claim factual/specific regardless of phrasing.
pub(super) fn claim_has_structural_specificity(claim: &str) -> bool {
    let has_digit = claim.chars().any(|c| c.is_ascii_digit());
    let has_quote = claim.contains('"') || claim.contains('\u{201c}') || claim.contains('\u{201d}');
    has_digit || has_quote
}

pub(super) fn is_self_referential_decline(text: &str) -> bool {
    let t = normalize_meta(text);
    if !meta_subject(&t) {
        return false;
    }
    [
        "does not",
        "do not",
        "doesn't",
        "don't",
        "cannot",
        "can't",
        "no evidence",
        "no information",
        "lacks",
        "not include",
        "not contain",
        "not have",
    ]
    .iter()
    .any(|n| t.contains(n))
}

/// Strip markdown emphasis ("does **not** have" must match "does not"),
/// then leading list/quote decoration; lowercase. Shared normalization for
/// the meta-language predicates below.
fn normalize_meta(text: &str) -> String {
    text.replace('*', "")
        .trim()
        .trim_start_matches(['-', ' ', '"', '\u{201c}'])
        .to_lowercase()
}

/// Explicit system/evidence-artifact subjects — safe to treat as
/// meta-language even WITHOUT a negation (a positive description of the
/// evidence still isn't a world-claim).
const META_SUBJECTS_CORE: &[&str] = &[
    "the system",
    "the assistant",
    "the model",
    "the app",
    "this system",
    "the provided",
    "the retrieved",
    "the sources",
    "the passages",
    "the evidence",
    "the corpus",
    "the collection",
    "the knowledge base",
    "the local corpus",
    "the initial answer",
];

/// Looser subject prefixes ("I …", "It …", "There is no …", "As of …") that
/// read as meta ONLY when the negation requirement of
/// [`is_self_referential_decline`] constrains them — "It was sent in May" is
/// a world-claim with a pronoun subject and must never match the
/// negation-free arm.
const META_SUBJECTS_LOOSE: &[&str] = &["i ", "it ", "there is no", "as of "];

/// Subject test for [`is_self_referential_decline`] (negation-guarded →
/// loose prefixes allowed).
fn meta_subject(t: &str) -> bool {
    META_SUBJECTS_CORE
        .iter()
        .chain(META_SUBJECTS_LOOSE)
        .any(|s| t.starts_with(s))
}

/// Strict subject test for the negation-free rider arm of
/// [`decline_rider_exempt`]: explicit evidence/system nouns only.
fn meta_subject_strict(t: &str) -> bool {
    META_SUBJECTS_CORE.iter().any(|s| t.starts_with(s))
}

/// Short-path jurisdiction scalpel (2026-07-21): should the gate SKIP
/// auditing this extracted claim because it is a decline's meta-rider, not a
/// world-claim? True when either:
///
///  1. the claim itself is a negated self-referential decline — the exact
///     shape the longform gate already exempts (asserts ABSENCE, cannot
///     launder a value); or
///  2. the ANSWER's headline act is a deterministic decline
///     (`answer_declines`) AND the claim's subject is the evidence/system —
///     the rider case ("I don't have reliable information on this. The
///     provided passages are Rust source code snippets…"). Auditing such a
///     rider is category-confused — no passage states facts about the
///     passages — so it reliably fails, burning the per-passage sweep
///     (measured 16 × 0.8s, 2026-07-21 soak step 91) and then a doomed
///     second-synthesis retry (the documented 50-160s slow abstention).
///
/// A decline that smuggles a WORLD-claim rider ("…However, John sent the
/// memo on May 5") keeps its full audit: the claim extractor strips
/// source-attribution wrappers, so a world rider arrives with a world
/// subject and fails arm 2's subject test.
pub(super) fn decline_rider_exempt(answer: &str, claim: &str) -> bool {
    is_self_referential_decline(claim)
        || (super::answer_declines(answer) && meta_subject_strict(&normalize_meta(claim)))
}

/// Anchor one specifics-scan line to the ANSWER, or reject it.
///
/// The scan is asked for verbatim answer wording ("Quote the answer's exact
/// wording"), and a well-behaved judge obliges. A judge that does not obliges
/// with commentary — a critique preamble, or a quoted span with its own
/// verdict appended — and that commentary used to pass through untouched.
/// Downstream, `longform_claims` turns every scan finding into a `GateClaim`
/// and the epistemic ledger renders it as a `failed_once` **holding**, so the
/// user read the judge's remarks as their own answer's failed claims. Measured
/// on `compound-killer-and-lugger` (see `testdata/README.md`): three of that
/// turn's five negative holdings were judge prose, and two of the three also
/// reached the user-visible verification note.
///
/// So this is a decision, not a cleanup: **an item that is not wording of the
/// answer is not a claim about the world, and gets no holding.** `None` is
/// that verdict, and the caller traces it — an item is dropped loudly, never
/// silently rewritten into something claim-shaped.
///
/// Deterministic ladder, first match wins:
/// 1. the longest QUOTED span that occurs in the answer → the span;
/// 2. a quoted span the judge ELIDED with a trailing ellipsis → its prefix,
///    when that prefix occurs in the answer and is substantial;
/// 3. the item is itself answer wording → the item;
/// 4. a prefix cut at a commentary dash that occurs in the answer → the prefix;
/// 5. otherwise `None` — the judge wrote ABOUT the answer, not FROM it.
///
/// Containment is judged by [`anchor_key`], which ignores emphasis markers:
/// the judge re-quotes `**Severin Quenholt**` as `Severin Quenholt`, and step 1
/// used to miss on exactly that difference and fall through to the old
/// pass-through arm.
fn anchor_scan_item(item: &str, answer: &str) -> Option<String> {
    /// A prefix recovered from an elided quote has to be long enough to still
    /// be a claim — `"Severin Quenholt... as harbormaster"` must not reduce to
    /// a bare name.
    const MIN_ELIDED_PREFIX: usize = 24;
    const MIN_SPAN: usize = 12;

    let item = &unwrap_unverified_excerpts(item);
    let ans = anchor_key(answer);
    let quoted: Vec<&str> = extract_quoted_spans(item);
    // 1. A quoted span the answer actually contains.
    if let Some(best) = quoted
        .iter()
        .filter(|s| s.chars().count() >= MIN_SPAN && ans.contains(&anchor_key(s)))
        .max_by_key(|s| s.chars().count())
    {
        return Some(best.trim().to_string());
    }
    // 2. A quoted span cut short with "…" — anchor on what precedes it.
    for span in &quoted {
        let head = span.trim_end().trim_end_matches(['"', '“', '”']).trim_end();
        for ellipsis in ["...", "…"] {
            if let Some(prefix) = head.strip_suffix(ellipsis) {
                let prefix = prefix.trim_end();
                if prefix.chars().count() >= MIN_ELIDED_PREFIX && ans.contains(&anchor_key(prefix))
                {
                    return Some(prefix.to_string());
                }
            }
        }
    }
    // 3. The whole item is answer wording (checked BEFORE the dash cut, so a
    //    legitimate interior dash in a present item is not treated as a seam).
    if ans.contains(&anchor_key(item)) {
        return Some(item.trim().trim_matches(['"', '“', '”']).trim().to_string());
    }
    // 4. Commentary appended after a dash. " - " is here because it is what the
    //    live judge emitted on the measured turn; the others predate it.
    for dash in [" — ", " – ", " -- ", " - "] {
        if let Some((head, _)) = item.split_once(dash) {
            let head = head.trim().trim_matches(['"', '“', '”']).trim();
            if head.chars().count() >= MIN_SPAN && ans.contains(&anchor_key(head)) {
                return Some(head.to_string());
            }
        }
    }
    None
}

/// Spans inside straight or curly double quotes, in order of appearance.
fn extract_quoted_spans(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = s;
    loop {
        let Some(open) = rest.find(['"', '“']) else {
            break;
        };
        let open_len = rest[open..].chars().next().map_or(1, char::len_utf8);
        let after = &rest[open + open_len..];
        let Some(close) = after.find(['"', '”']) else {
            break;
        };
        out.push(&after[..close]);
        let close_len = after[close..].chars().next().map_or(1, char::len_utf8);
        rest = &after[close + close_len..];
    }
    out
}

/// The one normal form for "does this text occur in the answer" —
/// lowercase, whitespace runs collapsed, and Markdown emphasis markers
/// dropped. Emphasis is presentation: the answer writes
/// `**Severin Quenholt**` and `*The Cold Lantern*`, and a judge quoting
/// either writes the plain words. Comparing raw made those spans read as
/// absent from the answer they came from.
///
/// Containment only. Never use it to build a value that is shown or stored —
/// [`anchor_scan_item`] returns slices of the ORIGINAL text.
fn anchor_key(s: &str) -> String {
    s.to_lowercase()
        .replace(['*', '_', '`'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// In-world attribution veto — the deterministic pre-check the yes-biased
/// joint judge needs. Measured (padghost replay 2026-07-02): "Betty Alexander
/// sent an email to Jeff Skilling on July 7, 2000" scored vp=0.010 — every
/// element of the claim is corpus-true EXCEPT the invented person (the real
/// sender is Rosalee; "Betty Alexander" appears nowhere in the evidence), and
/// a forced-choice judge shown a nearly-true claim answers "supports". The
/// same ghost shipped in three separate runs.
///
/// The veto is scoped to IN-WORLD attributions so correct general knowledge is
/// never shackled (the trust bar): it fires only when the claim is about a
/// corpus ARTIFACT (email/letter/document/passage/sent/wrote/…) AND carries a
/// person-name-shaped bigram (Capitalized-lowercase pair — acronyms like "HR"
/// don't match) absent from the ENTIRE evidence + labels. A name attributed to
/// a corpus artifact must exist in the corpus; a GK claim ("Noam Cohen wrote
/// in Wired…", no artifact noun) passes through to the judge untouched.
/// Returns the offending name for the glassbox.
/// Remove `[Source: …]` citation spans before any name/identifier sweep:
/// labels are pre-validated by the deterministic snap pass and are OUT OF
/// JURISDICTION here — sweeping them produced user-visible self-indictments
/// ("The answer references \"Source Psilocybin\", which does not appear in
/// the sources", persona-QA 2026-07-10: 4 of 9 answers ended that way).
/// Unclosed brackets strip to end-of-line (the bounded-bracket lesson).
pub(super) fn strip_citation_spans(claim: &str) -> String {
    let mut out = String::with_capacity(claim.len());
    let mut rest = claim;
    loop {
        let Some(i) = rest.to_lowercase().find("[source:") else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..i]);
        out.push(' ');
        let tail = &rest[i..];
        let end = tail
            .find(']')
            .map(|e| e + 1)
            .or_else(|| tail.find('\n'))
            .unwrap_or(tail.len());
        rest = &tail[end..];
    }
}

/// Capitalized FUNCTION/BOILERPLATE words are structurally never given
/// names — "From Retrieved" (a section header), "Source Federalist" (a
/// label fragment). Blocking them as bigram members costs a theoretical
/// missed fabrication and removes a measured class of self-indictments.
fn non_name_word(w: &str) -> bool {
    matches!(
        w.to_lowercase().as_str(),
        "from" | "the" | "this" | "these" | "those" | "your" | "their" | "our"
            | "its" | "based" | "initial" | "additional" | "retrieved"
            | "provided" | "source" | "sources" | "answer" | "web" | "search"
            | "note" | "summary" | "overview" | "key" | "corpus" | "evidence"
            | "passage" | "passages" | "section" | "document" | "knowledge"
            // Pronouns: "Webber He averaged…" flagged "Webber He" as a
            // fabricated name (persona-QA, the run after the label fix).
            | "he" | "she" | "they" | "we" | "his" | "her" | "him" | "them"
            | "who" | "which" | "when" | "where" | "while" | "after" | "before"
    )
}

/// Does `low` contain any of `words` as a WHOLE WORD?
///
/// Both deterministic vetoes below gate themselves on "is this claim even
/// about a corpus artifact?" and both used `low.contains(a)`, which is a
/// substring test. The consequences were not marginal — measured 2026-08-13,
/// the artifact gate opened on ordinary prose:
///
///   "designed"  contains "signed"     "presented" contains "sent"
///   "sentence"  contains "sent"       "absent"    contains "sent"
///   "consent"   contains "sent"       "represent" contains "sent"
///   "essential" contains "sent"       "classical" contains "class"
///   "denotes"   contains "notes"      "documented" contains "document"
///
/// So "Harry Frankfurt designed cases…" tripped the name veto — the gate
/// opened on "signed", and the bigram check then flagged "Harry Frankfurt"
/// because the corpus writes the surname alone. A gate meant to restrict these
/// vetoes to claims about emails, letters and source files was instead open on
/// most sentences an essay contains.
///
/// One helper for both call sites (ARCH §10.6): the two vetoes ask the same
/// question and must not answer it two ways.
fn mentions_artifact(low: &str, words: &[&str]) -> bool {
    words.iter().any(|w| {
        low.match_indices(w).any(|(i, _)| {
            let before_ok = i == 0
                || !low[..i]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric());
            let after = i + w.len();
            let after_ok = after >= low.len()
                || !low[after..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric());
            before_ok && after_ok
        })
    })
}

pub(super) fn absent_name_attribution(claim: &str, hay_lower: &str) -> Option<String> {
    const ARTIFACT: &[&str] = &[
        "email",
        "e-mail",
        "letter",
        "memo",
        "message",
        "document",
        "passage",
        "chapter",
        "section",
        "thread",
        "forwarded",
        "sent",
        "wrote",
        "authored",
        "signed",
        "replied",
    ];
    let claim = strip_citation_spans(claim);
    // Markdown headings / bold-only lines are TOPIC LABELS in Title Case
    // ("**Energy Costs**", "## Legislative Origination") — the sweep read
    // them as person names (overnight soak, 2026-07-11). The sweep's
    // sentence-splitter hands headings over as their own "sentences";
    // refuse label-shaped input outright.
    {
        let t = claim.trim();
        let heading = t.starts_with('#')
            || (t.starts_with("**") && t.trim_end_matches(':').ends_with("**"))
            || (t.ends_with(':') && t.split_whitespace().count() <= 6);
        if heading {
            return None;
        }
    }
    let low = claim.to_lowercase();
    if !mentions_artifact(&low, ARTIFACT) {
        return None;
    }
    fn cap_name(w: &str) -> Option<&str> {
        let t = w.trim_matches(|c: char| !c.is_alphanumeric());
        let mut chars = t.chars();
        let first = chars.next()?;
        (first.is_uppercase()
            && t.chars().count() >= 2
            && chars.all(|c| c.is_lowercase() && c.is_alphabetic()))
        .then_some(t)
    }
    let words: Vec<&str> = claim.split_whitespace().collect();
    for pair in words.windows(2) {
        // A separator on the first word means a LIST, not a name:
        // "Hamilton, Madison" is two people — fusing them minted the
        // fictitious "Hamilton Madison" (overnight soak, 2026-07-11).
        if pair[0].ends_with([',', ';', ':', '/', '&']) {
            continue;
        }
        // Markdown-emphasized words are HEADINGS/labels, not names — the
        // splitter glues "**Energy Costs**: The document…" into one
        // sentence, and trim_matches strips the asterisks before cap_name
        // sees them (same overnight soak).
        if pair[0].contains("**") || pair[1].contains("**") || pair[0].starts_with('#') {
            continue;
        }
        if let (Some(a), Some(b)) = (cap_name(pair[0]), cap_name(pair[1])) {
            if non_name_word(a) || non_name_word(b) {
                continue;
            }
            let full = format!("{a} {b}").to_lowercase();
            if !hay_lower.contains(&full) {
                return Some(format!("{a} {b}"));
            }
        }
    }
    None
}

/// Identifier sibling of `absent_name_attribution`: a claim about the corpus's
/// CODE/STRUCTURE artifacts (file/module/function/enum/values/defined/…)
/// naming a code-shaped identifier absent from the entire evidence is
/// fabricated. Observed (gen75c): "the material centers on `cmd_init` and
/// `design_signals.rs`" — neither exists in the corpus; "the StepKind values
/// are …, ReasonWithTools" — an invented variant. Identifier shapes are
/// distinctive (snake_case, dotted filenames, CamelCase humps), so absence is
/// decisive; general-knowledge identifiers in claims WITHOUT artifact context
/// pass through untouched.
pub(super) fn absent_identifier_attribution(claim: &str, hay_lower: &str) -> Option<String> {
    const ARTIFACT: &[&str] = &[
        "file",
        "module",
        "function",
        "struct",
        "enum",
        "variant",
        "field",
        "defined",
        "definition",
        "values",
        "type",
        "method",
        "class",
        "constant",
        "config",
        "material",
        "corpus",
        "notes",
        "document",
        "codebase",
        "snippet",
    ];
    // [Source: …] labels are the snap pass's jurisdiction — see
    // strip_citation_spans.
    let claim = strip_citation_spans(claim);
    let claim = claim.as_str();
    let low = claim.to_lowercase();
    if !mentions_artifact(&low, ARTIFACT) {
        return None;
    }
    fn identifier_shaped(t: &str) -> bool {
        let snake = t.contains('_')
            && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && t.chars().any(|c| c.is_ascii_alphabetic());
        let file = t.rsplit_once('.').is_some_and(|(stem, ext)| {
            !stem.is_empty()
                && [
                    "rs", "py", "js", "ts", "toml", "md", "json", "yaml", "yml", "txt", "mjs",
                ]
                .contains(&ext)
        });
        let camel_humps = {
            let mut humps = 0;
            let mut prev_lower = false;
            for c in t.chars() {
                if c.is_ascii_uppercase() && prev_lower {
                    humps += 1;
                }
                prev_lower = c.is_ascii_lowercase();
            }
            humps >= 1 && t.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && t.len() >= 8
        };
        (snake || file || camel_humps) && t.len() >= 6
    }
    for raw in claim.split(|c: char| c.is_whitespace() || "()[]{}<>,;:\"'`*".contains(c)) {
        let mut t =
            raw.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'));
        // A sentence-final period is not part of the identifier; real file
        // extensions keep their interior dot ("design_signals.rs").
        while let Some(stripped) = t.strip_suffix('.') {
            t = stripped;
        }
        if t.len() >= 6 && identifier_shaped(t) {
            let tl = t.to_lowercase();
            // Prose may space a CamelCase identifier ("step kind" for
            // StepKind) — accept a space-squashed match too.
            let squashed: String = hay_lower.split_whitespace().collect();
            if !hay_lower.contains(&tl) && !squashed.contains(&tl) {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// Leading literal of every claim-check prompt. Split out so the stable-prefix
/// byte math below and the prompt construction cannot drift apart.
const PASSAGES_SCAFFOLD: &str = "PASSAGES (multiple, separated by ---):\n\"\"\"\n";

/// Separator between passages, everywhere. One literal, so the renderer's
/// bytes and its boundary arithmetic cannot disagree about it.
const PASSAGE_SEP: &str = "\n---\n";

/// The system turn of the forced-choice judge register.
///
/// # This is a calibration surface, not a string
///
/// τ = 0.9 is calibrated against the bench critic
/// (`sovereign-cli-llm/src/bench_cmd/live_runner.rs`), and the transfer
/// argument in this module's header — "prompts are byte-identical to the bench
/// critic, so the bench-calibrated threshold transfers" — is only true while
/// the two registers really are identical. It used to be true by *coincidence
/// maintained by hand*: the same literal typed into two crates. This constant
/// and [`chunk_judge_prompt`] make it true STRUCTURALLY; the critic imports
/// both, so the identity cannot be broken by editing one side (ARCH §10.6).
///
/// **Land C changes this**, deliberately and with the adversarial set as its
/// evidence — and because the critic now shares the constant, it moves with
/// production instead of being left behind holding the calibration.
pub const CHUNK_JUDGE_SYSTEM: &str = "You are a careful classifier. Answer with a single letter.";

/// The forced-choice per-passage judge prompt — **the register τ is calibrated
/// on**, rendered once for both the runtime gate and the bench critic.
///
/// `passage` is capped at [`CHUNK_JUDGE_PASSAGE_CHARS`] here rather than by the
/// caller, so the cap cannot drift between the two either.
pub fn chunk_judge_prompt(passage: &str, claim: &str) -> String {
    let passage: String = passage.chars().take(CHUNK_JUDGE_PASSAGE_CHARS).collect();
    format!(
        "PASSAGE:\n\"\"\"\n{passage}\n\"\"\"\n\n\
         CLAIM: {claim}\n\n\
         Does the passage state or clearly imply this claim? Paraphrase counts; \
         the passage merely mentioning the people or things involved, without \
         establishing the claimed connection between them, does NOT count.\n\n\
         Answer with exactly one letter — A = the passage supports the claim, \
         B = it does not."
    )
}

/// Per-passage cap of the calibrated chunk-judge register. Untouched by land B:
/// the truncation land B removed is the *joint* window's 1,500-char cap inside
/// [`EvidenceFamily`], a register the critic has no counterpart for.
pub const CHUNK_JUDGE_PASSAGE_CHARS: usize = 2_400;

/// **The one renderer of the gate's shared evidence block, and the one decider
/// of where it ends.**
///
/// # Why this type exists
///
/// The boundary had two implementations: the prompt's bytes came from a
/// `format!` join, and the declared `stable_prefix_len` came from a *separate*
/// arithmetic re-derivation of the same byte count — two implementations of one
/// layout, kept aligned only by a test (ARCH §10.6, the smell-table row "two
/// implementations of one threshold, formula, or key"). Here the boundary is
/// `self.prefix.len()`: not a formula that agrees with the join, but the length
/// of the very `String` the join starts from. There is no arithmetic left to
/// drift.
///
/// # Why it matters beyond tidiness
///
/// The engine's pinned-prefix cache keys a DECLARED family — every call that
/// passes `stable_prefix_len`, which is every call in this module — on the
/// CONTENT of the declared prefix, and restores only when the declaration
/// matches that entry exactly (`prefix_state::directed_key`, 2026-09-01; it
/// keyed on the first 48 rendered tokens until then, which made two turns over
/// one corpus collide and pin at their common prefix — issue #57). Byte
/// identity across sibling calls is therefore not a nicety — it is the
/// difference between restoring a ~5,500-token prefix in ~26 ms and
/// re-prefilling it for ~7.7 s (measured 2026-08-13,
/// `bench/chaos_monkey/results/gate_call_census_20260813.txt`). A mismatch
/// does not error and does not change a verdict; it silently full-prefills.
/// Byte identity is therefore asserted at the request boundary by
/// `the_gate_shares_one_prefix_family`, not argued in prose.
///
/// # Land A scope
///
/// This introduction is **byte-identical to the inline `format!` it replaces**,
/// which is what makes it exempt from the adversarial gate — and that identity
/// is proven by `evidence_family_reproduces_the_legacy_judge_prompt`, a golden
/// test carrying the legacy construction, not by this sentence.
pub(super) struct EvidenceFamily {
    /// `PASSAGES_SCAFFOLD` + the shared window, joined. The family prefix.
    prefix: String,
    /// Whether the window carried any passage. A window of zero passages still
    /// renders the scaffold, but declares nothing and takes no separator before
    /// the first appended passage — the case an arithmetic boundary got to
    /// ignore and a real `String` does not.
    non_empty: bool,
}

impl EvidenceFamily {
    /// Render the shared window once per audit pass.
    ///
    /// `window` is the evidence every sibling call in the pass sees, in
    /// retrieval order. Callers append their own passages after it; nothing
    /// they append can move the boundary.
    pub(super) fn new(window: &[String]) -> Self {
        let mut prefix = String::from(PASSAGES_SCAFFOLD);
        for (i, chunk) in window.iter().enumerate() {
            if i > 0 {
                prefix.push_str(PASSAGE_SEP);
            }
            // FULL TEXT. The per-chunk 1,500-char cap that stood here is gone
            // (land B). Two reasons, and the second is the one that was
            // measured: a cut chunk MANUFACTURES ABSENCES — a judge asked
            // "do the passages support this claim" against a copy of the
            // evidence with the support snipped off will say no, and the
            // sibling specifics scan was observed doing exactly that,
            // flagging a phrase sitting verbatim at offset 1,497 of a chunk
            // it had been handed (note 95b82f97, which lifted the cap THERE
            // and left it here, unmeasured). And the pinned prefix contains
            // these bytes, so while they were truncated the scan's full-text
            // opening could not strict-prefix-match the judges' entry — the
            // cap was the thing standing between the two mechanisms and one
            // shared family.
            prefix.push_str(chunk);
        }
        Self {
            prefix,
            non_empty: !window.is_empty(),
        }
    }

    /// The family boundary, in bytes. `None` when the window carried no
    /// passage: every caller then declares nothing and degrades to an
    /// undeclared prompt. Absence is reported, never defaulted to 0 — a
    /// zero-length declaration is a different claim from "there is no stable
    /// window" (ARCH §18.3).
    pub(super) fn prefix_len(&self) -> Option<usize> {
        self.non_empty.then(|| self.prefix.len())
    }

    /// One claim-check prompt: the family prefix, then this call's own
    /// passages (summaries for a thematic claim, claim-conditioned hits), then
    /// the claim and the question. Returns the prompt and the boundary to
    /// declare.
    pub(super) fn claim_prompt(&self, appended: &[String], claim: &str) -> (String, Option<usize>) {
        let mut prompt = self.prefix.clone();
        for (i, chunk) in appended.iter().enumerate() {
            if self.non_empty || i > 0 {
                prompt.push_str(PASSAGE_SEP);
            }
            prompt.push_str(chunk);
        }
        prompt.push_str(&format!(
            "\n\"\"\"\n\n\
             CLAIM: {claim}\n\n\
             Do the passages, taken together, state or clearly imply this claim? \
             Support assembled across several passages counts; paraphrase counts; \
             the passages merely mentioning the people or things involved, without \
             establishing the claimed connection, does NOT count.\n\n\
             Answer with exactly one letter — A = the passages support the claim, \
             B = they do not."
        ));
        let boundary = self.prefix_len();
        debug_assert!(
            boundary.is_none_or(|n| prompt.is_char_boundary(n) && n <= prompt.len()),
            "the family boundary must be a char boundary inside the prompt"
        );
        debug_assert!(
            prompt.starts_with(&self.prefix),
            "a claim prompt must open with the family prefix"
        );
        (prompt, boundary)
    }

    /// The BATCHED register's prompt: the family prefix, then every extracted
    /// claim numbered, then one instruction — one prefill, N verdicts.
    ///
    /// Rendered HERE, by the family's own renderer, because family membership
    /// is the entire point of this register's 2026-08-14 reshape: D0 of order
    /// `audit-economy` measured the per-claim judges restoring the pinned
    /// evidence window in 34-53ms (129/129 calls), which made the original
    /// batched prompt — its own scaffold, its own 1,500-char chunk cuts, no
    /// declared boundary — a register that FULL-PREFILLS ~9K tokens to save
    /// calls that no longer pay for prefill. Opening with the byte-identical
    /// family prefix (and carrying [`CHUNK_JUDGE_SYSTEM`], asserted by
    /// `batched_register_joins_the_judges_prefix_family`) puts the one
    /// batched call in the same pinned-prefix family as its sibling judges:
    /// it restores the window the first per-claim call pinned, or pins it
    /// for them.
    ///
    /// The instruction language deliberately tracks [`Self::claim_prompt`]'s
    /// (assembly across passages counts, mere mention does not) — the batched
    /// verdict is judged against the same support standard, differing only in
    /// answer shape (N text lines vs one forced-choice logit). That shape
    /// difference is exactly what the judge-replay recalibration prices; see
    /// [`claims_support_batched`].
    pub(super) fn batched_claims_prompt(&self, claims: &[String]) -> (String, Option<usize>) {
        let mut prompt = self.prefix.clone();
        prompt.push_str("\n\"\"\"\n\nCLAIMS (numbered):\n");
        for (i, claim) in claims.iter().enumerate() {
            prompt.push_str(&format!("{}. {}\n", i + 1, claim));
        }
        prompt.push_str(&format!(
            "\nFor EACH numbered claim, do the passages, taken together, state or \
             clearly imply it? Support assembled across several passages counts; \
             paraphrase counts; the passages merely mentioning the people or things \
             involved, without establishing the claimed connection, does NOT count.\n\n\
             Output EXACTLY one line per claim, in order, formatted \"<n>: A\" (the \
             passages support it) or \"<n>: B\" (they do not). Output the {n} lines \
             and nothing else.",
            n = claims.len(),
        ));
        let boundary = self.prefix_len();
        debug_assert!(
            boundary.is_none_or(|n| prompt.is_char_boundary(n) && n <= prompt.len()),
            "the family boundary must be a char boundary inside the prompt"
        );
        debug_assert!(
            prompt.starts_with(&self.prefix),
            "a batched prompt must open with the family prefix"
        );
        (prompt, boundary)
    }

    /// The LOCATED-SPAN TRIAGE register's prompt: the family prefix — this
    /// call's candidate spans, one per chunk, in chunk order — then ONE claim
    /// and one instruction. One prefill, N verdicts.
    ///
    /// # The transpose of [`Self::batched_claims_prompt`]
    ///
    /// That register asks N claims against one shared window, and it is a
    /// family MEMBER because its window is shared with every sibling judge of
    /// the pass. This one's window is CLAIM-CONDITIONED — the spans are the
    /// ones cosine picked as being about this particular claim — so it has no
    /// sibling to share a prefix with and pins nothing for anyone. It renders
    /// here anyway because `impl EvidenceFamily` is the one place the scaffold
    /// and the separator may be written (`one_renderer_owns_the_family`);
    /// putting it anywhere else is how the boundary got two deciders before.
    ///
    /// Passages are addressed by ORDINAL POSITION rather than by an injected
    /// number, because the prefix render belongs to the family and this
    /// register does not get to change it. `n` is passed in rather than
    /// recomputed here so the instruction's count and the caller's expectation
    /// are one number and cannot drift apart.
    ///
    /// # A TRIAGE IS A RECALL INSTRUMENT — measured 2026-08-26
    ///
    /// The first cut of this prompt asked whether each passage supported the
    /// claim **on its own**, reasoning that the location loop wants origins
    /// and the whole-window judge upstream has already settled assembly. That
    /// is a STRICTER bar than [`Self::claim_prompt`]'s, and putting a stricter
    /// bar in front of a calibrated judge inverts what a triage is for. On the
    /// binder bed it voted B on 49 of 52 candidates and threw away BOTH chunks
    /// the calibrated register went on to bind — turning a `Passed` claim with
    /// two origins into a corroboration-floor `CouldNotJudge`.
    ///
    /// So the standard now tracks `claim_prompt`'s exactly, and the tie-break
    /// is stated explicitly and in the recall direction: when unsure, admit.
    /// The cost of a false admit is one ~2.5s calibrated call that says no;
    /// the cost of a false reject is a citation the deliverable never gets and
    /// a verdict that silently changes. Those are not symmetric and the prompt
    /// says which way to err.
    pub(super) fn span_triage_prompt(&self, claim: &str, n: usize) -> (String, Option<usize>) {
        let mut prompt = self.prefix.clone();
        prompt.push_str(&format!(
            "\n\"\"\"\n\nThe {n} passages above are numbered 1 to {n} in the order shown.\n\n\
             CLAIM: {claim}\n\n\
             For EACH numbered passage, could that passage support the CLAIM — does it \
             state, clearly imply, or supply part of it? Paraphrase counts; partial \
             support counts; a passage merely mentioning the people or things involved, \
             without bearing on the claimed connection at all, does NOT count.\n\n\
             This is a SHORTLIST, not a verdict: each passage you mark A is then checked \
             by a stricter judge, so a wrong A costs almost nothing and a wrong B loses \
             the evidence for good. WHEN IN DOUBT, ANSWER A.\n\n\
             Output EXACTLY one line per passage, in order, formatted \"<n>: A\" (could \
             support the claim) or \"<n>: B\" (clearly irrelevant to it). Output the {n} \
             lines and nothing else.",
            claim = claim.chars().take(2_000).collect::<String>(),
        ));
        let boundary = self.prefix_len();
        debug_assert!(
            boundary.is_none_or(|b| prompt.is_char_boundary(b) && b <= prompt.len()),
            "the family boundary must be a char boundary inside the prompt"
        );
        debug_assert!(
            prompt.starts_with(&self.prefix),
            "a span-triage prompt must open with the family prefix"
        );
        (prompt, boundary)
    }

    /// The specifics scan's prompt as a MEMBER of the family (order
    /// audit-economy D3 candidate A): the family prefix, then the summary
    /// tier appended after the boundary (same placement as a thematic claim
    /// check's summaries), then the question, the answer, and the scan
    /// instruction. The instruction is the pre-candidate scan's, with the
    /// item budget folded into the user prompt because the system turn is
    /// now the family's shared constant and cannot carry `max_items`.
    pub(super) fn scan_prompt(
        &self,
        summaries: &[String],
        question: &str,
        answer: &str,
        max_items: usize,
    ) -> (String, Option<usize>) {
        let mut prompt = self.prefix.clone();
        for (i, chunk) in summaries.iter().enumerate() {
            if self.non_empty || i > 0 {
                prompt.push_str(PASSAGE_SEP);
            }
            prompt.push_str(chunk);
        }
        prompt.push_str(&format!(
            "\n\"\"\"\n\nA user asked: {q}\n\n\
             The assistant's ANSWER:\n\"\"\"\n{ans}\n\"\"\"\n\n\
             Compare the ANSWER against the passages above and list every statement \
             in the ANSWER that is UNSUPPORTED or WRONG given those passages. Three \
             kinds to catch:\n\
             (1) A fabricated specific — a named person/place/thing, number, date, \
             direct quotation, section/version/chapter reference, code identifier or \
             value, or claimed programming language that is NOT in the passages.\n\
             (2) A misattribution — a statement, position, or quote the answer credits \
             to the wrong author/source/speaker relative to what the passages show.\n\
             (3) A false claim ABOUT the passages — e.g. the answer says the sources do \
             NOT contain something that they DO contain, or vice-versa.\n\
             (4) A stitched relation — the answer presents a person or position as \
             bridging, combining, or agreeing with another when the passages never \
             state that relation, even if both sides are real.\n\
             A detail the passages state, even paraphrased, is SUPPORTED — do not list \
             it. Ignore [Source: …] citation markers entirely — they are validated by a \
             separate pass; never list one as unsupported. \
             When genuinely unsure, leave it out, but DO flag a clear contradiction. \
             Quote the answer's exact wording. One item per line, at most {max_items} \
             lines. Reply with exactly NONE only if every statement in the answer is \
             supported by the passages.",
            q = question.chars().take(400).collect::<String>(),
            ans = answer.chars().take(12_000).collect::<String>(),
        ));
        let boundary = self.prefix_len();
        debug_assert!(
            boundary.is_none_or(|n| prompt.is_char_boundary(n) && n <= prompt.len()),
            "the family boundary must be a char boundary inside the prompt"
        );
        (prompt, boundary)
    }
}

/// Render one joint-register claim prompt without a model call — the
/// replay harness's window into [`EvidenceFamily`] (which stays
/// `pub(super)`: the harness gets bytes to fingerprint, not a second
/// renderer to drift). Byte-identical to what
/// [`claim_violation_joint`] sends for `chunks = shared ++ appended`,
/// `n_stable = shared.len()` — asserted by
/// `replay_render_matches_the_joint_register` below, not argued here.
pub(super) fn replay_render_claim_prompt(
    shared: &[String],
    appended: &[String],
    claim: &str,
) -> (String, Option<usize>) {
    EvidenceFamily::new(shared).claim_prompt(appended, claim)
}

/// The batched register's prompt without a model call — the replay harness's
/// window into the batched shape, same contract as
/// [`replay_render_claim_prompt`]: byte-identical to what
/// [`claims_support_batched`] sends for the same `(shared, claims)`, asserted
/// by `replay_render_matches_the_batched_register` below.
pub(super) fn replay_render_batched_claims_prompt(
    shared: &[String],
    claims: &[String],
) -> (String, Option<usize>) {
    EvidenceFamily::new(shared).batched_claims_prompt(claims)
}

/// Score EVERY candidate span of ONE claim in a single generation — the
/// deep-research audit's location loop, batched.
///
/// # Why this exists
///
/// `deep_research::audit::assess_claim` locates a claim's origins by judging
/// the claim against each chunk's best span separately. Measured on the
/// pin-validate flight of 2026-08-25 (`runs-pin-validate/pinned-1.log`, 328
/// claim audits over 102.5 minutes): 35 claims — 11% — reached that loop and
/// consumed 90.6 minutes, 88% of the whole audit, at ~130s each against a
/// 57-chunk window. The other 285 claims short-circuited before the loop and
/// averaged 1.85s. The loop is one model call per chunk and it returned 0-2
/// bound chunks out of 57.
///
/// # The window is the PINNED one, and that is the whole latency argument
///
/// `passages` MUST be the same slice the pass's whole-window judge was given,
/// so `EvidenceFamily::new` renders a byte-identical prefix and the daemon
/// restores it instead of prefilling it. The first cut built the window from
/// claim-conditioned best-spans, which by construction shares a prefix with
/// nothing: measured 2026-08-26, that cost **71,947ms of pure prefill per
/// claim** (43,816 prompt chars at this host's ~160 tok/s) against the 1,613ms
/// the same claim's whole-window judge paid on a warm prefix. A triage that
/// costs more than the 52 calls it saves is not a triage.
///
/// # TRIAGE ONLY — this is never the released verdict
///
/// This is a text A/B over N lines, not the calibrated single-token
/// forced-choice logit, so `SUPPORT_FLOOR`'s semantics do not transfer to it —
/// the same gap [`claims_support_batched`] carries. It is therefore used
/// strictly to decide WHICH spans are worth the calibrated call: a span this
/// register admits is re-judged by [`claim_violation_joint`] against
/// `SUPPORT_FLOOR` before it may bind, and a span it cannot settle (`None`)
/// falls through to that same call. The only verdict it can change is a span's
/// REJECTION, whose consequence is a claim losing support it might have had —
/// could-not-judge rather than passed. That direction is the honesty floor's,
/// which is why this may default on where a pass-direction substitution could
/// not (ARCH §18.3).
///
/// Alignment is hardened exactly as the sibling register's is: explicit
/// numbering, and a mis-count leaves the affected rows `None` (fallback to the
/// calibrated call), never a shifted verdict.
pub async fn spans_supporting_claim_batched(
    inference: &Arc<dyn InferenceProvider>,
    claim: &str,
    passages: &[String],
    posture: ShardingPrivacy,
) -> Vec<Option<bool>> {
    let spans = passages;
    if spans.is_empty() {
        return Vec::new();
    }
    let family = EvidenceFamily::new(spans);
    let (prompt, stable_prefix_len) = family.span_triage_prompt(claim, spans.len());
    let req = CompletionRequest {
        prompt,
        stable_prefix_len,
        system_message: Some(CHUNK_JUDGE_SYSTEM.into()),
        preferred_speed: Speed::Slow,
        oicp: Some(Workload::Judge.requirements(posture)),
        // ~5 tokens per "<n>: A\n" verdict line + headroom for two-digit indices.
        max_tokens: Some(spans.len() * 8 + 16),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        ..Default::default()
    };
    match gate_call(&**inference, &req, GateCallMechanism::LocatedSpanTriage).await {
        Ok(resp) => {
            let verdicts = parse_batched_verdicts(&resp.text, spans.len());
            let n_sup = verdicts.iter().filter(|v| **v == Some(true)).count();
            let n_none = verdicts.iter().filter(|v| v.is_none()).count();
            dbg(&format!(
                "span triage: {} spans -> {} admitted, {} unparsed | raw head: {:?}",
                spans.len(),
                n_sup,
                n_none,
                resp.text.chars().take(220).collect::<String>()
            ));
            verdicts
        }
        Err(e) => {
            tracing::warn!(target: "grounding_gate", error = %e, "span triage pass failed");
            dbg(&format!("span triage failed: {e}"));
            // Total failure -> every span falls through to the calibrated call,
            // which is exactly today's behaviour. A failed triage costs time,
            // never a verdict.
            vec![None; spans.len()]
        }
    }
}

/// `n_stable`: how many leading entries of `chunks` are the shared prompt
/// window (byte-identical across every claim of this gate pass); entries after
/// that are claim-conditioned and vary per call. 0 = declare nothing.
pub async fn claim_violation_joint(
    inference: &Arc<dyn InferenceProvider>,
    claim: &str,
    chunks: &[String],
    n_chunks: usize,
    n_stable: usize,
    posture: ShardingPrivacy,
) -> Option<f64> {
    // The window every sibling of this pass shares, then this call's own
    // passages. The split is the caller's `n_stable` contract, unchanged; what
    // changed is that the boundary now comes from the rendered window's length
    // rather than from a second formula computing the same number.
    let seen = chunks.len().min(n_chunks);
    let split = n_stable.min(seen);
    let family = EvidenceFamily::new(&chunks[..split]);
    let (prompt, stable_prefix_len) = family.claim_prompt(&chunks[split..seen], claim);
    let (a, b) = forced_choice_ab(
        inference,
        &prompt,
        stable_prefix_len,
        posture,
        GateCallMechanism::PerClaimJudge,
    )
    .await?;
    let denom = a + b;
    let support = if denom > 0.0 { a / denom } else { 0.0 };
    Some(1.0 - support)
}

/// Batched support pre-pass: every claim judged in a SINGLE generation off one
/// evidence window, returning per-claim support aligned to the input order
/// (`Some(true)` supported, `Some(false)` unsupported, `None` = no clean
/// aligned verdict → the caller re-verifies that row with the calibrated
/// per-claim `claim_violation_joint`).
///
/// # History — the premise this register was built on is measured stale
///
/// The original rationale ("the N per-claim calls re-prefill the SAME evidence
/// N times — ~11x prefill / ~9x slower", 2026-07-20) predates
/// `SOVEREIGN_PREFIX_STATE`: whole-context state restore now amortizes the
/// evidence prefill across sibling judges (D0 of order `audit-economy`,
/// 2026-08-14: 129/129 per-claim calls restored the 8.25K-token window in
/// 34-53ms; per-claim calls median 1.78s, not prefill-bound). The original
/// batched shape — own scaffold, 1,500-char chunk cuts, own system turn, no
/// declared boundary — therefore paid a FULL ~9K-token prefill to replace
/// calls that no longer pay one: measured net-zero to net-negative on the
/// composed-arm instrument (`audit_economy_d0_decomposition_20260814.md`).
///
/// # The reshape: the batched call JOINS the judges' prefix family
///
/// The prompt now opens with the byte-identical [`EvidenceFamily`] prefix,
/// carries [`CHUNK_JUDGE_SYSTEM`], and declares the family boundary — so the
/// one batched call restores the pin its sibling judges use (or pins it for
/// them), and "one prefill" becomes a ~40ms restore on warm evidence. The
/// 1,500-char cut is gone for the same reason land B removed it from the
/// family: a cut chunk manufactures absences, and cut bytes can never
/// strict-prefix-match the pinned window.
///
/// STUDY ONLY (behind `SOVEREIGN_GATE_BATCH_VERIFY`): the verdict here is a
/// TEXT A/B over N lines, NOT the calibrated single-token forced-choice logit,
/// so `tau` semantics do not transfer — the `svrn bench judge-replay`
/// recalibration (order `audit-economy` D1) prices exactly that gap before any
/// flip. The deterministic in-world name/identifier veto still runs first,
/// catching blatant fabrication regardless of this register's verdict.
/// Alignment is hardened by explicit numbering; a mis-count leaves the
/// affected rows `None` (fallback), never a shifted verdict.
pub(super) async fn claims_support_batched(
    inference: &Arc<dyn InferenceProvider>,
    claims: &[String],
    chunks: &[String],
    n_chunks: usize,
    posture: ShardingPrivacy,
) -> Vec<Option<bool>> {
    if claims.is_empty() {
        return Vec::new();
    }
    let seen = chunks.len().min(n_chunks);
    let family = EvidenceFamily::new(&chunks[..seen]);
    let (prompt, stable_prefix_len) = family.batched_claims_prompt(claims);
    let req = CompletionRequest {
        prompt,
        stable_prefix_len,
        system_message: Some(CHUNK_JUDGE_SYSTEM.into()),
        preferred_speed: Speed::Slow,
        oicp: Some(Workload::Judge.requirements(posture)),
        // ~5 tokens per "<n>: A\n" verdict line + headroom for two-digit indices.
        max_tokens: Some(claims.len() * 8 + 16),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        ..Default::default()
    };
    match gate_call(&**inference, &req, GateCallMechanism::BatchedSupport).await {
        Ok(resp) => {
            let verdicts = parse_batched_verdicts(&resp.text, claims.len());
            let n_sup = verdicts.iter().filter(|v| **v == Some(true)).count();
            let n_none = verdicts.iter().filter(|v| v.is_none()).count();
            dbg(&format!(
                "batched verify: {} claims -> {} supported, {} unparsed | raw head: {:?}",
                claims.len(),
                n_sup,
                n_none,
                resp.text.chars().take(220).collect::<String>()
            ));
            verdicts
        }
        Err(e) => {
            tracing::warn!(target: "grounding_gate", error = %e, "batched verify pass failed");
            dbg(&format!("batched verify failed: {e}"));
            vec![None; claims.len()] // total failure → per-claim fallback for all
        }
    }
}

/// Parse `"<n>: A|B"` verdict lines into a per-claim support vec (1-based `n` →
/// 0-based index). Tolerant of `:`/`.`/`)` separators and list bullets; last
/// write wins; out-of-range or malformed rows stay `None` so the caller
/// re-verifies them with the calibrated pass. Pure/synchronous so the alignment
/// contract is pinned by `cargo test` without a model.
fn parse_batched_verdicts(text: &str, n: usize) -> Vec<Option<bool>> {
    let mut out = vec![None; n];
    for line in text.lines() {
        let t = line.trim().trim_start_matches(['-', '*', '•', ' ']).trim();
        let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        let idx = match digits.parse::<usize>() {
            Ok(v) if v >= 1 && v <= n => v - 1,
            _ => continue,
        };
        let rest = t[digits.len()..]
            .trim_start_matches([':', '.', ')', ' ', '-', '=', '>'])
            .trim();
        match rest.chars().next().map(|c| c.to_ascii_uppercase()) {
            Some('A') => out[idx] = Some(true),
            Some('B') => out[idx] = Some(false),
            _ => {} // ambiguous → leave None (fallback re-verifies)
        }
    }
    out
}

#[cfg(test)]
#[cfg(test)]
mod tests;
