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
//! Long-form answers (past the profile's `longform_chars` pivot) take
//! the per-claim ladder instead: audit (each claim judged against the
//! prompt snapshot ∪ claim-conditioned sealed search) → ONE rewrite
//! fed the failed claims' corrective passages → visible verification
//! note on anything still unverified. An essay is never abstained.
//!
//! Surfaces, budgets, and the env surface (`SOVEREIGN_GROUNDING_GATE`,
//! per-surface overrides, `SOVEREIGN_GV_THRESHOLD`) live in
//! `config.rs` (`GateSurface`/`GroundingProfile`/
//! `grounding_gate_flags`); judges in `judge.rs` (prompts byte-pinned
//! to the bench critic); sealed claim search in `search.rs`.
//!
//! Scope guards (same as the bench critic): declines and explicitly
//! GK-attributed answers extract as NO_CLAIM and pass (the honest
//! OOD-caveat case must not be gated) — except on entity-anchored
//! questions, where a GK caveat cannot exempt an in-world claim.

mod citation;
mod citation_attribution;
mod config;
mod judge;
mod search;
mod value_presence;

// The gold-free groundedness primitive: the gate consumes it to DECIDE, the
// chaos scorer consumes it to MEASURE `blatant_confab_rate`. Re-exported up to
// `sovereign_core::runtime` (see runtime.rs) so the bench shares one
// implementation rather than re-deriving the check.
pub use value_presence::{assess_asserted_value, AssertedValue};

// The supporting-specifics half of groundedness: `value_presence` checks the
// answer's top-line VALUE, this strips `[Source: …]` citations whose title is
// absent from the evidence. The gate consumes it in `gate_held_answer`.
// `CitationAttribution` (the return type) is exported alongside but not yet named
// by a consumer — same `#[allow]` idiom as `grounding_gate_flags`.
#[allow(unused_imports)]
pub use citation_attribution::{attribute_citations, CitationAttribution};

pub(crate) use config::{dbg, grounding_gate_enabled, GateSurface, GroundingProfile};
// Registry export: consumed by the config-module coverage test today;
// the docs flag table renders from it (same contract as
// `retrieval_pipeline_flags`).
#[allow(unused_imports)]
pub use config::grounding_gate_flags;
#[allow(unused_imports)]
pub(crate) use judge::{verify_grounding, GateVerdict};
// `ClaimSearcher` is constructed via `Runtime::claim_searcher`; the
// type re-exports are for call sites that name them.
#[allow(unused_imports)]
pub(crate) use search::{AttachedAssetSearcher, ClaimSearcher, SealedEvidenceSearch};

use std::collections::HashSet;
use std::sync::Arc;

use crate::traits::InferenceProvider;
use crate::types::CompletionRequest;

use judge::{claim_violation_joint, extract_claim_list};

/// WHAT one released answer is verified against — the sealed evidence
/// universe for one turn. Owned values throughout (the gate runs in
/// spawned stream tasks that hold no `&Runtime`).
pub(crate) struct EvidenceContext {
    /// Prompt-snapshot evidence the draft was synthesized from.
    pub chunks: Vec<String>,
    /// Legitimate citation labels for the citation-attribution check — each
    /// retrieved chunk's title and corpus id (what the synthesis presents as
    /// `[Source: …]` headers and the model cites). A `[Source: X]` whose words are
    /// absent from the chunk BODY but present in a label is grounded, not a
    /// fabrication. Empty when labels are unavailable (tool-transcript / step-
    /// summary evidence) — the check is then body-only.
    pub source_labels: Vec<String>,
    /// Claim-conditioned widening WITHIN the sealed universe.
    /// `None` = the snapshot IS the universe (e.g. tool transcripts).
    pub searcher: Option<Arc<dyn SealedEvidenceSearch>>,
    /// In-world question: a general-knowledge attribution cannot
    /// exempt a claim from extraction (see `verify_grounding`).
    pub entity_anchored: bool,
    /// Best retrieval similarity (max cosine = `1 - vector_distance`) over the
    /// chunks the draft saw, when known. Used ONLY by the env-gated retry floor
    /// (`SOVEREIGN_KQ_RETRY_FLOOR`): the gate's retry exists for the
    /// good-evidence-but-bad-draft case, so a high value means "the answer is in
    /// the evidence — re-synthesise" while a low value means "the evidence can't
    /// ground an answer — skip the second 35B synthesis and abstain now". `None`
    /// (FTS-only / surfaces that don't thread it) disables the floor → the retry
    /// fires exactly as before. Default behaviour is unchanged.
    pub top_similarity: Option<f32>,
}

/// Fix B — provenance-aware grounding (2026-06-17). Build the gate's evidence
/// chunk strings from scored chunks, EXCLUDING derived RAPTOR summary chunks
/// (`metadata["source"]=="raptor"`) by default.
///
/// A RAPTOR summary is an abstractive, LLM-generated paraphrase: it
/// legitimately aids retrieval and synthesis (recall), but it must never be
/// the source-of-truth a factual claim is VERIFIED against. A summary that
/// inferred an unstated fact (the witnessed "the Russian agent Vladimir", with
/// "Russian" absent from the source) would otherwise "support" an answer
/// asserting the same — a fabrication grounding a fabrication. Excluding
/// derived summaries here keeps the gate anchored to actual source chunks
/// while leaving summaries in the upstream synthesis context.
///
/// Set `SOVEREIGN_GATE_EXCLUDE_RAPTOR=0`/`false` to disable (the A/B baseline
/// that reproduces the pre-fix "summaries are source-equivalent evidence"
/// behaviour).
pub(crate) fn gate_evidence_chunks(chunks: &[corpus_engine::ScoredChunk]) -> Vec<String> {
    let exclude = std::env::var("SOVEREIGN_GATE_EXCLUDE_RAPTOR")
        .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false")))
        .unwrap_or(true);
    chunks
        .iter()
        .filter(|c| !(exclude && c.metadata.get("source").map(String::as_str) == Some("raptor")))
        .map(|c| c.content.clone())
        .collect()
}

/// The legitimate citation LABELS for `attribute_citations`: each chunk's title
/// and corpus id — the source identifiers the synthesis presents as `[Source: …]`
/// headers and the model cites. Unlike `gate_evidence_chunks` these are NOT body
/// text; they only WIDEN what the citation check counts as grounded, so a citation
/// naming a source by its corpus or section title is not mistaken for a fabrication.
/// RAPTOR summaries are NOT excluded here: a summary's title/corpus is still a real
/// label, and since labels never narrow groundedness, including them is always safe.
pub(crate) fn gate_evidence_source_labels(chunks: &[corpus_engine::ScoredChunk]) -> Vec<String> {
    let mut out = Vec::with_capacity(chunks.len() * 2);
    for c in chunks {
        if let Some(t) = c.title.as_deref() {
            let t = t.trim();
            if !t.is_empty() {
                out.push(t.to_string());
            }
        }
        let cid = c.corpus_id.trim();
        if !cid.is_empty() {
            out.push(cid.to_string());
        }
    }
    out
}

/// One audit-failed claim plus the claim-conditioned passages its
/// targeted search returned — the rewrite's correction material.
struct FailedClaim {
    claim: String,
    evidence: Vec<String>,
}

/// The grounded abstention released when both drafts fail the gate.
///
/// Deliberately does NOT restate the rejected claim's value. The old wording
/// ("The draft answer asserted that Heat's first name is Vernon …") re-uttered
/// the fabrication even while disclaiming it: a strict judge reads the named
/// value as an answer (measured — the primary judge scored these as "answered",
/// so the gate's abstentions didn't count), and a skimming user sees the
/// fabricated specific anyway. The failed claim is preserved in the gate's
/// glassbox `meta` / trace, not in the user-facing text — observability without
/// leakage.
pub(crate) fn grounded_abstention(_claim: &str, chunks_checked: usize) -> String {
    format!(
        "I looked through the {chunks_checked} passages your sources turned up for \
         this, but none of them actually cover it — so I'd rather not guess at an \
         answer that isn't there. If you think it's in your sources, try rephrasing \
         with the specific names or terms involved and I'll take another look; \
         otherwise it may just not be recorded there."
    )
}

/// Remove a leading general-knowledge caveat ("Not in your sources — from
/// general knowledge: …") so the gate verifies the asserted CLAIM, not the
/// hedge. Applied ONLY on entity-anchored questions: there a GK caveat can never
/// legitimately answer an in-world question, so the value after it must be
/// grounded or dropped. For genuinely out-of-domain questions (not
/// entity-anchored) the caveat IS the honest move and is left intact — this is
/// why the strip is gated on `entity_anchored`, not applied unconditionally.
fn strip_gk_caveat(text: &str) -> String {
    if let Some(rest) = text.strip_prefix(crate::runtime::prompts::GK_CAVEAT_PREFIX) {
        return rest.trim_start().to_string();
    }
    // Robustness: the marker may not sit at the very start.
    let low = text.to_lowercase();
    if let Some(p) = low.find("from general knowledge:") {
        if let Some(after) = text[p..].splitn(2, ':').nth(1) {
            return after.trim().to_string();
        }
    }
    text.to_string()
}

/// System-message suffix for the single gated retry. Quotes the failed
/// claim back — the second draft knows exactly which assertion failed
/// verification and must either ground it or drop it.
pub(crate) fn retry_system_note(claim: &str, corrective: &[String]) -> String {
    const RETRY_EVIDENCE_PER_CLAIM: usize = 2;
    const RETRY_EVIDENCE_CHARS: usize = 700;
    let mut note = format!(
        "\n\nGROUNDING CHECK FAILED on your previous draft. It asserted: \"{claim}\" — \
         no retrieved passage supports that assertion."
    );
    if corrective.is_empty() {
        note.push_str(
            " Write a new answer using ONLY what the passages state. If the passages \
             do not contain the asked-for fact, say plainly that the sources do not \
             state it. Do not repeat the unsupported assertion.",
        );
    } else {
        // Parity with the long-form rewrite (measured v13c–v15): a
        // retry told only WHICH assertion failed, with no passages
        // stating the truth, can only delete and disclaim.
        note.push_str("\n  What the sources actually say on this point:");
        for p in corrective.iter().take(RETRY_EVIDENCE_PER_CLAIM) {
            let trimmed: String = p.chars().take(RETRY_EVIDENCE_CHARS).collect();
            note.push_str(&format!("\n  | {}", trimmed.replace('\n', "\n  | ")));
        }
        note.push_str(
            "\nWrite a new answer using ONLY what the passages state — if the \
             passages above contain the asked-for fact, state it (with citations); \
             do not repeat the unsupported assertion.",
        );
    }
    note
}

/// Final outcome of a full gate ladder over one draft answer.
pub(crate) struct GateOutcome {
    pub text: String,
    /// `grounding_gate` metadata for the message (action, retried,
    /// violation_prob / failed_claims, threshold).
    pub meta: serde_json::Value,
}

/// The complete gate ladder, shared by every gated surface (see
/// `GateSurface`): short answers go through the single-claim
/// verify → retry → abstain ladder; long-form answers (past the
/// profile's `longform_chars` pivot) go through the per-claim
/// audit → rewrite → annotate ladder. Fail-open on judge failure
/// everywhere — the gate is a quality lever, not an availability
/// risk.
/// Env-gated retry floor (`SOVEREIGN_KQ_RETRY_FLOOR`, absolute cosine
/// similarity in 0..1): when the best retrieval similarity for a turn is below
/// this, the gate skips its second-synthesis retry and abstains directly. Unset
/// (or out of range) → no floor, the retry fires exactly as before.
fn retry_floor_env() -> Option<f32> {
    std::env::var("SOVEREIGN_KQ_RETRY_FLOOR")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .filter(|f| *f > 0.0 && *f < 1.0)
}

pub(crate) async fn gate_answer(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    draft: String,
    evidence: &EvidenceContext,
    base_request: &CompletionRequest,
    profile: &GroundingProfile,
) -> GateOutcome {
    let tau = profile.tau;
    let chunks: &[String] = &evidence.chunks;
    let entity_anchored = evidence.entity_anchored;
    // Verify-correct pivot. gate_longform is the BS-catcher: it extracts each
    // asserted claim, RE-SEARCHES the sealed corpus for that claim's evidence,
    // and REWRITES the ones the corpus won't support — catching the load-bearing-
    // specific confabulation ("Ernest Rhys Jones" for "Ernest Rhys") that the
    // single-claim path waves through. Short factual answers skip it by default
    // (pivot 1_800); `SOVEREIGN_LONGFORM_CHARS` A/Bs routing them through it
    // (0 = always per-claim, the resilient default complex_task already uses) so
    // the architecture catches a model's first-pass BS rather than trusting it.
    let longform_pivot = std::env::var("SOVEREIGN_LONGFORM_CHARS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(profile.longform_chars);
    if draft.chars().count() > longform_pivot {
        return gate_longform(inference, question, draft, evidence, base_request, profile).await;
    }
    // Glassbox (debug-gated): record the pre-gate draft into the message meta so
    // the measurement layer can tell a gate-killed-CORRECT answer from a
    // confabulation the gate correctly caught — the partition's gate-vs-model
    // split (docs/CHAOS_MEASUREMENT_REDESIGN.md). `None` (→ null) in production:
    // the rejected draft can be the very confab the gate suppressed. Short-path
    // only — gate_longform never produces a clean abstention, so the split there
    // is vacuous. Moved into each diverging return below.
    let draft_for_meta: Option<String> = config::debug_enabled().then(|| draft.clone());
    // Active citation-grounding (entity-anchored fact queries, flag-gated).
    // Replaces generate-then-substring-verify with quote-then-answer: the model
    // must copy a verbatim supporting sentence before it answers, which forces
    // it to read the retrieved context (curing the measured A3B confabulations
    // where the answer was present but unused — "blowpipe" for the carving
    // knife) and grounds by quote-existence rather than value-substring (curing
    // the STOP-list/paraphrase false-negatives that killed "Chief Inspector").
    // Inconclusive (extraction error/unparseable) falls through to the legacy
    // ladder — fail-open, never a refusal from a hiccup. Does not consume the
    // draft, so the fall-through path is unchanged.
    if config::citation_grounding_enabled()
        && (entity_anchored || config::citation_broad_enabled())
    {
        if let citation::CitationOutcome::Grounded { answer, quote } =
            citation::citation_grounded_answer(&**inference, question, chunks).await
        {
            dbg(&format!(
                "citation: GROUNDED → release (answer={:?} quote_chars={})",
                answer.chars().take(60).collect::<String>(),
                quote.len()
            ));
            // Release the grounded value WITH its supporting quote as a
            // citation: glassbox (the user sees the exact sentence that grounds
            // the answer) AND a bare value ("the Doctor") is otherwise mis-read
            // as an abstention by the downstream answer/abstain classifier, which
            // wants a fuller response. The terse `answer` is what was verified
            // against the quote.
            let cited = format!(
                "{answer}\n\nGrounded in the source: \"{}\"",
                quote.chars().take(220).collect::<String>()
            );
            return GateOutcome {
                text: cited,
                meta: serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": "citation_grounded",
                    "retried": false,
                    "mode": "citation",
                    "quote_chars": quote.len(),
                    "draft": draft_for_meta,
                }),
            };
        }
        // Not tightly grounded (no quote, quote not verbatim, or answer not
        // supported by its own quote) → fall through to the legacy verify →
        // retry → abstain ladder. Citation is purely ADDITIVE: it can only
        // upgrade a legacy abstention into a grounded release — it never causes
        // an abstention itself nor replaces a correct draft, so the legacy path
        // stays the honesty floor (measured 1.00).
        dbg("citation: not tightly grounded → fall through to legacy ladder");
    }
    let mut text = draft;
    let mut action = "released";
    let mut retried = false;
    let mut final_vp: Option<f64> = None;
    dbg(&format!(
        "gate_answer entity_anchored={entity_anchored} chunks={} draft={:?}",
        chunks.len(),
        text.chars().take(240).collect::<String>()
    ));
    // Structural exemption-closing: on an entity-anchored question, strip any GK
    // caveat before extraction so the asserted claim is actually verified rather
    // than exempted as NO_CLAIM. The released `text` is unchanged; only what the
    // verifier reads is de-caveated.
    let verify_text = if entity_anchored { strip_gk_caveat(&text) } else { text.clone() };
    match verify_grounding(
        inference,
        question,
        &verify_text,
        chunks,
        entity_anchored,
        evidence.searcher.as_ref(),
    )
    .await
    {
        Some(v) => {
            final_vp = Some(v.violation_prob);
            dbg(&format!(
                "  verify: vp={:.3} tau={tau} claim={:?}",
                v.violation_prob,
                v.claim.as_deref().map(|c| c.chars().take(70).collect::<String>())
            ));
            if v.violation_prob >= tau {
                if let Some(claim) = v.claim.clone() {
                    if !profile.retry {
                        // Verify-only surfaces (Refinement): no second
                        // synthesis — the caller decides what replaces
                        // the failed text (typically: keep the prior
                        // verified answer).
                        text = grounded_abstention(&claim, chunks.len().min(12));
                        action = "abstained_no_retry";
                        return GateOutcome {
                            text,
                            meta: serde_json::json!({
                                "surface": profile.surface.id(),
                                "action": action,
                                "retried": false,
                                "violation_prob": final_vp,
                                "threshold": tau,
                                "mode": "single_claim",
                                "draft": draft_for_meta,
                            }),
                        };
                    }
                    // Env-gated retry floor: the retry below is a SECOND full
                    // 35B synthesis, justified only when the evidence could
                    // ground a better answer (the good-evidence-but-bad-draft
                    // case the retry exists for). When the best retrieval
                    // similarity is below the floor, the evidence can't ground an
                    // answer — the retry would near-certainly fail again after
                    // paying for it (the observed 50-160s slow-abstention) — so
                    // abstain now. This never changes the answer/abstain DECISION
                    // on a turn the gate already failed; it only skips a wasted
                    // retry (gates COST, not competence), so it can't trigger the
                    // Critic-as-gate over-abstain regression. Default-off no-op.
                    if let (Some(floor), Some(sim)) = (retry_floor_env(), evidence.top_similarity) {
                        if sim < floor {
                            tracing::info!(
                                target: "grounding_gate",
                                top_similarity = sim,
                                retry_floor = floor,
                                vp = v.violation_prob,
                                "grounding gate: retry skipped — evidence below retry floor, abstaining without a second synthesis"
                            );
                            text = grounded_abstention(&claim, chunks.len().min(12));
                            action = "abstained_weak_evidence";
                            return GateOutcome {
                                text,
                                meta: serde_json::json!({
                                    "surface": profile.surface.id(),
                                    "action": action,
                                    "retried": false,
                                    "violation_prob": final_vp,
                                    "threshold": tau,
                                    "top_similarity": sim,
                                    "retry_floor": floor,
                                    "mode": "single_claim",
                                    "draft": draft_for_meta,
                                }),
                            };
                        }
                    }
                    retried = true;
                    let mut retry_req = base_request.clone();
                    let base_sys = retry_req.system_message.clone().unwrap_or_default();
                    retry_req.system_message = Some(format!(
                        "{base_sys}{}",
                        retry_system_note(&claim, &v.claim_evidence)
                    ));
                    retry_req.assistant_prefix = None;
                    match inference.complete(&retry_req).await {
                        Ok(resp) => {
                            // Truncation trace (2026-06-30): the gate's non-streaming
                            // retry bypasses the synth.truncation glassbox — log its
                            // finish vs cap so a silent Length cut here is visible.
                            tracing::info!(
                                target: "gate.call",
                                kind = "retry",
                                finish = ?resp.finish_reason,
                                completion_tokens = ?resp.completion_tokens,
                                max_tokens = ?retry_req.max_tokens,
                                resp_chars = resp.text.chars().count(),
                                "gate internal completion"
                            );
                            let second = resp.text;
                            // Same structural strip on the retry: the documented
                            // leak is a retry that re-asserts the fabrication
                            // wearing the GK caveat and slips the exemption.
                            let verify_second = if entity_anchored {
                                strip_gk_caveat(&second)
                            } else {
                                second.clone()
                            };
                            match verify_grounding(
                                inference,
                                question,
                                &verify_second,
                                chunks,
                                entity_anchored,
                                evidence.searcher.as_ref(),
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
        top_similarity = ?evidence.top_similarity,
        "grounding gate verdict"
    );
    GateOutcome {
        text,
        meta: serde_json::json!({
            "surface": profile.surface.id(),
            "action": action,
            "retried": retried,
            "violation_prob": final_vp,
            "threshold": tau,
            "mode": "single_claim",
            "draft": draft_for_meta,
        }),
    }
}

/// Decode-committed opening for the long-form rewrite. Instruction-only
/// shape rules measured non-compliant (v14: the rewrite still led with
/// "I do not have access to passages detailing…" despite an explicit
/// "do not open with what the passages lack" rule — same ~60%
/// instruction-wall as the GK caveat). Committing the opening forces
/// the rewrite to continue into the supported account; the abstain
/// read of a disclaimer-led head disappears structurally. Like
/// GK_CAVEAT_PREFIX, assistant_prefix is decode-commit only — the
/// caller must prepend it to the returned text.
pub const LONGFORM_REWRITE_PREFIX: &str =
    "From the retrieved sources, here is what can be established:\n\n";

/// Rewrite-request system note: every failed claim, each with the
/// passages its targeted corpus search returned (when any). The
/// correction material is the point — v13c/v14/v14b measured that a
/// rewrite told only WHICH assertions failed, with no passages
/// stating the truth, can only delete and disclaim.
fn rewrite_system_note(failed: &[FailedClaim]) -> String {
    const REWRITE_EVIDENCE_PER_CLAIM: usize = 2;
    const REWRITE_EVIDENCE_CHARS: usize = 700;
    let list = failed
        .iter()
        .map(|f| {
            let mut entry = format!("- \"{}\"", f.claim);
            if f.evidence.is_empty() {
                entry.push_str(
                    "\n  (no corpus passage states this — remove it, or say the \
                     sources do not establish it)",
                );
            } else {
                entry.push_str("\n  What the sources actually say on this point:");
                for p in f.evidence.iter().take(REWRITE_EVIDENCE_PER_CLAIM) {
                    let trimmed: String = p.chars().take(REWRITE_EVIDENCE_CHARS).collect();
                    entry.push_str(&format!("\n  | {}", trimmed.replace('\n', "\n  | ")));
                }
            }
            entry
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "\n\nGROUNDING AUDIT FAILED on your previous draft. These assertions did not \
         verify against the sources:\n{list}\n\
         Rewrite the answer: keep everything the sources support. For each failed \
         assertion that has corrective passages above, REPLACE it with what those \
         passages actually state, citing them — do not merely delete it. Structure \
         the rewrite as an ANSWER, not a disclaimer: open directly with the \
         supported account, organized to address the question. Do not open with \
         what the sources lack, and do not enumerate the removed assertions in the \
         body. If material gaps remain, note them briefly in a single short \
         paragraph at the end."
    )
}

/// Cross-passage support check for ONE long-form claim: the top
/// passages are presented TOGETHER and the judge answers whether they
/// jointly state or imply the claim. Long-form synthesis legitimately
/// assembles claims across passages — per-chunk max-support is
/// structurally biased against exactly that (the bench critic's
/// documented blind spot; measured v13: a correct maximal essay was
/// rewritten into hedging because its synthesis claims had no single

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
    evidence: &EvidenceContext,
    base_request: &CompletionRequest,
    profile: &GroundingProfile,
) -> GateOutcome {
    let tau = profile.tau;
    let chunks: &[String] = &evidence.chunks;
    let per_claim_chunks = profile.max_chunks;
    let max_claims = profile.max_claims;
    let audit = |text: String| {
        let inference = inference.clone();
        let searcher = evidence.searcher.clone();
        async move {
            let claims = extract_claim_list(&inference, question, &text).await?;
            let mut failed: Vec<FailedClaim> = Vec::new();
            for claim in claims.iter().take(max_claims) {
                // Claim-conditioned retrieval: verify against the
                // sealed CORPUS, not just the prompt snapshot. Hits
                // go first (most relevant to THIS claim) and the cap
                // widens by their count, so they never displace a
                // prompt chunk the old audit would have judged.
                let extra = match &searcher {
                    Some(s) => {
                        let hits = s.search(claim).await;
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
                let dedup: HashSet<String> = extra
                    .iter()
                    .map(|c| c.chars().take(120).collect::<String>())
                    .collect();
                let mut judged: Vec<String> = extra.clone();
                judged.extend(
                    chunks
                        .iter()
                        .filter(|c| !dedup.contains(&c.chars().take(120).collect::<String>()))
                        .cloned(),
                );
                let cap = per_claim_chunks + extra.len();
                match claim_violation_joint(&inference, claim, &judged, cap).await {
                    Some(vp) => {
                        dbg(&format!("longform claim vp={vp:.3} {claim:?}"));
                        if vp >= tau {
                            failed.push(FailedClaim {
                                claim: claim.clone(),
                                evidence: extra,
                            });
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
                "surface": profile.surface.id(),
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
                "surface": profile.surface.id(),
                "action": "released", "retried": false,
                "claims_checked": n_claims, "failed_claims": [],
                "threshold": tau, "mode": "per_claim",
            }),
        };
    }
    if !profile.retry {
        // Verify-only surfaces: annotate the draft with the failed
        // claims — no second synthesis. The caller decides whether
        // an annotated draft is acceptable (Refinement keeps the
        // prior verified answer instead).
        let failed_claims: Vec<String> = failed.into_iter().map(|f| f.claim).collect();
        let note = format!(
            "\n\n---\n*Verification note: the following could not be \
             confirmed against your sources — treat as unverified:*\n{}",
            failed_claims
                .iter()
                .map(|c| format!("- {c}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        return GateOutcome {
            text: format!("{text}{note}"),
            meta: serde_json::json!({
                "surface": profile.surface.id(),
                "action": "annotated_no_retry", "retried": false,
                "claims_checked": n_claims, "failed_claims": failed_claims,
                "threshold": tau, "mode": "per_claim",
            }),
        };
    }
    dbg(&format!("longform rewrite: {} failed of {n_claims}", failed.len()));
    let mut rewrite_req = base_request.clone();
    let base_sys = rewrite_req.system_message.clone().unwrap_or_default();
    rewrite_req.system_message = Some(format!("{base_sys}{}", rewrite_system_note(&failed)));
    rewrite_req.assistant_prefix = Some(LONGFORM_REWRITE_PREFIX.to_string());
    match inference.complete(&rewrite_req).await {
        Ok(resp) => {
            // Truncation trace (2026-06-30): the longform rewrite is non-streaming
            // and bypasses synth.truncation — log its finish vs cap so a silent
            // Length cut on the rewrite (the prime suspect) is visible.
            tracing::info!(
                target: "gate.call",
                kind = "rewrite",
                finish = ?resp.finish_reason,
                completion_tokens = ?resp.completion_tokens,
                max_tokens = ?rewrite_req.max_tokens,
                resp_chars = resp.text.chars().count(),
                "gate internal completion"
            );
            let second = format!("{LONGFORM_REWRITE_PREFIX}{}", resp.text);
            let second_backup = second.clone();
            match audit(second).await {
                Some((text2, n2, failed2)) if failed2.is_empty() => GateOutcome {
                    text: text2,
                    meta: serde_json::json!({
                        "surface": profile.surface.id(),
                        "action": "rewrite_released", "retried": true,
                        "claims_checked": n2, "failed_claims": [],
                        "threshold": tau, "mode": "per_claim",
                    }),
                },
                Some((text2, n2, failed2)) => {
                    let failed_claims: Vec<String> =
                        failed2.into_iter().map(|f| f.claim).collect();
                    let note = format!(
                        "\n\n---\n*Verification note: the following could not be \
                         confirmed against your sources — treat as unverified:*\n{}",
                        failed_claims
                            .iter()
                            .map(|c| format!("- {c}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                    GateOutcome {
                        text: format!("{text2}{note}"),
                        meta: serde_json::json!({
                            "action": "rewrite_annotated", "retried": true,
                            "claims_checked": n2, "failed_claims": failed_claims,
                            "threshold": tau, "mode": "per_claim",
                        }),
                    }
                }
                None => GateOutcome {
                    text: second_backup,
                    meta: serde_json::json!({
                        "surface": profile.surface.id(),
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
            let failed_claims: Vec<String> = failed.into_iter().map(|f| f.claim).collect();
            let note = format!(
                "\n\n---\n*Verification note: the following could not be \
                 confirmed against your sources — treat as unverified:*\n{}",
                failed_claims
                    .iter()
                    .map(|c| format!("- {c}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            GateOutcome {
                text: format!("{text}{note}"),
                meta: serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": "annotated_rewrite_error", "retried": false,
                    "claims_checked": n_claims, "failed_claims": failed_claims,
                    "threshold": tau, "mode": "per_claim",
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::error::{Error, Result};
    use crate::types::{Depth, ProviderCapabilities};
    use crate::types::CompletionResponse;
    use futures::Stream;
    use std::pin::Pin;

    /// Prompt-routing mock for the gate's judge calls: claim
    /// extraction returns a fixed claim; every forced-choice support
    /// check returns `support` (as a logprob A/B distribution).
    struct GateMock {
        support: bool,
    }

    #[async_trait::async_trait]
    impl crate::traits::InferenceProvider for GateMock {
        async fn complete(
            &self,
            request: &crate::types::CompletionRequest,
        ) -> Result<CompletionResponse> {
            let text = if request
                .structured_output
                .as_ref()
                .map(|s| s.to_string().contains("x_forced_choice"))
                .unwrap_or(false)
            {
                if self.support {
                    r#"{"A": 0.98, "B": 0.02}"#.to_string()
                } else {
                    r#"{"A": 0.02, "B": 0.98}"#.to_string()
                }
            } else if request.prompt.contains("single central factual claim") {
                "The shop is located on Crescent Lane.".to_string()
            } else {
                "unexpected synthesis call".to_string()
            };
            Ok(CompletionResponse {
                text,
                tokens_used: 0,
                prompt_tokens: 0,
                model_id: "gate-mock".into(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            })
        }

        async fn complete_stream(
            &self,
            _request: &crate::types::CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            Err(Error::NotImplemented("GateMock: no streaming".into()))
        }

        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![])
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: true,
                relative_speed: crate::types::Speed::Fast,
                relative_reasoning: Depth::Moderate,
            }
        }
    }

    fn refinement_evidence() -> EvidenceContext {
        EvidenceContext {
            chunks: vec!["The shop sits on Harbour Row, by the quay.".to_string()],
            source_labels: Vec::new(),
            searcher: None,
            entity_anchored: false,
            top_similarity: None,
        }
    }

    /// The Phase-6 invariant's gate half: verify-only (retry: false)
    /// on an unsupported claim must return `abstained_no_retry` — the
    /// caller (collaboration refinement) keeps the verified original.
    #[tokio::test]
    async fn verify_only_failure_is_abstained_no_retry() {
        let inference: Arc<dyn crate::traits::InferenceProvider> =
            Arc::new(GateMock { support: false });
        let profile = GateSurface::Refinement.profile();
        assert!(!profile.retry);
        let outcome = gate_answer(
            &inference,
            "Where is the shop?",
            "The shop is on Crescent Lane.".to_string(),
            &refinement_evidence(),
            &CompletionRequest::default(),
            &profile,
        )
        .await;
        assert_eq!(
            outcome.meta.get("action").and_then(|a| a.as_str()),
            Some("abstained_no_retry")
        );
        // grounded_abstention was rewritten (2026-06-17) to stop restating the
        // rejected claim verbatim (it leaked the fabrication + read as "answered"
        // to the primary judge), then re-toned (2026-06-30) to drop the abrupt
        // "so I'm not going to state one" lecture for a warm, helpful refusal.
        // The action is the invariant; the wording is graceful, not brusque.
        assert!(outcome.text.starts_with("I looked through the"));
        assert!(!outcome.text.contains("not going to state"));
    }

    /// Supported claims release unchanged under verify-only.
    #[tokio::test]
    async fn verify_only_supported_claim_releases() {
        let inference: Arc<dyn crate::traits::InferenceProvider> =
            Arc::new(GateMock { support: true });
        let profile = GateSurface::Refinement.profile();
        let draft = "The shop is on Harbour Row.".to_string();
        let outcome = gate_answer(
            &inference,
            "Where is the shop?",
            draft.clone(),
            &refinement_evidence(),
            &CompletionRequest::default(),
            &profile,
        )
        .await;
        assert_eq!(
            outcome.meta.get("action").and_then(|a| a.as_str()),
            Some("released")
        );
        assert_eq!(outcome.text, draft);
    }

    fn fc(claim: &str, evidence: &[&str]) -> FailedClaim {
        FailedClaim {
            claim: claim.to_string(),
            evidence: evidence.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn rewrite_note_includes_corrective_passages() {
        let note = rewrite_system_note(&[fc(
            "Verloc instructs The Professor to bomb the Observatory.",
            &["Mr Vladimir tells Verloc the attack must be against the first meridian."],
        )]);
        assert!(note.contains("Verloc instructs The Professor"));
        assert!(
            note.contains("first meridian"),
            "corrective passage must reach the rewrite prompt"
        );
        assert!(note.contains("REPLACE"));
    }

    #[test]
    fn rewrite_note_marks_claims_with_no_corpus_support() {
        let note = rewrite_system_note(&[fc("Mrs Veronica Verloc shoots her husband.", &[])]);
        assert!(note.contains("no corpus passage states this"));
    }

    #[test]
    fn rewrite_note_caps_evidence_per_claim_and_length() {
        let long = "x".repeat(2_000);
        let note = rewrite_system_note(&[fc("c", &[&long, &long, &long])]);
        // 2 passages max, 700 chars each — the note stays prompt-sized.
        assert!(note.matches("  | ").count() <= 2);
        assert!(note.len() < 2_200);
    }

    #[test]
    fn rewrite_note_commits_answer_shape_rules() {
        let note = rewrite_system_note(&[fc("c", &[])]);
        assert!(note.contains("Do not open with what the sources lack"));
    }
}
