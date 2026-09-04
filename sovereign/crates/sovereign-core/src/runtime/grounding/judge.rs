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
mod batched;
mod prompts;
mod scan;
pub use batched::*;
pub use prompts::*;
pub use scan::*;

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


#[cfg(test)]
mod tests;
