// SPDX-License-Identifier: AGPL-3.0-or-later
//! The gate's judges. Prompts are byte-identical to the bench critic
//! (`bench_cmd/live_runner.rs`) so the bench-calibrated threshold
//! transfers; divergence between the two is a bug in whichever
//! changed (same contract as sovereign-lint vs sovereign-test).

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
        claim: Some(claim),
        claim_evidence: extra,
    })
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
pub(super) async fn scan_unsupported_specifics(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    answer: &str,
    evidence_chunks: &[String],
    max_items: usize,
    posture: ShardingPrivacy,
) -> Option<Vec<String>> {
    // FULL chunk text, not the first 1500 chars. The truncation made this
    // scan flag its own evidence: measured 2026-08-13, it flagged "The Luck
    // Objection" as a fabricated specific while that phrase sat verbatim at
    // offset 1497 of a chunk it had been given — two characters past its own
    // cut (note 95b82f97). A scan whose charter is "this specific is NOT in
    // the evidence" cannot be shown a truncated copy of the evidence and be
    // asked that question honestly — a cut chunk manufactures absences.
    //
    // SIZED HONESTLY, because the cap was not the dominant defect: over 27
    // distinct leaf chunks the median is 897 chars and only 19% exceed 1500,
    // so the cap hid ~7% of leaf text. It bit rarely and expensively rather
    // than constantly. Lifting it is cheap for the same reason — the scan's
    // evidence grew from ~42k chars nominal to ~31.6k actual, because adding
    // the Summary chunks costs less than the cap was notionally saving.
    //
    // The bound that replaces it is the same one the drafter already cleared:
    // these chunks were assembled into the synthesis prompt and passed
    // `prompt_budget::enforce` for this turn's context window, and this scan's
    // prompt carries the same evidence plus one answer, so what fit there fits
    // here. The answer itself is still capped below (12k chars).
    let evidence: String = evidence_chunks.join("\n---\n");
    // No evidence to check against → nothing this scan can adjudicate.
    if evidence.trim().is_empty() {
        return Some(Vec::new());
    }
    // Audit the CONTENT of honestly-labeled spans, not the label: the wrapper
    // words bias the judge against supported content (see
    // `unwrap_unverified_excerpts`).
    let answer = &unwrap_unverified_excerpts(answer);
    // The question + evidence half, built separately from the answer half so
    // its byte length is exactly the stable-prefix boundary below. Splitting
    // the `format!` is the whole change: the CONCATENATION is byte-identical
    // to the single literal it replaced, which is what makes this a pure cost
    // change and not a judge-input change.
    let head = format!(
        "A user asked: {q}\n\n\
         EVIDENCE the assistant was given (passages separated by ---):\n\"\"\"\n{ev}\n\"\"\"\n\n",
        q = question.chars().take(400).collect::<String>(),
        ev = evidence,
    );
    let prompt = format!(
        "{head}\
         The assistant's ANSWER:\n\"\"\"\n{ans}\n\"\"\"\n\n\
         Compare the ANSWER against the EVIDENCE and list every statement in the \
         ANSWER that is UNSUPPORTED or WRONG given the evidence. Three kinds to \
         catch:\n\
         (1) A fabricated specific — a named person/place/thing, number, date, \
         direct quotation, section/version/chapter reference, code identifier or \
         value, or claimed programming language that is NOT in the evidence.\n\
         (2) A misattribution — a statement, position, or quote the answer credits \
         to the wrong author/source/speaker relative to what the evidence shows.\n\
         (3) A false claim ABOUT the evidence — e.g. the answer says the sources do \
         NOT contain something that they DO contain, or vice-versa.\n\
         A detail the evidence states, even paraphrased, is SUPPORTED — do not list \
         it. Ignore [Source: …] citation markers entirely — they are validated by a \
         separate pass; never list one as unsupported. \
         When genuinely unsure, leave it out, but DO flag a clear contradiction. \
         Quote the answer's exact wording. One item per line. Reply with exactly \
         NONE only if every statement in the answer is supported by the evidence.",
        ans = answer.chars().take(12_000).collect::<String>(),
    );
    // PREFIX-CACHE ALIGNMENT (D1a of the gate big-O order). This scan was the
    // one gate mechanism paying a FULL prefill of the evidence window on every
    // call: measured 2026-08-13 over three live desktop turns, one scan call
    // prefilling 37,038 chars cost 10,881 ms while five per-claim judges in the
    // SAME turn prefilled 28.7-33.6k chars each for 767-2,066 ms — 5-14x
    // cheaper, and the only difference was that the judges declared
    // `stable_prefix_len` and this call declared `None`
    // (`embedded/prefix_state.rs`: "the gate is the pin's ONLY consumer —
    // judge.rs passes stable_prefix_len; ~20 other construction sites pass
    // None"; this was one of them).
    //
    // What it buys and what it does not, stated so the next reader does not
    // over-read it. The pin amortises across SIBLING calls sharing the prefix.
    // The question and the evidence are identical between a turn's audit scan
    // and its re-audit scan — only the ANSWER changes, and the answer is on the
    // far side of this boundary — so the SECOND scan of a rewrite turn can
    // restore instead of re-prefilling (~13-15 s off every rewrite turn, the
    // path that misses the wall-time bar). A CLEAN turn issues one scan and has
    // no sibling to hit: it learns the pin and pays full price. Closing the
    // clean-turn half needs the scan and the per-claim judges to share ONE
    // prefix family, which is a change to what the judge SEES and is gated on
    // the adversarial set rather than taken here.
    //
    // Risk is structurally zero rather than argued: `prompt` is byte-identical
    // to what this function built before (the `format!` was split, not
    // rewritten), and `stable_prefix_len` is advisory — an engine without the
    // pin ignores it, and a declaration that does not match observed tokens
    // degrades to a full prefill, never to a different verdict.
    debug_assert!(
        prompt.starts_with(&head) && prompt.is_char_boundary(head.len()),
        "the stable prefix must be a real prefix of the prompt on a char boundary"
    );
    let req = CompletionRequest {
        prompt,
        stable_prefix_len: Some(head.len()),
        system_message: Some(format!(
            "You audit an answer's specifics against evidence, precisely and \
             conservatively. Reply with up to {max_items} lines, or NONE."
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
/// The engine's pinned-prefix cache keys a request family on the first 48
/// tokens of the RENDERED prompt and restores only on a strict token-prefix
/// match, so byte identity across sibling calls is not a nicety — it is the
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
}

/// `n_stable`: how many leading entries of `chunks` are the shared prompt
/// window (byte-identical across every claim of this gate pass); entries after
/// that are claim-conditioned and vary per call. 0 = declare nothing.
pub(super) async fn claim_violation_joint(
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

/// Batched support pre-pass: the evidence is prefilled ONCE and every claim is
/// judged in a SINGLE generation, returning per-claim support aligned to the
/// input order (`Some(true)` supported, `Some(false)` unsupported, `None` = no
/// clean aligned verdict → the caller re-verifies that row with the calibrated
/// per-claim `claim_violation_joint`).
///
/// Why this exists: on the `qwen35moe` primary, prefix caching is vetoed (Gated
/// DeltaNet partial-KV-keep corruption), so the N per-claim forced-choice calls
/// re-prefill the SAME evidence N times — measured ~11x more prefill / ~9x slower
/// on a real long-form turn ([[project_35b_moe_gate_latency_2026_07_20]]). One
/// sequence, one prefill sidesteps that without touching the prefix-cache hazard.
///
/// STUDY ONLY (behind `SOVEREIGN_GATE_BATCH_VERIFY`): the verdict here is a TEXT
/// A/B, NOT the calibrated single-token forced-choice logit. `gate_longform` uses
/// it for BOTH directions (the fan-out is dominated by unsupported claims, so
/// trusting only "supported" yields no net win) and re-verifies only the `None`
/// (parse-gap) rows with the calibrated pass; the deterministic in-world
/// name/identifier veto still runs first, catching blatant fabrication regardless.
/// Because `tau` is calibrated against the forced-choice logit, borderline claims
/// shift under the binary A/B — hence STUDY, needs recalibration before default-on.
/// Alignment is hardened by explicit numbering; a mis-count leaves the affected
/// rows `None` (fallback), never a shifted verdict.
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
    let joined: String = chunks
        .iter()
        .take(n_chunks)
        .map(|c| c.chars().take(1_500).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n---\n");
    let numbered: String = claims
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{}. {}", i + 1, c))
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = format!(
        "PASSAGES (multiple, separated by ---):\n\"\"\"\n{joined}\n\"\"\"\n\n\
         CLAIMS (numbered):\n{numbered}\n\n\
         For EACH numbered claim, do the passages, taken together, state or clearly \
         imply it? Support assembled across several passages counts; paraphrase \
         counts; the passages merely mentioning the people or things involved, \
         without establishing the claimed connection, does NOT count.\n\n\
         Output EXACTLY one line per claim, in order, formatted \"<n>: A\" (the \
         passages support it) or \"<n>: B\" (they do not). Output the {n} lines \
         and nothing else.",
        n = claims.len(),
    );
    let req = CompletionRequest {
        prompt,
        system_message: Some(
            "You are a careful classifier. For each numbered claim answer A or B.".into(),
        ),
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
mod tests {
    use super::*;
    use crate::error::Result;
    use crate::types::{CompletionResponse, Depth, ProviderCapabilities};
    use futures::Stream;
    use std::pin::Pin;
    use std::sync::Mutex;

    /// Records every `CompletionRequest` the gate issues and answers with a
    /// constant. Prefix-family membership is a property of the REQUEST — its
    /// system message and its prompt bytes — so these tests assert at the wire
    /// boundary and need no model.
    #[derive(Default)]
    struct CaptureProvider(Mutex<Vec<CompletionRequest>>);

    #[async_trait::async_trait]
    impl InferenceProvider for CaptureProvider {
        async fn complete(&self, r: &CompletionRequest) -> Result<CompletionResponse> {
            self.0.lock().unwrap().push(r.clone());
            Ok(CompletionResponse {
                // Parses as NONE for the scan and as an unusable forced-choice
                // reply for the judges, which is fine: these tests read the
                // REQUESTS, never the verdicts.
                text: "NONE".into(),
                tokens_used: 0,
                prompt_tokens: 0,
                model_id: "capture".into(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            })
        }
        async fn complete_stream(
            &self,
            _r: &CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            unimplemented!("no stream in prefix-family tests")
        }
        async fn embed(&self, _t: &str) -> Result<Vec<f32>> {
            unimplemented!("no embed in prefix-family tests")
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 32_768,
                supports_structured_output: false,
                relative_speed: Speed::Slow,
                relative_reasoning: Depth::Deep,
            }
        }
    }

    /// **§5.2 — one renderer owns the family, enforced at compile time.**
    ///
    /// The boundary got two deciders once already (a `format!` join and an
    /// arithmetic re-derivation of the same byte count, kept in step by hand),
    /// and a third lived in this very test module. `EvidenceFamily` collapsed
    /// them; this is what stops a fourth. In production code the family's
    /// literals may appear only in their `const` definitions and inside the
    /// renderer — a second construction site fails here with a file:line
    /// rather than as a silent cache miss weeks later (ARCH §10.6).
    ///
    /// Same mechanism as `call_census`'s funnel guard: `include_str!` is
    /// resolved by the compiler relative to THIS file, so it cannot go stale
    /// against a moved module or pass vacuously from another directory.
    #[test]
    fn one_renderer_owns_the_family() {
        const SRC: &str = include_str!("judge.rs");
        // Production code only: the test module legitimately names the
        // literals to assert against them.
        let prod = SRC.split("\n#[cfg(test)]").next().unwrap_or(SRC);
        let mut offenders: Vec<String> = Vec::new();
        let mut in_renderer = false;
        for (i, line) in prod.lines().enumerate() {
            if line.starts_with("impl EvidenceFamily {") {
                in_renderer = true;
            } else if in_renderer && line == "}" {
                in_renderer = false;
            }
            let l = line.trim_start();
            if l.starts_with("//") || l.starts_with("///") {
                continue;
            }
            // Only the FAMILY's literals are policed here. The exported
            // calibration surface (`CHUNK_JUDGE_SYSTEM`,
            // `CHUNK_JUDGE_PASSAGE_CHARS`) is deliberately referenced from two
            // crates — that sharing IS the fix — so it is guarded by the
            // single-render check below instead.
            let is_definition =
                l.starts_with("const PASSAGES_SCAFFOLD") || l.starts_with("const PASSAGE_SEP");
            if in_renderer || is_definition {
                continue;
            }
            if line.contains("PASSAGES_SCAFFOLD") || line.contains("PASSAGE_SEP") {
                offenders.push(format!("judge.rs:{}: {}", i + 1, line.trim()));
            }
        }
        assert!(
            offenders.is_empty(),
            "the evidence family is rendered outside `impl EvidenceFamily` — that is how \
             the boundary got two deciders the first time. Move it into the renderer:\n{}",
            offenders.join("\n")
        );

        // The calibrated chunk-judge register has the same one-renderer rule,
        // for a sharper reason: its second copy lived in ANOTHER CRATE (the
        // bench critic) and kept tau's transfer argument alive by hand. Its
        // opening literal may appear only inside `chunk_judge_prompt`.
        let renders: Vec<usize> = prod
            .lines()
            .enumerate()
            .filter(|(_, l)| l.contains("\"PASSAGE:"))
            .map(|(i, _)| i + 1)
            .collect();
        assert_eq!(
            renders.len(),
            1,
            "the calibrated per-passage judge prompt is rendered in {} places (lines {:?}); \
             it must be rendered only by `chunk_judge_prompt`, which the bench critic \
             imports — a second copy is how the byte-identity this module's header \
             claims becomes a comment describing a dead identity",
            renders.len(),
            renders
        );
    }

    /// How far past the old 1,500-char cap the fixture's long chunk reaches.
    /// A fixture at 935 chars — which is what land A shipped — cannot tell a
    /// re-introduced `.take(1_500)` from a correct renderer, so the guard
    /// built on it was watched to fail on `take(400)` and would have sat
    /// green through the real regression.
    const LONG_CHUNK_TAIL: usize = 1_800;

    /// Evidence whose first leaf chunk is deliberately LONGER than the cap
    /// land B removed, with multi-byte characters throughout — the two ways a
    /// renderer silently diverges (a re-introduced cut, a byte index landing
    /// mid-char).
    fn family_evidence() -> Vec<String> {
        vec![
            format!(
                "Ada Lovelace — première note «G» — {}",
                "é".repeat(LONG_CHUNK_TAIL)
            ),
            "The Analytical Engine was designed by Charles Babbage.".to_string(),
            "Menabrea's memoir was translated in 1843.".to_string(),
        ]
    }

    /// **§5.1 — the family contract, asserted at the wire boundary.**
    ///
    /// Prefix-cache membership is a property of the RENDERED request, so this
    /// checks the captured `CompletionRequest`s rather than the strings: byte
    /// identity of the shared window across a factual judge (extras only) and
    /// a thematic judge (summaries + extras), a declared boundary that is a
    /// real char boundary, and suffixes that actually diverge past it.
    ///
    /// The system-message assertion is the one that would have been missed:
    /// the engine keys the family on the first 48 tokens of the rendered
    /// prompt, **system message first**, so equal user-prompt prefixes are NOT
    /// family membership on their own. Land B unifies the judges' system
    /// message with the scan's; until then this pins that the judges at least
    /// agree with each other.
    ///
    /// Land C extends this to the scan.
    #[tokio::test]
    async fn the_gate_shares_one_prefix_family() {
        let cap = Arc::new(CaptureProvider::default());
        let inf: Arc<dyn InferenceProvider> = cap.clone();
        let posture = ShardingPrivacy::LocalOnly;
        let leaves = family_evidence();
        let summary = "SUMMARY: early computing pioneers and their attributions.".to_string();
        let extra = "A claim-conditioned hit fetched for this claim only.".to_string();

        // Factual: leaf window + a claim-conditioned extra appended after it.
        let mut factual = leaves.clone();
        factual.push(extra.clone());
        claim_violation_joint(
            &inf,
            "Lovelace wrote the first algorithm.",
            &factual,
            factual.len(),
            leaves.len(),
            posture,
        )
        .await;
        // Thematic: the same leaf window, then summaries, then an extra.
        let mut thematic = leaves.clone();
        thematic.push(summary);
        thematic.push(extra);
        claim_violation_joint(
            &inf,
            "The memoir shaped how the engine was understood.",
            &thematic,
            thematic.len(),
            leaves.len(),
            posture,
        )
        .await;

        // A DIFFERENT MECHANISM on the same family. Without this the
        // system-message assertion below cannot fail: both `claim_violation_joint`
        // calls route through `forced_choice_ab` as `PerClaimJudge`, so a fork
        // that varied the system turn BY MECHANISM left this test green
        // (checked twice — before and after land B). `claim_chunk_support`
        // reaches the same function as `ChunkJudge`, which is the input that
        // makes the assertion real. Its prompt is a single passage carrying no
        // family boundary, so it is excluded from the prefix loop and checked
        // only for the system turn.
        claim_chunk_support(&inf, &leaves[1], "Babbage designed it.", posture).await;

        let all = cap.0.lock().unwrap();
        assert_eq!(all.len(), 3, "two claim checks and one chunk check");
        assert_eq!(
            all[2].system_message,
            Some(CHUNK_JUDGE_SYSTEM.to_string()),
            "the single-passage judge left the calibrated forced-choice system turn. \
             That string is shared with the bench critic and is what tau=0.9 was \
             calibrated on — moving one side of it silently voids the transfer \
             argument in this module's header. Land C moves BOTH sides together."
        );
        let reqs = &all[..2];
        let m = reqs[0]
            .stable_prefix_len
            .expect("a non-empty leaf window must declare a boundary");
        for (i, r) in reqs.iter().enumerate() {
            assert_eq!(
                r.stable_prefix_len,
                Some(m),
                "request {i} declared a different boundary — siblings must agree"
            );
            assert!(
                r.prompt.is_char_boundary(m),
                "request {i}: boundary off a char boundary (multi-byte evidence)"
            );
            assert!(m < r.prompt.len(), "request {i}: boundary is not interior");
            assert_eq!(
                r.prompt.as_bytes()[..m],
                reqs[0].prompt.as_bytes()[..m],
                "request {i}: the shared window is not byte-identical"
            );
            assert_eq!(
                r.system_message, reqs[0].system_message,
                "request {i}: differing system messages are DIFFERENT prefix families, \
                 whatever the user prompt looks like"
            );
        }
        // The watched-to-fail arm: prompts that never diverge would make every
        // assertion above vacuous.
        assert_ne!(
            reqs[0].prompt, reqs[1].prompt,
            "the two claims must produce different suffixes or this test proves nothing"
        );
        // The long chunk survived intact inside the window — a re-introduced
        // truncation shows up here rather than as a silent cache miss.
        let head = &reqs[0].prompt[..m];
        assert_eq!(
            head.matches('é').count(),
            LONG_CHUNK_TAIL,
            "the leaf chunk was CUT inside the family window. Land B removed the \
             1,500-char cap precisely because a cut chunk manufactures absences — \
             a judge cannot honestly be asked 'do the passages support this' \
             against evidence with the support snipped off."
        );
        drop(all);

        // ── THE SCAN: not in the family yet, and that is the point ──
        //
        // The system-message assertion above is VACUOUS on judges alone —
        // both come from `forced_choice_ab`, which carries one constant, so
        // no perturbation of a single call site can make them differ (checked:
        // varying it by mechanism leaves this test green, because both judge
        // calls are the same mechanism). A check with no failing input you can
        // name is not a check (ARCH §18.1). The scan is that input: it carries
        // a DIFFERENT system message today — one that interpolates
        // `max_items`, so its family is not even stable against a budget
        // change — and by the engine's keying rule that alone puts it in a
        // different prefix family no matter how its prompt is laid out.
        //
        // So this records the pre-B state as an assertion rather than as a
        // comment. Land B unifies the system messages and land C moves the
        // scan onto `scan_prompt`; BOTH of these assertions then invert, and
        // this block becomes the positive check that the scan shares the
        // judges' family. A test that failed to notice the unification is a
        // test that would have let C ship without its own win.
        scan_unsupported_specifics(
            &inf,
            "Who wrote it?",
            "Lovelace wrote it.",
            &leaves,
            4,
            posture,
        )
        .await;
        let all = cap.0.lock().unwrap();
        assert_eq!(all.len(), 4, "the scan added its own request");
        let scan = &all[3];
        assert_ne!(
            scan.system_message, all[0].system_message,
            "the scan's system message now MATCHES the judges' — that is land B landing. \
             Flip this to assert_eq! and extend the byte-identity loop above to include \
             the scan; the family is only real when both hold."
        );
        assert!(
            !scan.prompt.starts_with(PASSAGES_SCAFFOLD),
            "the scan now opens with the judges' scaffold — that is land C landing. \
             Flip this and assert the scan's prompt[..M] matches the judges' byte-for-byte."
        );
    }

    /// The specifics scan's prefix-cache declaration (D1a). Two scans of the
    /// SAME turn — the audit and the re-audit — differ only in the answer, so
    /// the declared prefix must be byte-identical between them or the engine
    /// has nothing to restore. This is the property the whole change exists
    /// for, and it is a property of the PROMPT LAYOUT, so it is pinned here
    /// rather than inferred from a latency number.
    #[tokio::test]
    async fn the_specifics_scan_declares_a_prefix_its_sibling_can_reuse() {
        let cap = Arc::new(CaptureProvider::default());
        let inf: Arc<dyn InferenceProvider> = cap.clone();
        let evidence = vec![
            "Ada Lovelace wrote the first algorithm intended for a machine.".to_string(),
            "The Analytical Engine was designed by Charles Babbage.".to_string(),
        ];
        let posture = ShardingPrivacy::LocalOnly;
        // The audit pass, then the re-audit pass over a repaired answer.
        for answer in [
            "Lovelace wrote the first algorithm. Babbage built it in 1837.",
            "Lovelace wrote the first algorithm.",
        ] {
            scan_unsupported_specifics(&inf, "Who wrote it?", answer, &evidence, 4, posture)
                .await
                .expect("the capture stub always answers");
        }
        let reqs = cap.0.lock().unwrap();
        assert_eq!(reqs.len(), 2, "one call per scan");
        let n = reqs[0]
            .stable_prefix_len
            .expect("the scan must declare a prefix — this is the D1a change");
        assert_eq!(
            reqs[1].stable_prefix_len,
            Some(n),
            "both scans of a turn must declare the SAME boundary or the pin cannot be reused"
        );
        assert_eq!(
            reqs[0].prompt.as_bytes()[..n],
            reqs[1].prompt.as_bytes()[..n],
            "the declared prefix must be byte-identical across siblings"
        );
        assert!(
            reqs[0].prompt.is_char_boundary(n),
            "a declaration off a char boundary is rejected by the engine"
        );
        // It is a real prefix of a longer prompt, and the part after it is
        // what actually varies — i.e. the answer sits on the far side.
        assert!(n < reqs[0].prompt.len() && n < reqs[1].prompt.len());
        assert_ne!(
            reqs[0].prompt, reqs[1].prompt,
            "the two scans do differ — otherwise this test proves nothing"
        );
        // And the layout is still the one the judge is calibrated on: the
        // evidence is inside the declared prefix, the answer is outside it.
        let head = &reqs[0].prompt[..n];
        assert!(
            head.contains("Analytical Engine"),
            "evidence inside the pin"
        );
        assert!(!head.contains("Babbage built it in 1837"), "answer outside");
    }

    #[test]
    fn structural_specificity_fires_on_numbers_and_quotes_only() {
        // Numbers and quotations are form-level specificity — factual
        // regardless of vocabulary. (Semantic class for everything else
        // is the embed classifier's job — see claim_class_classifier
        // tests; no vocabulary assertions here by design.)
        assert!(claim_has_structural_specificity(
            "The text discusses the 1894 Greenwich bombing."
        ));
        assert!(claim_has_structural_specificity(
            "The section argues that \"esse est percipi\" grounds idealism."
        ));
        assert!(!claim_has_structural_specificity(
            "The text explores the theme of betrayal within the family."
        ));
        assert!(!claim_has_structural_specificity("Verloc runs a shop."));
    }

    #[test]
    fn batched_verdicts_align_by_number_and_fallback_on_gaps() {
        // Clean case: all rows present, mixed A/B, tolerant separators.
        let v = parse_batched_verdicts("1: A\n2. B\n3) A", 3);
        assert_eq!(v, vec![Some(true), Some(false), Some(true)]);
        // Out-of-order lines still land on the right claim (numbering, not position).
        let v = parse_batched_verdicts("2: B\n1: A", 2);
        assert_eq!(v, vec![Some(true), Some(false)]);
        // A missing row stays None (caller re-verifies with the calibrated pass);
        // a bullet-prefixed / prose-wrapped line is tolerated.
        let v = parse_batched_verdicts("- 1: A\n3: B", 3);
        assert_eq!(v, vec![Some(true), None, Some(false)]);
        // Out-of-range index is ignored (no panic, no shifted verdict).
        let v = parse_batched_verdicts("1: A\n9: B", 2);
        assert_eq!(v, vec![Some(true), None]);
        // Ambiguous verdict token → None, not a coin-flip.
        let v = parse_batched_verdicts("1: maybe\n2: B", 2);
        assert_eq!(v, vec![None, Some(false)]);
    }

    /// The artifact gate is a WORD gate, not a substring gate.
    ///
    /// Watched failing on a live desktop turn 2026-08-13: "Harry Frankfurt
    /// designed cases intended to prove moral responsibility does not require
    /// alternate possibilities" was vetoed as a fabricated in-world
    /// attribution. The gate opened because "de-SIGNED" contains "signed", and
    /// the bigram check then flagged "Harry Frankfurt" — a philosopher named
    /// in four of the turn's own chunks — because the corpus writes the
    /// surname alone. That single veto was the only thing between that turn
    /// and a zero-failure turn.
    ///
    /// Every string below is ordinary essay prose. Before the fix each one
    /// opened a veto meant for claims about emails, letters and source files.
    #[test]
    fn artifact_gate_matches_whole_words_not_substrings() {
        let hay = "frankfurt cases are the primary compatibilist response.";
        // "designed" ⊃ "signed" — the live case.
        assert_eq!(
            absent_name_attribution("Harry Frankfurt designed cases about responsibility.", hay),
            None,
            "\"designed\" must not open the artifact gate via \"signed\""
        );
        // "present" / "represent" / "consent" / "absent" / "sentence" ⊃ "sent"
        for prose in [
            "Peter Strawson present arguments about reactive attitudes.",
            "Galen Strawson represent the basic-argument position.",
            "Susan Wolf absent from this particular debate entirely.",
            "Robert Kane sentence structures favour event-causal accounts.",
        ] {
            assert_eq!(
                absent_name_attribution(prose, hay),
                None,
                "ordinary prose must not open the artifact gate: {prose:?}"
            );
        }
        // "classical" ⊃ "class", "denotes" ⊃ "notes" — identifier sibling.
        assert_eq!(
            absent_identifier_attribution(
                "Classical compatibilism denotes the Hobbes-Hume position.",
                hay
            ),
            None,
            "\"classical\"/\"denotes\" must not open the identifier gate"
        );
        // ...and the gate still OPENS on the real thing it was built for.
        assert_eq!(
            absent_name_attribution(
                "Betty Alexander sent an email about the schedule.",
                "unrelated evidence with no such person"
            ),
            Some("Betty Alexander".to_string()),
            "a genuine in-world artifact attribution must still be vetoed"
        );
    }

    #[test]
    fn name_sweep_skips_citation_labels_and_boilerplate() {
        // The persona-QA self-indictment class (2026-07-10): label fragments
        // and header bigrams flagged as fabricated names.
        assert_eq!(
            absent_name_attribution(
                "The passage discusses effects as documented [Source: Psilocybin Mushrooms — Effects]",
                "some unrelated evidence text"
            ),
            None
        );
        assert_eq!(
            absent_name_attribution(
                "From Retrieved Sources: the document describes the mechanism in a later section.",
                "some unrelated evidence text"
            ),
            None
        );
        // Heading bigrams and comma-separated name lists are not names
        // (overnight soak receipts).
        assert_eq!(
            absent_name_attribution(
                "**Energy Costs**: The document describes rate changes for households.",
                "unrelated evidence"
            ),
            None
        );
        assert_eq!(
            absent_name_attribution(
                "The letter was signed by Hamilton, Madison and Jay together.",
                "hamilton wrote often. madison replied. jay concurred."
            ),
            None
        );
        // Surname + capitalized pronoun is not a name ("Webber He
        // averaged…" — observed live).
        assert_eq!(
            absent_name_attribution(
                "The document states Webber He averaged 19.1 points per game.",
                "webber averaged 19.1 points"
            ),
            None
        );
        // Positive control: a genuine in-world attribution absent from
        // evidence still trips the veto.
        assert_eq!(
            absent_name_attribution(
                "The email was sent by Betty Alexander to the finance team.",
                "totally different evidence"
            ),
            Some("Betty Alexander".to_string())
        );
        // Unclosed bracket strips to end-of-line, not end-of-answer.
        assert_eq!(
            absent_name_attribution(
                "cited in [Source: Broken Label\nThe letter was written by Elowen Marsh yesterday.",
                "nothing relevant"
            ),
            Some("Elowen Marsh".to_string())
        );
    }

    #[test]
    fn self_referential_declines_are_exempt() {
        // The two live-observed rejection shapes (persona-QA 2026-07-10).
        assert!(is_self_referential_decline(
            "The system does not have access to real-time earthquake or tsunami data for Japan."
        ));
        assert!(is_self_referential_decline(
            "As of 2026-07-10, there is no evidence that the assistant's capabilities include live seismic feeds."
        ));
        assert!(is_self_referential_decline(
            "The provided passages do not contain real-time viewership data."
        ));
        // Markdown-decorated variant (scan findings arrive with emphasis).
        assert!(is_self_referential_decline(
            "**The system does **not** have access to real-time earthquake data"
        ));
    }

    #[test]
    fn world_claims_are_not_exempt() {
        assert!(!is_self_referential_decline(
            "Azelaic acid inhibits tyrosinase and has anti-inflammatory properties."
        ));
        assert!(!is_self_referential_decline(
            "Family Guy remains a consistent driver of engagement on Hulu."
        ));
        // System-subject but AFFIRMATIVE (not a decline) stays in jurisdiction.
        assert!(!is_self_referential_decline(
            "The system retrieves twelve chunks per query."
        ));
    }

    const ANSWER: &str = "Robinson attacked aggregate production functions and \
        neoclassical production theory more broadly, a task she showed to be \
        circular reasoning [Source: Joan Robinson]. The lighthouse also appears \
        as a title of James Joyce's novel.";

    #[test]
    fn quoted_answer_span_is_extracted() {
        // The observed live shape: the model wraps the span in quotes and
        // appends judgment chatter after an em-dash.
        let item = "\"and neoclassical production theory more broadly\" — The \
                    evidence does not mention this";
        assert_eq!(
            anchor_scan_item(item, ANSWER).as_deref(),
            Some("and neoclassical production theory more broadly")
        );
    }

    #[test]
    fn dash_appended_commentary_is_cut() {
        let item = "a task she showed to be circular reasoning — not stated in the sources";
        assert_eq!(
            anchor_scan_item(item, ANSWER).as_deref(),
            Some("a task she showed to be circular reasoning")
        );
    }

    #[test]
    fn ascii_hyphen_appended_commentary_is_cut() {
        // The shape the live judge actually emitted on the measured turn: a
        // plain " - ", which the em/en-dash list did not cover.
        let item = "a task she showed to be circular reasoning - the evidence does not say this";
        assert_eq!(
            anchor_scan_item(item, ANSWER).as_deref(),
            Some("a task she showed to be circular reasoning")
        );
    }

    #[test]
    fn abstractive_finding_is_not_a_claim() {
        // REVERSED 2026-08-08. This case used to pass through unchanged, on
        // the reasoning that an abstractive finding still guides the
        // corrective search. It does — but the same value is ALSO recorded
        // as a `failed_once` holding and listed in the user's verification
        // note, and there it is the judge talking about the answer rather
        // than a claim the answer made. The search hint is not worth a false
        // holding; see `judge_commentary_never_becomes_a_claim` for the
        // transcript this was measured on.
        let item = "The answer claims there is no single item explicitly labeled";
        assert_eq!(anchor_scan_item(item, ANSWER), None);
    }

    #[test]
    fn curly_quotes_are_handled() {
        let item =
            "“The lighthouse also appears as a title of James Joyce's novel” — misattributed";
        assert_eq!(
            anchor_scan_item(item, ANSWER).as_deref(),
            Some("The lighthouse also appears as a title of James Joyce's novel")
        );
    }

    #[test]
    fn emphasis_markers_do_not_hide_an_answer_span() {
        // The judge drops the answer's `**bold**` when it re-quotes. Anchoring
        // must see through that, or a real span falls off the ladder.
        let ans = "Corwin Pellow was murdered by **Severin Quenholt**, the broker.";
        let item = "\"Corwin Pellow was murdered by Severin Quenholt\" - not in the evidence";
        assert_eq!(
            anchor_scan_item(item, ans).as_deref(),
            Some("Corwin Pellow was murdered by Severin Quenholt")
        );
    }

    #[test]
    fn an_elided_quote_anchors_on_its_prefix() {
        let ans = "The killing took place at the inn on a pleasant evening in summer, \
                   where he sat with his usual glass and agreed with neighbors.";
        let item = "\"The killing took place at the inn on a pleasant evening in summer, \
                    where he sat with his usual glass...\" - This is fabricated.";
        assert_eq!(
            anchor_scan_item(item, ans).as_deref(),
            Some(
                "The killing took place at the inn on a pleasant evening in summer, \
                 where he sat with his usual glass"
            )
        );
    }

    #[test]
    fn a_stitched_quote_is_not_salvaged_into_a_fragment() {
        // An INTERIOR ellipsis means the judge spliced two spans and appended
        // a verdict. Anchoring must reject it rather than reduce it to the
        // bare name in front — that name is not the claim.
        let ans = "Severin Quenholt was the broker. Corwin Pellow was the harbormaster.";
        let item = "\"Severin Quenholt... As harbormaster, his signature validated salvage \
                    lots.\" (Misattribution: the text identifies Corwin Pellow as harbormaster.)";
        assert_eq!(anchor_scan_item(item, ans), None);
    }

    #[test]
    fn legitimate_em_dash_inside_a_present_item_is_kept() {
        // The whole item occurs in the answer -> no cut at its interior dash.
        let ans = "The rule — quiet hours after ten — is strict.";
        let item = "The rule — quiet hours after ten — is strict.";
        assert_eq!(
            anchor_scan_item(item, ans).as_deref(),
            Some("The rule — quiet hours after ten — is strict.")
        );
    }

    #[test]
    fn quoted_spans_extraction_walks_pairs() {
        let spans = extract_quoted_spans(r#"cites "[Source: x]" for "the atomic idea" here"#);
        assert_eq!(spans, vec!["[Source: x]", "the atomic idea"]);
    }

    // ---- The judge-prose defect, replayed from the transcript that shipped it.
    //
    // Provenance and the byte-identity check: `testdata/README.md`.
    // `saltgrass_compound_gv_shadow_20260808.transcripts.jsonl`, turn
    // `compound-killer-and-lugger`. Three of that turn's five `failed_once`
    // holdings were the specifics scan's own commentary, and the user read
    // them — in the ledger AND in the appended verification note — as their
    // answer's failed claims.

    /// The draft body the specifics scan audited (released answer, minus the
    /// verification note the gate appended afterwards).
    const POLLUTED_ANSWER: &str = include_str!("testdata/polluted_answer.md");
    /// The scan's raw reply, one judge line per line.
    const POLLUTED_SCAN_REPLY: &str = include_str!("testdata/polluted_scan_items.txt");
    /// The three prose rows exactly as the ledger recorded them.
    const POLLUTED_HOLDINGS: &str = include_str!("testdata/polluted_holdings.txt");

    #[test]
    fn judge_commentary_never_becomes_a_claim() {
        let items = scan_items_from_reply(POLLUTED_SCAN_REPLY, POLLUTED_ANSWER, 12);
        for prose in POLLUTED_HOLDINGS.lines().filter(|l| !l.trim().is_empty()) {
            assert!(
                !items.iter().any(|i| i == prose),
                "the ledger's judge-prose holding came back as a claim: {:?}\n\
                 items: {items:#?}",
                prose.chars().take(90).collect::<String>()
            );
        }
    }

    #[test]
    fn every_scan_item_is_a_span_of_the_answer() {
        // The positive half of the contract: whatever survives must be
        // wording the ANSWER used, not wording the judge used. Compared
        // modulo emphasis markers, because the judge re-quotes
        // `**Severin Quenholt**` as `Severin Quenholt`.
        let strip = |s: &str| -> String {
            s.to_lowercase()
                .chars()
                .filter(|c| !matches!(c, '*' | '_' | '`'))
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        let ans = strip(POLLUTED_ANSWER);
        for item in scan_items_from_reply(POLLUTED_SCAN_REPLY, POLLUTED_ANSWER, 12) {
            assert!(
                ans.contains(&strip(&item)),
                "scan item is not a span of the answer: {:?}",
                item.chars().take(90).collect::<String>()
            );
        }
    }

    #[test]
    fn the_turns_real_claims_survive_the_filter() {
        // Guard against over-correcting into silence: the two spans the
        // answer genuinely asserted are still flagged.
        let items = scan_items_from_reply(POLLUTED_SCAN_REPLY, POLLUTED_ANSWER, 12);
        assert_eq!(items.len(), 2, "expected 2 answer spans, got {items:#?}");
        assert!(items
            .iter()
            .any(|i| i == "Corwin Pellow was murdered by Severin Quenholt"));
        assert!(items
            .iter()
            .any(|i| i.starts_with("The killing took place at *The Cold Lantern* inn")));
    }

    #[test]
    fn unverified_excerpt_wrappers_unwrap_to_content() {
        let s = "It holds [unverified excerpt: As Samuelson (1954) noted, free-riding \
                 justifies provision] and more.";
        assert_eq!(
            unwrap_unverified_excerpts(s),
            "It holds As Samuelson (1954) noted, free-riding justifies provision and more."
        );
        // Unclosed wrapper survives verbatim (never destroy text).
        let broken = "tail [unverified excerpt: cut off";
        assert_eq!(unwrap_unverified_excerpts(broken), broken);
        // No wrapper → untouched.
        assert_eq!(unwrap_unverified_excerpts("plain"), "plain");
    }

    #[test]
    fn in_world_attribution_with_absent_name_is_vetoed() {
        let hay = "ok, jeff, you requested that we be candid about enron. rosalee \
                   fleming forwarded this to kenneth lay."
            .to_string();
        // The measured ghost: cleared at vp=0.010 by the joint judge.
        assert_eq!(
            absent_name_attribution(
                "Betty Alexander sent an email to Jeff Skilling on July 7, 2000.",
                &hay
            ),
            Some("Betty Alexander".to_string())
        );
        // A present name passes to the judge.
        assert_eq!(
            absent_name_attribution("Rosalee Fleming forwarded the email to Kenneth Lay.", &hay),
            None
        );
        // No artifact noun → general-knowledge territory → never vetoed
        // (do not shackle the model).
        assert_eq!(
            absent_name_attribution(
                "Noam Cohen called Wikipedia the last best place on the Internet.",
                &hay
            ),
            None
        );
        // Acronyms/date fragments are not name bigrams.
        assert_eq!(
            absent_name_attribution("The email was escalated to HR VP leadership in July.", &hay),
            None
        );
    }

    #[test]
    fn absent_identifier_attribution_is_vetoed() {
        let hay = "the step kind enum defines reason, tool, user, plan, act, and                    awaituserinfo. see planner.rs and cmd_design."
            .to_string();
        // gen75c ghosts: invented snake_case fn + invented file + invented variant.
        assert_eq!(
            absent_identifier_attribution("The material centers on the cmd_init function.", &hay),
            Some("cmd_init".to_string())
        );
        assert_eq!(
            absent_identifier_attribution("The file design_signals.rs defines the gaps.", &hay),
            Some("design_signals.rs".to_string())
        );
        assert_eq!(
            absent_identifier_attribution(
                "The StepKind enum values include ReasonWithTools.",
                &hay
            ),
            Some("ReasonWithTools".to_string())
        );
        // Present identifiers pass (case-insensitive), including real variants.
        assert_eq!(
            absent_identifier_attribution("The enum defines AwaitUserInfo as a variant.", &hay),
            None
        );
        assert_eq!(
            absent_identifier_attribution("The file planner.rs holds the logic.", &hay),
            None
        );
        // No artifact context → GK territory → untouched.
        assert_eq!(
            absent_identifier_attribution("React's useStateHook pattern is popular.", &hay),
            None
        );
    }

    #[test]
    fn wrapped_scan_item_is_judged_on_content() {
        // A scan item echoing the app's own wrapper must reduce to the span
        // content so the note never lists a double-wrapped self-indictment.
        let answer = "The gate held [unverified excerpt: ships cannot pay tolls at sea] today.";
        let item = "[unverified excerpt: ships cannot pay tolls at sea]";
        assert_eq!(
            anchor_scan_item(item, answer).as_deref(),
            Some("ships cannot pay tolls at sea")
        );
    }

    /// The scalpel's two arms and — load-bearing — what it must NOT exempt.
    /// The step-91 shape (2026-07-21 soak): decline headline + a POSITIVE
    /// meta-rider about the passages, which the negation-requiring longform
    /// predicate deliberately lets through, burned 16 per-passage checks +
    /// a doomed retry. The conjunction (decline headline AND meta subject)
    /// exempts it; a world-claim rider keeps its audit.
    #[test]
    fn decline_rider_exemption_scalpel() {
        let decline_answer = "I don't have reliable information on this. The \
             provided passages are Rust source code snippets from a \
             corpus-engine project.";
        // Arm 2: positive evidence-meta rider under a decline headline → exempt.
        assert!(decline_rider_exempt(
            decline_answer,
            "The provided passages are Rust source code snippets from a corpus-engine project."
        ));
        // World-claim rider under the same decline headline → NOT exempt
        // (subject is the world, must stay audited).
        assert!(!decline_rider_exempt(
            "I don't have reliable information on this. However, John Smith sent the memo.",
            "John Smith sent the memo on May 5."
        ));
        // No decline headline → a positive meta-shaped claim is NOT exempt
        // via arm 2 (the decline supplies the safety).
        assert!(!decline_rider_exempt(
            "The passages are Rust source code snippets.",
            "The passages are Rust source code snippets."
        ));
        // Arm 1: a negated self-referential decline claim is exempt
        // regardless of the answer's headline (longform-established shape).
        assert!(decline_rider_exempt(
            "Summary of what I found.",
            "The sources do not contain information about the lamp mechanism."
        ));
        // Markdown emphasis must not defeat the subject/negation matching.
        assert!(decline_rider_exempt(
            "I don't have reliable information on this.",
            "The **provided** passages are configuration files."
        ));
        // Pronoun-subject world claim under an answer that merely CONTAINS
        // a decline phrase ("does not contain") — the loose "it " prefix is
        // negation-guarded and must NOT satisfy the negation-free rider arm.
        assert!(!decline_rider_exempt(
            "The report does not contain the exact date, but John sent it in May.",
            "It was sent in May."
        ));
    }

    /// The declared stable prefix must be byte-identical across sibling
    /// claim-check prompts — one with claim-conditioned extras appended,
    /// one without — and land on a char boundary. This is the contract
    /// the engine's directed pin relies on; if the prompt construction
    /// and `stable_passages_prefix_len` drift apart, restores silently
    /// degrade to full prefills (latency, not correctness — but the
    /// whole point of the feature evaporates).
    #[test]
    fn stable_prefix_is_shared_across_sibling_prompts() {
        // This test used to build its OWN copy of the prompt to assert
        // against — a third renderer, kept in step by hand, which is the
        // drift `EvidenceFamily` exists to end. It now drives the real
        // renderer, so a layout change cannot pass by being made twice.
        let shared = vec![
            "alpha passage with some grounding text — ünïcode too".to_string(),
            "beta passage carrying different content".to_string(),
        ];
        let extras = vec!["claim-conditioned hit only one sibling has".to_string()];
        let family = EvidenceFamily::new(&shared);

        let (p_extras, n_extras) = family.claim_prompt(&extras, "claim one");
        let (p_plain, n_plain) = family.claim_prompt(&[], "another claim");
        let n = n_extras.expect("a non-empty window declares a boundary");
        assert_eq!(n_plain, Some(n), "siblings must declare the same boundary");
        assert!(p_extras.is_char_boundary(n) && p_plain.is_char_boundary(n));
        assert_eq!(
            &p_extras.as_bytes()[..n],
            &p_plain.as_bytes()[..n],
            "siblings must share the declared prefix byte-for-byte"
        );
        // The prompts genuinely diverge just past the boundary (separator
        // + extra vs block close — both open with '\n', so compare a small
        // window, not the single next byte).
        assert_ne!(
            &p_extras.as_bytes()[n..n + 5],
            &p_plain.as_bytes()[n..n + 5]
        );

        // Degenerate window: nothing stable to declare. Reported as absence
        // rather than as a zero-length boundary, and the prompt still renders
        // — with no leading separator before the first appended passage,
        // which an arithmetic boundary never had to get right.
        let empty = EvidenceFamily::new(&[]);
        assert_eq!(empty.prefix_len(), None);
        let (p_empty, n_empty) = empty.claim_prompt(&shared, "claim");
        assert_eq!(n_empty, None, "no window means no declaration");
        assert!(
            p_empty.starts_with(&format!("{PASSAGES_SCAFFOLD}alpha passage")),
            "an empty window must not emit a dangling separator: {:?}",
            &p_empty[..80.min(p_empty.len())]
        );
    }
}
