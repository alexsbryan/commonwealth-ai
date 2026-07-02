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
        match assess_asserted_value(&**inference, question, answer, chunks).await {
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
    dbg(&format!(
        "chunks_checked={checked} max_support={max_support:.3} vp={vp:.3}"
    ));
    Some(GateVerdict {
        violation_prob: vp,
        claim: Some(claim),
        claim_evidence: extra,
    })
}

/// Extract up to 4 specific, checkable factual claims from a
/// long-form answer. Empty vec = nothing checkable (essay of analysis
/// / opinion) — passes ungated.
pub(super) async fn extract_claim_list(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    answer: &str,
    max_claims: usize,
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
        preferred_speed: Speed::Medium,
        model_id: Some("primary".into()),
        max_tokens: Some((max_claims * 48).max(160)),
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
) -> Option<Vec<String>> {
    let evidence: String = evidence_chunks
        .iter()
        .map(|c| c.chars().take(1_500).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n---\n");
    // No evidence to check against → nothing this scan can adjudicate.
    if evidence.trim().is_empty() {
        return Some(Vec::new());
    }
    // Audit the CONTENT of honestly-labeled spans, not the label: the wrapper
    // words bias the judge against supported content (see
    // `unwrap_unverified_excerpts`).
    let answer = &unwrap_unverified_excerpts(answer);
    let prompt = format!(
        "A user asked: {q}\n\n\
         EVIDENCE the assistant was given (passages separated by ---):\n\"\"\"\n{ev}\n\"\"\"\n\n\
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
        q = question.chars().take(400).collect::<String>(),
        ev = evidence,
        ans = answer.chars().take(12_000).collect::<String>(),
    );
    let req = CompletionRequest {
        prompt,
        system_message: Some(format!(
            "You audit an answer's specifics against evidence, precisely and \
             conservatively. Reply with up to {max_items} lines, or NONE."
        )),
        preferred_speed: Speed::Medium,
        model_id: Some("primary".into()),
        max_tokens: Some((max_items * 40).max(160)),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        ..Default::default()
    };
    match inference.complete(&req).await {
        Ok(resp) => {
            let t = resp.text.trim();
            if t.is_empty() || t.to_uppercase().contains("NONE") {
                return Some(Vec::new());
            }
            Some(
                t.lines()
                    .map(|l| l.trim().trim_start_matches(['-', '*', '•']).trim())
                    .map(|l| {
                        l.trim_start_matches(|c: char| c.is_ascii_digit())
                            .trim_start_matches(['.', ')'])
                            .trim()
                            .to_string()
                    })
                    .filter(|l| l.len() > 8)
                    .map(|l| normalize_scan_item(&l, answer))
                    .take(max_items)
                    .collect(),
            )
        }
        Err(e) => {
            tracing::warn!(target: "grounding_gate", error = %e, "specifics scan failed");
            None
        }
    }
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
/// evidence"). Deterministic reduction, ordered:
/// 1. the longest QUOTED span that actually occurs in the answer → the span;
/// 2. a prefix cut at " — " that occurs in the answer (dash-appended
///    commentary) → the prefix;
/// 3. otherwise unchanged — an abstractive finding still guides the
///    corrective search, and the note renderer quotes it as-is.
fn normalize_scan_item(item: &str, answer: &str) -> String {
    let item = &unwrap_unverified_excerpts(item);
    let ans = squash(answer);
    let quoted: Vec<&str> = extract_quoted_spans(item);
    if let Some(best) = quoted
        .iter()
        .filter(|s| s.chars().count() >= 12 && ans.contains(&squash(s)))
        .max_by_key(|s| s.chars().count())
    {
        return best.trim().to_string();
    }
    if !ans.contains(&squash(item)) {
        for dash in [" — ", " – ", " -- "] {
            if let Some((head, _)) = item.split_once(dash) {
                let head = head.trim().trim_matches(['"', '“', '”']).trim();
                if head.chars().count() >= 12 && ans.contains(&squash(head)) {
                    return head.to_string();
                }
            }
        }
    }
    item.trim().trim_matches(['"', '“', '”']).trim().to_string()
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

/// Lowercase + collapse whitespace runs, for tolerant containment checks.
fn squash(s: &str) -> String {
    s.to_lowercase()
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
pub(super) fn absent_name_attribution(claim: &str, hay_lower: &str) -> Option<String> {
    const ARTIFACT: &[&str] = &[
        "email", "e-mail", "letter", "memo", "message", "document", "passage",
        "chapter", "section", "thread", "forwarded", "sent", "wrote", "authored",
        "signed", "replied",
    ];
    let low = claim.to_lowercase();
    if !ARTIFACT.iter().any(|a| low.contains(a)) {
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
        if let (Some(a), Some(b)) = (cap_name(pair[0]), cap_name(pair[1])) {
            let full = format!("{a} {b}").to_lowercase();
            if !hay_lower.contains(&full) {
                return Some(format!("{a} {b}"));
            }
        }
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;

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
            normalize_scan_item(item, ANSWER),
            "and neoclassical production theory more broadly"
        );
    }

    #[test]
    fn dash_appended_commentary_is_cut() {
        let item = "a task she showed to be circular reasoning — not stated in the sources";
        assert_eq!(
            normalize_scan_item(item, ANSWER),
            "a task she showed to be circular reasoning"
        );
    }

    #[test]
    fn abstractive_finding_passes_through() {
        // Commentary with no answer span stays intact (it still guides the
        // corrective search); only wrapping quotes are trimmed.
        let item = "The answer claims there is no single item explicitly labeled";
        assert_eq!(normalize_scan_item(item, ANSWER), item);
    }

    #[test]
    fn curly_quotes_are_handled() {
        let item =
            "“The lighthouse also appears as a title of James Joyce's novel” — misattributed";
        assert_eq!(
            normalize_scan_item(item, ANSWER),
            "The lighthouse also appears as a title of James Joyce's novel"
        );
    }

    #[test]
    fn legitimate_em_dash_inside_a_present_item_is_kept() {
        // The whole item occurs in the answer -> no cut at its interior dash.
        let ans = "The rule — quiet hours after ten — is strict.";
        let item = "The rule — quiet hours after ten — is strict.";
        assert_eq!(
            normalize_scan_item(item, ans),
            "The rule — quiet hours after ten — is strict."
        );
    }

    #[test]
    fn quoted_spans_extraction_walks_pairs() {
        let spans = extract_quoted_spans(r#"cites "[Source: x]" for "the atomic idea" here"#);
        assert_eq!(spans, vec!["[Source: x]", "the atomic idea"]);
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
    fn wrapped_scan_item_is_judged_on_content() {
        // A scan item echoing the app's own wrapper must reduce to the span
        // content so the note never lists a double-wrapped self-indictment.
        let answer = "The gate held [unverified excerpt: ships cannot pay tolls at sea] today.";
        let item = "[unverified excerpt: ships cannot pay tolls at sea]";
        assert_eq!(
            normalize_scan_item(item, answer),
            "ships cannot pay tolls at sea"
        );
    }
}
